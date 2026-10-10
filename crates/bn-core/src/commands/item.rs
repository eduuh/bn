//! Item repair verbs: `bn drop`, `bn undone`, `bn reword` — fix a wrong item without opening
//! the whole note in `$EDITOR`. Each takes an id (`t3`, `b1`, `w2`, …) or a case-insensitive
//! substring, resolved the way `bn done` resolves its query.

use super::error::{emit_metric, CmdError};
use crate::model::{Item, Note, Section};
use crate::resolver::{Config, Resolved};
use crate::store;
use serde_json::json;

/// Result of an item repair: which item changed and its text after the change. `previous` is
/// the text before a reword (`None` for drop/undone).
#[derive(Debug)]
pub struct ItemOutcome {
    pub section: Section,
    pub id: String,
    pub text: String,
    pub previous: Option<String>,
}

/// `bn drop <id|substring>` — remove an item outright. Unlike `done` it records no completion,
/// so a mistaken or duplicate item leaves no trace in progress or metrics.
pub fn drop_item(cfg: &Config, ctx: &Resolved, query: &str) -> Result<ItemOutcome, CmdError> {
    edit_note(
        cfg,
        ctx,
        "drop",
        query,
        |_| true,
        |note, section, idx| {
            let item = note.front.items_mut(section).remove(idx);
            Ok(outcome(section, item, None))
        },
    )
}

/// `bn undone <id|substring>` — reopen a checked-off todo or done-when criterion, clearing its
/// completion stamp and any calibration actual. A substring searches only DONE items.
pub fn reopen_item(cfg: &Config, ctx: &Resolved, query: &str) -> Result<ItemOutcome, CmdError> {
    edit_note(
        cfg,
        ctx,
        "undone",
        query,
        |it| it.done == Some(true),
        |note, section, idx| {
            let item = &mut note.front.items_mut(section)[idx];
            if item.done != Some(true) {
                return Err(CmdError::NotDone(item.id.clone()));
            }
            item.done = Some(false);
            item.done_at = None;
            item.actual = None;
            Ok(outcome(section, item.clone(), None))
        },
    )
}

/// `bn reword <id|substring> <text>` — replace an item's text, keeping its id, dates and
/// state.
pub fn reword_item(
    cfg: &Config,
    ctx: &Resolved,
    query: &str,
    text: &str,
) -> Result<ItemOutcome, CmdError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }
    edit_note(
        cfg,
        ctx,
        "reword",
        query,
        |_| true,
        |note, section, idx| {
            let item = &mut note.front.items_mut(section)[idx];
            let previous = std::mem::replace(&mut item.text, text.to_string());
            Ok(outcome(section, item.clone(), Some(previous)))
        },
    )
}

fn outcome(section: Section, item: Item, previous: Option<String>) -> ItemOutcome {
    ItemOutcome {
        section,
        id: item.id,
        text: item.text,
        previous,
    }
}

/// Locked read-modify-write shared by the repair verbs: resolve `query` to one item, apply
/// `apply`, save, and log an `activity` metric under `action`.
fn edit_note(
    cfg: &Config,
    ctx: &Resolved,
    action: &str,
    query: &str,
    searchable: impl Fn(&Item) -> bool,
    apply: impl FnOnce(&mut Note, Section, usize) -> Result<ItemOutcome, CmdError>,
) -> Result<ItemOutcome, CmdError> {
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = store::read(&note_md)?;
    let (section, idx) = resolve(&note, query.trim(), searchable)?;
    let out = apply(&mut note, section, idx)?;
    note.front.record_session();
    store::save(&note, &note_md)?;
    emit_metric(
        cfg,
        ctx,
        "activity",
        vec![("action", json!(action)), ("item", json!(out.id))],
    );
    Ok(out)
}

/// An exact id in any section wins; otherwise a case-insensitive substring over the items
/// `searchable` admits, erroring on zero or several matches.
fn resolve(
    note: &Note,
    q: &str,
    searchable: impl Fn(&Item) -> bool,
) -> Result<(Section, usize), CmdError> {
    if q.is_empty() {
        return Err(CmdError::EmptyText);
    }
    for sec in Section::all() {
        if let Some(i) = note.front.items(sec).iter().position(|it| it.id == q) {
            return Ok((sec, i));
        }
    }
    let ql = q.to_lowercase();
    let mut matches: Vec<(Section, usize, String)> = Vec::new();
    for sec in Section::all() {
        for (i, it) in note.front.items(sec).iter().enumerate() {
            if searchable(it) && it.text.to_lowercase().contains(&ql) {
                matches.push((sec, i, it.id.clone()));
            }
        }
    }
    match matches.len() {
        0 => Err(CmdError::NotFound(q.to_string())),
        1 => Ok((matches[0].0, matches[0].1)),
        _ => Err(CmdError::Ambiguous(
            q.to_string(),
            matches.into_iter().map(|(_, _, id)| id).collect(),
        )),
    }
}

#[cfg(test)]
mod tests;
