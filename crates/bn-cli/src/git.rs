//! The single `git` seam for bn-cli. Capture reads (`git_out`, `git_capture_in`, `git_capture`,
//! `git_silent`) funnel through the shared [`bn_core::runner`] seam so they stay testable and
//! `RealRunner` reproduces the old behaviour exactly. Spawns that must show *live* terminal
//! progress (clone/fetch/push/commit/`worktree add`) can't be captured, so they get the sibling
//! [`git_run`] / [`git_run_quiet`] wrappers, and anything needing bespoke configuration
//! (`current_dir`, custom stdio, inspecting stderr) uses the [`git_command`] builder. All three
//! keep the one and only `Command::new("git")` in bn-cli here, so the consolidation is enforceable.

use bn_core::runner::{CommandRunner, RealRunner};
use std::path::Path;
use std::process::{Command, Stdio};

/// Build `git <args>` as a configurable [`Command`] — the single sanctioned `Command::new("git")`
/// in bn-cli, for the streaming / status-only / `current_dir` spawns the capturing runner seam
/// can't serve (they must show git's live progress on the terminal, which capture would swallow).
/// Capture reads should use [`git_capture`] / [`git_ok`] instead so they stay testable through the
/// runner.
pub(crate) fn git_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args);
    // Bound the network here, at the seam, rather than at each fetch/clone/push call site — the
    // one that was missed took the dispatcher offline for twenty-five minutes. See
    // [`bn_core::gitnet`].
    bn_core::gitnet::bound(&mut cmd);
    cmd
}

/// Run `git <args>` inheriting stdio; returns whether it exited 0. For clone/fetch/push/commit/
/// `worktree add` whose live progress must reach the terminal — routing these through the
/// capture-only runner would swallow the output the user relies on.
pub(crate) fn git_run(args: &[&str]) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let ok = git_command(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    log_git_spawn("run", args, ok, timer);
    ok
}

/// Like [`git_run`] but with stdout suppressed (stderr still inherited for progress) — for spawns
/// whose stdout chatter ("HEAD is now at …") must not leak into a captured value, yet whose
/// stderr progress should still reach the terminal (e.g. the worktree pool's `checkout`/`reset`).
pub(crate) fn git_run_quiet(args: &[&str]) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let ok = git_command(args)
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    log_git_spawn("run", args, ok, timer);
    ok
}

/// Emit a redacted action-log record for a streaming `git` spawn (clone/fetch/push/commit) that
/// bypasses the capturing runner seam. No-op when the action log is off (`timer` is `None`).
fn log_git_spawn(action: &str, args: &[&str], ok: bool, timer: Option<bn_core::log::Timer>) {
    let Some(timer) = timer else {
        return;
    };
    bn_core::log::record(
        bn_core::log::LogLevel::Debug,
        "git",
        action,
        serde_json::json!({ "argv": bn_core::log::redact_args(args) }),
        serde_json::json!({ "success": ok }),
        Some(timer.elapsed_ms()),
    );
}

/// Run `git <args>` (optionally in `dir` via `-C <dir>`), capturing stdout; `Some(stdout)` on a
/// zero exit, else `None`. `trim` reconciles the historical split the audit flagged — the old
/// `git_out` trimmed stdout while `git_capture_in` / `git_capture` returned it raw — so each caller
/// opts into the behaviour it always had rather than any site silently changing.
///
/// Uses `success.then(…)` (not [`bn_core::runner::CommandOutput::ok_stdout`]) so a *successful but
/// empty* command still yields `Some("")`, matching the wrappers this replaces (`ok_stdout` maps
/// empty output to `None`).
pub(crate) fn git_capture(args: &[&str], dir: Option<&Path>, trim: bool) -> Option<String> {
    let dir_cow = dir.map(|d| d.to_string_lossy());
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
    if let Some(d) = &dir_cow {
        full.push("-C");
        full.push(d.as_ref());
    }
    full.extend_from_slice(args);
    let out = RealRunner.git(&full);
    out.success.then(|| {
        if trim {
            out.stdout.trim().to_string()
        } else {
            out.stdout
        }
    })
}

/// Run `git -C <dir> <args>` capturing stdout, giving up after `budget`. `None` when git fails,
/// when it is still running at the deadline (the child is killed), or when the budget is already
/// spent before the spawn.
///
/// For the read-only display surfaces that rerun on a keystroke — the `bn tat` sub-picker's side
/// pane — where the repositories that make a call slow are exactly the ones where blocking is
/// unacceptable: on a ~100k-file monorepo a cold `ls-files --others` walk of the untracked tree
/// takes over two minutes, and the pane fires one per cursor move. A panel that renders `…` for a
/// count it could not afford is worth more than one that stalls the picker to be exact.
///
/// Deliberately outside the [`bn_core::runner`] seam, which captures synchronously and so has
/// nowhere to put a deadline; this is the "bespoke configuration" case [`git_command`] exists for.
/// Callers that must have the answer should use [`git_capture`].
pub(crate) fn git_capture_deadline(
    dir: &Path,
    args: &[&str],
    budget: std::time::Duration,
) -> Option<String> {
    if budget.is_zero() {
        return None;
    }
    let mut full: Vec<&str> = vec!["-C"];
    let dir_cow = dir.to_string_lossy();
    full.push(dir_cow.as_ref());
    full.extend_from_slice(args);
    let mut child = git_command(&full)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // stdout is drained on a thread rather than after the wait, so a command that outruns the pipe
    // buffer cannot deadlock against the deadline it is being held to.
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stdout).read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    // The send lands when git closes stdout, which is either its exit or the kill below — so this
    // recv is the deadline for the whole call, not merely for the first byte.
    let Ok(text) = rx.recv_timeout(budget) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    ok.then(|| text.trim().to_string())
}

/// Run `git <args>` (optionally in `dir` via `-C <dir>`), returning `(success, stdout, stderr)`,
/// both streams trimmed.
///
/// [`git_capture`] collapses every failure to `None`, which leaves a failing maintenance job able
/// to report only *that* git failed and never *why* — the trap that made chronic `commit-notes`
/// failures undiagnosable. Prefer this wherever the failure is surfaced to a human.
pub(crate) fn git_result(args: &[&str], dir: Option<&Path>) -> (bool, String, String) {
    let dir_cow = dir.map(|d| d.to_string_lossy());
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
    if let Some(d) = &dir_cow {
        full.push("-C");
        full.push(d.as_ref());
    }
    full.extend_from_slice(args);
    let out = RealRunner.git(&full);
    (
        out.success,
        out.stdout.trim().to_string(),
        out.stderr.trim().to_string(),
    )
}

/// Run `git <args>`, capturing (and discarding) output; returns whether it exited 0. Replaces the
/// null-stdout+null-stderr `git_silent`: capturing both streams and dropping them is observably
/// identical to routing them to `/dev/null`. Not for spawns that must show live progress.
pub(crate) fn git_ok(args: &[&str]) -> bool {
    RealRunner.git(args).success
}
