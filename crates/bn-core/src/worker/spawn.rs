//! Overcommit protection for spawned agents (#720, epic #707 M1). Every admitted agent is wrapped
//! in a cgroup memory cap so a runaway `cargo`/Node build is killed by the kernel rather than
//! dragging the whole host into swap OOM.
//!
//! Preferred: `systemd-run --scope -p MemoryMax=${hard}M -p MemoryHigh=${soft}M -- <cmd>` (cgroup
//! v2 — a hard ceiling with a soft throttle band). Degraded fallback where `systemd-run` is
//! absent (e.g. a non-systemd container): `sh -c 'ulimit -v <hardKB>; exec <cmd>'` — a per-process
//! address-space cap (coarser: no soft band, per-process not per-cgroup), which is why it is only
//! the fallback.
//!
//! The builders are pure (they take a `has_systemd_run` flag rather than probing the host) so the
//! wrapper shape is unit-testable without systemd; the CLI passes the real detection result.

/// The two cgroup memory thresholds for a wrapped agent, in MB. `hard` is the kernel OOM ceiling
/// (`MemoryMax`); `soft` is the reclaim-pressure band (`MemoryHigh`) — the kernel throttles the
/// cgroup and reclaims aggressively above it before the hard kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCap {
    pub hard_mb: u64,
    pub soft_mb: u64,
}

impl MemoryCap {
    /// Derive a cap from an agent's memory estimate: the estimate is the hard ceiling and the soft
    /// band sits at 80% of it (leaving a reclaim runway before the OOM kill). A zero estimate
    /// yields a zero cap, which [`wrap`] treats as "no cap".
    pub fn from_estimate_mb(estimate_mb: u64) -> MemoryCap {
        MemoryCap {
            hard_mb: estimate_mb,
            soft_mb: estimate_mb * 4 / 5,
        }
    }
}

/// Build the `systemd-run --scope` argv wrapping `inner`, applying `cap`. Transient scope
/// (`--scope`), non-interactive/quiet (`--collect`/`--quiet`), with the cgroup-v2 memory
/// properties. Pure.
pub fn systemd_run_scope(cap: MemoryCap, inner: &[String]) -> Vec<String> {
    let mut argv = vec![
        "systemd-run".to_string(),
        "--user".to_string(),
        "--scope".to_string(),
        "--quiet".to_string(),
        "--collect".to_string(),
        format!("-pMemoryMax={}M", cap.hard_mb),
        format!("-pMemoryHigh={}M", cap.soft_mb),
        "--".to_string(),
    ];
    argv.extend_from_slice(inner);
    argv
}

/// Build the degraded `ulimit -v` fallback: `sh -c 'ulimit -v <hardKB>; exec "$@"' bn-worker
/// <inner...>`. `-v` is an address-space (virtual memory) cap in KB — coarser than a cgroup
/// (per-process, no soft band) but universally available. Pure.
pub fn ulimit_v_wrapper(cap: MemoryCap, inner: &[String]) -> Vec<String> {
    let hard_kb = cap.hard_mb.saturating_mul(1024);
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("ulimit -v {hard_kb}; exec \"$@\""),
        "bn-worker".to_string(),
    ];
    argv.extend_from_slice(inner);
    argv
}

/// Wrap `inner` in the best available memory cap. With `cap.hard_mb == 0` (no estimate) the command
/// is returned unwrapped. Otherwise prefers [`systemd_run_scope`] when `has_systemd_run`, falling
/// back to [`ulimit_v_wrapper`]. Pure — the CLI supplies `has_systemd_run` from a real probe.
pub fn wrap(cap: MemoryCap, has_systemd_run: bool, inner: &[String]) -> Vec<String> {
    if cap.hard_mb == 0 {
        return inner.to_vec();
    }
    if has_systemd_run {
        systemd_run_scope(cap, inner)
    } else {
        ulimit_v_wrapper(cap, inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner() -> Vec<String> {
        vec!["agent".to_string(), "--flag".to_string()]
    }

    #[test]
    fn cap_from_estimate_sets_soft_below_hard() {
        let cap = MemoryCap::from_estimate_mb(2000);
        assert_eq!(cap.hard_mb, 2000);
        assert_eq!(cap.soft_mb, 1600);
    }

    #[test]
    fn systemd_scope_carries_memory_properties_and_inner() {
        let argv = systemd_run_scope(
            MemoryCap {
                hard_mb: 2000,
                soft_mb: 1600,
            },
            &inner(),
        );
        assert_eq!(argv[0], "systemd-run");
        assert!(argv.contains(&"--scope".to_string()));
        assert!(argv.contains(&"-pMemoryMax=2000M".to_string()));
        assert!(argv.contains(&"-pMemoryHigh=1600M".to_string()));
        // Inner command follows the `--` separator, unmangled.
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["agent", "--flag"]);
    }

    #[test]
    fn ulimit_fallback_sets_v_in_kb_and_execs_inner() {
        let argv = ulimit_v_wrapper(
            MemoryCap {
                hard_mb: 2000,
                soft_mb: 1600,
            },
            &inner(),
        );
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].contains("ulimit -v 2048000"));
        assert!(argv[2].contains("exec \"$@\""));
        assert_eq!(&argv[argv.len() - 2..], &["agent", "--flag"]);
    }

    #[test]
    fn wrap_prefers_systemd_when_available() {
        let argv = wrap(MemoryCap::from_estimate_mb(1000), true, &inner());
        assert_eq!(argv[0], "systemd-run");
    }

    #[test]
    fn wrap_falls_back_to_ulimit_without_systemd() {
        let argv = wrap(MemoryCap::from_estimate_mb(1000), false, &inner());
        assert_eq!(argv[0], "sh");
    }

    #[test]
    fn wrap_leaves_command_unwrapped_when_no_cap() {
        let argv = wrap(MemoryCap::from_estimate_mb(0), true, &inner());
        assert_eq!(argv, inner());
    }
}
