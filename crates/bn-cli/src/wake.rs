use notify::Watcher;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(100);

pub(crate) struct WakeWatcher {
    _watcher: notify::RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
    signal_path: PathBuf,
    recursive_roots: Vec<PathBuf>,
}

impl WakeWatcher {
    pub(crate) fn new(recursive_roots: &[PathBuf]) -> notify::Result<Self> {
        let queue = bn_core::wake::WakeQueue::default();
        Self::at(queue.signal_path().to_path_buf(), recursive_roots)
    }

    fn at(signal_path: PathBuf, recursive_roots: &[PathBuf]) -> notify::Result<Self> {
        let state_dir = signal_path
            .parent()
            .ok_or_else(|| notify::Error::generic("wake signal has no parent directory"))?;
        std::fs::create_dir_all(state_dir).map_err(notify::Error::io)?;
        // Compare against resolved paths: macOS reports FSEvents paths canonicalized (`/var/...`
        // arrives as `/private/var/...`), so a watch rooted at a symlinked ancestor would emit
        // events that `matches` rejects and the wait would time out despite a live signal.
        let state_dir = resolve(state_dir);
        let signal_path = match signal_path.file_name() {
            Some(name) => state_dir.join(name),
            None => signal_path,
        };
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(&state_dir, notify::RecursiveMode::NonRecursive)?;
        let mut watched_roots = Vec::new();
        for root in recursive_roots {
            if root.is_dir() {
                let root = resolve(root);
                watcher.watch(&root, notify::RecursiveMode::Recursive)?;
                watched_roots.push(root);
            }
        }
        Ok(Self {
            _watcher: watcher,
            receiver,
            signal_path,
            recursive_roots: watched_roots,
        })
    }

    /// Wait for a queue signal or a watched harness-log change. A timeout is an intentional
    /// fallback for time-based transitions such as an otherwise quiet agent becoming stuck.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            match self
                .receiver
                .recv_timeout(deadline.saturating_duration_since(now))
            {
                Ok(Ok(event)) if self.matches(&event) => break,
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(error.to_string()),
                Err(RecvTimeoutError::Timeout) => return Ok(false),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("filesystem watcher disconnected".into());
                }
            }
        }
        let debounce_deadline = Instant::now() + DEBOUNCE;
        loop {
            let now = Instant::now();
            if now >= debounce_deadline {
                return Ok(true);
            }
            match self
                .receiver
                .recv_timeout(debounce_deadline.saturating_duration_since(now))
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(error.to_string()),
                Err(RecvTimeoutError::Timeout) => return Ok(true),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("filesystem watcher disconnected".into());
                }
            }
        }
    }

    fn matches(&self, event: &notify::Event) -> bool {
        event.paths.is_empty()
            || event.paths.iter().any(|path| {
                path == &self.signal_path
                    || same_parent(path, &self.signal_path)
                    || self
                        .recursive_roots
                        .iter()
                        .any(|root| path.starts_with(root))
            })
    }
}

/// The symlink-resolved form of `path`, falling back to the path as given when it can't be
/// resolved (missing, or permission-denied on an ancestor) — best-effort, since the only cost of
/// falling back is the pre-existing literal comparison.
fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_parent(path: &Path, signal_path: &Path) -> bool {
    path.parent() == signal_path.parent()
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("wake-queue.signal"))
}
