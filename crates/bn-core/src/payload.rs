//! Shared `data` payload builders for the JSON envelope. Both the CLI and the MCP
//! server call these so the machine-readable shapes stay byte-identical across surfaces.

use crate::daily::{self, Daily};
use crate::doctor::Finding;
use crate::reports;
use crate::resolver::{RepoBranch, Resolved};
use crate::reviews::ReviewCache;
use crate::rollups::{ListEntry, StatusEntry, SummaryEntry, TodoEntry};
use crate::search::SearchHit;
use crate::skills::{SkillInfo, SkillScope};
use serde_json::{json, Value};

/// `agent sessions` → `{ "count", "sessions": [...] }`. Each session serializes from
/// `AgentSession` (the recall view); `duration_secs` is derivable from `started_ts`/`ended_ts`.
pub fn agent_sessions_data(sessions: &[crate::agent::AgentSession]) -> Value {
    json!({ "count": sessions.len(), "sessions": sessions })
}

/// `agent supervise` → the live `Fleet` (agents + health counts). Flattened so the envelope
/// `data` is `{ "agents": [...], "active", "idle", "stuck" }` — the shape the FLEET panel reads.
pub fn agent_fleet_data(f: &crate::agent::Fleet) -> Value {
    json!(f)
}

/// `orchestrate status` (one run) → the status viewmodel the CLI and MCP share. Recovery
/// is reversed to newest-first and capped at the last 20 events. `started_at`/`updated_at` carry
/// the run's clock so a monitor UI can render "started N ago" and staleness without a second call.
///
/// `reviews` are the open plan-reviews (#349), passed in (not read here, keeping this pure) so each
/// dispatched item is annotated with its `waiting_for_review` state — a dispatched agent paused on
/// an un-approved plan is surfaced distinctly in `bn orchestrate status`/watch.
pub fn orchestrate_status_data(
    st: &crate::orchestrate::OrchestrateState,
    reviews: &[crate::plan_review::Review],
) -> Value {
    json!({
        "repo": st.repo,
        "concurrency": st.concurrency,
        "research": st.research,
        "solo": st.solo,
        "stopped": st.stopped,
        "supervise": st.supervise,
        "self_feed": st.self_feed,
        "drained": st.is_drained(),
        "resident": st.is_resident(),
        "started_at": st.started_at,
        "updated_at": st.updated_at,
        "counts": st.counts(),
        "dispatch_bind": st.dispatch_bind,
        "dispatch_reason": st.dispatch_reason,
        "queue": st.queue.iter().map(|i| orchestrate_queue_item(st, i, reviews)).collect::<Vec<_>>(),
        "recovery": st.recovery.iter().rev().take(20).collect::<Vec<_>>(),
    })
}

/// One queue item plus its #372 navigation coordinates: the `repo` it belongs to and — once
/// dispatched — the tmux `session`/`window` its agent runs in (session = repo, window = the
/// sanitized branch), so a picker/cockpit jumps straight to it. `harness` rides along from the item
/// itself. `tmux_session`/`tmux_window` are absent while queued (no window yet); `repo` is always
/// present. Derived here (not persisted) so the CLI and MCP inherit one shape.
///
/// #349: a dispatched item is also annotated with `waiting_for_review` — `true` when a plan-review
/// for its repo+branch is still `pending` — plus the `review_id` linking to it. Surfaces the
/// waiting-for-review agent state distinctly (queued items are never waiting; they have no agent).
fn orchestrate_queue_item(
    st: &crate::orchestrate::OrchestrateState,
    item: &crate::orchestrate::QueueItem,
    reviews: &[crate::plan_review::Review],
) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        // #453: the item's own repo is the other half of its `(repo, item)` key — the session it
        // dispatches into. Falls back to the run repo for a legacy item with no repo; for a
        // single-repo run `item.repo == st.repo`, so the nav coordinates are unchanged.
        let repo = if item.repo.is_empty() {
            st.repo.as_str()
        } else {
            item.repo.as_str()
        };
        obj.insert("repo".to_string(), json!(repo));
        if let Some(window) = item.tmux_window() {
            obj.insert("tmux_session".to_string(), json!(repo));
            obj.insert("tmux_window".to_string(), json!(window));
        }
        // Only a dispatched (in-flight) item can have an agent paused on a plan-review; queued
        // items have no agent, so the key stays absent for them (like the nav coordinates).
        if item.status.in_flight() {
            let pending = item
                .branch
                .as_deref()
                .and_then(|b| crate::plan_review::pending_for(reviews, repo, b));
            obj.insert("waiting_for_review".to_string(), json!(pending.is_some()));
            if let Some(r) = pending {
                obj.insert("review_id".to_string(), json!(r.id));
            }
        }
    }
    v
}

