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

/// The task id governing `subject` in `repo`, minting one if nothing covers it yet (#1001).
///
/// Every enqueue must name a task — that invariant is what turns "work only ever starts from a
/// task" from a convention into something a test can assert. Rather than making callers refuse,
/// this mints the missing record so the ergonomic path stays one command: `bn orchestrate add
/// "..."` still works, it just leaves a durable trail behind it.
///
/// Idempotent on `key:enqueue:<hash of subject>`, so re-queuing the same subject reuses its task
/// instead of minting a second one.
pub(crate) fn task_for_enqueue(cfg: &Config, repo: &str, subject: &str, src: &str) -> String {
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
