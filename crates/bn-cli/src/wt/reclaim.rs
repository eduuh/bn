//! Reclaiming a worktree — **the** way it happens, and the one place that decides how.
//!
//! There used to be several. `bn wt cleanup` ran the gates and closed the note; `bn wt remove`
//! (`prefix D`) ran a bare `git worktree remove` past all of them, which on a `_pool/<n>` slot tore
//! the checkout out from under a live lease and left the pool permanently one slot short; and
//! `bn wt close` (`prefix x`) had its own copy of the pool-release branch. Three ways to give a
//! worktree back, agreeing on nothing, and only one of them checked whether it was safe.
//!
//! So: one synchronous implementation, one deferred form of it, and nothing else.
//!
//! * [`super::cleanup_one`] is the implementation — the gates, the note close, the removal, the
//!   pool-lease hand-back. Every caller that reclaims a worktree goes through it.
//! * [`defer`] is that same work, filed on the machine queue as a [`BacklogKind::Cleanup`] entry
//!   instead of run now. The entry's command re-enters this binary, so the deferred path is not a
//!   second implementation — it is the first one, later.
//!
//! Which of the two a caller picks is a question about *latency*, never about *policy*: a keystroke
//! defers because a human is waiting, a CLI invocation runs now because you typed it and want the
//! answer. Both reach identical code.

use super::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, ExecSpec};

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

/// The shell command a queued reclaim runs — this same binary, re-entered on its deferred half.
/// Spelled as a CLI invocation rather than a bespoke job so it is inspectable in `bn queue list` and
/// runnable by hand from the same string.
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

/// Run the reclaim in this process, for a machine whose queue will not.
///
/// The same command the queue entry would have carried, so the two paths cannot drift: one shell
/// string, executed here or executed by the tick.
fn run_reclaim_now(cmd: &str) -> bool {
    std::process::Command::new("/bin/sh")
        .arg("-lc")
        .arg(cmd)
        .status()
        .is_ok_and(|s| s.success())
}

/// The queue entry a deferred reclaim is. Pure over its two strings so the properties that make it
/// cheap — the kind, and a shell executor rather than an agent — are pinned by a test.
fn reclaim_spec(task: String, cmd: String) -> AddSpec {
    AddSpec {
        kind: BacklogKind::Cleanup,
        source: "wt-close".to_string(),
        task,
        exec: ExecSpec::Shell { cmd },
        ..Default::default()
    }
}

/// File a reclaim on the machine queue. Returns the one-line outcome for the caller to report.
///
/// Deduped on the derived id of the item, so leaning on the key — or closing a worktree that is
/// already waiting to be reclaimed — files one entry, not a pile. The executor is
/// [`ExecSpec::Shell`], which makes the entry *express*: it needs no worktree and no agent lease, so
/// the tick runs it inline rather than charging it against the coding or research lane. Reclaiming a
/// checkout must not queue behind a coding agent — that is the resource it is trying to give back.
pub(crate) fn defer(cfg: &Config, repo: &str, label: &str, path: &Path, confirmed: bool) -> String {
    let item = format!("reclaim worktree {repo}/{label}");
    // Reclaiming a worktree is housekeeping the machine owes itself, never something you sat down
    // to do — 15 of these were on the day's list before it could say so.
    let task = crate::cmd::task_mint::task_for_machine_enqueue(cfg, repo, &item, "wt-close");
    let cmd = reclaim_cmd(path, confirmed);
    // Only file it if something will pick it up. Every other producer asks this
    // (`queue_producers::active_with_fallback`); this one did not, so on a machine with no
    // dispatcher a closed worktree left a leased pool slot behind and an entry nobody would ever
    // run — the slot silently never came back. Running it here instead costs the close a few
    // seconds and actually reclaims the slot, which is the whole point of the call.
    if !crate::cmd::queue_producers::active_with_fallback(
        cfg,
        crate::cmd::queue_producers::CRON,
        "reclaiming inline",
    ) {
        return match run_reclaim_now(&cmd) {
            true => format!("closed {repo}/{label} — reclaimed"),
            false => format!(
                "closed {repo}/{label} — the reclaim failed; run `bn wt close --reclaim --path {}`",
                path.display()
            ),
        };
    }
    let spec = reclaim_spec(task, cmd);
    let now = bn_core::util::now_epoch_secs();
    match bn_core::queue::enqueue::enqueue(&backlog::backlog_path(), repo, &item, &spec, now) {
        Ok(q) if q.is_new() => format!("closed {repo}/{label} — reclaim queued"),
        Ok(_) => format!("closed {repo}/{label} — reclaim already queued"),
        // The window is already gone by the time we get here, so this cannot be silent: say the
        // reclaim was lost and name the command that does it, rather than leaving a leased slot
        // nothing will ever hand back.
        Err(e) => format!(
            "closed {repo}/{label} — could not queue the reclaim ({e}); run `bn wt close --reclaim --path {}`",
            path.display()
        ),
    }
}

#[cfg(test)]
#[path = "reclaim_tests.rs"]
mod reclaim_tests;