/// `daily` → `{ "date", "open", "done", "goals": [...], "upcoming": [...] }`. `upcoming` is
/// the not-yet-due follow-up queue (non-empty only for today's view). The rendered text view
/// lives at `rendered`.
pub fn daily_data(d: &Daily, upcoming: &[daily::ScheduledItem]) -> Value {
    let goals: Vec<Value> = d
        .front
        .goals
        .iter()
        .map(|g| {
            json!({
                "id": g.id,
                "text": g.text,
                "link": g.link,
                "done": g.done == Some(true),
                "done_at": g.done_at,
                "created": g.created,
                "pct": g.pct,
                "est": g.est,
                "actual": g.actual,
                "q": g.q,
                "notes": g.notes,
            })
        })
        .collect();
    let up: Vec<Value> = upcoming
        .iter()
        .map(|s| json!({ "id": s.id, "text": s.text, "link": s.link, "scheduled": s.scheduled }))
        .collect();
    json!({
        "date": d.front.date,
        "open": d.open().count(),
        "done": d.completed().count(),
        "goals": goals,
        "upcoming": up,
        "rendered": daily::render(d, upcoming),
    })
}

/// `daily add` → `{ "item": { "id", "text", "link" } }`. A scheduled add (`--on`/`--in`) also
/// carries `scheduled` (target date) and `landed_today`.
pub fn daily_add_data(a: &daily::AddOutcome) -> Value {
    let mut item = json!({ "id": a.id, "text": a.text, "link": a.link });
    if let Some(scheduled) = &a.scheduled {
        item["scheduled"] = json!(scheduled);
        item["landed_today"] = json!(a.landed_today);
    }
    json!({ "item": item })
}

/// `daily done` → `{ "item": { "id", "text", "done": true, "actual" } }`.
pub fn daily_done_data(d: &daily::DoneOutcome) -> Value {
    match d.pct {
        Some(p) => json!({ "item": { "id": d.id, "text": d.text, "done": false, "pct": p } }),
        None => json!({ "item": { "id": d.id, "text": d.text, "done": true, "actual": d.actual } }),
    }
}

/// `daily drop` → the goal removed from today (no completion recorded).
pub fn daily_drop_data(d: &daily::DropOutcome) -> Value {
    json!({ "item": { "id": d.id, "text": d.text, "link": d.link } })
}

/// `daily defer` → the goal moved off today and its new scheduled date.
pub fn daily_defer_data(d: &daily::DeferOutcome) -> Value {
    json!({ "item": { "id": d.id, "text": d.text, "scheduled": d.scheduled } })
}

/// Goal-note connect/disconnect (#564) → the goal the note now hangs on + the affected note.
pub fn goal_note_data(o: &daily::NoteOutcome) -> Value {
    json!({
        "goal": { "id": o.goal_id, "text": o.goal_text },
        "note": o.note,
    })
}

/// `daily rollover` → what advanced, plus the rendered summary.
pub fn rollover_data(r: &daily::RolloverOutcome) -> Value {
    let carried: Vec<Value> = r
        .carried
        .iter()
        .map(|i| json!({ "id": i.id, "text": i.text, "link": i.link }))
        .collect();
    let due: Vec<Value> = r
        .due
        .iter()
        .map(|s| json!({ "id": s.id, "text": s.text, "scheduled": s.scheduled }))
        .collect();
    let retired: Vec<Value> =
        r.retired
            .goals
            .iter()
            .map(|i| json!({ "id": i.id, "text": i.text, "link": i.link, "from": "today" }))
            .chain(r.retired.scheduled.iter().map(
                |s| json!({ "id": s.id, "text": s.text, "link": s.link, "from": "scheduled" }),
            ))
            .collect();
    json!({
        "created_today": r.created_today,
        "carried": carried,
        "due": due,
        "inbox_routed": r.inbox_routed,
        "inbox_left": r.inbox_left,
        "retired": retired,
        "rendered": rollover_text(r),
    })
}

