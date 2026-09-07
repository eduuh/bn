//! The machine-wide tick lock: one dispatcher pass at a time, whichever driver started it.
//!
//! Ownership is held by the **kernel**, via a non-blocking `flock` on an open descriptor, rather
//! than by a pid written into a file. That is the whole design, and it is a correction: every
//! file-content scheme has to answer "is the recorded owner still alive?", and on a machine that
//! reboots or recycles pids there is no honest answer.
//!
//! The incident that moved it here. A tick took the lock, wrote its pid, and was killed before
//! `Drop` could run; the file survived. The machine then restarted its user session, and the kernel
//! handed that same pid to a system agent that never exits. From then on the liveness check —
//! `!pid_alive(owner)` — said the owner was alive, every acquire was denied, and the resident
//! dispatcher skipped **every** pass for hours while reporting itself healthy. Nothing dispatched.
//! The queue grew to thirty-five entries behind a lock held by a text-input menu.
//!
//! `flock` removes the question instead of answering it better. The lock lives on the open file
//! description, so the kernel drops it when the holder exits *however* it exits — clean return,
//! SIGKILL, panic, power cut — and a pid is never consulted, so a recycled one cannot impersonate a
//! holder. Non-blocking (`LOCK_NB`) keeps the original semantics: a pass that cannot take the lock
//! **skips**, because another pass is already deciding and there will be one along shortly. A tick
//! that queued behind its predecessor would pile up and then fire a burst, which is the
//! over-commitment the queue exists to prevent.
//!
//! The pid still goes *into* the file, purely so an operator reading it learns who holds the lock.
//! Nothing reads it back to make a decision.

use std::fs::File;
use std::path::PathBuf;

/// The lock that makes "one tick per machine" true regardless of which driver started it.
pub(super) fn lock_path() -> PathBuf {
    bn_core::resolver::state_dir().join("queue-tick.lock")
}

/// A non-blocking machine-wide tick lock, released when the holder's descriptor closes.
pub(crate) struct TickLock {
    /// The open descriptor **is** the lock: `flock` binds to the open file description, so the
    /// kernel releases it when this closes. Holding the `File` for the guard's lifetime is
    /// therefore not bookkeeping — drop it early and the lock is gone.
    _file: File,
}

impl TickLock {
    pub(super) fn try_acquire() -> Result<TickLock, LockDenied> {
        Self::try_acquire_at(&lock_path())
    }

    /// [`try_acquire`](Self::try_acquire) against an explicit path, so the locking behaviour can be
    /// tested without going through the process-global state directory (which other tests move out
    /// from under it).
    fn try_acquire_at(path: &std::path::Path) -> Result<TickLock, LockDenied> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // `create`, never `create_new`: the file outlives any single holder now, because ownership
        // is the flock rather than the file's existence.
        let file = match File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => return Err(LockDenied::Unusable(e.to_string())),
        };

        match try_flock(&file) {
            FlockOutcome::Acquired => {}
            FlockOutcome::Held => return Err(LockDenied::Held),
            FlockOutcome::Failed(e) => return Err(LockDenied::Unusable(e)),
        }

        // Diagnostics only — recorded after the lock is ours so the file always names its actual
        // holder. A failure here costs an operator a hint, never correctness, so it is not fatal.
        let _ = record_holder(&file);
        Ok(TickLock { _file: file })
    }
}

/// What a non-blocking `flock` attempt did.
enum FlockOutcome {
    Acquired,
    /// Another live descriptor holds it — the healthy contention case.
    Held,
    Failed(String),
}

/// Take an exclusive, non-blocking `flock` on `file`.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::flock is the portable advisory whole-file lock; the fd is owned by `file` and \
              valid for the call"
)]
fn try_flock(file: &File) -> FlockOutcome {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: `file` owns a valid, open descriptor for the duration of this call, and `flock`
        // neither retains the fd past it nor writes through any pointer.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return FlockOutcome::Acquired;
        }
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            // A signal landed mid-call. `flock` is interruptible even with `LOCK_NB`, and the
            // dispatcher is the *most* likely process to see it: every pass spawns children, so
            // `SIGCHLD` arrives constantly. Treating that as a failure would make the tick skip
            // for a reason that has nothing to do with contention — retry, which is what `EINTR`
            // means.
            Some(code) if code == libc::EINTR => continue,
            // `EWOULDBLOCK` (== `EAGAIN`) is the one error that means "someone else has it".
            Some(code) if code == libc::EWOULDBLOCK => FlockOutcome::Held,
            // Anything else — a read-only state dir, a filesystem without advisory locking — is a
            // different problem, and reporting it as contention sends the reader hunting for a
            // concurrent tick that does not exist.
            _ => FlockOutcome::Failed(err.to_string()),
        };
    }
}

