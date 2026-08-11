//! The dispatcher's two liveness signals: that it is alive, and that it is getting anywhere.
//!
//! They were one signal, and that was the bug. `beat()` used to be the first statement in the
//! resident loop's body, which makes heartbeat freshness a function of how long a pass takes — and
//! a pass waits synchronously on `bn dispatch`, which fetches, which was one dead TCP connection
//! away from never returning. The dispatcher sat alive-but-blocked for twenty-five minutes while
//! `dispatcher_alive` went false, the dashboard reported "no tick is running", and launchd — whose
//! process was perfectly healthy — had no reason to restart anything. Nothing was in a position to
//! notice, because the only liveness signal was downstream of the thing that hung.
//!
//! So they are separate facts now, stored separately: a timer thread that nothing can block writes
//! the heartbeat, and each completed pass stamps its own file. "Alive" means alive; whether the
//! work is moving is [`stamp_pass`]'s question, read back by
//! [`bn_core::resolver::dispatcher_pass_age`].

use std::path::PathBuf;

/// The resident dispatcher's heartbeat file.
///
/// Distinct from `queue-decision.json` on purpose, and the distinction is the whole point: a
/// decision is written by *any* tick, including the inline fallback `bn` runs when it notices the
/// dispatcher is missing. Using that as the liveness signal made the fallback mask the very failure
/// it compensates for — run `bn` often enough and the decision file never goes stale, so the repair
/// that would restart the daemon never fires and the machine quietly runs on inline ticks forever.
///
/// Only [`super::emit_run`] writes this, so "recent" means the daemon itself is alive.
pub(crate) fn heartbeat_path() -> PathBuf {
    bn_core::resolver::dispatcher_heartbeat_path()
}

/// A background thread that writes the heartbeat on a timer, for as long as it is held.
///
/// A thread that only writes a timestamp cannot be blocked by the work — which is the entire
/// reason it is a thread and not a line at the top of the loop.
pub(super) struct Heartbeat {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Seconds between heartbeats. Well inside `DISPATCHER_TTL_SECS` (600) so a single missed write —
/// a full disk, a slow sync — is not enough to declare the dispatcher dead.
const BEAT_SECS: u64 = 30;

impl Heartbeat {
    pub(super) fn start() -> Heartbeat {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        beat();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                // Slept a second at a time so shutdown is prompt rather than up to BEAT_SECS late.
                for _ in 0..BEAT_SECS {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                beat();
            }
        });
        Heartbeat {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Record that the resident loop is alive. Best-effort: a dispatcher must not stop dispatching
/// because it could not write a heartbeat.
pub(super) fn beat() {
    write_stamp(&heartbeat_path());
}

/// Record that a pass finished. The signal that separates "wedged" from "idle" — see
/// [`bn_core::resolver::dispatcher_pass_age`].
pub(super) fn stamp_pass() {
    write_stamp(&bn_core::resolver::dispatcher_pass_path());
}

fn write_stamp(path: &std::path::Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, bn_core::util::now_epoch_secs().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::test_support::with_state_dir;

    /// Liveness and progress are separate files because they are separate facts — the whole point
    /// of moving the heartbeat onto a thread.
    #[test]
    fn heartbeat_and_pass_are_recorded_separately() {
        with_state_dir(|| {
            beat();
            assert!(bn_core::resolver::dispatcher_alive(
                bn_core::util::now_epoch_secs()
            ));
            assert!(
                bn_core::resolver::dispatcher_pass_age(bn_core::util::now_epoch_secs()).is_none(),
                "a heartbeat alone must not claim a pass has completed"
            );

            stamp_pass();
            assert_eq!(
                bn_core::resolver::dispatcher_pass_age(bn_core::util::now_epoch_secs()),
                Some(0)
            );
        });
    }
}
