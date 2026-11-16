//! Atomic artifact writes and cross-process file locks.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Parse(String),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::Parse(m) => write!(f, "parse error: {m}"),
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// A temp sibling of `path` no other writer can collide with: pid (cross-process) plus a
/// process-local counter (cross-thread). A *fixed* temp name is the subtle bug this avoids — two
/// writers would share one scratch file and each could rename the other's half-written bytes into
/// place. Shared with the task store, which needs the same guarantee.
pub(crate) fn atomic_temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let fname = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".into());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{fname}.{}.{}.tmp", std::process::id(), seq))
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    Ok(())
}

fn replace_atomic(tmp: &Path, path: &Path) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

/// Replace an authoritative artifact without exposing a partial write.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = atomic_temp_path(path);
    write_synced(&tmp, bytes)?;
    replace_atomic(&tmp, path)
}

/// Serialize `value` to pretty JSON, mapping a serialization failure to an `io::Error` rather than
/// silently yielding an empty string. Callers persist the result (often via [`write_atomic`]), so a
/// swallowed error would make an empty `""` file durable — losing state. Fail loudly instead (#689).
pub(crate) fn to_json_pretty<T: serde::Serialize>(value: &T) -> std::io::Result<String> {
    serde_json::to_string_pretty(value).map_err(std::io::Error::other)
}

/// Serialize `value` before touching the filesystem, then atomically replace `path`.
pub(crate) fn save_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let text = to_json_pretty(value)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, text.as_bytes())
}

/// Path of the advisory lock file guarding a target's read-modify-write: `<target>.lock` beside it.
pub fn lock_path(target: &Path) -> PathBuf {
    let mut p = target.as_os_str().to_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

/// A stale lock (owner crashed mid-edit) is stolen after this long so a target can't wedge forever.
pub const LOCK_STALE_SECS: u64 = 30;
/// How long a best-effort [`FileLock::acquire`] waits for a contended lock before giving up and
/// proceeding *unowned* (the atomic write still prevents corruption; only a lost update is at
/// stake, and blocking an edit forever is worse).
///
/// Kept comfortably under [`LOCK_STALE_SECS`]: a live holder releases a note read-modify-write in
/// milliseconds, so the only thing that can push a waiter onto the lossy unowned path is a truly
/// wedged holder — and that one is reclaimed by the stale-steal backstop at [`LOCK_STALE_SECS`].
/// The previous 2s budget was too tight under heavy multi-process contention (many agents
/// appending to one branch note at once): the serialized lock time could exceed it and silently
/// drop an update. A generous budget lets every contender wait its turn instead.
const LOCK_WAIT_MS: u64 = 20_000;
/// How long a durable [`FileLock::acquire_required`] waits before returning `WouldBlock`.
/// Deliberately short: these callers surface the timeout as an error and retry or abort rather than
/// risk proceeding without the lock, so they should not block for the full best-effort budget.
const REQUIRED_LOCK_WAIT_MS: u64 = 2_000;

/// Jittered retry backoff (in ms) for a contended-lock spin. Mixing the pid and attempt count
/// through a multiplicative hash desynchronizes the thundering herd of processes racing on the same
/// `create_new`, cutting the total time they waste colliding. Centered near the old fixed 15ms so a
/// single waiter's latency is unchanged.
fn backoff_ms(attempt: u64) -> u64 {
    let mix = (std::process::id() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(attempt.wrapping_mul(0x2545_F491_4F6C_DD1D));
    8 + (mix >> 32) % 16
}

/// Best-effort cross-process guard for a load-mutate-atomic-save transaction.
#[must_use = "hold the guard for the whole read-modify-write, then drop it"]
pub struct FileLock {
    path: PathBuf,
    owned: bool,
}

impl FileLock {
    /// Acquire (or, on timeout, best-effort skip) the lock for `target`.
    pub fn acquire(target: &Path) -> Self {
        use std::time::{Duration, Instant};
        let path = lock_path(target);
        if let Some(parent) = path.parent() {
            // Best-effort: if this fails the create_new open below can't make the lock file
            // either, so we surface it through the loud `owned: false` path rather than here.
            let _ = std::fs::create_dir_all(parent);
        }
        let start = Instant::now();
        let mut attempt: u64 = 0;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return FileLock { path, owned: true },
                Err(_) => {
                    if lock_is_stale(&path) {
                        // Steal the stale lock; if removal races another stealer the next
                        // create_new attempt just retries, so the error is safe to drop.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() >= Duration::from_millis(LOCK_WAIT_MS) {
                        // Give up waiting and proceed unowned rather than block the edit — but
                        // loudly: without the lock a concurrent read-modify-write can silently
                        // lose an update, so leave the operator a trace instead of a silent race.
                        eprintln!(
                            "bn: warning: proceeding without lock after {LOCK_WAIT_MS}ms on {} — a concurrent edit may be lost",
                            path.display()
                        );
                        return FileLock { path, owned: false };
                    }
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(backoff_ms(attempt)));
                }
            }
        }
    }

    /// Acquire the lock for a durable read-modify-write, failing instead of proceeding unowned.
    pub fn acquire_required(target: &Path) -> std::io::Result<Self> {
        Self::acquire_required_with_wait(
            target,
            std::time::Duration::from_millis(REQUIRED_LOCK_WAIT_MS),
        )
    }

    fn acquire_required_with_wait(
        target: &Path,
        wait: std::time::Duration,
    ) -> std::io::Result<Self> {
        use std::io::ErrorKind;
        use std::time::Instant;

        let path = lock_path(target);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to create lock directory {}: {e}", parent.display()),
                )
            })?;
        }

        let start = Instant::now();
        let mut attempt: u64 = 0;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => {
                    return Ok(FileLock { path, owned: true });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        match std::fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(e) if e.kind() == ErrorKind::NotFound => continue,
                            Err(e) => {
                                return Err(std::io::Error::new(
                                    e.kind(),
                                    format!("failed to remove stale lock {}: {e}", path.display()),
                                ));
                            }
                        }
                    }
                    if start.elapsed() >= wait {
                        return Err(std::io::Error::new(
                            ErrorKind::WouldBlock,
                            format!(
                                "timed out after {}ms waiting for lock {}",
                                wait.as_millis(),
                                path.display()
                            ),
                        ));
                    }
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms(attempt)));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("failed to acquire lock {}: {e}", path.display()),
                    ));
                }
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if self.owned {
            // Release only a lock we created; a failed remove just leaves a file the next
            // acquirer ages out as stale, so the error is safe to drop.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// True if the lock file exists but is older than [`LOCK_STALE_SECS`] — its owner likely crashed.
pub fn lock_is_stale(path: &Path) -> bool {
    use std::time::{Duration, SystemTime};
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            SystemTime::now()
                .duration_since(t)
                .unwrap_or(Duration::ZERO)
                >= Duration::from_secs(LOCK_STALE_SECS)
        })
        .unwrap_or(false)
}
