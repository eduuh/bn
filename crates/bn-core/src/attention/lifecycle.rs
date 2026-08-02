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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::*;
    use crate::test_support::with_state_dir;

    #[test]
    fn dead_pr_items_are_retired_as_cancelled_not_resolved() {
        with_state_dir(|| {
            let today = "2026-06-23";
            let dead_pr = "https://github.com/eduuh/wira360/pull/923";
            let stale = add(
                "staff-standup",
                "Unblock stalled PR: wira360#923 — ping reviewers",
                "info",
                Some("wira360"),
                None,
                Some(dead_pr),
                None,
                today,
            )
            .unwrap();
            let live = add(
                "staff-standup",
                "Unblock stalled PR: mt5-data-api#168 — ping reviewers",
                "info",
                Some("mt5-data-api"),
                None,
                Some("https://github.com/eduuh/mt5-data-api/pull/168"),
                None,
                today,
            )
            .unwrap();
            let unlinked = add_simple("review the design doc", "warn", None);

            let dead: std::collections::HashSet<(String, i64)> =
                [("eduuh/wira360".to_string(), 923)].into_iter().collect();
            let retired = retire_dead_pr_items(&dead, today);
            assert_eq!(retired.len(), 1);
            assert_eq!(retired[0].id, stale.id);

            // Still open, severity-ranked (the `warn` first): the live PR and the unlinked item.
            let left: Vec<String> = open_items(today).into_iter().map(|i| i.id).collect();
            assert_eq!(left, [unlinked.id, live.id]);
            // Cancelled by the world, not resolved by you — the metrics must be able to tell.
            assert!(events::read_all().iter().any(|e| e.kind == "attn_remove"));
            assert!(!events::read_all().iter().any(|e| e.kind == "attn_ack"));
            // Idempotent: a second sweep finds nothing still open on that PR.
            assert!(retire_dead_pr_items(&dead, today).is_empty());
        });
    }

    #[test]
    fn mark_dispatched_folds_out_and_records_backlog_trail() {
        with_state_dir(|| {
            let it = add_simple("promote to queue", "warn", None);
            assert!(mark_dispatched(&it.id, "bn", "460", "start-new").unwrap());
            // It leaves the open lane — it's now tracked as queued orchestrate work.
            assert!(open_items("2026-06-23").is_empty());
            let ev = events::read_all()
                .into_iter()
                .find(|e| e.kind == "attn_dispatched")
                .expect("attn_dispatched event");
            assert_eq!(field_str(&ev, "repo").as_deref(), Some("bn"));
            assert_eq!(field_str(&ev, "backlog").as_deref(), Some("460"));
            assert_eq!(field_str(&ev, "action").as_deref(), Some("start-new"));
        });
    }

    #[test]
    fn mark_dispatched_rejects_unknown_id() {
        with_state_dir(|| {
            assert!(!mark_dispatched("a999", "bn", "460", "start-new").unwrap());
        });
    }

    #[test]
    fn snooze_hides_until_date_then_resurfaces() {
        with_state_dir(|| {
            let it = add_simple("later", "warn", None);
            assert!(snooze(&it.id, "2026-06-30").unwrap());
            // before the date: hidden
            assert!(open_items("2026-06-25").is_empty());
            // on/after the date: back
            assert_eq!(open_items("2026-06-30").len(), 1);
            assert_eq!(open_items("2026-07-01").len(), 1);
        });
    }

    #[test]
    fn to_notify_is_urgent_open_and_unnotified() {
        with_state_dir(|| {
            let urgent = add_simple("prod down", "urgent", None);
            add_simple("minor", "warn", None); // never pushes
                                               // only the urgent item is queued
            let q = to_notify("2026-06-23");
            assert_eq!(q.len(), 1);
            assert_eq!(q[0].id, urgent.id);
            // once marked, it leaves the queue (no re-toast)
            assert!(mark_notified(&urgent.id).unwrap());
            assert!(to_notify("2026-06-23").is_empty());
        });
    }

    #[test]
    fn acked_urgent_item_is_not_queued_for_notify() {
        with_state_dir(|| {
            let it = add_simple("transient urgent", "urgent", None);
            ack(&it.id).unwrap();
            assert!(to_notify("2026-06-23").is_empty());
        });
    }

    #[test]
    fn mark_notified_rejects_unknown_id() {
        with_state_dir(|| {
            assert!(!mark_notified("a999").unwrap());
        });
    }

    #[test]
    fn ack_and_snooze_reject_unknown_id() {
        with_state_dir(|| {
            assert!(!ack("a999").unwrap());
            assert!(!snooze("a999", "2026-06-30").unwrap());
        });
    }

    #[test]
    fn snooze_rejects_bad_date() {
        with_state_dir(|| {
            let it = add_simple("x", "info", None);
            assert!(snooze(&it.id, "next-tuesday").is_err());
        });
    }

    #[test]
    fn later_snooze_reopens_an_acked_item() {
        with_state_dir(|| {
            let it = add_simple("recurring", "warn", None);
            ack(&it.id).unwrap();
            assert!(open_items("2026-06-23").is_empty());
            // snooze after the ack: the item comes back once the snooze lapses
            snooze(&it.id, "2026-06-30").unwrap();
            assert!(open_items("2026-06-25").is_empty()); // still snoozed
            assert_eq!(open_items("2026-07-01").len(), 1); // resurfaced, not acked
        });
    }

    #[test]
    fn update_overlays_fields_and_resorts() {
        with_state_dir(|| {
            let a = add_simple("a", "info", None);
            add_simple("b", "warn", None);
            // before: warn (b) outranks info (a)
            let before: Vec<_> = open_items("2026-06-23")
                .into_iter()
                .map(|i| i.title)
                .collect();
            assert_eq!(before, vec!["b", "a"]);
            // bump a to urgent and reword it
            assert!(update(
                &a.id,
                Some("A!"),
                Some("urgent"),
                None,
                None,
                None,
                None,
                None,
                None
            )
            .unwrap());
            let after = open_items("2026-06-23");
            assert_eq!(after[0].id, a.id);
            assert_eq!(after[0].title, "A!");
            assert_eq!(after[0].severity, "urgent");
            // untouched fields keep their value; b is unchanged
            assert_eq!(after[1].title, "b");
        });
    }

    #[test]
    fn update_only_writes_given_fields() {
        with_state_dir(|| {
            let it = add(
                "manual",
                "title",
                "warn",
                Some("repo"),
                Some("branch"),
                Some("http://x"),
                None,
                "2026-06-23",
            )
            .unwrap();
            // change just the link; repo/branch/title/severity must survive
            assert!(update(
                &it.id,
                None,
                None,
                None,
                None,
                Some("http://y"),
                None,
                None,
                None
            )
            .unwrap());
            let item = get(&it.id).unwrap();
            assert_eq!(item.link.as_deref(), Some("http://y"));
            assert_eq!(item.repo.as_deref(), Some("repo"));
            assert_eq!(item.branch.as_deref(), Some("branch"));
            assert_eq!(item.title, "title");
            assert_eq!(item.severity, "warn");
        });
    }

    #[test]
    fn update_rejects_unknown_id() {
        with_state_dir(|| {
            assert!(!update("a999", Some("x"), None, None, None, None, None, None, None).unwrap());
        });
    }

    #[test]
    fn remove_folds_out_with_distinct_event() {
        with_state_dir(|| {
            let it = add_simple("mistake", "warn", None);
            assert!(remove(&it.id).unwrap());
            assert!(open_items("2026-06-23").is_empty());
            // recorded as a cancellation, not an ack — metrics can tell them apart
            let kinds: Vec<_> = events::read_all().into_iter().map(|e| e.kind).collect();
            assert!(kinds.contains(&"attn_remove".to_string()));
            assert!(!kinds.contains(&"attn_ack".to_string()));
        });
    }

    #[test]
    fn remove_rejects_unknown_id() {
        with_state_dir(|| {
            assert!(!remove("a999").unwrap());
        });
    }

    #[test]
    fn mark_planned_folds_out_and_records_todo() {
        with_state_dir(|| {
            let it = add_simple("promote me", "warn", None);
            assert!(mark_planned(&it.id, "t7").unwrap());
            assert!(open_items("2026-06-23").is_empty());
            let planned = events::read_all()
                .into_iter()
                .find(|e| e.kind == "attn_planned")
                .expect("attn_planned event");
            assert_eq!(field_str(&planned, "todo").as_deref(), Some("t7"));
        });
    }

    #[test]
    fn mark_planned_rejects_unknown_id() {
        with_state_dir(|| {
            assert!(!mark_planned("a999", "t1").unwrap());
        });
    }

    #[test]
    fn update_edits_mode_and_prompt() {
        with_state_dir(|| {
            let it = add_simple("x", "warn", None);
            assert!(update(
                &it.id,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("assist"),
                Some("do the thing")
            )
            .unwrap());
            let item = get(&it.id).unwrap();
            assert_eq!(item.mode(), "assist");
            assert_eq!(item.prompt.as_deref(), Some("do the thing"));
        });
    }

    #[test]
    fn started_moves_item_from_open_to_in_flight() {
        with_state_dir(|| {
            let it = add_simple("do work", "warn", None);
            assert_eq!(open_items("2026-06-23").len(), 1);
            assert!(started_items().is_empty());

            assert!(mark_started(&it.id).unwrap());
            // left the to-start lane, entered the in-flight lane
            assert!(open_items("2026-06-23").is_empty());
            let flight = started_items();
            assert_eq!(flight.len(), 1);
            assert_eq!(flight[0].item.id, it.id);
            assert!(!flight[0].done, "started but not yet done");
        });
    }

    #[test]
    fn done_flags_in_flight_item_awaiting_review() {
        with_state_dir(|| {
            let it = add_simple("do work", "warn", None);
            mark_started(&it.id).unwrap();
            assert!(mark_done(&it.id, Some("shipped it"), None).unwrap());
            // still in-flight (needs your review), now marked done with notes
            assert!(open_items("2026-06-23").is_empty());
            let flight = started_items();
            assert_eq!(flight.len(), 1);
            assert!(flight[0].done);
            assert_eq!(flight[0].notes.as_deref(), Some("shipped it"));
        });
    }

    #[test]
    fn done_with_link_lands_the_pr_on_the_item() {
        with_state_dir(|| {
            let it = add_simple("ship feature", "warn", None);
            mark_started(&it.id).unwrap();
            assert!(mark_done(
                &it.id,
                Some("opened PR"),
                Some("https://github.com/x/pull/7")
            )
            .unwrap());
            let flight = started_items();
            assert_eq!(flight.len(), 1);
            assert!(flight[0].done);
            // The link overlays the item itself — the needs-you row opens straight to the diff.
            assert_eq!(
                flight[0].item.link.as_deref(),
                Some("https://github.com/x/pull/7")
            );
            assert_eq!(flight[0].notes.as_deref(), Some("opened PR"));
        });
    }

    #[test]
    fn ack_closes_an_in_flight_item() {
        with_state_dir(|| {
            let it = add_simple("do work", "warn", None);
            mark_started(&it.id).unwrap();
            mark_done(&it.id, None, None).unwrap();
            // reviewing + acking removes it from every lane
            assert!(ack(&it.id).unwrap());
            assert!(open_items("2026-06-23").is_empty());
            assert!(started_items().is_empty());
        });
    }

    #[test]
    fn mark_started_and_done_reject_unknown_id() {
        with_state_dir(|| {
            assert!(!mark_started("a999").unwrap());
            assert!(!mark_done("a999", None, None).unwrap());
        });
    }
}
