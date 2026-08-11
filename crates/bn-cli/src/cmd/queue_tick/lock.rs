//! The machine-wide tick lock: one dispatcher pass at a time, whichever driver started it.
//!
//! The rule that makes it work is narrower than it looks. Acquiring may steal a lock whose owner is
//! **gone** — `Drop` does not run on SIGKILL or a power cut, and a machine that stays locked out
//! waiting for a release that will never come dispatches nothing. Releasing may only remove a lock
//! that is **still ours**.
//!
//! Both halves were wrong in the same incident. Acquire also stole any lock older than
//! `LOCK_STALE_SECS` (30) — shorter than an ordinary pass that launches an agent — so the
//! once-a-minute cron tick took the lock from a working daemon; release then deleted whatever file
//! it found, which by then belonged to the thief, leaving the machine unlocked from there on. Two
//! dispatchers realizing the same plan is precisely the double-dispatch this file exists to
//! prevent.

use std::path::PathBuf;

/// The lock that makes "one tick per machine" true regardless of which driver started it.
pub(super) fn lock_path() -> PathBuf {
    bn_core::resolver::state_dir().join("queue-tick.lock")
}

/// A non-blocking machine-wide tick lock, released on drop.
///
/// Non-blocking on purpose: a tick that waited for its predecessor would pile up behind a slow
/// pass and then fire a burst, which is precisely the over-commitment the queue exists to prevent.
/// Skipping is correct — another pass is already deciding, and there will be one along shortly.
pub(super) struct TickLock {
    path: PathBuf,
}

impl TickLock {
    pub(super) fn try_acquire() -> Result<TickLock, LockDenied> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A lock whose owner is gone is not a lock, and recording the owner is what turns that from
        // a guess into a fact we can check.
        if let Ok(prev) = std::fs::read_to_string(&path) {
            let owner: Option<u32> = prev.trim().parse().ok();
            if lock_abandoned(owner, || bn_core::store::lock_is_stale(&path), &pid_alive) {
                let _ = std::fs::remove_file(&path);
            }
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = write!(f, "{}", std::process::id());
                Ok(TickLock { path })
            }
            // Only `AlreadyExists` means another tick holds it. Anything else — a read-only state
            // dir, a full disk — is a different problem entirely, and reporting it as contention
            // sends the reader hunting for a concurrent tick that does not exist.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(LockDenied::Held),
            Err(e) => Err(LockDenied::Unusable(e.to_string())),
        }
    }
}

/// Whether an existing lock file may be taken from whoever wrote it.
///
/// Pure so the rule is testable without a clock or a filesystem, because the rule is the part that
/// was wrong: **a live owner is never abandoned, however old its lock**. Staleness decides one case
/// only — a lock file with no readable pid, where there is nothing better to go on. `stale` is a
/// closure so that check is skipped entirely in the common path.
fn lock_abandoned(
    owner: Option<u32>,
    stale: impl FnOnce() -> bool,
    alive: &impl Fn(u32) -> bool,
) -> bool {
    match owner {
        Some(pid) => !alive(pid),
        None => stale(),
    }
}

/// Why a pass could not take the machine lock.
pub(super) enum LockDenied {
    /// Another tick is running. The healthy case: it is doing the work.
    Held,
    /// The lock could not be created at all — an unwritable state dir, a full disk.
    Unusable(String),
}

/// Whether `pid` names a live process — [`bn_core::queue::build::pid_alive`], re-exported so the
/// tick, the status line and the dashboard cannot disagree about what "alive" means.
pub(crate) use bn_core::queue::build::pid_alive;

impl Drop for TickLock {
    /// Release the lock — but only if it is still ours.
    ///
    /// An unconditional `remove_file` here deletes whatever lock file happens to exist, which after
    /// a steal is somebody else's. That turns one lost race into a permanently unlocked machine:
    /// the thief keeps running believing it holds the lock, and every subsequent tick walks
    /// straight in. Checking the pid first makes releasing safe even when acquiring was not.
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .is_some_and(|pid| pid == std::process::id());
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::test_support::with_state_dir;

    /// A pid that is certainly not running: spawn something trivial and reap it.
    fn a_dead_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    /// The regression that took the machine down: a pass slower than `LOCK_STALE_SECS` used to have
    /// its lock stolen while it was still working, so a second dispatcher realized the same plan.
    #[test]
    fn a_live_owner_keeps_its_lock_however_old() {
        let alive = |_: u32| true;
        assert!(
            !lock_abandoned(Some(4242), || true, &alive),
            "a lock held by a live process must not be stolen for being old"
        );
    }

    /// Age still decides the one case with no pid to consult — an old-format or truncated lock.
    #[test]
    fn a_pidless_lock_falls_back_to_age() {
        let alive = |_: u32| true;
        assert!(lock_abandoned(None, || true, &alive), "stale and pidless");
        assert!(
            !lock_abandoned(None, || false, &alive),
            "a fresh pidless lock is someone mid-write, not an abandoned one"
        );
    }

    /// The other half: a lock whose owner died (SIGKILL, power cut) is not a lock, and the machine
    /// must not stay locked out waiting for a `Drop` that will never run.
    #[test]
    fn a_dead_owners_lock_is_reclaimed() {
        with_state_dir(|| {
            let path = lock_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, a_dead_pid().to_string()).unwrap();

            let guard = TickLock::try_acquire();
            assert!(guard.is_ok(), "a dead owner's lock should be reclaimable");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap().trim(),
                std::process::id().to_string(),
                "the reclaiming pass should own the lock it took"
            );
        });
    }

    /// Releasing must be narrower than acquiring: dropping a guard whose lock was taken by someone
    /// else has to leave the new owner's lock alone, or one lost race unlocks the machine for good.
    #[test]
    fn dropping_does_not_delete_someone_elses_lock() {
        with_state_dir(|| {
            let path = lock_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let guard = TickLock::try_acquire().ok().expect("acquire a free lock");
            // Simulate the steal: another pass now owns the file.
            std::fs::write(&path, "999999").unwrap();
            drop(guard);

            assert_eq!(
                std::fs::read_to_string(&path).unwrap().trim(),
                "999999",
                "a released guard must only remove a lock still bearing its own pid"
            );
        });
    }
}
