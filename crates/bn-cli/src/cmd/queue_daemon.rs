//! Starting, replacing and retiring the dispatcher — bn's own job now, not the init system's.
//!
//! An enabled queue whose dispatcher is not running is a queue that quietly stops the machine. The
//! liveness guard makes that *safe* — producers fall back to launching inline — but safe is not the
//! same as working, and on the machine this was written for it meant 364 entries and an oldest
//! pending item three and a half days old.
//!
//! bn used to hand the problem to the init system: a `bn-queue.service` with `Restart=always`, or a
//! `com.bn.queue` plist with `KeepAlive`, on the argument that restart, boot and logout are solved
//! problems and bn should not own a second implementation of them. That argument was right about
//! restart and wrong about everything it cost:
//!
//! * **It only worked on two platforms.** There was no Windows arm at all, so `install_daemon`
//!   returned "no systemd or launchd on this machine" and the queue simply had no dispatcher there.
//! * **It made liveness a per-machine install.** Asking whether the dispatcher was running meant
//!   spawning `systemctl`/`launchctl`, so the answer was expensive enough to need a back-off stamp,
//!   a staleness heuristic and a repair path — three mechanisms in place of one question.
//! * **It could not be sandboxed.** launchd is per-uid and systemd is per-user; neither respects
//!   `BN_HOME`. bn's own test harnesses believed they were isolated and twice deleted the
//!   developer's real dispatcher on every run.
//!
//! What we give up by owning it: **nothing starts the dispatcher at boot**. The first ordinary `bn`
//! invocation does, via [`ensure`]. In practice that is close to continuous — the tmux status bar
//! alone runs `bn` every few seconds — but it is a real difference and worth stating plainly rather
//! than discovering. What we get is a dispatcher that runs on every platform, needs no install,
//! costs one syscall pair to check, and cannot be removed by anything but bn.
//!
//! The supervision itself is three lines of policy, because the kernel does the hard part: a
//! dispatcher holds [`bn_core::queue::daemon::DaemonLock`] for its whole life, the lock is released
//! on *any* exit, and whoever finds it free starts a replacement.
use bn_core::queue::daemon;
use bn_core::resolver::Config;
use crate::Envelope;

/// Start a dispatcher in the background, detached from whatever invoked us.
///
/// Detached properly — `setsid`, not merely a new process group. This is called from arbitrary `bn`
/// invocations, most of them inside a tmux pane the person is about to close or Ctrl-C; a
/// dispatcher that died with the terminal that happened to start it would be worse than no
/// dispatcher, because it would look installed. Mirrors [`crate::serve::spawn_detached`], which
/// already does exactly this for the dashboard when no service manager is available.
///
/// The child's output goes to a real file rather than `/dev/null`. It used to land in
/// `/tmp/com.bn.queue.log` — named for the launchd label even on Linux, because launchd was what
/// redirected the stream. bn opens it now, so it lives with the rest of the state and keeps the
/// truncation `cap_own_log` already applies.
///
/// Returns whether the spawn itself succeeded. It says nothing about whether the child went on to
/// take the lock: two `bn` invocations racing here both spawn, and the loser exits immediately on
/// [`daemon::Denied::Running`]. That race is the reason the child re-checks rather than trusting
/// its parent's probe.
#[allow(
    unsafe_code,
    reason = "CommandExt::pre_exec is required to detach the dispatcher before exec"
)]
pub(crate) fn spawn_dispatcher() -> bool {
    let bin = crate::cmd::queue_install::dispatcher_bin();
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(["queue", "run"]);
    // The dispatcher must not inherit the extent marker: inside `BN_IN_QUEUE_TICK` every producer
    // runs work inline instead of queueing it, so a dispatcher started from within a tick would
    // spend its whole life refusing to queue anything.
    cmd.env_remove("BN_IN_QUEUE_TICK");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and the closure touches nothing else between fork
        // and exec.
        unsafe {
            cmd.pre_exec(|| {
                setsid_detached();
                Ok(())
            });
        }
    }
    cmd.stdin(std::process::Stdio::null());
    match open_log() {
        Some((out, err)) => {
            cmd.stdout(std::process::Stdio::from(out));
            cmd.stderr(std::process::Stdio::from(err));
        }
        // A log we cannot open is not a reason to leave the machine without a dispatcher.
        None => {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        }
    }
    cmd.spawn().is_ok()
}

