//! Whether this machine has a dispatcher, answered by the kernel.
//!
//! bn supervises its own dispatcher. There is no systemd unit, no launchd plist, and no
//! once-a-minute timer: a resident `bn queue run` holds the lock below for its whole life, and any
//! ordinary `bn` invocation that finds the lock free starts a replacement. That is the whole
//! mechanism.
//!
//! **Why the lock is the liveness signal, and not a heartbeat.** The previous design wrote a
//! timestamp file. It began as one line at the top of the dispatcher's loop, which made freshness a
//! function of how long a pass took — and a pass waits on work that can hang, so the dispatcher
//! once sat alive-but-blocked for twenty-five minutes while every reader concluded it was dead.
//! The fix at the time was to move the write onto a timer thread "that nothing can block". A
//! `flock` *is* that guarantee, supplied by the kernel: it is held for exactly as long as the
//! process exists, however wedged it is, and released on any exit at all — clean return, SIGKILL,
//! panic, power cut. So there is no thread to keep running, no TTL to tune, and no window in which
//! a dead dispatcher still reads as alive.
//!
//! That last point is what [`crate::resolver::capture_active`]'s callers depend on. A producer only
//! enqueues if something will later dequeue; a stale-but-not-yet-expired heartbeat meant work could
//! be filed into a queue that had already stopped. `is_running` cannot be stale.
//!
//! **Liveness is not progress**, and the two stay separate facts. A dispatcher blocked inside
//! something it started holds this lock perfectly well. Whether it is *getting anywhere* is
//! [`crate::resolver::dispatcher_pass_age`]'s question, stamped by each completed pass.
//!
//! The lock file's *contents* are the holder's pid, for an operator reading it. Nothing consults
//! them: a pid cannot answer "is the owner alive" on a machine that recycles pids, which is a
//! lesson this codebase learned the expensive way — see [`crate::queue::flock`].

use crate::queue::flock::{self, FlockOutcome};
use std::fs::File;
use std::path::PathBuf;

/// The lock a resident dispatcher holds for its whole life.
///
/// Deliberately distinct from the tick lock (`queue-tick.lock`), which is held for **one pass** so
/// two passes never decide at once. They answer different questions and must not share a file: a
/// dispatcher between passes holds this one and not that one.
pub fn lock_path() -> PathBuf {
    crate::resolver::state_dir().join("queue-daemon.lock")
}

/// The dispatcher's own stdout/stderr, when bn started it.
///
/// It had been `/tmp/com.bn.queue.log`, named after the launchd label even on Linux, because
/// launchd was what redirected the stream. bn opens the file itself now, so the name no longer
/// borrows from a supervisor that is not there, and the log lives with the rest of the state.
pub fn log_path() -> PathBuf {
    crate::resolver::state_dir().join("queue-dispatcher.log")
}

/// A held dispatcher lock. Dropping it — or exiting in any manner — releases the machine.
#[must_use = "the lock lives exactly as long as this guard; drop it and another dispatcher may start"]
pub struct DaemonLock {
    /// The open descriptor **is** the lock: `flock` binds to the open file description, so the
    /// kernel releases it when this closes. Holding the `File` is not bookkeeping.
    _file: File,
}

impl DaemonLock {
    /// Become this machine's dispatcher, or report that one already is.
    pub fn acquire() -> Result<DaemonLock, Denied> {
        Self::acquire_at(&lock_path())
    }

    /// [`acquire`](Self::acquire) against an explicit path, so the behaviour is testable without
    /// the process-global state directory that other tests move out from under it.
    pub fn acquire_at(path: &std::path::Path) -> Result<DaemonLock, Denied> {
        let file = flock::open_lock_file(path).map_err(Denied::Unusable)?;
        match flock::try_lock(&file) {
            FlockOutcome::Acquired => {}
            FlockOutcome::Held => return Err(Denied::Running),
            FlockOutcome::Failed(e) => return Err(Denied::Unusable(e)),
        }
        // Diagnostics only, recorded after the lock is ours so the file always names its actual
        // holder. Failing here costs an operator a hint, never correctness.
        let _ = flock::record_holder(&file);
        Ok(DaemonLock { _file: file })
    }
}

/// Why this process may not become the dispatcher.
#[derive(Debug)]
pub enum Denied {
    /// Another dispatcher holds the lock. The healthy case.
    Running,
    /// The lock could not be taken at all — a read-only state dir, a filesystem without advisory
    /// locking. Reporting this as "already running" would send a reader hunting for a process that
    /// does not exist.
    Unusable(String),
}

