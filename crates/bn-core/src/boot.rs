//! This boot's unique id — the marker that lets a durable record tell "written by a process that
//! is still around" from "written before the machine restarted".
//!
//! A lease, a snapshot, or any other file that outlives the process which wrote it has the same
//! problem: the pid it recorded means nothing after a reboot, because the kernel recycles pids and
//! a *new* process may now answer to it. A value that changes on every (re)boot settles it — a
//! record tagged with a different id predates the current boot, so its writer is provably gone no
//! matter what the pid check says.
//!
//! # Two different questions
//!
//! "Is this record from this boot?" and "did this happen before the machine came back?" look
//! alike and need *different* sources, which is the distinction that makes the `kern.boottime`
//! drift below harmless rather than fatal:
//!
//! | | source | stable? | used for |
//! |---|---|---|---|
//! | [`current_boot_id`] — identity | `/proc/sys/kernel/random/boot_id`; `kern.bootsessionuuid` | exactly | equality |
//! | [`boot_started_at`] — timestamp | `/proc/stat` `btime`; `kern.boottime` `sec` | ±1s | a threshold |
//!
//! A ±1s wobble is meaningless against the 30-minute window a caller compares it to, and it never
//! reaches an `==`. One identity source per platform, never two — a second format for the same
//! boot would compare unequal to itself.
//!
//! # Why identity is not `kern.boottime`
//!
//! It is the obvious choice and it is wrong. `kern.boottime` is **derived** — calendar-now minus
//! uptime — so XNU recomputes it on clock set and on sleep/wake, and it drifts *within a single
//! boot*. Measured on one host at uptime 2:09, with no reboot in between:
//!
//! ```text
//! { sec = 1786219538, usec = 983139 }   … 19 minutes later …   { sec = 1786219539, usec = 50068 }
//! ```
//!
//! The seconds field itself moved. [`crate::pool::lease_liveness`] compares boot ids with a bare
//! `!=`, so a drifting id makes every *live* lease read as `StalePriorBoot` mid-session — and a
//! slot reclaimed on that verdict is reset with `git switch --discard-changes` + `git clean -df`.
//! An id that can flip is therefore not merely useless, it destroys uncommitted work.
//!
//! `kern.bootsessionuuid` is a genuine per-boot UUID and is stable for the life of the boot.
//! Do **not** substitute `kern.bootuuid`: it is byte-identical to `kern.apfsprebootuuid` (the boot
//! *volume*'s uuid), so it is constant across reboots and would make every stale lease look live —
//! the same failure in the opposite direction.
//!
//! When no source answers, this returns `None` rather than a best guess. Callers then fall back to
//! a pid check, which cannot see across a reboot but never invents a false verdict. Failing closed
//! is the only safe default when the wrong answer discards work.
//!
//! The `sysctl` read goes through [`CommandRunner`] rather than `libc::sysctlbyname` because the
//! workspace denies `unsafe_code` — and it keeps the whole thing testable from `FakeRunner`.

use crate::runner::{CommandRunner, RealRunner};
use std::sync::OnceLock;

/// This boot's id, or `None` when no source can supply a trustworthy one.
///
/// Memoized: this lands on hot paths (every lease read consults it) and forking `sysctl` per call
/// would be a real regression. Both answers are constant for the life of the boot, so a process
/// that outlives a reboot is not a case that exists.
pub fn current_boot_id() -> Option<String> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| current_boot_id_with(&RealRunner)).clone()
}

/// The epoch second this boot began, for "did this happen before the machine came back?".
///
/// A *threshold*, never an identity — see the module docs. Memoized for the same reason as
/// [`current_boot_id`], with the extra benefit that a drifting source is pinned to one value for
/// the life of the process instead of wobbling between reads.
pub fn boot_started_at() -> Option<i64> {
    static AT: OnceLock<Option<i64>> = OnceLock::new();
    *AT.get_or_init(|| boot_started_at_with(&RealRunner))
}

/// [`boot_started_at`] with an injected runner, so tests can drive the `sysctl` branch.
pub fn boot_started_at_with(runner: &dyn CommandRunner) -> Option<i64> {
    if let Some(secs) = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|stat| parse_proc_stat_btime(&stat))
    {
        return Some(secs);
    }
    parse_kern_boottime(&runner.run("sysctl", &["-n", "kern.boottime"]).ok_stdout()?)
}

