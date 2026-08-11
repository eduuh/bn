//! Minting the task that governs work which arrived from somewhere else (#1001).
//!
//! Every enqueue must name a task — the invariant that turns "work only ever starts from a task"
//! from a convention into something a test can assert. But the producers that enqueue (a PR review,
//! an attention item, a cron, an accepted Today suggestion) are not places a person types a task, so
//! refusing them would only mean the invariant is worked around. Each mints the missing record here
//! instead, idempotently, and leaves a durable trail behind it.
//!
//! Its own module because `cmd/task.rs` is at the 1000-line limit, and because these are one
//! coherent thing: the doorway through which non-typed work acquires an identity.
use crate::*;

/// Who the task being minted belongs to.
///
/// The invariant is that every enqueue names a task; it was never that every enqueue is *yours*. A
/// cron firing, a worktree reclaim, a pool warm and a dashboard-generated repo setup prompt each
/// minted a line indistinguishable from one you typed, and the day's actionable list came back 96
/// items deep with three of them human. The bit is declared by the caller rather than sniffed from
/// `src`, because the same producer serves both: `bn queue add --job pool-warm` and `bn queue add
/// "fix the header"` arrive through one door.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskOwner {
    /// A person chose this work. It belongs on the list.
    Human,
    /// The machine generated this for itself — stamped `auto:1` and folded out of the day's view.
    Machine,
}

/// The task id governing `subject` in `repo`, minting one if nothing covers it yet (#1001).
///
/// Every enqueue must name a task — that invariant is what turns "work only ever starts from a
/// task" from a convention into something a test can assert. Rather than making callers refuse,
/// this mints the missing record so the ergonomic path stays one command: `bn orchestrate add
/// "..."` still works, it just leaves a durable trail behind it.
///
/// Idempotent on `key:enqueue:<hash of subject>`, so re-queuing the same subject reuses its task
/// instead of minting a second one.
///
/// Mints a **person's** task. Machine bookkeeping calls [`task_for_machine_enqueue`] instead — the
/// default is this way round because a task belonging to whoever is looking at the list is the safe
/// mistake, and because it leaves the many human producers untouched.
pub(crate) fn task_for_enqueue(cfg: &Config, repo: &str, subject: &str, src: &str) -> String {
    mint(cfg, repo, subject, src, TaskOwner::Human)
}

/// The task id governing machine bookkeeping — a cron firing, a worktree reclaim, a pool warm, a
/// generated setup prompt.
///
/// Same record and same idempotency as [`task_for_enqueue`]; the line is stamped `auto:1`, which
/// keeps it out of `bn task`'s day view and out of the SMART gate. Re-queuing an existing subject
/// does **not** reclassify it: a person who adopted one of these should not have it silently taken
/// back off their list.
pub(crate) fn task_for_machine_enqueue(
    cfg: &Config,
    repo: &str,
    subject: &str,
    src: &str,
) -> String {
    mint(cfg, repo, subject, src, TaskOwner::Machine)
}

fn mint(cfg: &Config, repo: &str, subject: &str, src: &str, owner: TaskOwner) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    subject.trim().hash(&mut h);
    repo.hash(&mut h);
    let key = format!("enqueue:{:016x}", h.finish());

    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = bn_core::todo::load(&path) else {
        return String::new();
    };
    if let Some(existing) = file
        .tasks()
        .find(|t| t.meta("key") == Some(key.as_str()))
        .and_then(|t| t.id())
    {
        return existing.to_string();
    }

    let today = today();
    let id = file.next_id();
    // The subject may be free text from anywhere, so defang it before it becomes a description —
    // the same rule #1003 applies to issue titles.
    let text = bn_core::todo::neutralize_meta(subject);
    let mut task = bn_core::todo::Task::new_todo(&id, &text, None, "", src, &today);
    task.set_meta_raw("repo", repo);
    task.set_meta_raw("key", &key);
    if owner == TaskOwner::Machine {
        bn_core::task_owner::mark_machine(&mut task);
    }
    file.push_task(task);
    if bn_core::todo::save(&path, &file).is_err() {
        return String::new();
    }
    id
}

/// One task by id, or `None` when the store has no such task.
pub(crate) fn load_task(cfg: &Config, id: &str) -> Option<bn_core::todo::Task> {
    bn_core::todo::load(&task_path(cfg))
        .ok()?
        .tasks()
        .find(|t| t.id() == Some(id))
        .cloned()
}

/// The meta key linking a task back to the Today suggestion it came from.
///
/// Close-out reads it: when the work lands, the task is marked done and this says which proposal to
/// retire with it, so the day's loop closes instead of leaving an `accepted` suggestion open forever.
pub(crate) const SUGGESTION_KEY: &str = "suggestion";

/// The task governing an accepted Today suggestion, minting one on first accept.
///
/// Idempotent on `key:today:<suggestion-id>`, so accepting twice reuses the task — the same
/// discipline as [`task_for_enqueue`], keyed on the suggestion rather than on a hash of its words so
/// that re-wording a finding cannot mint a second task for the same proposal.
///
/// `repo` is `None` when the finding named none. That is a **deliberate** omission rather than a
/// guessed default: leaving `repo:` unset is what makes the SMART gate report `no_repo`, which is
/// what routes the task to the enhance agent that resolves where the work happens. Filling it in
/// with a plausible-looking repo would silently skip that step and book the work in the wrong place.
pub(crate) fn task_for_suggestion(
    cfg: &Config,
    suggestion: &bn_core::suggestions::Suggestion,
    repo: Option<&str>,
) -> String {
    let key = format!("today:{}", suggestion.id);
    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = bn_core::todo::load(&path) else {
        return String::new();
    };
    if let Some(existing) = file
        .tasks()
        .find(|t| t.meta("key") == Some(key.as_str()))
        .and_then(|t| t.id())
    {
        return existing.to_string();
    }

    let today = today();
    let id = file.next_id();
    // A suggestion's title is synthesized from a finding — an issue title, a PR body, an API
    // response — so it is untrusted text. Defang it before it becomes a task description, or a
    // finding titled `... smart:ok repo:victim` would forge its own gate verdict (#1003).
    let text = bn_core::todo::neutralize_meta(&suggestion.title);
    // `src:` is the finding's URL, which is also what satisfies the gate's `not_relevant` check —
    // a suggestion traceable to an issue is relevant by construction.
    let mut task = bn_core::todo::Task::new_todo(&id, &text, None, "", &suggestion.url, &today);
    if let Some(repo) = repo {
        task.set_meta_raw("repo", repo);
    }
    task.set_meta_raw("key", &key);
    task.set_meta_raw(SUGGESTION_KEY, &suggestion.id);
    file.push_task(task);
    if bn_core::todo::save(&path, &file).is_err() {
        return String::new();
    }
    id
}
