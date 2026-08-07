//! Shared blocking reactor for daemon-style CLI commands.
//!
//! Filesystem notifications provide the fast path; deadlines preserve time-based reconciliation
//! and serve as the cross-platform fallback; SIGINT/SIGTERM become an explicit shutdown event.

use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(100);
const SHUTDOWN_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopEvent {
    Filesystem,
    Deadline,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Watch {
    File(PathBuf),
    Directory(PathBuf),
    Tree(PathBuf),
}

impl Watch {
    pub(crate) fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    pub(crate) fn directory(path: impl Into<PathBuf>) -> Self {
        Self::Directory(path.into())
    }

    pub(crate) fn tree(path: impl Into<PathBuf>) -> Self {
        Self::Tree(path.into())
    }
}

enum Filter {
    File(PathBuf),
    Directory(PathBuf),
    Tree(PathBuf),
}

impl Filter {
    fn matches(&self, path: &Path) -> bool {
        match self {
            Self::File(target) => file_event_matches(path, target),
            Self::Directory(root) => path == root || path.parent() == Some(root.as_path()),
            Self::Tree(root) => path.starts_with(root),
        }
    }
}

struct Notifier {
    _watcher: notify::RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
    filters: Vec<Filter>,
}

impl Notifier {
    fn new(watches: Vec<Watch>) -> notify::Result<Option<Self>> {
        let mut filters = Vec::new();
        let mut roots: BTreeMap<PathBuf, bool> = BTreeMap::new();

        for watch in watches {
            match watch {
                Watch::File(path) => {
                    let Some(parent) = path.parent() else {
                        continue;
                    };
                    std::fs::create_dir_all(parent).map_err(notify::Error::io)?;
                    let target = normalize(&path);
                    let Some(parent) = target.parent().map(Path::to_path_buf) else {
                        continue;
                    };
                    filters.push(Filter::File(target));
                    roots.entry(parent).or_insert(false);
                }
                Watch::Directory(path) => {
                    std::fs::create_dir_all(&path).map_err(notify::Error::io)?;
                    let root = normalize(&path);
                    filters.push(Filter::Directory(root.clone()));
                    roots.entry(root).or_insert(false);
                }
                Watch::Tree(path) => {
                    if !path.is_dir() {
                        continue;
                    }
                    let root = normalize(&path);
                    filters.push(Filter::Tree(root.clone()));
                    roots
                        .entry(root)
                        .and_modify(|recursive| *recursive = true)
                        .or_insert(true);
                }
            }
        }

        if roots.is_empty() {
            return Ok(None);
        }

        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        for (root, recursive) in roots {
            watcher.watch(
                &root,
                if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                },
            )?;
        }

        Ok(Some(Self {
            _watcher: watcher,
            receiver,
            filters,
        }))
    }

    fn matches(&self, event: &notify::Event) -> bool {
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        event.paths.is_empty()
            || event.paths.iter().any(|path| {
                let path = normalize(path);
                self.filters.iter().any(|filter| filter.matches(&path))
            })
    }
}

pub(crate) struct EventLoop {
    label: String,
    notifier: Option<Notifier>,
}

impl EventLoop {
    pub(crate) fn new(label: impl Into<String>, watches: Vec<Watch>) -> Self {
        install_shutdown_handlers();
        let label = label.into();
        let notifier = match Notifier::new(watches) {
            Ok(notifier) => notifier,
            Err(error) => {
                eprintln!("{label}: filesystem notifications unavailable ({error}); polling");
                None
            }
        };
        Self { label, notifier }
    }

    pub(crate) fn with_wake_queue(
        label: impl Into<String>,
        mut watches: Vec<Watch>,
        recursive_roots: &[PathBuf],
    ) -> Self {
        let queue = bn_core::wake::WakeQueue::default();
        watches.push(Watch::file(queue.signal_path()));
        watches.extend(recursive_roots.iter().cloned().map(Watch::tree));
        Self::new(label, watches)
    }

    pub(crate) fn wait(&mut self, timeout: Duration) -> LoopEvent {
        let deadline = Instant::now() + timeout;
        loop {
            if shutdown_requested() {
                return LoopEvent::Shutdown;
            }
            let now = Instant::now();
            if now >= deadline {
                return LoopEvent::Deadline;
            }
            let wait = deadline.saturating_duration_since(now).min(SHUTDOWN_POLL);
            let Some(notifier) = self.notifier.as_ref() else {
                std::thread::sleep(wait);
                continue;
            };
            let received = notifier.receiver.recv_timeout(wait);
            match received {
                Ok(Ok(event)) if notifier.matches(&event) => {
                    self.debounce();
                    return if shutdown_requested() {
                        LoopEvent::Shutdown
                    } else {
                        LoopEvent::Filesystem
                    };
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => self.disable_notifications(&error),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.disable_notifications("filesystem watcher disconnected");
                }
            }
        }
    }

    fn debounce(&mut self) {
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            if shutdown_requested() {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let Some(notifier) = self.notifier.as_ref() else {
                return;
            };
            let received = notifier
                .receiver
                .recv_timeout(deadline.saturating_duration_since(now).min(SHUTDOWN_POLL));
            match received {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    self.disable_notifications(&error);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => {
                    self.disable_notifications("filesystem watcher disconnected");
                    return;
                }
            }
        }
    }

