//! The lease store's records — what `leases.json` holds for each leased slot. Split out of
//! `wt_pool.rs` under the file-size ratchet; the store's I/O (`read_leases`, `write_leases`, the
//! lock) stays with the pool.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize, Default)]
pub(super) struct Leases {
    pub(super) slots: BTreeMap<u32, Lease>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Lease {
    pub(super) branch: String,
    pub(super) holder: String,
    /// PID of the process that took the lease — the *caller* (parent of the `bn` invocation, e.g.
    /// the agent or shell), not the ephemeral `bn` process which exits immediately. This is what
    /// makes the lease durable yet recoverable: a slot is only reclaimable once this process is
    /// gone. See [`super::caller_pid`].
    pub(super) pid: u32,
    /// Epoch seconds when the lease was taken.
    pub(super) at: i64,
    /// The boot id of the machine when the lease was taken (see [`super::current_boot_id`]). A lease whose
    /// id differs from the current boot's was taken *before* a restart (WSL shutdown / reboot), so
    /// its holder is provably gone even if the recycled `pid` now reads as alive — this is what lets
    /// the pool recover its slots after WSL dies and comes back. Empty for leases written before
    /// boot-scoping (treated as an unknown boot → falls back to the pid check), hence `default`.
    #[serde(default)]
    pub(super) boot_id: String,
    /// A second worker slot reserved by this lease as a detached primary-branch comparison tree.
    /// The relationship is stored only on the owning feature lease, so it cannot become a
    /// half-written pair.
    #[serde(default)]
    pub(super) comparison_slot: Option<u32>,
    /// Taken by `wt pool get`: an explicit lease, held until `wt pool return`. The idle-slot
    /// sweeps judge a lease abandoned from who is *in* the slot, and a caller that leases with
    /// `--path --no-tmux` and works from its own shell never is: its holder pid (the tool call's
    /// shell) exits at once, no pane opens there, and the slot read as leaked seconds after the
    /// get — reclaimed, detached under the agent, its next commits landing on a detached HEAD. A
    /// pinned lease is only recovered after a reboot, like every lease from a prior boot.
    #[serde(default)]
    pub(super) pinned: bool,
}

impl Leases {
    pub(super) fn reserved_slots(&self) -> BTreeSet<u32> {
        self.slots
            .iter()
            .flat_map(|(&owner, lease)| [Some(owner), lease.comparison_slot])
            .flatten()
            .collect()
    }

    pub(super) fn owner_slot(&self, n: u32) -> Option<u32> {
        self.slots.contains_key(&n).then_some(n).or_else(|| {
            self.slots
                .iter()
                .find_map(|(&owner, lease)| (lease.comparison_slot == Some(n)).then_some(owner))
        })
    }

    pub(super) fn comparison_owner(&self, n: u32) -> Option<u32> {
        self.slots
            .iter()
            .find_map(|(&owner, lease)| (lease.comparison_slot == Some(n)).then_some(owner))
    }
}

/// Mark the lease on `slot` as [`Lease::pinned`]. Called by `wt pool get` right after it leases.
pub(super) fn pin(pool_dir: &Path, slot: &Path) {
    let _lock = super::lock_leases(pool_dir);
    let mut leases = super::read_leases(pool_dir);
    let Some((_, n, _)) = super::slot_parts(slot) else {
        return;
    };
    if let Some(lease) = leases.slots.get_mut(&n) {
        lease.pinned = true;
        let _ = super::write_leases(pool_dir, &leases);
    }
}

#[cfg(test)]
mod tests {
    use super::super::git_integration_tests::{head_ref, mk_bare};
    use super::super::reclaim::{reclaim_leaked_leases, reclaim_stale, ReclaimScope};
    use super::super::*;

    /// A pool with one slot leased to `feat` the way an agent's `wt pool get --path --no-tmux`
    /// leaves it: the holder pid (the tool call's shell) already gone, nobody in the checkout,
    /// nothing uncommitted, and the lease past the startup grace.
    fn abandoned_looking(pinned: bool) -> (tempfile::TempDir, PoolCtx) {
        let (tmp, bare) = mk_bare();
        TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));
        let wt_root = tmp.path().join("wt");
        let pdir = wt_root.join("bn").join("_pool");
        std::fs::create_dir_all(&pdir).unwrap();
        let slot = slot_path(&pdir, 2);
        assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
        assert!(git_quiet(&[
            "-C",
            &slot.to_string_lossy(),
            "switch",
            "-q",
            "feat"
        ]));
        let mut leases = Leases::default();
        leases.slots.insert(
            2,
            Lease {
                branch: "feat".into(),
                holder: "task-enhance".into(),
                pid: u32::MAX - 1, // no such process
                at: now_epoch() - reclaim::LEAKED_LEASE_GRACE_SECS - 1,
                boot_id: String::new(),
                comparison_slot: None,
                pinned,
            },
        );
        write_leases(&pdir, &leases).unwrap();
        let ctx = PoolCtx {
            bare: PathBuf::from(&bare),
            repo: "bn".into(),
            pool_dir: pdir,
            worktree_dir: wt_root,
            size: 4,
        };
        (tmp, ctx)
    }

    #[test]
    fn a_pinned_lease_survives_every_idle_sweep() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, ctx) = abandoned_looking(true);
        let slot = slot_path(&ctx.pool_dir, 2);
        let mut leases = read_leases(&ctx.pool_dir);

        assert!(
            reclaim_stale(&ctx, &mut leases, ReclaimScope::AnyDeadHolder)
                .this_boot
                .is_empty()
        );
        assert!(reclaim_leaked_leases(&ctx, &mut leases, now_epoch(), false).is_empty());
        // The same-branch lookup must hand the slot back, not reclaim it and lease afresh.
        let again = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false).expect("lease");
        assert_eq!(again.slot, slot);
        assert!(!again.fresh);
        assert!(read_leases(&ctx.pool_dir)
            .slots
            .get(&2)
            .is_some_and(|l| l.pinned));
        assert_eq!(
            head_ref(&slot),
            "feat",
            "the branch is never detached under its holder"
        );
    }

    /// The control: the same lease unpinned is exactly what the sweep exists to free.
    #[test]
    fn an_unpinned_lease_like_it_is_still_reclaimed() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, ctx) = abandoned_looking(false);
        let mut leases = read_leases(&ctx.pool_dir);
        assert_eq!(
            reclaim_leaked_leases(&ctx, &mut leases, now_epoch(), false),
            vec![2]
        );
    }

    #[test]
    fn pin_marks_the_lease_on_that_slot() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, ctx) = abandoned_looking(false);
        super::pin(&ctx.pool_dir, &slot_path(&ctx.pool_dir, 2));
        assert!(read_leases(&ctx.pool_dir).slots[&2].pinned);
    }
}
