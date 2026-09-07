//! Advisory whole-file locking — the one implementation both queue locks use.
//!
//! Ownership is held by the **kernel**, via a non-blocking `flock` on an open descriptor, rather
//! than by a pid written into a file. That is the whole design, and it is a correction.
//!
//! The incident. A tick took the lock, wrote its pid, and was killed before `Drop` could run; the
//! file survived. The machine then restarted its user session, and the kernel handed that same pid
//! to a system agent that never exits. From then on the liveness check — `!pid_alive(owner)` — said
//! the owner was alive, every acquire was denied, and the dispatcher skipped **every** pass for
//! hours while reporting itself healthy. Nothing dispatched. The queue grew to thirty-five entries
//! behind a lock held by a text-input menu.
//!
//! `flock` removes the question instead of answering it better. The lock lives on the open file
//! description, so the kernel drops it when the holder exits *however* it exits — clean return,
//! SIGKILL, panic, power cut — and a pid is never consulted, so a recycled one cannot impersonate a
//! holder.
//!
//! Two callers, two lifetimes, one primitive: [`crate::queue::daemon`] holds a lock for a
//! dispatcher's whole life ("a dispatcher exists"), and the tick lock holds one for a single pass
//! ("a pass is deciding"). Non-blocking in both cases — a caller that cannot take the lock has a
//! better answer than waiting, and a queue of waiters would fire a burst on release, which is the
//! over-commitment the queue exists to prevent.
//!
//! The pid still goes *into* the file, purely so an operator reading it learns who holds the lock.
//! Nothing reads it back to make a decision.

use std::fs::File;
use std::path::Path;

/// What a non-blocking lock attempt did.
pub enum FlockOutcome {
    Acquired,
    /// Another live descriptor holds it — the healthy contention case.
    Held,
    Failed(String),
}

/// Open (creating if needed) the file a lock will be taken on.
///
/// `create`, never `create_new`: the file outlives any single holder, because ownership is the
/// flock rather than the file's existence. `truncate(false)` for the same reason — a previous
/// holder's recorded pid is inert, not a claim, and truncating on open would race a live holder's
/// own record.
pub fn open_lock_file(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())
}

/// Take an exclusive, non-blocking `flock` on `file`.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::flock is the portable advisory whole-file lock; the fd is owned by `file` and \
              valid for the call"
)]
pub fn try_lock(file: &File) -> FlockOutcome {
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
            // `SIGCHLD` arrives constantly. Treating that as a failure would make the caller give
            // up for a reason that has nothing to do with contention — retry, which is what
            // `EINTR` means.
            Some(code) if code == libc::EINTR => continue,
            // `EWOULDBLOCK` (== `EAGAIN`) is the one error that means "someone else has it".
            Some(code) if code == libc::EWOULDBLOCK => FlockOutcome::Held,
            // Anything else — a read-only state dir, a filesystem without advisory locking — is a
            // different problem, and reporting it as contention sends the reader hunting for a
            // concurrent holder that does not exist.
            _ => FlockOutcome::Failed(err.to_string()),
        };
    }
}

#[cfg(not(unix))]
pub fn try_lock(_file: &File) -> FlockOutcome {
    FlockOutcome::Failed("advisory file locking is unavailable on this platform".to_string())
}

/// Write the holding pid into the lock file, replacing whatever a previous holder left.
///
/// Called only once the lock is ours, so the file always names its actual holder.
pub fn record_holder(file: &File) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = file;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    write!(file, "{}", std::process::id())?;
    file.flush()
}

// A lock file is never unlinked. Removing it would reintroduce the classic flock race: a waiter
// that already opened the file would end up locking an unlinked inode while the next acquirer
// creates and locks a fresh one, and two holders would believe they were alone. Closing the
// descriptor is the entire release.

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let path = dir.path().join("some.lock");
        (dir, path)
    }

    #[test]
    fn a_free_lock_is_acquirable_and_records_its_holder() {
        let (_dir, path) = scratch();
        let file = open_lock_file(&path).expect("open");
        assert!(matches!(try_lock(&file), FlockOutcome::Acquired));
        record_holder(&file).expect("record");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string(),
            "the holder should name itself in the lock file"
        );
    }

    /// `flock` binds to the open file description, so a second acquire — even from this same
    /// process — is a genuine second holder and must be denied.
    #[test]
    fn a_held_lock_denies_the_next_acquirer() {
        let (_dir, path) = scratch();
        let held = open_lock_file(&path).expect("open");
        assert!(matches!(try_lock(&held), FlockOutcome::Acquired));
        let second = open_lock_file(&path).expect("open");
        assert!(matches!(try_lock(&second), FlockOutcome::Held));
    }

    #[test]
    fn closing_the_descriptor_releases_it() {
        let (_dir, path) = scratch();
        {
            let held = open_lock_file(&path).expect("open");
            assert!(matches!(try_lock(&held), FlockOutcome::Acquired));
        }
        let next = open_lock_file(&path).expect("open");
        assert!(matches!(try_lock(&next), FlockOutcome::Acquired));
    }

    /// Opening must not truncate: a live holder's recorded pid has to survive another process
    /// merely probing the lock.
    #[test]
    fn opening_does_not_clobber_a_live_holders_record() {
        let (_dir, path) = scratch();
        let held = open_lock_file(&path).expect("open");
        assert!(matches!(try_lock(&held), FlockOutcome::Acquired));
        record_holder(&held).expect("record");
        let _prober = open_lock_file(&path).expect("open");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }
}
