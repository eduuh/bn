//! launchd user-agent management — the macOS twin of [`crate::timers`]. Reads the `com.bn.*`
//! agents `bn cron register` wrote to `~/Library/LaunchAgents` and (un)loads them via
//! `launchctl bootstrap`/`bootout` in the user's GUI domain. Every entry point degrades to an
//! empty list / no-op when `launchctl` is absent, so callers never special-case the platform.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A bn-generated launchd agent on disk (`~/Library/LaunchAgents/com.bn.*.plist`).
#[derive(Debug, Clone)]
pub struct RegisteredAgent {
    /// `com.bn.<job>` / `com.bn.<repo>.<job>`.
    pub label: String,
    /// The bn job the agent runs (the label's last segment).
    pub job: String,
    pub path: PathBuf,
}

/// The per-user LaunchAgents dir.
pub fn agents_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library")
        .join("LaunchAgents")
}

/// Whether `launchctl` is available (macOS).
pub fn have_launchctl() -> bool {
    Command::new("launchctl")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Every bn-generated agent on disk, by scanning `com.bn.*.plist` files. File presence is the
/// registration source of truth (mirroring the systemd unit-file diff); liveness is re-asserted by
/// `bootstrap` on every register run.
pub fn registered() -> Vec<RegisteredAgent> {
    let Ok(rd) = std::fs::read_dir(agents_dir()) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let path = e.path();
            let label = path.file_stem()?.to_string_lossy().into_owned();
            // Orchestrate daemons (#338/#395), the bn-web dashboard daemon (#388), and the
            // routine watcher daemon (#801 amendment) are long-running KeepAlive agents, NOT
            // cron jobs — keep `com.bn.orchestrate*`, `com.bn.web`, and `com.bn.routine` out of
            // the cron reconcile so `bn cron register` never treats one as an orphaned cron and
            // unloads/deletes it.
            if is_orchestrate_label(&label)
                || is_web_label(&label)
                || is_routine_label(&label)
                || is_queue_label(&label)
            {
                return None;
            }
            let job = crate::cron_gen::label_job(&label)?.to_string();
            (path.extension().and_then(|x| x.to_str()) == Some("plist"))
                .then_some(RegisteredAgent { label, job, path })
        })
        .collect()
}

/// Whether launchd currently has `label` loaded in the user's GUI domain.
///
/// Asks the supervisor, not the filesystem: a plist on disk proves an install once happened, not
/// that the service survived. A job that crash-looped until launchd dropped it leaves its plist
/// exactly where it was.
pub fn is_loaded(label: &str) -> bool {
    Command::new("launchctl")
        .args(["print", &format!("{}/{label}", gui_domain())])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The user's GUI launchd domain (`gui/<uid>`).
fn gui_domain() -> String {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string());
    format!("gui/{uid}")
}

