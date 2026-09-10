//! Keeping a dispatcher running, without anyone having to remember to.
//!
//! One question, asked once per `bn` invocation: **is a dispatcher running?** If not, and this
//! machine wants one, start it. [`crate::cmd::queue_daemon`] owns the mechanism and the argument
//! for bn supervising its own dispatcher; this module is the policy that decides when to.
//!
//! Three properties make asking on every invocation defensible rather than presumptuous:
//!
//! * **Targeted.** Only the dispatcher. Registering the whole cron roster as a side effect of
//!   running `bn` would start every staff agent on a machine that never asked for them, which is a
//!   genuinely surprising thing for a command to do.
//! * **Cheap.** One open, one non-blocking `flock`, one close — cheaper than the `stat` this cost
//!   when liveness was a heartbeat, and far cheaper than the `systemctl` spawn before that. `bn`
//!   runs constantly; anything more would be felt.
//! * **Quiet on failure.** A machine where the spawn fails degrades to the inline fallback below.
//!   It never fails the command the user actually ran.
//!
//! Modelled on [`crate::cmd::cron::maybe_fire_catchup`], which already establishes that bn may do a
//! small amount of work on your behalf when you invoke it.
use crate::*;
use bn_core::queue::daemon;

/// Env opt-out, matching `BN_NO_CATCHUP`'s shape.
const OPT_OUT: &str = "BN_NO_AUTO_TICK";

/// How long a machine that cannot start a dispatcher may go without a tick before an ordinary `bn`
/// invocation runs one inline.
///
/// The fallback exists for locked-down boxes and filesystems with no advisory locking. It is
/// explicitly *not* the primary mechanism: a dispatcher that only runs when you type `bn` cannot
/// drain the queue while you are at lunch, which is precisely when you most want it to.
const FALLBACK_TICK_AFTER_SECS: i64 = 600;

/// Make sure a dispatcher is running; if one cannot be started, run a tick inline occasionally.
///
/// Called once per `bn` invocation. Returns immediately in the common case.
pub(crate) fn ensure(cfg: &Config) {
    // The opt-out comes first, and it opts out of *both* directions.
    //
    // It used to sit below the reconcile, so `BN_NO_AUTO_TICK=1` stopped bn starting a dispatcher
    // but not from stopping one — which is the more destructive half, and the half a person setting
    // this variable most obviously means to prevent.
    //
    // What that cost: every one of bn's own sandbox harnesses (`scripts/queue-shakedown.sh`,
    // `scripts/try-queue.sh`) runs bn under its own `BN_HOME`/`BN_STATE_DIR`/`BN_NOTES_DIR` and
    // sets this variable believing it is isolated. Against launchd it was not — launchd is per-uid,
    // not per-`BN_HOME` — so running the test suite deleted the real machine's dispatcher every
    // time. bn owning the dispatcher removes that failure mode outright (the lock lives under
    // `BN_STATE_DIR`, so a sandbox really is one), but the ordering stays: it was right on its own
    // terms.
    //
    // A tick must also never start or invoke itself.
    if std::env::var_os(OPT_OUT).is_some()
        || std::env::var_os("BN_NO_CATCHUP").is_some()
        || std::env::var_os("BN_IN_QUEUE_TICK").is_some()
    {
        return;
    }
    // Off only when a profile says `queue = false`: a missing or unreadable config switches nothing
    // off, so it can never uninstall a dispatcher on its account.
    if !cfg.queue_enabled_here() {
        disable();
        return;
    }
    // Nothing to keep alive if no producer would use it.
    if !bn_core::resolver::ALL_PRODUCERS
        .iter()
        .any(|p| cfg.queue.captures(p))
    {
        return;
    }
    if daemon::is_running() {
        return;
    }
    // Not running. Either it never started, or it exited — and both have the same answer, which is
    // the whole benefit of the kernel holding the signal: there is no third "installed but dead"
    // state to detect, diagnose and repair.
    if backed_off() {
        fallback_tick(cfg);
        return;
    }
    start(cfg);
}

