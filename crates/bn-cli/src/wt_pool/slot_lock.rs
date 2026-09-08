//! Advisory locks for the git mutations bn performs in a pool checkout: one per slot, one per
//! pool for `warm`.
//!
//! The incident. Two `pool warm` runs — the cron timer and the queue's dispatch of the same job —
//! walked the BigMono pool at once. On each slot the first one's `git switch` was still writing the
//! tracked files the new base had added when the second one's `git status` (its own `switch` had
//! already died on the first one's `index.lock`) read the *old* index, saw those files on disk as
//! untracked, and its `clean -df` deleted them. Every idle slot came out of it with dozens of
//! tracked files missing, and from then on every lease either failed its switch outright or handed
//! out a checkout with holes in it. Nothing in the pool could repair that: warm skips dirty slots,
//! return refuses them, and get failed on them.
//!
//! [`lock_slot`] serialises the mutations per slot, so a second process waits for the first to
//! finish the slot rather than interleaving git calls with it. [`try_lock_warm`] stops a whole
//! second warm from starting while one runs: a warm is idempotent, so the running one already is
//! the answer. Both are kernel `flock`s (see [`bn_core::queue::flock`]), released however the
//! holder exits.

use bn_core::queue::flock::{self, FlockOutcome};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Held for as long as the value lives; the kernel releases it on drop or process death.
pub(super) struct PoolLock {
    _file: File,
}

/// Lock files sit beside `leases.json`, dot-prefixed so nothing that scans the pool dir for slot
/// numbers ever mistakes one for a slot.
fn lock_file_path(pool_dir: &Path, name: &str) -> PathBuf {
    pool_dir.join(format!(".{name}.lock"))
}

/// Exclusive lock on slot `n`'s git state, waiting for any other holder to finish with it. `None`
/// when the lock cannot be opened or taken at all (a read-only pool dir, a filesystem without
/// advisory locking): the caller then proceeds unlocked, which is exactly what it did before this
/// existed, rather than refusing a lease over a lock a quiet pool never needed.
pub(super) fn lock_slot(pool_dir: &Path, n: u32) -> Option<PoolLock> {
    let file = flock::open_lock_file(&lock_file_path(pool_dir, &format!("slot-{n}"))).ok()?;
    match flock::lock(&file) {
        FlockOutcome::Acquired => Some(PoolLock { _file: file }),
        _ => None,
    }
}

/// The pool-wide warm lock, taken without waiting. `None` means another warm holds it and the
/// caller should yield to it. A lock that cannot be taken for any other reason is returned as
/// held-in-name-only, so a platform without advisory locking still warms — unguarded, as before.
pub(super) fn try_lock_warm(pool_dir: &Path) -> Option<PoolLock> {
    let file = flock::open_lock_file(&lock_file_path(pool_dir, "warm")).ok()?;
    match flock::try_lock(&file) {
        FlockOutcome::Held => None,
        FlockOutcome::Acquired | FlockOutcome::Failed(_) => Some(PoolLock { _file: file }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_warm_lock_on_the_same_pool_is_refused_until_the_first_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let held = try_lock_warm(tmp.path()).expect("first warm takes the lock");
        assert!(
            try_lock_warm(tmp.path()).is_none(),
            "a second warm must yield while the first holds the pool"
        );
        drop(held);
        assert!(try_lock_warm(tmp.path()).is_some());
    }

    #[test]
    fn slot_locks_are_per_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let _two = lock_slot(tmp.path(), 2).expect("slot 2 lock");
        // Slot 3 is a different lock; taking it must not wait on slot 2's holder.
        assert!(lock_slot(tmp.path(), 3).is_some());
    }
}
