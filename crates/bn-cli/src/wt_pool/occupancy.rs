//! Is anybody working in this slot? — the process-level half of pool occupancy.
//!
//! Occupancy is what stops `bn dispatch` from switching a branch into a checkout somebody is
//! already working in, so it is the one pool signal that must never guess *idle*. This module
//! exists because it used to.
//!
//! The old `any_proc_in` read `/proc` and was compiled out entirely off Linux:
//!
//! ```text
//! #[cfg(not(target_os = "linux"))]
//! fn any_proc_in(_slot: &Path) -> bool { false }
//! ```
//!
//! On macOS that is not "best-effort", it is a constant `false` — the process signal was simply
//! absent, leaving occupancy resting entirely on tmux window identity. When a slot's identity
//! drifted (a window renamed, a branch switched under it), the ledger called the slot idle,
//! `reclaim_leaked_leases` handed it back, and the next dispatch switched a new branch into a
//! checkout with a live agent in it. The agent's worktree moved under its feet and it exited —
//! cleanly, printing `Resume this session with: …`, which is why it read as a crash rather than as
//! a reclaim.
//!
//! Two rules follow, and both are the point of this module:
//!
//! **Every platform gets a real answer.** [`crate::reap::process_cwds`] already reads `/proc` on
//! Linux and shells out to one `lsof -d cwd` pass elsewhere, and its parser is unit-tested. Pool
//! occupancy asks that, rather than keeping a second, weaker copy that silently degrades.
//!
//! **Unknown is not empty.** "I could not look" and "nobody is there" must be different answers,
//! because only one of them is safe to act on. An unreadable process table reports *occupied*: the
//! cost of being wrong is a slot that stays leased until someone runs `wt pool return --force`,
//! against the cost of killing a running agent and losing its session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a captured process-cwd table stays usable.
///
/// A sweep asks about every slot in turn, and off Linux each miss would otherwise be its own
/// `lsof` pass — ten slots, ten full scans of the process table. One capture serves the whole
/// pass. It is a TTL and not a `OnceLock` because a bn process can be long-lived: a table
/// cached for its lifetime would answer for a fleet that has since turned over completely, and a
/// stale table is exactly the wrong thing to decide occupancy from.
const PROC_CWD_TTL: Duration = Duration::from_secs(2);

/// A captured process-cwd table: every `(pid, cwd)` on the machine, shared between the callers
/// that read it within one [`PROC_CWD_TTL`] window.
type CwdTable = Arc<Vec<(i32, PathBuf)>>;

/// The last captured table and when it was taken. `None` until the first capture.
static PROC_CWDS: Mutex<Option<(Instant, CwdTable)>> = Mutex::new(None);

/// Every running process's cwd, captured at most once per [`PROC_CWD_TTL`].
///
/// `None` when the table could not be read. A live machine always has at least this process in it,
/// so an *empty* result never means "nothing is running" — it means the probe failed (no `lsof` on
/// PATH, an unreadable `/proc`) and the caller must not read it as an idle slot.
fn process_cwds_cached() -> Option<CwdTable> {
    // A poisoned lock is not a reason to answer "idle": fall through to a fresh, uncached capture.
    if let Ok(cache) = PROC_CWDS.lock() {
        if let Some((at, rows)) = cache.as_ref() {
            if at.elapsed() < PROC_CWD_TTL {
                return Some(Arc::clone(rows));
            }
        }
    }
    let rows = crate::reap::process_cwds();
    if rows.is_empty() {
        return None;
    }
    let rows = Arc::new(rows);
    if let Ok(mut cache) = PROC_CWDS.lock() {
        *cache = Some((Instant::now(), Arc::clone(&rows)));
    }
    Some(rows)
}

/// Whether any process in `cwds` is working inside `slot` — the pure decision, so the fail-closed
/// rule is tested rather than re-derived at each call site.
///
/// `None` is an unreadable process table and answers `true`. See the module docs for why that
/// direction is the safe one.
pub(crate) fn slot_occupied_by_process(cwds: Option<&[(i32, PathBuf)]>, slot: &Path) -> bool {
    match cwds {
        None => true,
        Some(rows) => rows.iter().any(|(_, cwd)| cwd.starts_with(slot)),
    }
}

/// The pids whose cwd is inside `slot`. `None` is an unreadable process table.
fn pids_in(cwds: Option<&[(i32, PathBuf)]>, slot: &Path) -> Option<Vec<i32>> {
    let rows = cwds?;
    Some(
        rows.iter()
            .filter(|(_, cwd)| cwd.starts_with(slot))
            .map(|(pid, _)| *pid)
            .collect(),
    )
}

