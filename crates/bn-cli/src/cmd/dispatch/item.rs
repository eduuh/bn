//! Parsing helpers for dispatch item identifiers and captured text.

use crate::{Config, Resolved};

/// Whether `value` looks like a todo id: `t` followed by digits (`t3`, `t12`).
pub(crate) fn is_dispatch_todo_id(value: &str) -> bool {
    value
        .strip_prefix('t')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Split a captured item's stored text into its title and optional URL.
pub(crate) fn split_item_text(text: &str) -> (String, Option<String>) {
    if let Some((title, url)) = text.rsplit_once(" — ") {
        let url = url.trim();
        if url.starts_with("http") && !url.contains(' ') {
            return (title.trim().to_string(), Some(url.to_string()));
        }
    }
    (text.trim().to_string(), None)
}

/// Resolve a URL, todo id, unique todo substring, or free-text dispatch target.
pub(crate) fn resolve_dispatch_item(
    cfg: &Config,
    ctx: Option<&Resolved>,
    raw: &str,
) -> (String, Option<String>, Option<String>) {
    let raw = raw.trim();
    if raw.starts_with("http") {
        return (super::capture_title(raw), Some(raw.to_string()), None);
    }
    let tasks = bn_core::task_store::task_store(cfg)
        .load()
        .unwrap_or_else(|error| {
            eprintln!("bn dispatch: cannot load configured task provider: {error}");
            std::process::exit(1);
        });
    let found = if is_dispatch_todo_id(raw) {
        tasks.tasks().find(|task| task.id() == Some(raw))
    } else {
        ctx.and_then(|context| {
            let query = raw.to_lowercase();
            let mut matching = tasks.tasks().filter(|task| {
                !task.done
                    && task.meta("repo") == Some(context.repo.as_str())
                    && task.summary().to_lowercase().contains(&query)
            });
            let first = matching.next();
            if matching.next().is_none() {
                first
            } else {
                None
            }
        })
    };
    if let Some(todo) = found {
        let (title, url) = split_item_text(&todo.summary());
        return (title, url, todo.id().map(str::to_string));
    }
    (raw.to_string(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_global_task_id_resolves_outside_any_git_repo() {
        let _lock = bn_core::test_support::env_lock();
        let root = tempfile::tempdir().unwrap();
        let cfg = bn_core::test_support::config(root.path());
        let task =
            bn_core::task_store::capture(&cfg, "Preserve task authority", None, "test").unwrap();
        let (title, _, id) = resolve_dispatch_item(&cfg, None, task.id().unwrap());
        assert_eq!(title, "Preserve task authority");
        assert_eq!(id.as_deref(), task.id());
    }
}