/// [`current_boot_id`] with an injected runner, so tests can drive the `sysctl` branch.
///
/// The `/proc` reads are left as direct filesystem access: on the platform where they matter they
/// always exist, and on the platform where they don't, they fail fast and fall through.
pub fn current_boot_id_with(runner: &dyn CommandRunner) -> Option<String> {
    if let Ok(id) = std::env::var("BN_BOOT_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    let out = runner.run("sysctl", &["-n", "kern.bootsessionuuid"]);
    parse_boot_session_uuid(&out.ok_stdout()?)
}

/// The boot epoch-seconds from the `btime <epoch_secs>` line of `/proc/stat`.
///
/// Unlike Darwin's `kern.boottime`, Linux's `btime` is recorded once at boot rather than derived
/// from the current clock — but it is still consumed as a threshold, not an identity, so the two
/// platforms behave the same way.
pub fn parse_proc_stat_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|secs| secs.trim().parse::<i64>().ok())
        .filter(|secs| *secs > 0)
}

/// The boot epoch-seconds from `sysctl -n kern.boottime`.
///
/// Darwin prints a struct-ish line, with the trailing human date present on some releases and
/// absent on others:
///
/// ```text
/// { sec = 1786219538, usec = 983139 } Sat Aug  8 23:05:38 2026
/// ```
///
/// Parsed field-wise rather than by searching for `sec = `, because that substring also occurs
/// inside `usec = `.
pub fn parse_kern_boottime(out: &str) -> Option<i64> {
    let body = out
        .trim()
        .trim_start_matches('{')
        .split('}')
        .next()
        .unwrap_or_default();
    body.split(',')
        .filter_map(|field| field.split_once('='))
        .find(|(key, _)| key.trim() == "sec")
        .and_then(|(_, value)| value.trim().parse::<i64>().ok())
        .filter(|secs| *secs > 0)
}