/// One-line-per-section rollover summary, shared by CLI text and MCP.
pub fn rollover_text(r: &daily::RolloverOutcome) -> String {
    let mut out = if r.created_today {
        format!("Rolled over to {}\n", r.today.front.date)
    } else {
        format!("{} already rolled\n", r.today.front.date)
    };
    if r.carried.is_empty() && r.due.is_empty() && r.inbox_routed.is_empty() && r.retired.is_empty()
    {
        out.push_str("  (nothing to carry)\n");
    }
    for i in &r.carried {
        out.push_str(&format!("  ↻ {}\n", i.text));
    }
    for s in &r.due {
        out.push_str(&format!("  → {} (was scheduled {})\n", s.text, s.scheduled));
    }
    for q in &r.inbox_routed {
        out.push_str(&format!("  ⚡ {} → {}\n", q.text, q.dest));
    }
    for i in &r.retired.goals {
        out.push_str(&format!("  ✗ {} (PR merged/closed)\n", i.text));
    }
    for s in &r.retired.scheduled {
        out.push_str(&format!(
            "  ✗ {} (PR merged/closed — was scheduled {})\n",
            s.text, s.scheduled
        ));
    }
    if r.inbox_left > 0 {
        let s = if r.inbox_left == 1 { "" } else { "s" };
        out.push_str(&format!(
            "  ({} capture{s} still in inbox — bn q drain)\n",
            r.inbox_left
        ));
    }
    out
}

/// `weekly` → the full structured view plus a `rendered` text field for display.
pub fn weekly_data(w: &reports::Weekly) -> Value {
    let mut v = serde_json::to_value(w).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(reports::render_weekly(w)));
    }
    v
}

/// `insights` / `retro` → the full computed retro (metrics + the one suggestion) plus a
/// `rendered` text field, matching `weekly`'s shape so nvim/jobs can read either.
pub fn insights_data(ins: &crate::insights::Insights) -> Value {
    let mut v = serde_json::to_value(ins).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(crate::insights::render(ins)));
    }
    v
}

/// `wrap` → the full end-of-day digest plus a `rendered` text field for display.
pub fn wrap_data(w: &reports::WrapDigest) -> Value {
    let mut v = serde_json::to_value(w).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(reports::render_wrap(w)));
    }
    v
}

/// `link`/`pr` resolve → `{ "url": "<url>" }` (the value the CLI would open/copy/show).
pub fn url_data(url: &str) -> Value {
    json!({ "url": url })
}

/// `status` → the `StatusEntry`, or a zeroed shape carrying the resolved context when no
/// note exists for the branch.
pub fn status_data(status: &Option<StatusEntry>, ctx: &Resolved) -> Value {
    match status {
        Some(s) => json!(s),
        None => json!({
            "repo": ctx.repo,
            "branch": ctx.branch,
            "open_todos": 0,
            "blockers": 0,
            "work_item": null,
            "pr": null,
        }),
    }
}

/// `todo` → `{ "todos": [TodoEntry, ...] }`. Wrapped in an object because MCP
/// `structuredContent` must be a JSON object, never a bare array.
pub fn todos_data(entries: &[TodoEntry]) -> Value {
    json!({ "todos": entries })
}

/// `list` → `{ "branches": [ListEntry, ...] }`. Object-wrapped (see `todos_data`).
pub fn list_data(entries: &[ListEntry]) -> Value {
    json!({ "branches": entries })
}

/// `summary` → `{ "branches": [SummaryEntry, ...] }`. Object-wrapped (see `todos_data`).
pub fn summary_data(entries: &[SummaryEntry]) -> Value {
    json!({ "branches": entries })
}

/// `worktrees` → `{ "worktrees": [WorktreeEntry, ...] }`. Object-wrapped (see `todos_data`).
pub fn worktrees_data(entries: &[crate::rollups::WorktreeEntry]) -> Value {
    json!({ "worktrees": entries })
}

/// `search` → `{ "hits": [SearchHit, ...] }`. Object-wrapped (see `todos_data`).
pub fn search_data(hits: &[SearchHit]) -> Value {
    json!({ "hits": hits })
}

/// `doctor` → `{ "findings": [Finding, ...] }`. Object-wrapped (see `todos_data`).
pub fn doctor_data(findings: &[Finding]) -> Value {
    json!({ "findings": findings })
}

/// `skills` → `{ "count", "skills": [SkillInfo, ...] }`. Object-wrapped (see `todos_data`);
/// `count` makes the empty result definitive for agents (`count: 0`, not a missing field).
pub fn skills_data(skills: &[SkillInfo]) -> Value {
    json!({ "count": skills.len(), "skills": skills })
}

/// `skill_new` → `{ "skill": { "name", "scope", "path" } }` for the just-created file.
pub fn skill_new_data(path: &std::path::Path, scope: SkillScope, name: &str) -> Value {
    json!({ "skill": { "name": name, "scope": scope, "path": path.display().to_string() } })
}