#[cfg(not(unix))]
fn try_flock(_file: &File) -> FlockOutcome {
    FlockOutcome::Failed("advisory file locking is unavailable on this platform".to_string())
}

/// Write the holding pid into the lock file, replacing whatever a previous holder left.
fn record_holder(file: &File) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = file;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    write!(file, "{}", std::process::id())?;
    file.flush()
}

/// Why a pass could not take the machine lock.
pub(super) enum LockDenied {
    /// Another tick is running. The healthy case: it is doing the work.
    Held,
    /// The lock could not be taken at all — an unwritable state dir, a filesystem with no advisory
    /// locking.
    Unusable(String),
}

/// Whether `pid` names a live process — [`bn_core::queue::build::pid_alive`], re-exported so the
/// tick, the status line and the dashboard cannot disagree about what "alive" means.
///
/// Deliberately **not** used to decide lock ownership: see the module docs for the outage that
/// taught us a pid cannot answer that question.
pub(crate) use bn_core::queue::build::pid_alive;

// The lock file is never unlinked. Removing it would reintroduce the classic flock race: a waiter
// that already opened the file would end up locking an unlinked inode while the next acquirer
// creates and locks a fresh one, and two passes would dispatch the same plan. Closing the
// descriptor is the entire release.

#[cfg(test)]
mod tests {
    use super::*;

    /// A private lock path for one test. Deliberately *not* the process-global state directory:
    /// these tests exercise the locking primitive, and routing them through `$BN_STATE_DIR` makes
    /// them race with any concurrently running test that touches the same machine lock.
    fn scratch(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let path = dir.path().join(name);
        (dir, path)
    }

    /// Acquire, or fail the test naming *why*. A bare `is_ok()` cannot tell a lock genuinely held
    /// by someone else from an unusable state dir, and those want opposite investigations.
    fn acquire(path: &std::path::Path) -> TickLock {
        match TickLock::try_acquire_at(path) {
            Ok(guard) => guard,
            Err(LockDenied::Held) => panic!("expected a free lock, but it was held"),
            Err(LockDenied::Unusable(why)) => panic!("lock unusable: {why}"),
        }
    }

    #[test]
    fn a_free_lock_is_acquirable_and_records_its_holder() {
        let (_dir, path) = scratch("queue-tick.lock");
        let _guard = acquire(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string(),
            "the holding pass should name itself in the lock file"
        );
    }

    /// The regression this module was rewritten for. A lock file left behind by a killed tick names
    /// a pid the OS later handed to an unrelated, immortal process (on the machine that found this,
    /// a system text-input agent). Under the old pid-liveness rule that lock could never be
    /// reclaimed and the dispatcher skipped every pass for hours. Ownership is the flock now, so
    /// leftover content is inert.
    #[test]
    fn a_lock_file_naming_a_live_unrelated_pid_is_still_acquirable() {
        let (_dir, path) = scratch("queue-tick.lock");
        // Our own pid: certainly alive, and certainly not a tick holding this lock.
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        // Must be acquirable: an abandoned lock file never outlives its writer, whatever pid it names.
        let _guard = acquire(&path);
    }

    /// Contention still skips. `flock` binds to the open file description, so a second acquire —
    /// even from this same process — is a genuine second holder and must be denied.
    #[test]
    fn a_held_lock_denies_the_next_pass() {
        let (_dir, path) = scratch("queue-tick.lock");
        let _held = acquire(&path);
        assert!(
            matches!(TickLock::try_acquire_at(&path), Err(LockDenied::Held)),
            "a second pass must skip while the first holds the lock"
        );
    }

    /// And releasing actually releases: the next pass gets in.
    #[test]
    fn dropping_the_guard_frees_the_machine() {
        let (_dir, path) = scratch("queue-tick.lock");
        drop(acquire(&path));
        // A released lock must be immediately re-acquirable.
        let _reacquired = acquire(&path);
    }
}