    fn disable_notifications(&mut self, error: impl std::fmt::Display) {
        eprintln!(
            "{}: filesystem notifications failed ({error}); polling",
            self.label
        );
        self.notifier = None;
    }
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    path.file_name()
        .map_or(parent.clone(), |name| parent.join(name))
}

fn file_event_matches(path: &Path, target: &Path) -> bool {
    if path == target {
        return true;
    }
    if path.parent() != target.parent() {
        return false;
    }
    let Some(path_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(target_name) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    path_name == format!("{target_name}.tmp")
        || path_name
            .strip_prefix('.')
            .is_some_and(|name| name.starts_with(&format!("{target_name}.")))
}

#[cfg(unix)]
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_shutdown_signal(_signal: i32) {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "installing process signal handlers requires the Unix signal FFI"
)]
fn install_shutdown_handlers() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        // SAFETY: the handler has C ABI, touches only an atomic, and remains valid for process life.
        unsafe {
            signal(SIGINT, handle_shutdown_signal as *const () as usize);
            signal(SIGTERM, handle_shutdown_signal as *const () as usize);
        }
    });
}

#[cfg(not(unix))]
fn install_shutdown_handlers() {}

#[cfg(unix)]
fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(unix))]
fn shutdown_requested() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_filter_matches_direct_and_atomic_replacements_only() {
        let target = PathBuf::from("state/wake-queue.signal");
        assert!(file_event_matches(&target, &target));
        assert!(file_event_matches(
            Path::new("state/.wake-queue.signal.10.2.tmp"),
            &target
        ));
        assert!(file_event_matches(
            Path::new("state/wake-queue.signal.tmp"),
            &target
        ));
        assert!(!file_event_matches(
            Path::new("state/wake-queue.signal.lock"),
            &target
        ));
        assert!(!file_event_matches(
            Path::new("other/wake-queue.signal"),
            &target
        ));
    }

    #[test]
    fn directory_and_tree_filters_have_distinct_scope() {
        let directory = Filter::Directory(PathBuf::from("state"));
        let tree = Filter::Tree(PathBuf::from("state"));
        assert!(directory.matches(Path::new("state/queue.json")));
        assert!(!directory.matches(Path::new("state/nested/queue.json")));
        assert!(tree.matches(Path::new("state/nested/queue.json")));
    }

    #[test]
    fn event_loop_reaches_its_deadline_without_sources() {
        let mut reactor = EventLoop::new("test reactor", Vec::new());
        assert_eq!(reactor.wait(Duration::from_millis(5)), LoopEvent::Deadline);
    }

    #[test]
    fn event_loop_wakes_for_an_atomic_file_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        let mut reactor = EventLoop::new("test reactor", vec![Watch::file(&target)]);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let tmp = target.with_file_name(".state.json.1.1.tmp");
            std::fs::write(&tmp, b"ready").unwrap();
            std::fs::rename(tmp, target).unwrap();
        });

        assert_eq!(reactor.wait(Duration::from_secs(5)), LoopEvent::Filesystem);
        writer.join().unwrap();
    }

    #[test]
    fn event_loop_delivers_a_real_worker_completion() {
        let dir = tempfile::tempdir().unwrap();
        let queue = bn_core::wake::WakeQueue::at(
            dir.path().join("wake-queue.json"),
            dir.path().join("wake-queue.signal"),
        );
        let mut reactor = EventLoop::new("test reactor", vec![Watch::file(queue.signal_path())]);
        let publisher_queue = queue.clone();
        let publisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let wake = bn_core::wake::ActionableWake::classify(
                "fleet:session-123:done",
                123,
                bn_core::wake::FleetTransition::Done,
                "bn/event-loop — worker completed",
                bn_core::wake::WakeOwner::Controller("controller-1".into()),
            )
            .unwrap();
            publisher_queue.publish(wake).unwrap()
        });

        assert_eq!(reactor.wait(Duration::from_secs(5)), LoopEvent::Filesystem);
        assert!(publisher.join().unwrap());

        let watch_target = bn_core::wake::WakeTarget::Watch;
        let pending = queue.pending(&watch_target).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_id, "fleet:session-123:done");
        assert_eq!(pending[0].transition, bn_core::wake::FleetTransition::Done);

        let ids = std::collections::BTreeSet::from([pending[0].event_id.clone()]);
        assert_eq!(queue.acknowledge(&watch_target, &ids).unwrap(), 1);
        assert!(queue.pending(&watch_target).unwrap().is_empty());
        assert_eq!(
            queue
                .pending(&bn_core::wake::WakeTarget::Controller {
                    id: "controller-1".into(),
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn event_loop_turns_sigterm_into_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "event_loop::tests::sigterm_child",
                "--nocapture",
            ])
            .env("BN_EVENT_LOOP_SIGNAL_CHILD", "1")
            .env("BN_EVENT_LOOP_SIGNAL_READY", &ready)
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("signal test child exited before becoming ready: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal test child did not become ready");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let signal = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(signal.success());
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess helper for event_loop_turns_sigterm_into_shutdown"]
    fn sigterm_child() {
        assert_eq!(
            std::env::var("BN_EVENT_LOOP_SIGNAL_CHILD").as_deref(),
            Ok("1")
        );
        let ready = PathBuf::from(std::env::var_os("BN_EVENT_LOOP_SIGNAL_READY").unwrap());
        let mut reactor = EventLoop::new("signal test reactor", Vec::new());
        std::fs::write(ready, b"ready").unwrap();
        assert_eq!(reactor.wait(Duration::from_secs(10)), LoopEvent::Shutdown);
    }
}