/// `attn list` → `{ "items": [AttnItem, ...] }`. Object-wrapped (see `todos_data`).
pub fn attn_list_data(items: &[crate::attention::AttnItem]) -> Value {
    json!({ "items": items })
}

/// `attn add` → `{ "item": { "id", "skipped" } }`. `skipped` is true when an open item with
/// the same `key` already existed, so nothing was written.
pub fn attn_add_data(a: &crate::attention::AddOutcome) -> Value {
    json!({ "item": { "id": a.id, "skipped": a.skipped } })
}

/// `attn ingest` → batch summary plus per-row ids. `resolved` counts previously-open keyed items
/// from the replaced source that were acked because they were absent from the batch.
pub fn attn_ingest_data(o: &crate::attention::IngestOutcome) -> Value {
    json!({
        "raised": o.raised,
        "skipped": o.skipped,
        "resolved": o.resolved,
        "items": o.items,
    })
}

/// `attn ack`/`attn snooze`/`attn update`/`attn remove` → `{ "id", "ok" }`. `ok` is false when
/// `id` matched no item.
pub fn attn_resolve_data(id: &str, ok: bool) -> Value {
    json!({ "id": id, "ok": ok })
}

/// The branch todo an `attn plan` produced: its own id, the section it landed in, and the
/// `(repo, branch)` it belongs to. Grouping these keeps [`attn_plan_data`] from taking five
/// adjacent `&str` params (an easy silent-swap footgun).
pub struct PlannedTodo<'a> {
    pub id: &'a str,
    pub section: &'a str,
    pub rb: RepoBranch<'a>,
}

/// `attn plan` → `{ "id", "ok": true, "todo": { id, section, repo, branch } }`. The attention
/// item `id` was resolved (planned); `todo` is the branch item it became.
pub fn attn_plan_data(id: &str, todo: PlannedTodo<'_>) -> Value {
    json!({
        "id": id,
        "ok": true,
        "todo": {
            "id": todo.id,
            "section": todo.section,
            "repo": todo.rb.repo,
            "branch": todo.rb.branch,
        }
    })
}

/// `reviews` → `{ "generated_at", "prs": [ReviewPr, ...] }`.
pub fn reviews_data(cache: &ReviewCache) -> Value {
    json!({ "generated_at": cache.generated_at, "prs": cache.prs })
}

/// `review list` → `{ "reviews": [Review, ...] }` (#349). Each record carries
/// its plan text, status, and notes; the CLI, web, and MCP share this one shape.
pub fn plan_reviews_data(reviews: &[crate::plan_review::Review]) -> Value {
    let reviews: Vec<_> = reviews
        .iter()
        .map(|review| review.reviewer_view())
        .collect();
    json!({ "reviews": reviews })
}

/// `review show`/`approve`/`request-changes`/`reject` → `{ "review": {...} }` — one record.
pub fn plan_review_data(review: &crate::plan_review::Review) -> Value {
    json!({ "review": review.reviewer_view() })
}

/// `next` → `{ "actions": [Action + category, ...] }`. Object-wrapped (see `todos_data`). The
/// derived dashboard `category` is attached per action so an agent (or the prefix-J picker) can
/// group exactly as the human view does, without re-deriving the mapping.
pub fn next_data(actions: &[crate::next::Action]) -> Value {
    let arr: Vec<Value> = actions
        .iter()
        .map(|a| {
            let mut v = serde_json::to_value(a).unwrap_or_else(|_| json!({}));
            if let Value::Object(m) = &mut v {
                m.insert(
                    "category".into(),
                    serde_json::to_value(a.category()).unwrap_or(Value::Null),
                );
            }
            v
        })
        .collect();
    json!({ "actions": arr })
}

/// `q` capture → the banked item and the hint it was stamped with.
pub fn q_capture_data(o: &crate::inbox::CaptureOutcome) -> Value {
    json!({ "id": o.id, "text": o.text, "repo": o.repo, "branch": o.branch })
}

/// `q` list → the pending captures, in capture order.
pub fn q_list_data(items: &[crate::inbox::InboxItem]) -> Value {
    json!({ "items": items })
}

/// `q drain` → what routed where, and how many captures still need triage.
pub fn q_drain_data(routed: &[crate::inbox::RoutedOutcome], left: usize) -> Value {
    json!({ "routed": routed, "left": left })
}

/// `metrics` → the full `Scorecard` (close-out velocity + throughput). Already a record.
pub fn scorecard_data(s: &crate::metrics::Scorecard) -> Value {
    json!(s)
}