/// `sysctl -n kern.bootsessionuuid` output as a prefixed boot id.
///
/// Shape-checked rather than taken on trust: a `sysctl` that does not know the key can still print
/// something to stdout, and admitting a stray line as a boot id would give every record the same
/// bogus "boot". Only a canonical 8-4-4-4-12 hex UUID is accepted.
pub fn parse_boot_session_uuid(out: &str) -> Option<String> {
    let id = out.trim();
    let groups: Vec<&str> = id.split('-').collect();
    let shaped = groups.len() == 5
        && [8, 4, 4, 4, 12] == groups.iter().map(|g| g.len()).collect::<Vec<_>>()[..]
        && groups
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_hexdigit()));
    shaped.then(|| format!("bootsession:{}", id.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    /// The real value read from the host this was written on.
    const UUID: &str = "26BD7A88-07D6-45B4-823F-66D4203E9231";

    #[test]
    fn parses_boot_session_uuid() {
        assert_eq!(
            parse_boot_session_uuid(&format!("{UUID}\n")),
            Some(format!("bootsession:{UUID}"))
        );
    }

    #[test]
    fn rejects_anything_that_is_not_a_uuid() {
        assert_eq!(parse_boot_session_uuid(""), None);
        assert_eq!(parse_boot_session_uuid("unknown oid 'kern.nope'"), None);
        assert_eq!(parse_boot_session_uuid("26BD7A88-07D6-45B4-823F"), None);
        // Right shape, non-hex payload.
        assert_eq!(
            parse_boot_session_uuid("ZZBD7A88-07D6-45B4-823F-66D4203E9231"),
            None
        );
        // The `kern.boottime` struct must never be mistaken for an id.
        assert_eq!(
            parse_boot_session_uuid("{ sec = 1786219538, usec = 983139 }"),
            None
        );
    }

    /// Case-normalized so two reads of the same boot compare equal under `lease_liveness`'s bare
    /// `!=`, whatever case the platform prints.
    #[test]
    fn uuid_is_case_normalized() {
        assert_eq!(
            parse_boot_session_uuid(&UUID.to_ascii_lowercase()),
            parse_boot_session_uuid(UUID)
        );
    }

    #[test]
    fn parses_proc_stat_btime() {
        let stat = "cpu  1 2 3\nintr 0\nbtime 1786219538\nprocesses 99\n";
        assert_eq!(parse_proc_stat_btime(stat), Some(1786219538));
        assert_eq!(parse_proc_stat_btime("cpu 1 2 3\n"), None);
        assert_eq!(parse_proc_stat_btime("btime \n"), None);
        assert_eq!(parse_proc_stat_btime("btime 0\n"), None);
    }

    /// The exact string Darwin printed on the machine this was written for.
    const BOOTTIME: &str = "{ sec = 1786219538, usec = 983139 } Sat Aug  8 23:05:38 2026";

    #[test]
    fn parses_kern_boottime() {
        assert_eq!(parse_kern_boottime(BOOTTIME), Some(1786219538));
        assert_eq!(
            parse_kern_boottime("{ sec = 1786219538, usec = 983139 }"),
            Some(1786219538)
        );
        assert_eq!(parse_kern_boottime(""), None);
        assert_eq!(parse_kern_boottime("{ usec = 983139 }"), None);
        assert_eq!(parse_kern_boottime("{ sec = 0, usec = 1 }"), None);
        assert_eq!(parse_kern_boottime("{ sec = nope }"), None);
    }

    /// `usec = ` contains `sec = `; a substring scan would return the microseconds, which move
    /// constantly within one boot.
    #[test]
    fn does_not_mistake_usec_for_sec() {
        assert_eq!(parse_kern_boottime(BOOTTIME), Some(1786219538));
    }

    /// The whole point of the identity/timestamp split: the drifting source is reachable *only*
    /// as an `i64` threshold. There is no path by which it becomes a boot id.
    #[test]
    fn the_drifting_source_never_produces_an_identity() {
        assert_eq!(parse_boot_session_uuid(BOOTTIME), None);
        // Two readings one second apart — the drift observed on a real host, same boot.
        assert_ne!(
            parse_kern_boottime(BOOTTIME),
            parse_kern_boottime("{ sec = 1786219539, usec = 50068 }")
        );
    }

    /// The end-to-end probes only mean anything where the earlier sources are genuinely absent: on
    /// Linux `/proc` answers first, and an ambient `$BN_BOOT_ID` short-circuits everything. Both
    /// are properties of the machine running the suite, not of the code, so skip rather than fail.
    fn skip_fallback_probe() -> bool {
        std::path::Path::new("/proc/stat").exists()
            || std::env::var("BN_BOOT_ID").is_ok_and(|v| !v.trim().is_empty())
    }

    #[test]
    fn falls_back_to_sysctl_when_proc_is_absent() {
        if skip_fallback_probe() {
            return;
        }
        let runner = FakeRunner::new().on("sysctl", &["-n", "kern.bootsessionuuid"], UUID);
        assert_eq!(
            current_boot_id_with(&runner),
            Some(format!("bootsession:{UUID}"))
        );
    }

    /// Failing closed matters more than answering: a fabricated id feeds a reclaim that runs
    /// `git clean -df` on the slot.
    #[test]
    fn no_source_yields_none() {
        if skip_fallback_probe() {
            return;
        }
        // FakeRunner fails every unregistered call, standing in for "sysctl absent".
        assert_eq!(current_boot_id_with(&FakeRunner::new()), None);
    }

    /// The point of the module, end to end on a real host: Linux answers from `/proc`, macOS from
    /// `sysctl`. Before this existed, Darwin fell through every branch and returned `None`, which is
    /// what left `boot_id` empty in every pool lease on a Mac.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_real_unix_host_always_has_a_boot_id() {
        let id = current_boot_id().expect("this host must expose a boot id");
        assert!(!id.trim().is_empty());
        // Memoized, so two reads can never disagree even if the underlying source moves.
        assert_eq!(Some(id), current_boot_id());
    }

    /// The threshold half, end to end: a real host must be able to say when it booted, and that
    /// answer must be in the past.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_real_unix_host_knows_when_it_booted() {
        let at = boot_started_at().expect("this host must expose a boot time");
        assert!(at > 0);
        assert!(
            at <= crate::util::now_epoch_secs(),
            "boot time in the future"
        );
    }

    /// The regression this module's docs exist for. `kern.boottime` is derived from the current
    /// clock and drifts within one boot, so an id built from it makes live leases read as
    /// prior-boot. Reading it twice here would be a flaky test; instead pin the invariant that
    /// actually protects us — whatever `sysctl` says, only a UUID is ever accepted.
    #[test]
    fn boottime_output_can_never_become_a_boot_id() {
        if skip_fallback_probe() {
            return;
        }
        let drifting = FakeRunner::new().on(
            "sysctl",
            &["-n", "kern.bootsessionuuid"],
            "{ sec = 1786219539, usec = 50068 } Sat Aug  8 23:05:39 2026",
        );
        assert_eq!(current_boot_id_with(&drifting), None);
    }
}
