//! Single-owner arbitration for a repo's orchestrate run: who drives it, and how a second
//! controller decides the first one is gone.
//!
//! Split out of `orchestrate.rs` so the rule and its tests live together. The whole point of this
//! module is a liveness judgement, and the judgement had never been tested — which is how a probe
//! that answered "dead" for every live owner off Linux survived (see [`owner_liveness`]).

use serde::{Deserialize, Serialize};

/// A controller's identity for single-owner arbitration of a repo's run. Two concurrent daemons for
/// one repo (e.g. `run --all-repos` and `run --repo bn`) both adopt the same tmux lanes and
/// double-count in-flight, pinning the cap so the backlog stops pulling. The owner lease lets a
/// second controller detect a live first owner and defer, so exactly one loop drives a repo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerOwner {
    /// The driving process id.
    pub pid: u32,
    /// The host the process runs on (owners on other hosts can't be pid-probed, so they age out by
    /// [`OWNER_LEASE_TTL_SECS`] instead).
    pub host: String,
    /// Unix-epoch seconds of the last heartbeat; refreshed every tick by the owning loop.
    pub heartbeat: i64,
}

/// How long an owner lease stays authoritative without a heartbeat refresh. Comfortably above the
/// idle poll cadence (45s) so a healthy owner mid-idle-tick is never mistaken for abandoned, yet
/// short enough that a crashed owner on another host frees the repo within a couple of minutes.
pub const OWNER_LEASE_TTL_SECS: i64 = 120;

/// Whether `me` may claim (drive) a repo's run, given the recorded `existing` owner (if any),
/// whether that owner's process is still alive (`Some(true|false)` for a same-host pid probe,
/// `None` when unknowable — a different host or an unreadable record), and `now`.
///
/// **Fail-open**: yield *only* to an owner that is unambiguously live and different — a fresh
/// heartbeat within [`OWNER_LEASE_TTL_SECS`] and (when probeable) an alive pid. A crashed, stale, or
/// same-identity owner, or any uncertainty, resolves to "claim", so a transient glitch can never
/// leave a repo driverless.
pub fn may_claim_owner(
    existing: Option<&ControllerOwner>,
    me: &ControllerOwner,
    owner_alive: Option<bool>,
    now: i64,
    ttl: i64,
) -> bool {
    let Some(cur) = existing else {
        return true; // unowned
    };
    if cur.pid == me.pid && cur.host == me.host {
        return true; // our own lease (re-entrant refresh)
    }
    if owner_alive == Some(false) {
        return true; // same-host owner whose process is gone
    }
    now.saturating_sub(cur.heartbeat) >= ttl // stale heartbeat ⇒ abandoned
}

/// Same-host pid liveness for an owner: `Some(true|false)` when `owner` and `me` are on one host,
/// `None` when they are not — a remote owner cannot be probed and ages out by heartbeat TTL alone.
///
/// `pid_alive` is injected so the rule is testable without a process table. Callers pass the
/// portable `kill(pid, 0)` probe; the original inlined a `/proc/<pid>` existence check, which does
/// not exist off Linux and so answered `Some(false)` for every **live** same-host owner. That fed
/// [`may_claim_owner`]'s "same-host owner whose process is gone" branch, so on macOS a second
/// controller claimed a repo the first was actively driving — the precise double-driving
/// [`ControllerOwner`] exists to prevent: two loops adopt the same tmux lanes, double-count what is
/// in flight, pin the cap, and the backlog stops pulling.
pub fn owner_liveness(
    owner: &ControllerOwner,
    me: &ControllerOwner,
    pid_alive: impl Fn(u32) -> bool,
) -> Option<bool> {
    (owner.host == me.host).then(|| pid_alive(owner.pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(pid: u32, host: &str, heartbeat: i64) -> ControllerOwner {
        ControllerOwner {
            pid,
            host: host.to_string(),
            heartbeat,
        }
    }

    // --- owner_liveness ---------------------------------------------------

    #[test]
    fn a_live_same_host_owner_reads_alive() {
        // The regression: this answered Some(false) off Linux, for an owner that was running fine.
        let me = owner(2, "mac", 0);
        assert_eq!(
            owner_liveness(&owner(1, "mac", 0), &me, |_| true),
            Some(true)
        );
    }

    #[test]
    fn a_dead_same_host_owner_reads_dead() {
        let me = owner(2, "mac", 0);
        assert_eq!(
            owner_liveness(&owner(1, "mac", 0), &me, |_| false),
            Some(false)
        );
    }

    #[test]
    fn a_remote_owner_is_unprobeable() {
        // Probing our own process table for a pid on another box would be meaningless — and pids
        // collide across hosts, so a wrong answer here would steal a live remote owner's repo.
        let me = owner(2, "mac", 0);
        assert_eq!(
            owner_liveness(&owner(1, "linux-box", 0), &me, |_| true),
            None
        );
    }

    // --- may_claim_owner --------------------------------------------------

    const TTL: i64 = OWNER_LEASE_TTL_SECS;

    #[test]
    fn an_unowned_repo_is_claimable() {
        assert!(may_claim_owner(None, &owner(1, "mac", 0), None, 1_000, TTL));
    }

    #[test]
    fn our_own_lease_is_reclaimable_re_entrantly() {
        let me = owner(1, "mac", 900);
        assert!(may_claim_owner(Some(&me), &me, Some(true), 1_000, TTL));
    }

    #[test]
    fn a_live_owner_with_a_fresh_heartbeat_is_yielded_to() {
        // The one case that must refuse: exactly one loop may drive a repo.
        let me = owner(2, "mac", 1_000);
        let cur = owner(1, "mac", 1_000);
        assert!(!may_claim_owner(Some(&cur), &me, Some(true), 1_000, TTL));
    }

    #[test]
    fn a_dead_same_host_owner_is_claimed_immediately() {
        // No waiting out the TTL when the process is provably gone.
        let me = owner(2, "mac", 1_000);
        let cur = owner(1, "mac", 1_000);
        assert!(may_claim_owner(Some(&cur), &me, Some(false), 1_000, TTL));
    }

    #[test]
    fn an_unprobeable_owner_ages_out_by_heartbeat() {
        let me = owner(2, "mac", 1_000);
        let cur = owner(1, "linux-box", 1_000);
        // Fresh: yield, since we cannot prove it is gone.
        assert!(!may_claim_owner(
            Some(&cur),
            &me,
            None,
            1_000 + TTL - 1,
            TTL
        ));
        // Lapsed: claim.
        assert!(may_claim_owner(Some(&cur), &me, None, 1_000 + TTL, TTL));
    }
}
