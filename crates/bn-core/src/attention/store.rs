//! Event folding and accessors shared by every attention read path.

use super::model::AttnItem;
use crate::events::{self, Event};
use serde_json::{Map, Value};

/// Merge an `attn_update` event's edited fields into a per-id override map (every field except
/// the `ref` back-pointer), latest value per field winning.
pub(crate) fn merge_update(slot: &mut Map<String, Value>, ev: &Event) {
    for (k, v) in &ev.fields {
        if k != "ref" {
            slot.insert(k.clone(), v.clone());
        }
    }
}

/// Build an [`AttnItem`] from its base `attn` event, preferring any `attn_update` `overrides`
/// over the original field. `created` always stays the raise time, not an edit time.
pub(crate) fn fold_item(
    base: &Event,
    id: String,
    overrides: Option<&Map<String, Value>>,
) -> AttnItem {
    let field = |key: &str| -> Option<String> {
        overrides
            .and_then(|m| m.get(key))
            .or_else(|| base.fields.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    // Read a JSON string array (an override wins over the base, like `field`), dropping any
    // non-string element. Used for the `uncommitted_files` list on a `wt-stuck` item.
    let string_list = |key: &str| -> Vec<String> {
        overrides
            .and_then(|m| m.get(key))
            .or_else(|| base.fields.get(key))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    AttnItem {
        id,
        source: field("source").unwrap_or_else(|| "manual".into()),
        title: field("title").unwrap_or_default(),
        severity: field("severity").unwrap_or_else(|| "info".into()),
        repo: field("repo"),
        branch: field("branch"),
        link: field("link"),
        key: field("key"),
        detail: field("detail"),
        mode: field("mode"),
        prompt: field("prompt"),
        task_type: field("task_type"),
        worktree_path: field("worktree_path"),
        uncommitted_files: string_list("uncommitted_files"),
        created: base.ts,
    }
}

/// The folded item for `id` with any `attn_update` edits applied, regardless of whether it's
/// still open (acked/removed/planned items included). `None` if no `attn` event ever raised `id`.
/// Used by `plan` to seed a todo from the item's latest title/locator.
pub fn get(id: &str) -> Option<AttnItem> {
    let events = events::read_all();
    let mut overrides: Map<String, Value> = Map::new();
    for ev in &events {
        if ev.kind == "attn_update" && field_str(ev, "ref").as_deref() == Some(id) {
            merge_update(&mut overrides, ev);
        }
    }
    let ov = (!overrides.is_empty()).then_some(&overrides);
    events
        .iter()
        .find(|ev| ev.kind == "attn" && field_str(ev, "id").as_deref() == Some(id))
        .map(|ev| fold_item(ev, id.to_string(), ov))
}

/// Whether any `attn` event ever raised `id` (regardless of resolution state). Lets the mutating
/// ops (`ack`/`snooze`/`remove`/`update`/`mark_planned`) reject typos without re-deriving the set.
pub(crate) fn exists(id: &str) -> bool {
    events::read_all()
        .iter()
        .any(|e| e.kind == "attn" && field_str(e, "id").as_deref() == Some(id))
}

/// Next `a<N>` id: one past the highest `a<N>` ever raised, so ids of acked items are never
/// reused (matching the note-section id discipline in `id.rs`).
pub(crate) fn next_id() -> String {
    let max = events::read_all()
        .iter()
        .filter(|e| e.kind == "attn")
        .filter_map(|e| field_str(e, "id"))
        .filter_map(|id| id.strip_prefix('a').and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    format!("a{}", max + 1)
}

pub(crate) fn field_str(ev: &Event, key: &str) -> Option<String> {
    ev.fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::*;
    use crate::test_support::with_state_dir;

    #[test]
    fn get_returns_folded_item_even_after_resolution() {
        with_state_dir(|| {
            let it = add_simple("planned soon", "warn", None);
            update(
                &it.id,
                Some("renamed"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            mark_planned(&it.id, "t1").unwrap();
            // gone from the open list, but still fetchable with the edit applied
            assert!(open_items("2026-06-23").is_empty());
            let got = get(&it.id).unwrap();
            assert_eq!(got.title, "renamed");
            assert!(get("a999").is_none());
        });
    }
}