/// Whether a process is a build daemon the checkout started rather than somebody working in it.
///
/// A tenant is reachable: an agent runs under a tmux pane and a person runs under a shell, so
/// either way there is a controlling terminal and a parent that is not init. A build daemon
/// double-forks and outlives whatever started it, landing on `ppid == 1` with no tty.
///
/// Deliberately a shape rather than a list of names. `nx`, `tsserver`, `vite`, `esbuild`,
/// `watchman` and the next one all share this shape, and a name list would be stale the week after
/// it was written.
fn is_detached_daemon(ppid: i32, tty: &str) -> bool {
    let detached = ppid <= 1;
    let no_terminal = matches!(tty.trim(), "" | "?" | "??" | "-");
    detached && no_terminal
}

/// `(pid, ppid, tty, command)` for each pid, or `None` when the lookup failed.
///
/// One `ps` for the handful of pids found in a slot, not a second full table scan. Empty output
/// for a non-empty request is a failure, not an answer: every pid came from a table captured
/// moments ago, so at least one should still resolve.
fn process_kinds(pids: &[i32]) -> Option<Vec<(i32, i32, String, String)>> {
    if pids.is_empty() {
        return Some(Vec::new());
    }
    let list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let out = std::process::Command::new("ps")
        .args(["-o", "pid=,ppid=,tty=,command=", "-p", &list])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rows = parse_ps_kinds(&String::from_utf8_lossy(&out.stdout));
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// Parses `ps -o pid=,ppid=,tty=,command=` output. Split out so the fail-closed rule is tested.
fn parse_ps_kinds(text: &str) -> Vec<(i32, i32, String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            // A missing tty column means no controlling terminal.
            let tty = f.next().unwrap_or("").to_string();
            let command = f.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, tty, command))
        })
        .collect()
}

/// Whether anything in the slot is a tenant rather than a build daemon.
///
/// `None` kinds is a failed lookup and answers *occupied*, the same direction as an unreadable
/// process table: see the module docs.
fn tenants_among(pids: &[i32], kinds: Option<&[(i32, i32, String, String)]>) -> bool {
    if pids.is_empty() {
        return false;
    }
    let Some(kinds) = kinds else {
        return true;
    };
    pids.iter().any(|pid| {
        // A pid that exited between the two probes resolves to nothing. Treat it as gone rather
        // than as a tenant: the slot it vanished from is genuinely free of it.
        kinds
            .iter()
            .find(|(p, _, _, _)| p == pid)
            .is_some_and(|(_, ppid, tty, command)| {
                !is_bn_tmux_git(command) && !is_detached_daemon(*ppid, tty)
            })
    })
}

/// Whether the process is bn's own tmux-git poller.
fn is_bn_tmux_git(command: &str) -> bool {
    let mut parts = command.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(bin), Some("tmux-git")) => bin
            .rsplit('/')
            .next()
            .is_some_and(|base| base.ends_with("bn")),
        _ => false,
    }
}

