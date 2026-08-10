//! Closing the loop when work lands — the far end of the Today↔queue tie.
//!
//! `bn agent done` used to raise a review item and stop there, so the task that governed the work,
//! and the Today suggestion that proposed it, stayed open forever. The next reconcile sweep would
//! then find a proposal it believed still needed queueing, and re-queue work that had already
//! shipped. This is the write that prevents that.
//!
//! Its own module because it is a distinct concern from reporting a completion, and because
//! `cmd/agent.rs` is over the size ratchet and may not grow.
use crate::*;

/// What closing out a completion did, for the `done` surfaces to report.
#[derive(Debug, Default)]
pub(crate) struct ClosedOut {
    /// The governing task that was marked done.
    pub(crate) task: Option<String>,
    /// The Today suggestion that proposed it, retired with the task.
    pub(crate) suggestion: Option<String>,
}

/// Mark the task governing `(repo, branch)` done — and, if it came from Today, retire its suggestion.
///
/// This is the far end of the tie: Today accepting a suggestion is what queued the work, so the work
/// landing is what closes both. Without it an `accepted` suggestion stays accepted forever, and the
/// reconcile sweep keeps finding a proposal it believes still needs queueing.
///
/// Two completions deliberately close **nothing**:
///
/// * `--failed` — the work stopped, so the task is still open. Closing it would erase the only
///   record that something remains to be done.
/// * `--report-only` — the agent produced a recommendation rather than the change the task asked
///   for. That is a real outcome and it surfaces as a review item, but it is not the task's
///   done-when, so a person decides whether it counts. Conservative on purpose: a task wrongly left
///   open costs a glance, a task wrongly closed costs the work.
pub(crate) fn run(
    cfg: &Config,
    ctx: &Resolved,
    failed: bool,
    report_only: bool,
    as_json: bool,
) -> ClosedOut {
    if failed || report_only {
        return ClosedOut::default();
    }
    let Some(task) = bn_core::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch) else {
        return ClosedOut::default();
    };
    let Some(id) = task.id().map(str::to_string) else {
        return ClosedOut::default();
    };

    let path = crate::cmd::task::task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    let Ok(mut file) = bn_core::todo::load(&path) else {
        return ClosedOut::default();
    };
    if !file.mark_done(&id, &today()) {
        return ClosedOut::default();
    }
    if bn_core::todo::save(&path, &file).is_err() {
        return ClosedOut::default();
    }

    // The reverse link the task carries, stamped when Today accepted the suggestion.
    let suggestion = task
        .meta(crate::cmd::task_mint::SUGGESTION_KEY)
        .map(str::to_string)
        .filter(|s| {
            bn_core::suggestions::set_status(s, bn_core::suggestions::SuggestionStatus::Done)
                .unwrap_or(false)
        });
    let closed = ClosedOut {
        task: Some(id),
        suggestion,
    };
    if !as_json {
        closed.report();
    }
    closed
}

impl ClosedOut {
    /// Print what was closed, for the human `bn agent done` surface. Silent when nothing was —
    /// a completion with no governing task is the ordinary pre-task-store case, not an anomaly.
    pub(crate) fn report(&self) {
        let Some(task) = &self.task else { return };
        match &self.suggestion {
            Some(s) => println!("  closed task {task} and suggestion {s}"),
            None => println!("  closed task {task}"),
        }
    }
}
