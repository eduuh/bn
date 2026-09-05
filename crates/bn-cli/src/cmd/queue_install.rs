//! Keeping the dispatcher installed, without anyone having to remember to.
//!
//! The queue is explicitly enabled per machine, and an enabled queue whose dispatcher is not
//! running is a queue that quietly stops the machine. The liveness guard makes that *safe* —
//! producers fall back to launching inline — but safe is not the same as working.
//!
//! So bn ensures the timer exists rather than waiting to be asked. Three properties make that
//! defensible rather than presumptuous:
//!
//! * **Targeted.** Only `queue-tick` — the dispatcher itself. Registering the whole roster as a
//!   side effect of running `bn` would start every staff agent on a machine that never asked for
//!   them, which is a genuinely surprising thing for a command to do.
//! * **Cheap.** One `stat` on the hot path. `bn` runs constantly; anything more would be felt.
//! * **Quiet on failure.** A machine with no init system, or a read-only config dir, degrades to
//!   the inline fallback below. It never fails the command the user actually ran.
//!
//! Modelled on [`crate::cmd::cron::maybe_fire_catchup`], which already establishes that bn may do a
//! small amount of work on your behalf when you invoke it.
use crate::*;

/// Install the dispatcher as a **resident daemon**.
///
/// A timer would spawn a fresh process once a minute, do one pass and exit — so work arriving at
/// 12:00:05 waits until 12:01, and the machine pays a process spawn per minute forever. A resident
/// loop drains continuously, backs off when idle, and costs one process.
///
/// The init system supervises it: `KeepAlive` on launchd, `Restart=always` on systemd. That is the
/// whole reason not to write our own supervisor — restart, boot, and logout are solved problems and
/// bn should not own a second implementation of them.
fn install_daemon() -> Result<(), String> {
    let bn_path = dispatcher_bin();
    if !can_dispatch(&bn_path) {
        // Installing a unit that execs a binary with no `queue` subcommand produces a service that
        // fails instantly, is restarted by KeepAlive, throttles, and is finally dropped — the exact
        // state this code exists to prevent. Refuse instead, and say why.
        return Err(format!(
            "{bn_path} cannot run `bn queue run` — it is an older build. \
             Reinstall bn from a revision that has the queue."
        ));
    }
    if crate::cmd::cron::have_systemctl() {
        let dir = crate::cmd::cron::systemd_user_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(UNIT), crate::systemd::queue_unit(&bn_path))
            .map_err(|e| e.to_string())?;
        crate::cmd::cron::systemctl(&["daemon-reload"])?;
        crate::cmd::cron::systemctl(&["enable", "--now", UNIT])
    } else if crate::launchd::have_launchctl() {
        let dir = crate::launchd::agents_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.plist", crate::launchd::QUEUE_LABEL));
        std::fs::write(&path, crate::launchd::queue_daemon_plist(&bn_path))
            .map_err(|e| e.to_string())?;
        crate::launchd::bootstrap(&path, crate::launchd::QUEUE_LABEL)
    } else {
        Err("no systemd or launchd on this machine".to_string())
    }
}

/// The systemd unit name for the dispatcher.
const UNIT: &str = "bn-queue.service";

/// The bn the supervisor will exec — the installed anchor, not this process's own binary.
///
/// `current_exe()` would be wrong: a dispatcher started from a build tree must still install a unit
/// pointing at the stable installed path, or the service would break the moment that tree moved.
pub(crate) fn dispatcher_bin() -> String {
    let anchor = resolver::config_dir().join("bn");
    if anchor.exists() {
        anchor.to_string_lossy().into_owned()
    } else {
        resolver::bin_dir()
            .join("bn")
            .to_string_lossy()
            .into_owned()
    }
}

