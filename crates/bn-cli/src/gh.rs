//! The single `gh` seam for bn-cli. Plain current-dir reads (`gh pr view`, `gh pr list`) funnel
//! through the shared [`bn_core::runner`] seam via [`gh_capture`] / [`gh_ok`] so `RealRunner`
//! reproduces the old `.output()` behaviour exactly. Spawns that need bespoke configuration —
//! running in a specific worktree (`current_dir`), streaming to the terminal, or inspecting the
//! raw [`std::process::Output`] — use the [`gh_command`] builder (and the [`gh_capture_in`] /
//! [`gh_run_in`] conveniences built on it). All keep the one and only `Command::new("gh")` in
//! bn-cli here, so the consolidation is enforceable.

use bn_core::runner::{CommandRunner, RealRunner};
use std::path::Path;
use std::process::Command;

/// Build `gh <args>` as a configurable [`Command`] — the single sanctioned `Command::new("gh")`
/// in bn-cli, for the gh spawns that need `current_dir`, custom stdio, or the raw [`Output`] the
/// runner seam doesn't expose. Plain cwd reads should prefer [`gh_capture`] / [`gh_ok`] so they
/// stay routed through the runner.
///
/// [`Output`]: std::process::Output
pub(crate) fn gh_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    cmd
}

/// Run `gh <args>` in the cwd, returning trimmed non-empty stdout on a zero exit, else `None` (gh
/// absent / not authed / no output). Routes through the shared [`bn_core::runner`] seam so it
/// matches the git capture helpers' testable path; `RealRunner` reproduces the old inline
/// `.output()` → `success` → `trim` → non-empty wrappers exactly.
pub(crate) fn gh_capture(args: &[&str]) -> Option<String> {
    RealRunner.gh(args).ok_stdout()
}

/// Like [`gh_capture`] but with a wall-clock deadline, for callers that run on a timer and must
/// never block on a hung, offline or rate-limited `gh` — the tmux status bar reruns its segments
/// every `status-interval` seconds, so an unbounded call there stalls the whole row. Routes
/// through the same bounded runner seam `bn_core::issues` uses for its own `gh` fetches
/// (process-group kill on timeout), so no new dependency and one consistent failure shape.
pub(crate) fn gh_capture_bounded(args: &[&str], timeout: std::time::Duration) -> Option<String> {
    RealRunner.run_with_timeout("gh", args, timeout).ok_stdout()
}

/// Like [`gh_capture`] but run in `dir` (gh has no `-C` flag, so this sets the child's
/// `current_dir` via the builder). Trimmed non-empty stdout on success, else `None`.
pub(crate) fn gh_capture_in(dir: &Path, args: &[&str]) -> Option<String> {
    let out = gh_command(args).current_dir(dir).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run `gh <args>` in the cwd, capturing (and discarding) output; returns whether it exited 0.
/// Capturing both streams and dropping them is observably identical to routing them to
/// `/dev/null`, so it replaces the `auth status` / `cs list` health checks that null both streams.
/// Routes through the runner seam.
pub(crate) fn gh_ok(args: &[&str]) -> bool {
    RealRunner.gh(args).success
}

/// Run `gh <args>` in `dir`, inheriting stdio, for exit status only; true if it exited 0. For the
/// side-effecting spawns that must run against a specific worktree (e.g. `pr merge` from the repo
/// root) and whose live output should reach the terminal.
pub(crate) fn gh_run_in(dir: &Path, args: &[&str]) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let ok = gh_command(args)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if let Some(timer) = timer {
        bn_core::log::record(
            bn_core::log::LogLevel::Debug,
            "gh",
            "run",
            serde_json::json!({ "argv": bn_core::log::redact_args(args), "dir": dir.display().to_string() }),
            serde_json::json!({ "success": ok }),
            Some(timer.elapsed_ms()),
        );
    }
    ok
}