/// The dispatcher's log, opened twice so stdout and stderr interleave in one file.
fn open_log() -> Option<(std::fs::File, std::fs::File)> {
    let path = daemon::log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let out = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let err = out.try_clone().ok()?;
    Some((out, err))
}

/// setsid(2) via raw syscall — detach into a new session without reaching for a wrapper crate.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "setsid has no standard-library wrapper and is isolated to this FFI boundary"
)]
fn setsid_detached() {
    extern "C" {
        fn setsid() -> i32;
    }
    // SAFETY: `setsid` takes no arguments, writes through no pointer, and is async-signal-safe.
    unsafe {
        setsid();
    }
}

/// Stop the running dispatcher, if there is one.
///
/// Used when a machine's profile turns the queue off. SIGTERM rather than SIGKILL because the
/// dispatcher's event loop already treats it as a clean shutdown, which lets an in-flight pass
/// finish writing its decision instead of leaving the last word half-written.
///
/// Best-effort by design: the pid comes from the lock file, which is advisory. If it names a
/// process that has already exited — or, worse, a recycled pid — the signal is simply not
/// delivered to a dispatcher, and the lock being free is what actually matters. So we check the
/// lock, not the signal's success.
pub(crate) fn stop_dispatcher() -> bool {
    if !daemon::is_running() {
        return false;
    }
    let Some(pid) = daemon::holder_pid() else {
        return false;
    };
    signal_term(pid);
    true
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::kill is the only way to signal another process; the pid is validated as \
              positive and the call writes through no pointer"
)]
fn signal_term(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    if pid <= 0 {
        return;
    }
    // SAFETY: a plain signal delivery to a positive pid; no memory is shared with the call.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn signal_term(_pid: u32) {}

/// `bn queue stop` — stop this machine's dispatcher.
///
/// Deliberately not a persistent "off" switch. The dispatcher is meant to be running, so the next
/// ordinary `bn` invocation starts a fresh one; this is for the moments you want the machine to
/// stop starting work *now* — before a reboot, or while you take the box apart. Staying off is what
/// `BN_NO_AUTO_TICK=1` and the machine profile's `queue = false` are for, and the message says so
/// rather than letting someone discover it by watching a dispatcher come back.
pub(crate) fn emit_stop(json: bool) {
    let pid = daemon::holder_pid();
    let stopped = stop_dispatcher();
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-stop",
                serde_json::json!({ "stopped": stopped, "pid": pid }),
            )
            .to_json()
        );
        return;
    }
    if !stopped {
        println!("no dispatcher is running on this machine.");
        return;
    }
    match pid {
        Some(pid) => println!("stopped the queue dispatcher (pid {pid})."),
        None => println!("stopped the queue dispatcher."),
    }
    println!("  the next `bn` command starts a new one — BN_NO_AUTO_TICK=1 keeps it off.");
}

// --- the dispatcher process's own lifecycle ---------------------------------

/// What the dispatcher should do after checking on the world it started in.
pub(crate) enum Lifecycle {
    /// Carry on.
    Continue,
    /// Stop. A successor is already starting, or this machine no longer wants a dispatcher.
    Retire,
}

/// The two facts about a long-lived dispatcher's world that can change under it: the binary it runs
/// and the config it read.
///
/// Both matter for the same reason. A process that lives for weeks accumulates a stale view of a
/// world that keeps moving, and a dispatcher holding the config you edited an hour ago is worse
/// than one that is down, because it looks like it is working.
///
/// Kept here rather than in the tick because both are questions about the dispatcher *process*, and
/// the answer to one of them — starting a successor — is this module's job anyway.
pub(crate) struct Supervision {
    config_seen: Option<Stamp>,
    exe_seen: Option<Stamp>,
}

/// A cheap fingerprint of a file — modified time and length.
///
/// Two fields rather than mtime alone: an editor that writes-and-restores a timestamp still changes
/// the length, and a same-length edit still moves the clock. `None` when unreadable, which compares
/// equal to itself, so a missing file does not read as changing every pass.
type Stamp = (std::time::SystemTime, u64);

impl Supervision {
    pub(crate) fn start() -> Supervision {
        Supervision {
            config_seen: config_stamp(),
            exe_seen: exe_stamp(),
        }
    }

    /// Look once at the binary and the config, acting on either if it moved.
    ///
    /// Reloads `cfg` in place when the config changed, so the caller's next pass sees it.
    pub(crate) fn check(&mut self, cfg: &mut Config) -> Lifecycle {
        if matches!(self.check_binary(), Lifecycle::Retire) {
            return Lifecycle::Retire;
        }
        self.check_config(cfg)
    }

