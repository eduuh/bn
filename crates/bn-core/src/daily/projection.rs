//! Goals as a projection over tasks (#1004, EPIC #993) — the lens that renders the task store as
//! a day's goals, extracted from `daily.rs` so the owner filter below has a testable home.
//!
//! **Machine work is not your work.** #1151 established `task_owner::is_machine` and taught
//! `bn task`, the queue classifier and the SMART gate to respect it, but it never reached this
//! projection — so `bn task` hid the cron chores while `bn daily`, `/api/goals`, `bn next` and the
//! standup packet went on listing "run tools-reconcile" and "run wt-cleanup" as the day's focus.
//! Filtering here fixes every one of those readers at once, because they all come through
//! [`task_goals`].

use crate::model::Item;
use crate::resolver::Config;

/// One task rendered as a daily goal.
///
/// A daily goal and a task stamped `day:` were the same idea in two stores, which meant keeping
/// them in sync by hand — exactly what #993 removes. Tasks are now the store; this is the lens.
pub(super) fn task_as_goal(task: &crate::todo::Task, today: &str) -> Item {
    let done = matches!(
        task.status(today),
        crate::todo::Status::Done | crate::todo::Status::Cancelled
    );
    Item {
        id: task.id().unwrap_or("?").to_string(),
        text: task.summary(),
        created: task.created.clone().unwrap_or_else(|| today.to_string()),
        done: Some(done),
        done_at: task.completion.clone(),
        // The issue a pulled task came from is the goal's link — the same field `bn daily add
        // --link` fills, so the rendering needs no special case.
        link: task.meta("src").map(str::to_string),
        pct: None,
        est: task.meta("est").map(str::to_string),
        actual: task.meta("actual").map(str::to_string),
        q: task.meta("q").map(str::to_string),
        notes: Vec::new(),
    }
}

/// Whether a task belongs on a *person's* day at all.
///
/// Ownership is read from the task's `auto:` marker via [`crate::task_owner::is_machine`], never
/// inferred from its text or its `src:` link — `task_owner.rs` and the `is_warming` cautionary
/// tale in `queue::classify` both make that a rule, because a machine task is not identifiable by
/// how it happens to be worded.
pub(super) fn is_mine(task: &crate::todo::Task) -> bool {
    !crate::task_owner::is_machine(task)
}

/// Whether a task is part of `today` — open and dated today or earlier (work that slipped is still
/// today's work), or finished today whenever it was scheduled.
fn is_todays(task: &crate::todo::Task, today: &str) -> bool {
    let day = task
        .meta("day")
        .filter(|d| crate::util::epoch_day(d).is_some());
    match (day, task.status(today)) {
        (Some(d), crate::todo::Status::Open | crate::todo::Status::Wip) => d <= today,
        (_, crate::todo::Status::Done | crate::todo::Status::Cancelled) => {
            task.completion.as_deref() == Some(today)
        }
        _ => false,
    }
}

/// Today's goals, projected from the task store.
///
/// **Carry-forward is free here, and that is the point.** `ensure_today` copies yesterday's open
/// goals into a new file, which is a *write* — so `GET /api/goals` (a read) never triggered it and
/// carried-forward goals were invisible in bn-web until the CLI happened to touch the day. A query
/// needs no write, so the bug cannot recur: an open task dated today *or earlier* is simply still
/// today's work.
///
/// Included: open tasks dated today or earlier, and tasks completed today. Excluded: future-dated
/// work (not today's), snoozed work (deliberately out of view), anything finished on an earlier day
/// (it belongs to that day's record), and machine-owned bookkeeping (see [`is_mine`]).
pub fn task_goals(cfg: &Config, today: &str) -> Vec<Item> {
    let Ok(file) = crate::todo::load(&cfg.tasks_file()) else {
        return Vec::new();
    };
    file.tasks()
        .filter(|t| is_mine(t) && is_todays(t, today))
        .map(|t| task_as_goal(t, today))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::TodoFile;

    /// Parse one `TODO.txt` line into the task it describes.
    fn task(line: &str) -> crate::todo::Task {
        TodoFile::parse(line)
            .tasks()
            .next()
            .expect("test task should parse")
            .clone()
    }

    /// The live regression: these exact rows were being served as the day's focus.
    #[test]
    fn a_cron_chore_is_not_my_goal() {
        let t = task("(B) 2026-08-06 run tools-reconcile id:t30 src:cron day:2026-08-11 auto:1");
        assert!(!is_mine(&t));
        assert!(
            is_todays(&t, "2026-08-11"),
            "it is dated today — only ownership excludes it"
        );
    }

    /// Ownership comes from `auto:`, not from wording. A task of mine may legitimately be about
    /// a cron and must survive.
    #[test]
    fn my_own_task_about_a_cron_survives() {
        let t =
            task("(A) 2026-08-06 fix the flaky tools-reconcile cron id:t9 src:cron day:2026-08-11");
        assert!(is_mine(&t), "no auto: marker — this is a person's work");
    }

    #[test]
    fn work_that_slipped_is_still_todays() {
        let t = task("(A) 2026-08-01 ship the brief id:t1 day:2026-08-04");
        assert!(is_todays(&t, "2026-08-11"));
    }

    #[test]
    fn future_dated_work_is_not_todays() {
        let t = task("(A) 2026-08-01 ship the brief id:t1 day:2026-08-20");
        assert!(!is_todays(&t, "2026-08-11"));
    }
}
