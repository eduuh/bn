//! Which rows a `bn task` listing shows, and in what order.
//!
//! Split out of `cmd/task.rs` (which is at the 1000-line limit) because it is one coherent thing:
//! the *view* semantics, as pure functions over a parsed store. `emit_task_list` renders whatever
//! this returns; nothing here reads a terminal, a clock or a disk, so "what does the bare view
//! show?" is a question with a test rather than a session in front of a real task list.
//!
//! Three filters compose, and they hide work for three different reasons:
//!
//! - **repo scope** — you asked for one repo;
//! - **day bucket** — it is scheduled for a later day, so it is not actionable *now*;
//! - **owner** — the machine minted it for itself and it was never your work.
use crate::*;

/// Which slice of the store a listing shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskView {
    /// The default: work that is actionable *now* — `day:` today or earlier (so an overdue task
    /// stays visible rather than silently ageing out), plus everything unscheduled.
    Today,
    /// Every open task, whatever it is scheduled for and whoever it belongs to.
    All,
}

/// The repo scope a listing is filtered to.
pub(crate) enum RepoScope {
    /// No filter — every repo.
    Any,
    /// A specific repo name (already lowercased).
    Named(String),
}

/// Resolve `--repo`: `.` means the repo you are standing in, anything else is a name. Exits with
/// guidance when `.` is used outside a git repo, since there is nothing to resolve it to.
pub(crate) fn repo_scope(cfg: &Config, repo: Option<&str>) -> RepoScope {
    match repo {
        None => RepoScope::Any,
        Some(".") => match crate::cmd::task::task_ctx_opt(cfg) {
            Some(ctx) => RepoScope::Named(ctx.repo.to_ascii_lowercase()),
            None => fail("bn task --repo .: not in a git repo — pass a repo name instead"),
        },
        Some(name) => RepoScope::Named(name.trim().to_ascii_lowercase()),
    }
}

impl RepoScope {
    fn admits(&self, task: &bn_core::todo::Task) -> bool {
        match self {
            RepoScope::Any => true,
            RepoScope::Named(want) => task
                .meta("repo")
                .is_some_and(|r| r.eq_ignore_ascii_case(want)),
        }
    }
}

/// Where a task sits relative to `today`, for the bare view's ordering.
///
/// `None` means "not actionable today" — scheduled for a future day, so it folds out of the
/// default listing the way a snoozed task does. A malformed `day:` is treated as unscheduled
/// rather than as a future date: the same discipline as `snooze:`, so a hand-edit or a merge
/// artifact can never hide work forever.
fn day_bucket(task: &bn_core::todo::Task, today: &str) -> Option<u8> {
    match task
        .meta("day")
        .filter(|d| bn_core::util::epoch_day(d).is_some())
    {
        Some(day) if day > today => None,
        Some(_) => Some(0),
        None => Some(1),
    }
}

/// The rows a listing shows, paired with their day bucket, most-actionable first.
///
/// Pure, so the view semantics are testable without a store or a terminal. Ordering is the day
/// bucket first (today/overdue, then unscheduled, then — in `--all` — future work), and within a
/// bucket the sort is stable, so `open_sorted`'s priority → due → id ranking is preserved.
///
/// Machine-minted tasks ([`bn_core::task_owner`]) fold out of the default view the way future-dated
/// work does — present in the store, absent from the question "what could I do today". Every
/// enqueue must name a task (#1001), and almost every enqueue is the machine's: crons firing,
/// worktrees being reclaimed, pools being warmed, setup agents the dashboard generated. Left in,
/// they were 93 of the 96 rows here, and the three that were actually a person's work were
/// unfindable among them. `--all` still shows them, because hiding a record is not the same as not
/// keeping one.
pub(crate) fn select_tasks<'a>(
    file: &'a bn_core::todo::TodoFile,
    view: TaskView,
    scope: &RepoScope,
    today: &str,
) -> Vec<(u8, &'a bn_core::todo::Task)> {
    let mut items: Vec<(u8, &bn_core::todo::Task)> = file
        .open_sorted(today, None)
        .into_iter()
        .filter(|t| scope.admits(t))
        .filter(|t| view == TaskView::All || !bn_core::task_owner::is_machine(t))
        .filter_map(|t| match (view, day_bucket(t, today)) {
            // `--all` keeps future-dated work, sorted after everything actionable.
            (TaskView::All, bucket) => Some((bucket.unwrap_or(2), t)),
            (TaskView::Today, bucket) => bucket.map(|b| (b, t)),
        })
        .collect();
    items.sort_by_key(|(bucket, _)| *bucket);
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::todo::TodoFile;

    const TODAY: &str = "2026-08-11";

    /// One human task among the machine's bookkeeping — the real shape of the store, in miniature.
    ///
    /// `t4` stands in for a dispatched repo-setup agent, whose real title is the whole generated
    /// prompt: the rows that made the day's list unreadable were paragraphs, not phrases.
    fn store() -> TodoFile {
        TodoFile::parse(concat!(
            "refactor the goal write side id:t1 repo:bn mod:10\n",
            "run keep-warm id:t2 repo:machine auto:1 mod:20\n",
            "reclaim worktree byte_s/repo-setup id:t3 repo:byte_s auto:1 mod:30\n",
            "repo setup agent seed for wira360 id:t4 repo:wira360 auto:1 mod:40\n",
        ))
    }

    fn ids(items: &[(u8, &bn_core::todo::Task)]) -> Vec<String> {
        items
            .iter()
            .filter_map(|(_, t)| t.id())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_day_view_shows_your_work_not_the_machines() {
        // The list this fixes was 96 rows deep with 3 of them a person's. A view that answers
        // "what could I do today" with 93 crons, reclaims and generated prompts has stopped
        // answering.
        let file = store();
        let items = select_tasks(&file, TaskView::Today, &RepoScope::Any, TODAY);
        assert_eq!(ids(&items), vec!["t1"]);
    }

    #[test]
    fn the_backlog_view_still_holds_every_record() {
        // Folding out of a view is not deletion. Every enqueue names a task (#1001) precisely so
        // the trail exists; `--all` is where it stays readable.
        let file = store();
        let items = select_tasks(&file, TaskView::All, &RepoScope::Any, TODAY);
        assert_eq!(ids(&items), vec!["t1", "t2", "t3", "t4"]);
    }

    #[test]
    fn a_repo_scope_still_narrows_the_machines_own_work() {
        // The filters are independent: `--all --repo machine` is a real question ("what is the
        // machine doing for itself?") and hiding by owner must not answer it for you.
        let file = store();
        let scope = RepoScope::Named("machine".to_string());
        let items = select_tasks(&file, TaskView::All, &scope, TODAY);
        assert_eq!(ids(&items), vec!["t2"]);
    }

    #[test]
    fn future_dated_work_still_folds_out_of_the_day_view() {
        // The pre-existing rule, kept honest now that a second filter sits beside it.
        let file = TodoFile::parse(concat!(
            "today's work id:t1 repo:bn mod:10\n",
            "next week's work id:t2 repo:bn day:2026-08-20 mod:20\n",
        ));
        let items = select_tasks(&file, TaskView::Today, &RepoScope::Any, TODAY);
        assert_eq!(ids(&items), vec!["t1"]);
    }
}