/// Whether a dispatcher is running on this machine right now.
///
/// One open, one non-blocking lock attempt, one close — cheaper than the `stat` the heartbeat cost,
/// and exact rather than a ten-minute window. Called on the hot path (every `bn` invocation asks
/// whether it needs to start one), so it must stay this cheap.
///
/// A lock we could not evaluate reads as **not running**, which is the safe direction in both
/// places this feeds: a producer falls back to launching inline (the pre-queue behaviour), and
/// `ensure` tries to start a dispatcher, whose own acquire then fails loudly rather than silently.
pub fn is_running() -> bool {
    is_running_at(&lock_path())
}

/// [`is_running`] against an explicit path.
pub fn is_running_at(path: &std::path::Path) -> bool {
    let Ok(file) = flock::open_lock_file(path) else {
        return false;
    };
    match flock::try_lock(&file) {
        // We took it, so nobody held it. Dropping `file` at the end of this scope releases it
        // again — the probe must not leave the machine looking occupied by a process that has
        // already moved on.
        FlockOutcome::Acquired => false,
        FlockOutcome::Held => true,
        FlockOutcome::Failed(_) => false,
    }
}

/// The pid recorded in the lock file, for an operator asking "who?".
///
/// Advisory only, and deliberately not consulted by any decision here: the process it names may
/// have exited, and the pid may since have been reused. Ownership is the flock.
pub fn holder_pid() -> Option<u32> {
    std::fs::read_to_string(lock_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let path = dir.path().join(name);
        (dir, path)
    }

    fn acquire(path: &std::path::Path) -> DaemonLock {
        match DaemonLock::acquire_at(path) {
            Ok(guard) => guard,
            Err(Denied::Running) => panic!("expected a free lock, but a dispatcher held it"),
            Err(Denied::Unusable(why)) => panic!("lock unusable: {why}"),
        }
    }

    /// The property `capture_active` rests on: no holder means no dispatcher, so a producer must
    /// run inline rather than file work nothing will drain.
    #[test]
    fn an_unheld_lock_reports_no_dispatcher() {
        let (_dir, path) = scratch("queue-daemon.lock");
        assert!(!is_running_at(&path));
    }

    /// And the converse — while a dispatcher holds it, producers may safely enqueue.
    #[test]
    fn a_held_lock_reports_a_running_dispatcher() {
        let (_dir, path) = scratch("queue-daemon.lock");
        let _guard = acquire(&path);
        assert!(is_running_at(&path));
    }

    /// The probe must not itself claim the machine. It takes the lock to discover the lock was
    /// free, and has to give it straight back — otherwise merely *asking* whether a dispatcher is
    /// running would stop one from starting.
    #[test]
    fn probing_an_idle_machine_leaves_it_startable() {
        let (_dir, path) = scratch("queue-daemon.lock");
        assert!(!is_running_at(&path));
        let _guard = acquire(&path);
    }

    /// A second dispatcher is refused rather than admitted alongside the first.
    #[test]
    fn a_second_dispatcher_is_denied() {
        let (_dir, path) = scratch("queue-daemon.lock");
        let _held = acquire(&path);
        assert!(
            matches!(DaemonLock::acquire_at(&path), Err(Denied::Running)),
            "one dispatcher per machine"
        );
    }

    /// Exit releases the machine, so the next `bn` invocation starts a replacement.
    #[test]
    fn releasing_lets_the_next_dispatcher_take_over() {
        let (_dir, path) = scratch("queue-daemon.lock");
        drop(acquire(&path));
        assert!(!is_running_at(&path));
        let _next = acquire(&path);
    }

    /// The regression the tick lock was rewritten for, restated here because this lock is now the
    /// liveness signal and inherits the stakes. A lock file left behind by a killed dispatcher
    /// names a pid the OS may later hand to an unrelated, immortal process. Under a
    /// pid-liveness rule the machine could never start a dispatcher again; ownership is the flock,
    /// so leftover content is inert.
    #[test]
    fn a_lock_file_naming_a_live_unrelated_pid_is_still_free() {
        let (_dir, path) = scratch("queue-daemon.lock");
        // Our own pid: certainly alive, and certainly not a dispatcher holding this lock.
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        assert!(!is_running_at(&path), "an abandoned file is not a holder");
        let _guard = acquire(&path);
    }
}