    /// A new binary means this process is the old one, and there is no supervisor to notice.
    ///
    /// So the handover is ours to perform: start the successor, **then** exit. The order is
    /// load-bearing in both directions — the successor blocks on the lock this process still holds
    /// until it returns, so the two can never dispatch at once, and starting it before leaving is
    /// what stops the machine going dispatcher-less if this is the last `bn` invocation for hours.
    ///
    /// Only once the new image is known to work. **This process is the last dispatcher the machine
    /// has**, and handing over is irreversible: if the successor cannot run `queue run` it exits
    /// instantly and nothing is left to try again. That is not hypothetical — `self-update` once
    /// rebuilt bn from a revision predating the queue, and the dispatcher dutifully retired itself
    /// in favour of a binary that had no `queue` subcommand at all.
    ///
    /// Each new binary is probed exactly once — `exe_seen` advances either way — so a machine stuck
    /// on a bad build costs one probe per build, not one per pass.
    fn check_binary(&mut self) -> Lifecycle {
        let current = exe_stamp();
        if current == self.exe_seen {
            return Lifecycle::Continue;
        }
        self.exe_seen = current;
        let successor = crate::cmd::queue_install::dispatcher_bin();
        if !crate::cmd::queue_install::can_dispatch(&successor) {
            eprintln!(
                "queue dispatcher: {successor} changed but cannot run `bn queue run` — staying on \
                 the running image. The queue keeps draining; reinstall bn from a revision with the \
                 queue to hand over."
            );
            return Lifecycle::Continue;
        }
        if spawn_dispatcher() {
            println!("queue dispatcher: binary changed — handed over to {successor}");
            return Lifecycle::Retire;
        }
        eprintln!(
            "queue dispatcher: {successor} is a valid successor but could not be started — staying \
             on the running image so the machine keeps a dispatcher."
        );
        Lifecycle::Continue
    }

    /// Config is cheap to stat and only re-parsed when it actually moved.
    fn check_config(&mut self, cfg: &mut Config) -> Lifecycle {
        let stamp = config_stamp();
        if stamp == self.config_seen {
            return Lifecycle::Continue;
        }
        self.config_seen = stamp;
        *cfg = Config::load();
        println!("queue dispatcher: config reloaded");
        if !cfg.queue_enabled_here() {
            println!("queue dispatcher: no longer enabled on this machine — exiting");
            return Lifecycle::Retire;
        }
        Lifecycle::Continue
    }

    /// Keep the dispatcher's own log from growing without bound.
    ///
    /// bn opens this file itself (see [`spawn_dispatcher`]), and nothing rotates it. The volume is
    /// not really ours either: express jobs run *inline* in the tick, so every captured cron's
    /// output lands here too — duplicating the 15-line tail `run_native_job` already records
    /// against each `cron_run` event. The durable record is the event log; this file is a
    /// convenience.
    ///
    /// So it is truncated rather than rotated. Losing yesterday's copy of something already stored
    /// twice is not a loss, and a rotation scheme would be more machinery than the thing is worth.
    pub(crate) fn cap_own_log(&self) {
        const MAX_BYTES: u64 = 4 * 1024 * 1024;
        let path = daemon::log_path();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES)
            // Our own fd is in append mode, so truncating underneath it is safe: the next write
            // lands at the new end rather than leaving a sparse hole.
            && std::fs::File::create(&path).is_ok()
        {
            println!("queue dispatcher: truncated its own log (>4MB; the event log keeps the record)");
        }
    }
}

fn config_stamp() -> Option<Stamp> {
    stamp(&bn_core::resolver::config_file_path())
}

/// The running binary, resolved through any symlink so replacing the target of `~/.bn/config/bn`
/// counts as a change.
pub(crate) fn executable_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

fn exe_stamp() -> Option<Stamp> {
    stamp(&executable_path()?)
}

fn stamp(path: &std::path::Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::test_support::with_state_dir;

    /// The property `ensure` leans on: a machine with no dispatcher reports none, so exactly one
    /// invocation goes on to start one.
    #[test]
    fn a_machine_with_no_dispatcher_has_nothing_to_stop() {
        with_state_dir(|| {
            assert!(!daemon::is_running());
            assert!(!stop_dispatcher(), "nothing to stop on an idle machine");
        });
    }

}
