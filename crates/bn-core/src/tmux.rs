//! Reusable tmux / process-integration wrappers (issue #428).
//!
//! tmux integration used to be walled off in bn-cli under the "interactive / side-effecting
//! spawns stay in the CLI" doctrine. That doctrine is dropped: the shared wrappers live here in
//! `bn-core` so every crate (bn-cli, bn-web, bn-mcp, …) can reuse them in-process rather than
//! re-implementing the bridge or shelling out to a `bn` subprocess.
//!
//! Every wrapper spawns `tmux` with discrete argv (never a shell), so caller-supplied names are
//! not injection vectors.

use std::ffi::OsStr;
use std::process::Command;

/// Build the `tmux` command with `args` forwarded verbatim — the single sanctioned
/// `Command::new("tmux")` in the workspace. Callers that need bespoke configuration
/// (`current_dir`, environment, conditional args, `.spawn()`, or inspecting the raw
/// [`std::process::Output`] for stderr) build on this; the common cases go through [`tmux`] /
/// [`tmux_raw`] / [`tmux_run`]. Generic over `AsRef<OsStr>` so both `&[&str]` and `&[String]` call
/// sites route through it unchanged.
pub fn tmux_command<S: AsRef<OsStr>>(args: &[S]) -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(args);
    cmd
}

/// Run tmux, returning stdout verbatim on success (no trim) — for multi-line `-F` enumerations
/// whose rows carry trailing empty `@user-option` columns a trim would eat.
pub fn tmux_raw<S: AsRef<OsStr>>(args: &[S]) -> Option<String> {
    let out = tmux_command(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run tmux, returning trimmed non-empty stdout — for single-value captures (a window/session/pane
/// id).
pub fn tmux<S: AsRef<OsStr>>(args: &[S]) -> Option<String> {
    tmux_raw(args)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run tmux for exit status only, returning whether it succeeded (false if tmux can't be spawned).
pub fn tmux_run<S: AsRef<OsStr>>(args: &[S]) -> bool {
    tmux_command(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run tmux for exit status only, capturing (and discarding) output; true if it exited 0 (false if
/// tmux can't be spawned). The captured sibling of [`tmux_run`], for control commands
/// (`send-keys`, `paste-buffer`) whose output must not leak to the terminal — the faithful
/// replacement for the old `.output().map(|o| o.status.success())` call sites.
pub fn tmux_ok<S: AsRef<OsStr>>(args: &[S]) -> bool {
    tmux_command(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}


