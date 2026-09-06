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