/// Load (or reload) the agent at `path`: bootout any existing copy of `label` (ignoring
/// not-loaded), then bootstrap fresh — the same reload dance as `jobs/launchd/install.sh`.
pub fn bootstrap(path: &Path, label: &str) -> Result<(), String> {
    let domain = gui_domain();
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .output(); // best-effort: fails when not loaded, which is fine
    let out = Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Unload `label` from the GUI domain, best-effort (a not-loaded agent is already the goal
/// state). Only a failure to *spawn* launchctl is an error.
pub fn bootout(label: &str) -> Result<(), String> {
    Command::new("launchctl")
        .args(["bootout", &format!("{}/{label}", gui_domain())])
        .output()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// --- `bn orchestrate` daemons (#338 / #395) ----------------------------------
// Each orchestrate loop is a long-running daemon (RunAtLoad + KeepAlive), not a periodic cron job,
// so it gets its own launchd plumbing distinct from the `StartInterval` cron agents above — and is
// excluded from the cron reconcile (see `registered`) so `bn cron register` never reaps it.
// #395: one daemon **per repo** (`com.bn.orchestrate.<repo>`), so two concurrent runs never share
// (or hijack) a single machine-wide agent. The bare legacy label lingers only until migration.

/// The label prefix every orchestrate daemon shares — also the pre-#395 singleton's exact label.
pub(crate) const ORCHESTRATE_LABEL_PREFIX: &str = "com.bn.orchestrate";

/// The per-repo launchd label for a run's daemon: `com.bn.orchestrate.<repo>`.
pub(crate) fn orchestrate_label(repo: &str) -> String {
    format!(
        "{ORCHESTRATE_LABEL_PREFIX}.{}",
        bn_core::orchestrate::sanitize_run_key(repo)
    )
}

/// Whether a launchd label is an orchestrate daemon — the legacy singleton (`com.bn.orchestrate`)
/// or any per-repo daemon (`com.bn.orchestrate.<repo>`). The cron reconcile must skip ALL of them.
pub(crate) fn is_orchestrate_label(label: &str) -> bool {
    label == ORCHESTRATE_LABEL_PREFIX
        || label
            .strip_prefix(ORCHESTRATE_LABEL_PREFIX)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn orchestrate_plist_path(label: &str) -> PathBuf {
    agents_dir().join(format!("{label}.plist"))
}

/// The per-repo daemon plist: `RunAtLoad` starts the loop immediately; `KeepAlive
/// {SuccessfulExit=false}` restarts it only on a *crash* (non-zero exit) — a clean drain / `bn
/// orchestrate stop` (exit 0) stays down. A crashed daemon is relaunched by launchd, reloads the
/// persisted queue, and resumes — the resilience a scheduled prompt can't offer (it survives the
/// launching session AND a crash). `ThrottleInterval` guards against a hot restart loop. Login
/// shell so `$PATH` finds gh/copilot. `--repo` pins the daemon to its own run's state file (#395).
pub(crate) fn orchestrate_daemon_plist(bn_path: &str, repo: &str) -> String {
    let label = orchestrate_label(repo);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <!-- bn orchestrate daemon (#338/#395) — event-driven fleet loop for one repo's run. Managed by `bn orchestrate`. -->\n\
         \x20 <key>Label</key>\n  <string>{label}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-lc</string>\n\
         \x20   <string>exec \"{bn_path}\" orchestrate run --repo \"{repo}\"</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n\
         \x20 <key>StandardOutPath</key>\n  <string>/tmp/{label}.log</string>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>/tmp/{label}.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// The label for the queue dispatcher daemon.
pub(crate) const QUEUE_LABEL: &str = "com.bn.queue";

/// The `bn queue run` daemon plist — a **resident loop**, not a timer.
///
/// A per-minute timer spawns a fresh process, does one pass, and exits: work that arrives at
/// 12:00:05 waits until 12:01. A resident loop drains continuously and backs off when idle, which
/// is what "keep the tick active" actually means. It is also cheaper — no process spawn per minute.
///
/// `KeepAlive` restarts it if it dies; `RunAtLoad` starts it at login; `ThrottleInterval` stops a
/// crash-loop from becoming a spin.
pub(crate) fn queue_daemon_plist(bn_path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <!-- bn queue dispatcher — a resident loop draining the machine-wide work queue. -->\n\
         \x20 <key>Label</key>\n  <string>{QUEUE_LABEL}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-lc</string>\n\
         \x20   <string>exec \"{bn_path}\" queue run</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <true/>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n\
         \x20 <key>StandardOutPath</key>\n  <string>/tmp/{QUEUE_LABEL}.log</string>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>/tmp/{QUEUE_LABEL}.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Install (register + load) the per-repo `bn orchestrate run --repo <repo>` launchd daemon.
/// Writes the plist to `~/Library/LaunchAgents` and bootstraps it (reloading any existing copy).
/// Err when launchctl is unavailable (non-macOS / no GUI session) — the caller then falls back to
/// a detached child.
pub(crate) fn orchestrate_install(repo: &str) -> Result<(), String> {
    if !have_launchctl() {
        return Err("launchctl unavailable".into());
    }
    let bn = std::env::current_exe().map_err(|e| e.to_string())?;
    let label = orchestrate_label(repo);
    let path = orchestrate_plist_path(&label);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, orchestrate_daemon_plist(&bn.to_string_lossy(), repo))
        .map_err(|e| e.to_string())?;
    bootstrap(&path, &label)
}

/// Unload a repo's orchestrate daemon and delete its plist (best-effort — a not-loaded agent is
/// already the goal state).
pub(crate) fn orchestrate_uninstall(repo: &str) {
    let label = orchestrate_label(repo);
    let _ = bootout(&label);
    let _ = std::fs::remove_file(orchestrate_plist_path(&label));
}

/// Delete a repo's orchestrate daemon plist WITHOUT booting the service out — used by the daemon
/// itself when its run drains: the loop exits cleanly (KeepAlive leaves it down) and removing the
/// plist stops the next login's RunAtLoad from resurrecting a loop for an already-finished run.
/// A self-bootout here would flip the exit non-zero and make KeepAlive restart the daemon.
pub(crate) fn orchestrate_forget(repo: &str) {
    let _ = std::fs::remove_file(orchestrate_plist_path(&orchestrate_label(repo)));
}

/// Unload the pre-#395 machine-wide singleton daemon and delete its plist. Called once by the
/// legacy-state migration so the old daemon can't keep driving (or resurrecting) the moved file.
pub(crate) fn orchestrate_uninstall_legacy() {
    let _ = bootout(ORCHESTRATE_LABEL_PREFIX);
    let _ = std::fs::remove_file(orchestrate_plist_path(ORCHESTRATE_LABEL_PREFIX));
}

// #455: one cross-repo supervisor daemon that sweeps EVERY repo's session in a single loop, rather
// than a per-repo daemon each blind to the other sessions. It gets its own reserved label so it
// coexists with any per-repo daemons; `is_orchestrate_label` already covers it (cron never reaps it).

/// The reserved "repo" token for the #455 cross-repo supervisor daemon — it isn't tied to any one
/// repo, so it takes a token no real bare-repo stem produces.
pub(crate) const ORCHESTRATE_ALL_TOKEN: &str = "__all__";

/// The launchd label for the cross-repo supervisor daemon: `com.bn.orchestrate.__all__`.
pub(crate) fn orchestrate_all_label() -> String {
    format!("{ORCHESTRATE_LABEL_PREFIX}.{ORCHESTRATE_ALL_TOKEN}")
}

/// The cross-repo supervisor daemon plist (#455): same `RunAtLoad` + crash-only `KeepAlive` shape
/// as the per-repo daemon, but its program is `bn orchestrate run --all-repos` — the single loop
/// that sweeps every configured repo's session each tick.
pub(crate) fn orchestrate_all_daemon_plist(bn_path: &str) -> String {
    let label = orchestrate_all_label();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <!-- bn cross-repo orchestrate supervisor (#455) — one loop sweeping every repo's session. Managed by `bn orchestrate`. -->\n\
         \x20 <key>Label</key>\n  <string>{label}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-lc</string>\n\
         \x20   <string>exec \"{bn_path}\" orchestrate run --all-repos</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n\
         \x20 <key>StandardOutPath</key>\n  <string>/tmp/{label}.log</string>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>/tmp/{label}.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Install (register + load) the `bn orchestrate run --all-repos` cross-repo supervisor daemon.
/// Mirrors [`orchestrate_install`]; `Err` when launchctl is unavailable so the caller falls back to
/// a detached child.
pub(crate) fn orchestrate_install_all() -> Result<(), String> {
    if !have_launchctl() {
        return Err("launchctl unavailable".into());
    }
    let bn = std::env::current_exe().map_err(|e| e.to_string())?;
    let label = orchestrate_all_label();
    let path = orchestrate_plist_path(&label);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, orchestrate_all_daemon_plist(&bn.to_string_lossy()))
        .map_err(|e| e.to_string())?;
    bootstrap(&path, &label)
}

/// Unload the cross-repo supervisor daemon and delete its plist (best-effort — a not-loaded agent
/// is already the goal state).
pub(crate) fn orchestrate_uninstall_all() {
    let label = orchestrate_all_label();
    let _ = bootout(&label);
    let _ = std::fs::remove_file(orchestrate_plist_path(&label));
}

// --- `bn serve --daemon` bn-web dashboard (#388) -----------------------------
// The web server is a long-running daemon (RunAtLoad + KeepAlive), same shape as the orchestrate
// daemons above — and excluded from the cron reconcile (see `registered`) so `bn cron register`
// never reaps it.

/// The launchd label for the bn-web dashboard daemon.
pub(crate) const WEB_LABEL: &str = "com.bn.web";

/// Whether a launchd label is the bn-web daemon — the cron reconcile must skip it.
pub(crate) fn is_web_label(label: &str) -> bool {
    label == WEB_LABEL
}

fn web_plist_path() -> PathBuf {
    agents_dir().join(format!("{WEB_LABEL}.plist"))
}

/// The bn-web daemon plist: `RunAtLoad` starts it immediately; `KeepAlive {SuccessfulExit=false}`
/// restarts it only on a *crash* (a clean `bn serve`/Ctrl-C exit stays down). `ThrottleInterval`
/// guards a hot restart loop; a login shell so `$PATH` is populated. `extra_args` are appended to
/// the `bn-web` invocation (e.g. `--port 7770`, `--lan`).
pub(crate) fn web_daemon_plist(bn_web_path: &str, extra_args: &[String]) -> String {
    let joined = extra_args.join(" ");
    let invocation = if joined.is_empty() {
        format!("exec \"{bn_web_path}\"")
    } else {
        format!("exec \"{bn_web_path}\" {joined}")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <!-- bn-web dashboard daemon (#388) — the web server. Managed by `bn serve --daemon`. -->\n\
         \x20 <key>Label</key>\n  <string>{WEB_LABEL}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-lc</string>\n\
         \x20   <string>{invocation}</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n\
         \x20 <key>StandardOutPath</key>\n  <string>/tmp/{WEB_LABEL}.log</string>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>/tmp/{WEB_LABEL}.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Install (register + load) the bn-web daemon: write the plist and bootstrap it (reloading any
/// existing copy). Err when launchctl is unavailable (non-macOS / no GUI session) — the caller then
/// falls back to a detached child.
pub(crate) fn web_install(bn_web_path: &str, extra_args: &[String]) -> Result<(), String> {
    if !have_launchctl() {
        return Err("launchctl unavailable".into());
    }
    let path = web_plist_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, web_daemon_plist(bn_web_path, extra_args)).map_err(|e| e.to_string())?;
    bootstrap(&path, WEB_LABEL)
}

/// Whether the bn-web daemon is *installed* (its plist is on disk). File presence is the same
/// registration source-of-truth `registered()` uses, so `bn serve status` reports "managed" the
/// moment `bn serve --daemon` wrote the plist — independent of whether it's currently listening.
pub(crate) fn web_installed() -> bool {
    web_plist_path().exists()
}

/// Unload the bn-web daemon and delete its plist (best-effort — a not-loaded agent is already the
/// goal state). The inverse of [`web_install`], called by `bn serve stop`. `bootout` before the
/// `remove_file` so launchd's KeepAlive can't relaunch it after the plist is gone.
pub(crate) fn web_uninstall() {
    // The launchd twin of the guard in [`crate::systemd::web_uninstall`]: `bootout` costs a
    // `gui_domain()` (`id -u`) spawn plus a `launchctl` round-trip, on every `bn` invocation, to
    // unload something that was never loaded. The plist is [`web_installed`]'s source of truth.
    if !web_plist_path().exists() {
        return;
    }
    let _ = bootout(WEB_LABEL);
    let _ = std::fs::remove_file(web_plist_path());
}

// --- `bn routine watch` managed daemon (#801 amendment) ----------------------
// The watcher is a long-running daemon (RunAtLoad + KeepAlive), same shape as the orchestrate/web
// daemons above — and excluded from the cron reconcile (see `registered`) so `bn cron register`
// never reaps it. It coexists with the `routine-tick` timer fallback: both share the same
// transaction lock and at-least-once dedupe state, so running both is safe, just redundant.

/// Whether a launchd label is the queue dispatcher — the cron reconcile must skip it.
///
/// It is a KeepAlive daemon, not a cron, but [`crate::cron_gen::label_job`] maps its label to the
/// job name "queue", which appears in no cron manifest. Without this the reconcile classifies it as
/// an orphaned cron and does to it what it does to orphans: boots it out and deletes its plist.
///
/// `install.sh` registers crons and `self-update` runs `install.sh`, so the nightly update was
/// deleting the dispatcher outright — the same omission the orchestrate, web and routine daemons
/// above each had to be spelled out to avoid.
pub(crate) fn is_queue_label(label: &str) -> bool {
    label == QUEUE_LABEL
}

/// The launchd label for the routine watcher daemon.
pub(crate) const ROUTINE_LABEL: &str = "com.bn.routine";

/// Whether a launchd label is the routine watcher daemon — the cron reconcile must skip it.
pub(crate) fn is_routine_label(label: &str) -> bool {
    label == ROUTINE_LABEL
}

fn routine_plist_path() -> PathBuf {
    agents_dir().join(format!("{ROUTINE_LABEL}.plist"))
}

/// The routine watcher daemon plist: `RunAtLoad` starts `bn routine watch` immediately (and on
/// every login/reboot); `KeepAlive {SuccessfulExit=false}` restarts it only on a *crash* — a
/// clean SIGTERM/Ctrl-C exit (0) stays down. `ThrottleInterval` guards a hot restart loop.
pub(crate) fn routine_daemon_plist(bn_path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <!-- bn routine watcher daemon (#801 amendment) — deadline-driven reconciliation loop. Managed by `bn routine service`. -->\n\
         \x20 <key>Label</key>\n  <string>{ROUTINE_LABEL}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-lc</string>\n\
         \x20   <string>exec \"{bn_path}\" routine watch</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n\
         \x20 <key>StandardOutPath</key>\n  <string>/tmp/{ROUTINE_LABEL}.log</string>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>/tmp/{ROUTINE_LABEL}.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Install (register + load) the routine watcher daemon. `Err` when launchctl is unavailable
/// (non-macOS / no GUI session) — the caller then reports no resident coverage rather than a
/// detached fallback (#801 amendment invariant 6: no fake coverage claims).
pub(crate) fn routine_install(bn_path: &str) -> Result<(), String> {
    if !have_launchctl() {
        return Err("launchctl unavailable".into());
    }
    let path = routine_plist_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, routine_daemon_plist(bn_path)).map_err(|e| e.to_string())?;
    bootstrap(&path, ROUTINE_LABEL)
}

/// Whether the routine watcher daemon is *installed* (its plist is on disk) — the same
/// file-presence source of truth `registered()`/`web_installed` use.
pub(crate) fn routine_installed() -> bool {
    routine_plist_path().exists()
}

/// Unload the routine watcher daemon and delete its plist (best-effort — a not-loaded agent is
/// already the goal state).
pub(crate) fn routine_uninstall() {
    let _ = bootout(ROUTINE_LABEL);
    let _ = std::fs::remove_file(routine_plist_path());
}
