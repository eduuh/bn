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
    if let Some(note) = ctx.and_then(|context| bn_core::store::read(&cfg.note_file(context)).ok()) {
        let found = if is_dispatch_todo_id(raw) {
            note.front.todos.iter().find(|todo| todo.id == raw)
        } else {
            let query = raw.to_lowercase();
            note.front
                .todos
                .iter()
                .find(|todo| todo.is_open_todo() && todo.text.to_lowercase().contains(&query))
        };
        if let Some(todo) = found {
            let (title, url) = split_item_text(&todo.text);
            return (title, url, Some(todo.id.clone()));
        }
    }
    (raw.to_string(), None, None)
}
