//! Reclaiming a worktree — **the** way it happens, and the one place that decides how.
//!
//! There used to be several. `bn wt cleanup` ran the gates and closed the note; `bn wt remove`
//! (`prefix D`) ran a bare `git worktree remove` past all of them, which on a `_pool/<n>` slot tore
//! the checkout out from under a live lease and left the pool permanently one slot short; and
//! `bn wt close` (`prefix x`) had its own copy of the pool-release branch. Three ways to give a
//! worktree back, agreeing on nothing, and only one of them checked whether it was safe.
//!
//! So: one implementation, reached two ways, and nothing else.
//!
//! * [`super::cleanup_one`] is the implementation — the gates, the note close, the removal, the
//!   pool-lease hand-back. Every caller that reclaims a worktree goes through it.
//! * [`run_now`] is that same work, re-entered as `bn wt close --reclaim` from the keystroke's own
//!   backgrounded process. It used to be filed on the machine queue instead, and that is the
//!   change: a queue under load holds everything, so a slot closed by hand stayed leased for as
//!   long as the dispatcher stayed busy, with the status line saying "reclaim queued". Nothing a
//!   person does by hand in tmux should wait on the queue — `run-shell -b` already keeps the UI
//!   free, so the keystroke can afford to do the work itself.

use super::*;

/// The worktree root a reclaim acts on: a pool slot's root, or the path itself.
///
/// A pane can sit anywhere inside a checkout, and for a slot it is the *slot* that gets handed
/// back, not the subdirectory you happened to be in.
pub(crate) fn target_root(wt_path: &Path) -> PathBuf {
    crate::wt_pool::pool_slot_root(wt_path).unwrap_or_else(|| wt_path.to_path_buf())
}

/// The label a reclaim is filed under: the branch, or — for a detached checkout, which has no
/// branch name — its directory. The same fallback [`super::cleanup_one`] makes.
pub(crate) fn label_for(wt_path: &Path, branch: &str) -> String {
    if !branch.is_empty() {
        return branch.to_string();
    }
    wt_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "worktree".to_string())
}

/// The shell command a reclaim runs — this same binary, re-entered on its `--reclaim` half.
/// Spelled as a CLI invocation rather than an in-process call so it is the exact string a person
/// can run by hand when the automatic one fails, and so the reclaim re-derives its facts (dirty
/// files, a live agent) from a tree whose windows are already gone.
///
/// `--confirmed` is carried across verbatim: the reclaim re-derives its plan from
/// [`super::close::close_plan`] when it runs, and that answer must reflect what the human actually
/// authorised, not what the tree looks like by then.
fn reclaim_cmd(path: &Path, confirmed: bool) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "bn".to_string());
    let mut cmd = format!(
        "{} wt close --reclaim --path {}",
        shq(&exe),
        shq(&path.to_string_lossy())
    );
    if confirmed {
        cmd.push_str(" --confirmed");
    }
    cmd
}

fn run_reclaim(cmd: &str) -> bool {
    std::process::Command::new("/bin/sh")
        .arg("-lc")
        .arg(cmd)
        .status()
        .is_ok_and(|s| s.success())
}

/// Wait, briefly, for tmux to have reaped every pane rooted in `root`.
///
/// Slot occupancy is read from live panes, so a reclaim that runs before the killed window is gone
/// is refused as "slot in use". `kill-window` is synchronous, so this normally returns at once; the
/// bound is insurance, not an expected wait.
fn wait_for_windows_gone(root: &Path, branch: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !tmux_windows_for(root, branch).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Reclaim the worktree now, in this process. Returns the one-line outcome for the caller to flash.
pub(crate) fn run_now(repo: &str, label: &str, branch: &str, path: &Path, confirmed: bool) -> String {
    wait_for_windows_gone(path, branch);
    let cmd = reclaim_cmd(path, confirmed);
    match run_reclaim(&cmd) {
        true => format!("closed {repo}/{label} — reclaimed"),
        false => format!(
            "closed {repo}/{label} — the reclaim failed; run `bn wt close --reclaim --path {}`",
            path.display()
        ),
    }
}

#[cfg(test)]
#[path = "reclaim_tests.rs"]
mod reclaim_tests;