/// Start a dispatcher, and say so.
///
/// Announced rather than silent: starting a background process that outlives your shell is a real
/// change to the machine, and someone who did not ask for it should at least be told it happened
/// and how to stop it.
fn start(cfg: &Config) {
    let stamp = start_stamp();
    // Refuse before spawning if the installed binary cannot run the dispatcher. Without this the
    // spawn "succeeds", the child exits instantly on an unknown subcommand, the next `bn`
    // invocation finds the lock free and spawns again — a spawn loop at the rate the person types.
    let bin = dispatcher_bin();
    if !can_dispatch(&bin) {
        let why = format!(
            "{bin} cannot run `bn queue run` — it is an older build. \
             Reinstall bn from a revision that has the queue."
        );
        let _ = std::fs::write(&stamp, why.as_bytes());
        eprintln!("bn: could not start the queue dispatcher — {why}");
        fallback_tick(cfg);
        return;
    }
    if crate::cmd::queue_daemon::spawn_dispatcher() {
        let _ = std::fs::remove_file(&stamp);
        eprintln!(
            "bn: started the queue dispatcher (a background loop bn supervises itself). \
             Stop it with `bn queue stop`, or set {OPT_OUT}=1 to stop starting it."
        );
        return;
    }
    let _ = std::fs::write(&stamp, b"spawn failed");
    eprintln!("bn: could not start the queue dispatcher — the spawn failed");
    fallback_tick(cfg);
}

/// Stop bn's dispatcher.
///
/// The disable half of reconciliation: changing the synced machine profile takes effect on the next
/// ordinary `bn` invocation.
pub(crate) fn disable() {
    crate::cmd::queue_daemon::stop_dispatcher();
}

/// The bn a dispatcher should be started from — the installed anchor, not this process's binary.
///
/// `current_exe()` would be wrong: a dispatcher started from a build tree must still point at the
/// stable installed path, or it would break the moment that tree moved.
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
/// The question nothing asked, and the reason the tick once died: `self-update` rebuilt bn from a
/// revision without the queue, the unit kept pointing at it, and `bn queue run` became
/// `unrecognized subcommand` on every restart until the supervisor gave up. A binary is not
/// interchangeable with a binary of the same name.
///
/// It matters *more* now that bn is its own supervisor: an init system throttles a crash-looping
/// service and eventually drops it, whereas [`ensure`] would cheerfully respawn on every
/// invocation. This check and [`backed_off`] are what stand in for that throttle.
///
/// `queue --help` rather than a real pass: it proves the subcommand is *present* without
/// dispatching anything, so probing can never have side effects. clap exits non-zero on an unknown
/// subcommand, which is exactly the discriminator wanted.
pub(crate) fn can_dispatch(path: &str) -> bool {
    std::process::Command::new(path)
        .args(["queue", "--help"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Where the last failed start recorded why.
fn start_stamp() -> std::path::PathBuf {
    bn_core::resolver::state_dir().join("queue-dispatcher-failed")
}

/// Whether a recent start attempt failed, so this one should not retry yet.
///
/// A permission error, a read-only state dir, or — the case that prompted this — an installed
/// binary that cannot run the dispatcher are all conditions that persist. Retrying on every `bn`
/// invocation would spend a process spawn each time to reach the same conclusion, and would turn a
/// bad build into a fork bomb paced by the user's typing.
fn backed_off() -> bool {
    const RETRY_AFTER_SECS: u64 = 3600;
    std::fs::metadata(start_stamp())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < RETRY_AFTER_SECS)
}

/// Run one tick inline, if nothing has for a while.
///
/// The degraded mode: better than a queue that never drains, worse than a resident dispatcher, and
/// honest about which it is. Bounded by the same machine lock every other driver takes, so it can
/// never overlap a real pass.
fn fallback_tick(cfg: &Config) {
    let now = bn_core::util::now_epoch_secs();
    let recent = bn_core::queue::decision::QueueDecision::read()
        .is_some_and(|d| !d.dry_run && now.saturating_sub(d.at) < FALLBACK_TICK_AFTER_SECS);
    if recent {
        return;
    }
    crate::cmd::queue_tick::job_queue_tick(cfg, None);
}