/// Whether the binary at `path` can actually run the dispatcher.
///
/// The question nothing asked, and the reason the tick died: `self-update` rebuilt bn from a
/// revision without the queue, the unit kept pointing at it, and `bn queue run` became
/// `unrecognized subcommand` on every restart until the supervisor gave up. A binary is not
/// interchangeable with a binary of the same name.
///
/// `queue --help` rather than a real pass: it proves the subcommand is *present* without dispatching
/// anything, so probing can never have side effects. clap exits non-zero on an unknown subcommand,
/// which is exactly the discriminator wanted.
pub(crate) fn can_dispatch(path: &str) -> bool {
    std::process::Command::new(path)
        .args(["queue", "--help"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether the init system currently has the dispatcher loaded.
///
/// Deliberately asks the supervisor rather than the filesystem. A unit file on disk says somebody
/// once installed it; it says nothing about whether the service survived — and a service that
/// crash-looped until launchd dropped it leaves its plist behind, looking installed forever.
fn service_loaded() -> bool {
    if crate::cmd::cron::have_systemctl() {
        return std::process::Command::new("systemctl")
            .args(["--user", "is-active", UNIT])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    }
    crate::launchd::is_loaded(crate::launchd::QUEUE_LABEL)
}

/// Remove the older per-minute `queue-tick` timer, if a previous version installed one.
///
/// Left in place it would keep firing one-shot ticks alongside the resident loop — harmless, since
/// they contend on the same lock, but two dispatchers where the user was told there is one.
fn retire_timer() {
    // Staged by cost, like [`repair_if_dead`]: `have_systemctl()` is a `systemctl --user
    // show-environment` spawn, and this runs on every `bn` invocation via [`disable`]. Stat the
    // files it would remove first — no timer on disk means nothing to retire. The launchd arm
    // below already checks its plist; this is the systemd half of that.
    let sd = crate::cmd::cron::systemd_user_dir();
    if !sd.join("bn-queue-tick.timer").exists()
        && !sd.join("bn-queue-tick.service").exists()
        && !crate::launchd::agents_dir()
            .join("com.bn.queue-tick.plist")
            .exists()
    {
        return;
    }
    if crate::cmd::cron::have_systemctl() {
        let _ = crate::cmd::cron::systemctl(&["disable", "--now", "bn-queue-tick.timer"]);
        let dir = crate::cmd::cron::systemd_user_dir();
        let _ = std::fs::remove_file(dir.join("bn-queue-tick.timer"));
        let _ = std::fs::remove_file(dir.join("bn-queue-tick.service"));
    } else if crate::launchd::have_launchctl() {
        let path = crate::launchd::agents_dir().join("com.bn.queue-tick.plist");
        if path.exists() {
            let _ = crate::launchd::bootout("com.bn.queue-tick");
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Env opt-out, matching `BN_NO_CATCHUP`'s shape.
const OPT_OUT: &str = "BN_NO_AUTO_TICK";

/// How long a machine with no installable timer may go without a tick before an ordinary `bn`
/// invocation runs one inline.
///
/// The fallback exists for containers and locked-down boxes. It is explicitly *not* the primary
/// mechanism: a dispatcher that only runs when you type `bn` cannot drain the queue while you are
/// at lunch, which is precisely when you most want it to.
const FALLBACK_TICK_AFTER_SECS: i64 = 600;

/// Make sure the dispatcher is scheduled; if it cannot be, run one inline occasionally.
///
/// Called once per `bn` invocation. Returns immediately in the common case.
pub(crate) fn ensure(cfg: &Config) {
    // The opt-out comes first, and it opts out of *both* directions.
    //
    // It used to sit below the reconcile, so `BN_NO_AUTO_TICK=1` stopped bn installing a
    // dispatcher but not from removing one — which is the more destructive half, and the half a
    // person setting this variable most obviously means to prevent.
    //
    // What that cost: every one of bn's own sandbox harnesses (`scripts/queue-shakedown.sh`,
    // `scripts/try-queue.sh`) runs bn under its own `BN_HOME`/`BN_STATE_DIR`/`BN_NOTES_DIR` and
    // sets this variable believing it is isolated. It is not. A sandbox config declares no
    // `[machines.<host>]` profile, so `queue_enabled_here()` is false, so the reconcile fired —
    // and launchd is per-uid, not per-`BN_HOME`. Running the test suite deleted the real
    // machine's dispatcher, every time, and the isolation assertion at the end of the shakedown
    // only noticed because it happened to checksum the plist.
    //
    // A tick must also never install or invoke itself.
    if std::env::var_os(OPT_OUT).is_some()
        || std::env::var_os("BN_NO_CATCHUP").is_some()
        || std::env::var_os("BN_IN_QUEUE_TICK").is_some()
    {
        return;
    }
    if !cfg.queue_enabled_here() {
        if cfg.destructive_reconcile_allowed() {
            disable();
        }
        return;
    }
    // Nothing to keep alive if no producer would use it.
    if !bn_core::resolver::ALL_PRODUCERS
        .iter()
        .any(|p| cfg.queue.captures(p))
    {
        return;
    }
    match install_state() {
        // The unit is on disk — which is not the same as running. Cheap check first.
        Install::Present => repair_if_dead(cfg),
        Install::Missing => install(cfg),
        // No init system to install into. Fall back to ticking inline, rarely.
        Install::Unsupported => fallback_tick(cfg),
    }
}

/// Remove only bn's managed resident queue service. This is the disable half of reconciliation:
/// changing the synced machine profile takes effect on the next ordinary `bn` invocation.
pub(crate) fn disable() {
    // Reconciliation, run on every `bn` invocation whose machine profile has `queue = false` —
    // which on a machine that never installed a dispatcher means four subprocess spawns
    // (two `have_systemctl()` probes plus two `disable --now`) to discover there is nothing to
    // remove. Unit/plist presence is the same source of truth [`install_state`] uses, so stat
    // first and only spawn when there is genuinely something to tear down.
    let sd = crate::cmd::cron::systemd_user_dir();
    let queue_plist =
        crate::launchd::agents_dir().join(format!("{}.plist", crate::launchd::QUEUE_LABEL));
    if !sd.join(UNIT).exists()
        && !queue_plist.exists()
        && !sd.join("bn-queue-tick.timer").exists()
        && !sd.join("bn-queue-tick.service").exists()
        && !crate::launchd::agents_dir()
            .join("com.bn.queue-tick.plist")
            .exists()
    {
        return;
    }
    if cfg!(target_os = "linux") {
        let systemd = crate::cmd::cron::have_systemctl();
        if systemd {
            let _ = crate::cmd::cron::systemctl(&["disable", "--now", UNIT]);
        }
        let path = crate::cmd::cron::systemd_user_dir().join(UNIT);
        if std::fs::remove_file(path).is_ok() && systemd {
            let _ = crate::cmd::cron::systemctl(&["daemon-reload"]);
        }
    }
    if cfg!(target_os = "macos") {
        let path =
            crate::launchd::agents_dir().join(format!("{}.plist", crate::launchd::QUEUE_LABEL));
        if path.exists() {
            if crate::launchd::have_launchctl() {
                let _ = crate::launchd::bootout(crate::launchd::QUEUE_LABEL);
            }
            let _ = std::fs::remove_file(path);
        }
    }
    retire_timer();
}

/// Notice a dispatcher that is installed but gone, and put it back.
///
/// [`install_state`] only knows whether the unit *file* exists, and that was enough to make bn
/// permanently blind to its own dispatcher dying: a service that crash-looped until the supervisor
/// dropped it leaves its plist behind, so `ensure` saw `Present`, did nothing, and no amount of
/// running `bn` would ever repair it. A self-heal that cannot detect the failure it heals is not one.
///
/// Staged by cost, because this runs on every single `bn` invocation:
///
///   1. **One stat** on the decision file. A dispatcher that ticked recently is fine — and that is
///      the overwhelmingly common case, so it is the only cost almost every invocation pays.
///   2. Only if that says *stale*: ask the init system whether the service is loaded.
///   3. Only if it is not: reinstall — which [`install_daemon`] itself refuses if the installed
///      binary cannot run the dispatcher, so a bad image is reported rather than crash-looped.
fn repair_if_dead(cfg: &Config) {
    if daemon_alive() {
        return;
    }
    // A dispatcher that is down stays down for a while — a merge, a reinstall — and every `bn`
    // invocation in that window would otherwise pay for a `launchctl` spawn to be told the same
    // thing. The existing back-off already throttles the *repair*; check it before the diagnosis
    // too, or the cheap path stops being cheap exactly when the machine is already unhappy.
    if backed_off() {
        fallback_tick(cfg);
        return;
    }
    if service_loaded() {
        // Loaded but not ticking is a different fault — a wedged pass, a held lock — and
        // reinstalling would just restart something that is already there. Cover the gap inline.
        fallback_tick(cfg);
        return;
    }
    eprintln!("bn: the queue dispatcher is installed but not running — restarting it");
    install(cfg);
}

/// Whether the resident dispatcher itself beat recently, by file mtime alone.
///
/// Reads the daemon's heartbeat rather than the decision file. A decision is written by any tick,
/// including the inline fallback below — so gating on it let the fallback hide a dead daemon
/// indefinitely, which is the opposite of what this is for.
///
/// One stat, because this runs on every `bn` invocation.
fn daemon_alive() -> bool {
    let path = crate::cmd::queue_tick::heartbeat_path();
    std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < bn_core::resolver::DISPATCHER_TTL_SECS as u64)
}

enum Install {
    Present,
    Missing,
    Unsupported,
}

/// Whether the `queue-tick` unit is on disk — one stat, no process spawn.
fn install_state() -> Install {
    let Some(path) = unit_path() else {
        return Install::Unsupported;
    };
    if path.exists() {
        Install::Present
    } else {
        Install::Missing
    }
}

/// Where this platform's dispatcher artifact lives.
fn unit_path() -> Option<std::path::PathBuf> {
    if cfg!(target_os = "macos") {
        Some(crate::launchd::agents_dir().join(format!("{}.plist", crate::launchd::QUEUE_LABEL)))
    } else if cfg!(target_os = "linux") {
        Some(crate::cmd::cron::systemd_user_dir().join(UNIT))
    } else {
        None
    }
}

/// Where the last failed install recorded why.
fn install_stamp() -> std::path::PathBuf {
    bn_core::resolver::state_dir().join("queue-tick-install-failed")
}

/// Whether a recent install attempt failed, so this one should not retry yet.
///
/// A permission error, a missing init system, or — the case that prompted this — an installed
/// binary that cannot run the dispatcher are all conditions that persist. Retrying them on every
/// `bn` invocation would spend a process spawn each time to reach the same conclusion.
fn backed_off() -> bool {
    const RETRY_AFTER_SECS: u64 = 3600;
    std::fs::metadata(install_stamp())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < RETRY_AFTER_SECS)
}

/// Install the timer, once, and say so.
///
/// Announced rather than silent: installing a scheduled job is a real change to the machine, and a
/// user who did not ask for it should at least be told it happened and how to undo it.
fn install(cfg: &Config) {
    if backed_off() {
        fallback_tick(cfg);
        return;
    }
    let stamp = install_stamp();
    match install_daemon() {
        Ok(()) => {
            let _ = std::fs::remove_file(&stamp);
            retire_timer();
            eprintln!(
                "bn: started the queue dispatcher (a resident loop, supervised by the init system). \
                 Set {OPT_OUT}=1 to stop installing it."
            );
        }
        Err(e) => {
            let _ = std::fs::write(&stamp, e.as_bytes());
            eprintln!("bn: could not install the queue dispatcher — {e}");
            fallback_tick(cfg);
        }
    }
}

/// Run one tick inline, if nothing has for a while.
///
/// The degraded mode: better than a queue that never drains, worse than a timer, and honest about
/// which it is. Bounded by the same lock every other driver takes, so it can never overlap one.
fn fallback_tick(cfg: &Config) {
    let now = bn_core::util::now_epoch_secs();
    let recent = bn_core::queue::decision::QueueDecision::read()
        .is_some_and(|d| !d.dry_run && now.saturating_sub(d.at) < FALLBACK_TICK_AFTER_SECS);
    if recent {
        return;
    }
    crate::cmd::queue_tick::job_queue_tick(cfg, None);
}
