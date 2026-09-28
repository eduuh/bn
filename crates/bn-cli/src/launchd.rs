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
            if is_orchestrate_label(&label) || is_web_label(&label) || is_routine_label(&label) {
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

/// Whether a launchd label is an orchestrate daemon — the legacy singleton (`com.bn.orchestrate`)
/// or any per-repo daemon (`com.bn.orchestrate.<repo>`). The cron reconcile must skip ALL of them.
pub(crate) fn is_orchestrate_label(label: &str) -> bool {
    label == ORCHESTRATE_LABEL_PREFIX
        || label
            .strip_prefix(ORCHESTRATE_LABEL_PREFIX)
            .is_some_and(|rest| rest.starts_with('.'))
}

// #455: one cross-repo supervisor daemon that sweeps EVERY repo's session in a single loop, rather
// than a per-repo daemon each blind to the other sessions. It gets its own reserved label so it
// coexists with any per-repo daemons; `is_orchestrate_label` already covers it (cron never reaps it).

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
// The watcher was a long-running daemon (RunAtLoad + KeepAlive), same shape as the orchestrate/web
// daemons above. The routine layer itself went in #1297; the label is still excluded from the cron
// reconcile (see `registered`) so `bn cron register` leaves a machine's leftover plist alone.

/// The launchd label for the routine watcher daemon.
pub(crate) const ROUTINE_LABEL: &str = "com.bn.routine";

/// Whether a launchd label is the routine watcher daemon — the cron reconcile must skip it.
pub(crate) fn is_routine_label(label: &str) -> bool {
    label == ROUTINE_LABEL
}
