//! Whether the dispatcher is *getting anywhere* — the half that liveness deliberately cannot answer.
//!
//! Liveness and progress were once one signal, and that was the bug. `beat()` used to be the first
//! statement in the resident loop's body, which makes freshness a function of how long a pass takes
//! — and a pass waits synchronously on work that can hang. The dispatcher sat alive-but-blocked for
//! twenty-five minutes while every reader concluded it was dead, and launchd, whose process was
//! perfectly healthy, had no reason to restart anything.
//!
//! The fix at the time was to split them and put liveness on a timer thread that nothing could
//! block. Liveness has since moved to [`bn_core::queue::daemon`], where the *kernel* holds it: a
//! dispatcher's `flock` is held for exactly as long as the process exists, however wedged it is.
//! That retires the thread, and this file keeps the half it never covered.
//!
//! So a dispatcher blocked inside something it started now reports `alive` with a stale pass —
//! which is the truth, and points at the stuck work rather than at a daemon that is running fine.
//! Read back by [`bn_core::resolver::dispatcher_pass_age`].

/// Record that a pass finished.
///
/// Only a pass that actually *decided* stamps this. A pass that skipped on the machine lock must
/// not: this is the one signal separating a wedged dispatcher from a working one, and a loop that
/// stamped it while contending forever would report perfect health while dispatching nothing.
pub(super) fn stamp_pass() {
    stamp_pass_at(&bn_core::resolver::dispatcher_pass_path());
}

/// [`stamp_pass`] against an explicit path.
///
/// Split out so the behaviour is testable without `BN_STATE_DIR`. That is not a stylistic
/// preference: mutating the process environment races every other test that spawns a child, because
/// `Command::spawn` reads `environ` to build the child's copy while `setenv` is reallocating it. The
/// tick lock's tests make the same split for the same reason.
fn stamp_pass_at(path: &std::path::Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Best-effort: a dispatcher must not stop dispatching because it could not write a stamp.
    let _ = std::fs::write(path, bn_core::util::now_epoch_secs().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let path = dir.path().join("queue-daemon.pass");
        (dir, path)
    }

    fn stamped_at(path: &std::path::Path) -> i64 {
        std::fs::read_to_string(path)
            .expect("a pass should have been stamped")
            .trim()
            .parse()
            .expect("the stamp is an epoch second")
    }

    /// The stamp is the current time, which is what makes an *age* meaningful downstream.
    #[test]
    fn a_finished_pass_records_when_it_finished() {
        let (_dir, path) = scratch();
        let before = bn_core::util::now_epoch_secs();
        stamp_pass_at(&path);
        let stamped = stamped_at(&path);
        assert!(
            (before..=bn_core::util::now_epoch_secs()).contains(&stamped),
            "expected a stamp inside the call, got {stamped}"
        );
    }

    /// Each pass replaces the last, so the age tracks the most recent completion rather than the
    /// first. A stamp that only ever moved once would report a wedged dispatcher as healthy forever.
    #[test]
    fn a_later_pass_replaces_the_earlier_stamp() {
        let (_dir, path) = scratch();
        std::fs::write(&path, "1000").expect("seed an old stamp");
        stamp_pass_at(&path);
        assert!(
            stamped_at(&path) > 1000,
            "a new pass must overwrite an older stamp"
        );
    }

    /// Progress is a *separate fact* from liveness, and the two are now kept by different
    /// mechanisms entirely: the kernel holds a dispatcher's lock, this file records its passes.
    /// Neither can be inferred from the other — which is exactly what lets `bn queue status` tell a
    /// wedged dispatcher (alive, no recent pass) from a dead one.
    #[test]
    fn a_stamp_says_nothing_about_whether_a_dispatcher_exists() {
        let (dir, path) = scratch();
        stamp_pass_at(&path);
        assert!(
            !bn_core::queue::daemon::is_running_at(&dir.path().join("queue-daemon.lock")),
            "a completed pass must not make a departed dispatcher look alive"
        );
    }
}
