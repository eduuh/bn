//! Closing the loop when work lands — the far end of the Today suggestion tie.
//!
//! `bn agent done` used to raise a review item and stop there, so the task that governed the work,
//! and the Today suggestion that proposed it, stayed open forever. The next reconcile sweep would
//! then find a proposal it believed still needed starting, and start work that had already
//! shipped. This is the write that prevents that.
//!
//! Its own module because it is a distinct concern from reporting a completion, and because
//! `cmd/agent.rs` is over the size ratchet and may not grow.
//!
//! # With the Forge provider
//!
//! The completion is also the report back to whoever assigned the task: the summary and PR link go
//! on the Forge activity as a comment, and the PR is stamped as `pr:`. A task with a PR is not
//! done when the agent is: it stays open (wip) until the PR merges, which `bn forge sweep` notices
//! ([`crate::cmd::forge_app::sweep::poll_forge_merges`]) and completes. The file provider keeps the
//! old behaviour exactly: close on done, no stamp, no note.
use crate::*;
use bn_core::task_store::TaskProviderKind;

/// What closing out a completion did, for the `done` surfaces to report.
#[derive(Debug, Default)]
pub(crate) struct ClosedOut {
    /// The governing task that was marked done.
    pub(crate) task: Option<String>,
    /// The Today suggestion that proposed it, retired with the task.
    pub(crate) suggestion: Option<String>,
    /// The governing task left open until its PR merges (Forge provider only).
    pub(crate) waiting_on_pr: Option<String>,
}

/// Mark the task governing `(repo, branch)` done — and, if it came from Today, retire its suggestion.
///
/// This is the far end of the tie: Today accepting a suggestion is what started the work, so the
/// work landing is what closes both. Without it an `accepted` suggestion stays accepted forever, and
/// the reconcile sweep keeps finding a proposal it believes still needs starting.
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
    summary: &str,
    pr: &Option<String>,
    failed: bool,
    report_only: bool,
    as_json: bool,
) -> ClosedOut {
    if cfg.tasks_provider == TaskProviderKind::ForgeApp {
        let outcome = Outcome {
            summary,
            pr: pr.as_deref().map(str::trim).filter(|p| !p.is_empty()),
            failed,
            report_only,
        };
        let closed = run_forge(cfg, ctx, &outcome);
        if !as_json {
            closed.report();
        }
        return closed;
    }
    if failed || report_only {
        return ClosedOut::default();
    }
    let Some(task) = bn_core::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch) else {
        return ClosedOut::default();
    };
    let Some(id) = task.id().map(str::to_string) else {
        return ClosedOut::default();
    };

    let store = crate::cmd::task::task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
        return ClosedOut::default();
    };
    if !file.mark_done(&id, &today()) {
        return ClosedOut::default();
    }
    if store.save(&file).is_err() {
        return ClosedOut::default();
    }

    let suggestion = None;
    let closed = ClosedOut {
        task: Some(id),
        suggestion,
        waiting_on_pr: None,
    };
    if !as_json {
        closed.report();
    }
    closed
}

/// What `bn agent done` reported.
struct Outcome<'a> {
    summary: &'a str,
    pr: Option<&'a str>,
    failed: bool,
    report_only: bool,
}

/// The Forge close-out: stamp `pr:`, close or hold the task, and comment. One load, one save.
fn run_forge(cfg: &Config, ctx: &Resolved, outcome: &Outcome<'_>) -> ClosedOut {
    let store = crate::cmd::task::task_store(cfg);
    let _lock = store.lock();
    let mut file = match store.load() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("bn agent done: could not load Forge tasks — {e}");
            return ClosedOut::default();
        }
    };
    let Some(id) = bn_core::pipeline::gate::governing_in(&file, &ctx.repo, &ctx.branch)
        .and_then(|t| t.id())
        .map(str::to_string)
    else {
        return ClosedOut::default();
    };

    let mut closed = ClosedOut::default();
    let closes = !outcome.failed && !outcome.report_only;
    if let Some(task) = file.task_mut(&id) {
        if let Some(pr) = outcome.pr {
            if task.meta("pr") != Some(pr) {
                task.set_meta("pr", pr);
            }
        }
        if closes && task.meta("pr").is_some() {
            task.mark_wip();
            closed.waiting_on_pr = Some(id.clone());
        } else if closes {
            task.mark_done(&today());
            closed.task = Some(id.clone());
        }
    }
    if let Err(e) = store.save(&file) {
        eprintln!("bn agent done: could not update Forge task {id} — {e}");
        closed = ClosedOut::default();
    }
    let pr = file
        .tasks()
        .find(|t| t.id() == Some(id.as_str()))
        .and_then(|t| t.meta("pr"));
    let text = done_note(outcome, pr, closed.waiting_on_pr.is_some());
    bn_core::task_store::note_best_effort(store.as_ref(), &id, &text);
    closed
}

/// The comment a completion leaves on its Forge task.
fn done_note(outcome: &Outcome<'_>, pr: Option<&str>, waiting: bool) -> String {
    let head = if outcome.failed {
        "agent failed"
    } else if outcome.report_only {
        "agent reported (no code change)"
    } else {
        "agent done"
    };
    let mut text = match outcome.summary.trim() {
        "" => head.to_string(),
        summary => format!("{head}: {summary}"),
    };
    if let Some(pr) = pr {
        text.push_str(&format!("\nPR: {pr}"));
    }
    if waiting {
        text.push_str("\nThis task completes when the PR merges.");
    }
    text
}

impl ClosedOut {
    /// Print what was closed, for the human `bn agent done` surface. Silent when nothing was —
    /// a completion with no governing task is the ordinary pre-task-store case, not an anomaly.
    pub(crate) fn report(&self) {
        if let Some(task) = &self.waiting_on_pr {
            println!("  task {task} stays open until its PR merges");
        }
        let Some(task) = &self.task else { return };
        match &self.suggestion {
            Some(s) => println!("  closed task {task} and suggestion {s}"),
            None => println!("  closed task {task}"),
        }
    }
}

#[cfg(test)]
mod tests;
