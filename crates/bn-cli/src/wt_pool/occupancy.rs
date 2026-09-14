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

/// Whether any running process has its cwd inside `slot`.
///
/// Answers on every platform, and reports *occupied* when the process table cannot be read.
pub(crate) fn any_proc_in(slot: &Path) -> bool {
    slot_occupied_by_process(process_cwds_cached().as_deref().map(Vec::as_slice), slot)
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
