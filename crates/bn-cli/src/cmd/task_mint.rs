//! Minting the task that governs work which arrived from somewhere else (#1001).
//!
//! A decomposed issue's children are not places a person types a task, so each child mints its
//! record here, idempotently, and the agent started for it is bound to that task.
//!
//! Its own module because `cmd/task.rs` is at the 1000-line limit.
use crate::*;

/// The task id governing `subject` in `repo`, minting one if nothing covers it yet (#1001).
///
/// Idempotent on a hash of the subject and repo, so starting the same subject again reuses its task
/// instead of minting a second one.
pub(crate) fn task_for_subject(cfg: &Config, repo: &str, subject: &str, src: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    subject.trim().hash(&mut h);
    repo.hash(&mut h);
    // The `enqueue:` prefix is kept so tasks minted before the rename are still found.
    let key = format!("enqueue:{:016x}", h.finish());

    let store = task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
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
    if store.save(&file).is_err() {
        return String::new();
    }
    id
}
