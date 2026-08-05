//! Lifecycle mutations on an existing item: ack, snooze, update, remove, planned,
//! started, done, and the notify bookkeeping.

use super::model::AttnItem;
use super::query::open_items;
use super::store::{exists, field_str};
use crate::events;
use crate::util::epoch_day;
use serde_json::{json, Map};

/// Ack item `id` — mark it resolved. Returns `false` (without writing) if `id` matches no known
/// attn item, so the caller can warn on a typo. Appends `attn_ack {ref:id}`.
pub fn ack(id: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    events::append("attn_ack", fields)?;
    Ok(true)
}

/// Snooze item `id` until `until` (`YYYY-MM-DD`) — hidden from the open set until that date.
/// `Ok(false)` if `id` is unknown; `Err` if `until` isn't a valid date. Appends
/// `attn_snooze {ref:id, until}`.
pub fn snooze(id: &str, until: &str) -> std::io::Result<bool> {
    if epoch_day(until).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid date '{until}' (want YYYY-MM-DD)"),
        ));
    }
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    fields.insert("until".into(), json!(until));
    events::append("attn_snooze", fields)?;
    Ok(true)
}

/// Edit an existing item in place — any subset of title/severity/repo/branch/link/detail. Only
/// the `Some` fields are written; the rest keep their current value. Returns `false` (without
/// writing) if `id` matches no known item, so the caller can warn on a typo. Appends
/// `attn_update {ref:id, <changed fields>}`, folded latest-value-per-field-wins by [`open_items`].
#[allow(clippy::too_many_arguments)]
pub fn update(
    id: &str,
    title: Option<&str>,
    severity: Option<&str>,
    repo: Option<&str>,
    branch: Option<&str>,
    link: Option<&str>,
    detail: Option<&str>,
    mode: Option<&str>,
    prompt: Option<&str>,
) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    for (k, v) in [
        ("title", title),
        ("severity", severity),
        ("repo", repo),
        ("branch", branch),
        ("link", link),
        ("detail", detail),
        ("mode", mode),
        ("prompt", prompt),
    ] {
        if let Some(val) = v {
            fields.insert(k.into(), json!(val));
        }
    }
    events::append("attn_update", fields)?;
    Ok(true)
}

/// Remove item `id` — dismiss it as cancelled or raised in error. Folds out of the open set like
/// [`ack`], but records a distinct `attn_remove` so metrics can tell a *cancelled* item from a
/// *resolved* one. Returns `false` (without writing) if `id` is unknown. Appends
/// `attn_remove {ref:id}`.
pub fn remove(id: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    events::append("attn_remove", fields)?;
    Ok(true)
}

/// Dismiss every open item hanging off a PR in `dead` — merged or closed, so "ping reviewers on
/// wira360#923" is moot and would otherwise sit in the queue forever (#966). A [`remove`], not an
/// [`ack`]: the item was cancelled by the world, you didn't action it, and the metrics should say
/// so. Items with no link, or a link that isn't a PR URL, are untouched. Returns the dismissed
/// items so the caller can report what left the queue.
pub fn retire_dead_pr_items(
    dead: &std::collections::HashSet<(String, i64)>,
    today: &str,
) -> Vec<AttnItem> {
    if dead.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in open_items(today) {
        let is_dead = item
            .link
            .as_deref()
            .is_some_and(|l| crate::pr_observe::link_is_terminal(dead, l));
        if is_dead && remove(&item.id).unwrap_or(false) {
            out.push(item);
        }
    }
    out
}

/// Mark item `id` as promoted to a branch todo — folds it out of the open set (it's now tracked
/// as planned work) and records the created `todo` id for the trail. Distinct from [`ack`] so
/// metrics can tell planned work from a plain resolution. Returns `false` (without writing) if
/// `id` is unknown. Appends `attn_planned {ref:id, todo}`.
pub fn mark_planned(id: &str, todo: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    fields.insert("todo".into(), json!(todo));
    events::append("attn_planned", fields)?;
    Ok(true)
}

/// Mark item `id` as promoted into the orchestrate backlog as a task — folds it out of the open
/// set (it's now tracked as queued work) and records the target `repo`, the backlog entry `id`,
/// and the promotion `action` (`start-new` | `reprioritize` | `interrupt`) for the trail. Distinct
/// from [`mark_planned`] (a branch todo) and [`ack`] so metrics can tell dispatched work from a
/// plain resolution. Returns `false` (without writing) if `id` is unknown. Appends
/// `attn_dispatched {ref:id, repo, backlog, action}`.
pub fn mark_dispatched(id: &str, repo: &str, backlog: &str, action: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    fields.insert("repo".into(), json!(repo));
    fields.insert("backlog".into(), json!(backlog));
    fields.insert("action".into(), json!(action));
    events::append("attn_dispatched", fields)?;
    Ok(true)
}

/// Mark item `id` as *started* — moves it out of the open "to pick" set into the in-flight set
/// ([`started_items`]), where its live status is derived from its tmux window/agent. Not terminal:
/// a later `attn_ack`/`attn_remove` closes it, or `attn_done` flips it to done-awaiting-review.
/// Re-starting simply re-stamps the moment (latest event wins). Returns `false` (without writing)
/// if `id` is unknown. Appends `attn_started {ref:id}`.
pub fn mark_started(id: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    events::append("attn_started", fields)?;
    Ok(true)
}

/// Mark in-flight item `id` as *done — awaiting your review*: the work (agent or hand) is finished
/// but the item stays visible (in the needs-you lane) until you [`ack`] it. The human gate on
/// autonomous work — distinct from `ack` (you've reviewed and closed it) so an `auto` agent reports
/// completion without self-closing. Optional `notes` capture the summary; optional `link` (the
/// draft PR the work shipped as) lands on the item itself via the `attn_update` overlay, so the
/// needs-you row opens straight to the diff. Returns `false` (without writing) if `id` is unknown.
/// Appends `attn_done {ref:id, notes?}` (+ an `attn_update {link}` when given).
pub fn mark_done(id: &str, notes: Option<&str>, link: Option<&str>) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    if let Some(l) = link {
        update(id, None, None, None, None, Some(l), None, None, None)?;
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    if let Some(n) = notes {
        fields.insert("notes".into(), json!(n));
    }
    events::append("attn_done", fields)?;
    Ok(true)
}

/// Open `urgent` items not yet pushed as a notification — the `notify` job's work queue. An
/// item leaves this queue once [`mark_notified`] records it, so the job never re-toasts. Warn/
/// info items never push (only the badge and `bn doctor` surface them), per the urgent-only
/// threshold.
pub fn to_notify(today: &str) -> Vec<AttnItem> {
    let notified = notified_ids();
    open_items(today)
        .into_iter()
        .filter(|i| i.severity == "urgent" && !notified.contains(&i.id))
        .collect()
}

/// Record that item `id` was pushed as a notification — the notify job's dedup state. `Ok(false)`
/// if `id` matches no item. Appends `attn_notified {ref:id}`.
pub fn mark_notified(id: &str) -> std::io::Result<bool> {
    if !exists(id) {
        return Ok(false);
    }
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(id));
    events::append("attn_notified", fields)?;
    Ok(true)
}

/// Item ids that already had a notification pushed (an `attn_notified` event).
fn notified_ids() -> std::collections::HashSet<String> {
    events::read_all()
        .iter()
        .filter(|e| e.kind == "attn_notified")
        .filter_map(|e| field_str(e, "ref"))
        .collect()
}