/// Whether anybody is working inside `slot`.
///
/// Answers on every platform, and reports *occupied* when the process table cannot be read.
///
/// Build daemons do not count. An Nx daemon (and every other watcher the build starts) detaches
/// and keeps its cwd in the slot long after the agent that spawned it is gone, so counting it
/// pinned the lease permanently: `wt pool return-unused` reported "someone is in it" forever and
/// no sweep could ever free the slot. Four wira360 slots were held that way by daemons running for
/// hours with no window and no agent.
pub(crate) fn any_proc_in(slot: &Path) -> bool {
    let table = process_cwds_cached();
    let rows = table.as_deref().map(Vec::as_slice);

    // Step one: is anything in the slot at all? Also the fail-closed gate — an unreadable table
    // answers `true` here and again below, so "could not look" never reaches the daemon check.
    if !slot_occupied_by_process(rows, slot) {
        return false;
    }
    let Some(pids) = pids_in(rows, slot) else {
        return true;
    };

    // Step two: is any of it a tenant rather than a daemon the build left behind?
    tenants_among(&pids, process_kinds(&pids).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(paths: &[&str]) -> Vec<(i32, PathBuf)> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i as i32 + 1, PathBuf::from(p)))
            .collect()
    }

    #[test]
    fn a_detached_daemon_is_not_a_tenant() {
        // The Nx daemon this fix exists for: ppid 1, no controlling terminal.
        assert!(is_detached_daemon(1, "??"));
        assert!(is_detached_daemon(1, "?"));
        assert!(is_detached_daemon(1, "-"));
        assert!(is_detached_daemon(1, ""));
    }

    #[test]
    fn a_process_on_a_terminal_is_a_tenant() {
        // An agent under a tmux pane, or a person in a shell.
        assert!(!is_detached_daemon(1, "ttys004"));
        assert!(!is_detached_daemon(4242, "??"));
        assert!(!is_detached_daemon(4242, "ttys004"));
    }

    #[test]
    fn a_slot_holding_only_daemons_is_free() {
        let kinds = vec![
            (10, 1, "??".to_string(), "nx".to_string()),
            (11, 1, "?".to_string(), "nx".to_string()),
        ];
        assert!(!tenants_among(&[10, 11], Some(&kinds)));
    }

    #[test]
    fn one_tenant_among_daemons_still_occupies() {
        let kinds = vec![
            (10, 1, "??".to_string(), "nx".to_string()),
            (11, 900, "ttys004".to_string(), "nx".to_string()),
        ];
        assert!(tenants_among(&[10, 11], Some(&kinds)));
    }

    #[test]
    fn a_failed_kind_lookup_reads_as_occupied() {
        // Same direction as an unreadable process table: "could not look" is never "idle".
        assert!(tenants_among(&[10], None));
    }

    #[test]
    fn a_pid_that_exited_between_probes_does_not_occupy() {
        assert!(!tenants_among(&[10], Some(&[])));
    }

    #[test]
    fn the_bn_tmux_git_poller_is_not_a_tenant() {
        let kinds = vec![(
            10,
            900,
            "ttys004".to_string(),
            "bn tmux-git /home/me/projects/worktree/BigMono/_pool/2".to_string(),
        )];
        assert!(!tenants_among(&[10], Some(&kinds)));
    }

    #[test]
    fn ps_rows_without_a_tty_column_parse_as_no_terminal() {
        let rows = parse_ps_kinds(
            "  123     1 ?? bn tmux-git /tmp\n  456   123 ttys004 nx\n  789     1  nx\n",
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            (123, 1, "??".to_string(), "bn tmux-git /tmp".to_string())
        );
        assert_eq!(rows[1], (456, 123, "ttys004".to_string(), "nx".to_string()));
        assert_eq!(rows[2], (789, 1, "nx".to_string(), String::new()));
    }

    #[test]
    fn pids_in_keeps_the_unreadable_table_distinct_from_an_empty_slot() {
        let table = rows(&["/w/repo/_pool/7", "/elsewhere"]);
        assert_eq!(
            pids_in(Some(&table), Path::new("/w/repo/_pool/7")),
            Some(vec![1])
        );
        assert_eq!(
            pids_in(Some(&table), Path::new("/w/repo/_pool/9")),
            Some(vec![])
        );
        assert_eq!(pids_in(None, Path::new("/w/repo/_pool/7")), None);
    }

    #[test]
    fn unreadable_process_table_reads_as_occupied() {
        // The regression this module exists for: "could not look" must never answer "idle", or the
        // pool leases a slot with a live agent in it and dispatch kills the agent.
        assert!(slot_occupied_by_process(None, Path::new("/w/repo/_pool/7")));
    }

    #[test]
    fn empty_process_table_is_not_reachable_as_idle() {
        // An empty *slice* is a genuine "nothing is in this slot" answer — distinct from `None`.
        // `process_cwds_cached` never hands one back, because a machine with zero processes on it
        // is not a state that exists; it maps that case to `None` instead.
        assert!(!slot_occupied_by_process(
            Some(&[]),
            Path::new("/w/repo/_pool/7")
        ));
    }

    #[test]
    fn a_process_inside_the_slot_occupies_it() {
        let table = rows(&["/w/repo/_pool/7", "/elsewhere"]);
        assert!(slot_occupied_by_process(
            Some(&table),
            Path::new("/w/repo/_pool/7")
        ));
    }

    #[test]
    fn a_process_below_the_slot_occupies_it() {
        // An agent's shell sits in a subdirectory far more often than at the checkout root.
        let table = rows(&["/w/repo/_pool/7/crates/bn-cli/src"]);
        assert!(slot_occupied_by_process(
            Some(&table),
            Path::new("/w/repo/_pool/7")
        ));
    }

    #[test]
    fn a_sibling_slot_does_not_occupy_it() {
        // `starts_with` is per path *component*, so slot 7 must not be occupied by slot 70 — the
        // string-prefix version of this check would have said it was.
        let table = rows(&["/w/repo/_pool/70", "/w/repo/_pool/7x"]);
        assert!(!slot_occupied_by_process(
            Some(&table),
            Path::new("/w/repo/_pool/7")
        ));
    }

    #[test]
    fn an_unrelated_checkout_does_not_occupy_it() {
        let table = rows(&["/w/other/_pool/7", "/home/me"]);
        assert!(!slot_occupied_by_process(
            Some(&table),
            Path::new("/w/repo/_pool/7")
        ));
    }
}
