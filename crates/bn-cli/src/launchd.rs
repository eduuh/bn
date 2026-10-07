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
            // Orchestrate daemons (#338/#395) and the
            // routine watcher daemon (#801 amendment) are long-running KeepAlive agents, NOT
            // cron jobs — keep `com.bn.orchestrate*` and `com.bn.routine` out of
            // the cron reconcile so `bn cron register` never treats one as an orphaned cron and
            // unloads/deletes it.
            if is_orchestrate_label(&label) || is_routine_label(&label) {
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

// --- `bn routine watch` managed daemon (#801 amendment) ----------------------
// The watcher was a long-running daemon (RunAtLoad + KeepAlive), same shape as the orchestrate
// daemons above. The routine layer itself went in #1297; the label is still excluded from the cron
// reconcile (see `registered`) so `bn cron register` leaves a machine's leftover plist alone.

/// The launchd label for the routine watcher daemon.
pub(crate) const ROUTINE_LABEL: &str = "com.bn.routine";

/// Whether a launchd label is the routine watcher daemon — the cron reconcile must skip it.
pub(crate) fn is_routine_label(label: &str) -> bool {
    label == ROUTINE_LABEL
}
