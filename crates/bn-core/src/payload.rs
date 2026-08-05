//! Shared `data` payload builders for the JSON envelope. Both the CLI and the MCP
//! server call these so the machine-readable shapes stay byte-identical across surfaces.

use crate::commands::{AddOutcome, CloseOutcome, DoneOutcome, GoalOutcome, PrOutcome};
use crate::daily::{self, Daily};
use crate::doctor::Finding;
use crate::events::CronJobStatus;
use crate::metrics::Metrics;
use crate::quarter::{self, Quarter};
use crate::reports;
use crate::resolver::{RepoBranch, Resolved};
use crate::reviews::ReviewCache;
use crate::rollups::{ListEntry, StatusEntry, SummaryEntry, TodoEntry};
use crate::search::SearchHit;
use crate::skills::{SkillInfo, SkillScope};
use crate::track::{self, Interval, PunchOutcome, ReportBy, ReportRow};
use serde_json::{json, Value};

/// `agent sessions` → `{ "count", "sessions": [...] }`. Each session serializes from
/// `AgentSession` (the recall view); `duration_secs` is derivable from `started_ts`/`ended_ts`.
pub fn agent_sessions_data(sessions: &[crate::agent::AgentSession]) -> Value {
    json!({ "count": sessions.len(), "sessions": sessions })
}

/// `agent digest` → the `Digest` record (since/sessions/proposals). Already a serde record;
/// object-wrapped so the envelope `data` is `{ "digest": {...} }`, matching `sync`'s shape.
pub fn agent_digest_data(d: &crate::agent::Digest) -> Value {
    json!({ "digest": d })
}

/// `agent supervise` → the live `Fleet` (agents + health counts). Flattened so the envelope
/// `data` is `{ "agents": [...], "active", "idle", "stuck" }` — the shape the FLEET panel reads.
pub fn agent_fleet_data(f: &crate::agent::Fleet) -> Value {
    json!(f)
}

/// `bearings` → the bounded current-state snapshot. Shared by CLI, REST, and SSE so every machine
/// surface emits the same typed data.
pub fn bearings_data(snapshot: &crate::bearings::BearingsSnapshot) -> Value {
    json!(snapshot)
}

/// `GET /api/machine` (#525) → the machine-health snapshot: the sampled `cpu`/`ram`/`saturation`
/// flattened with the live fleet's `agents { active }`, so the dashboard's health widget renders
/// the host *and* how many agents are loading it in one read. Kept here (not inline in bn-web) so
/// the shape is shared and unit-testable.
pub fn machine_health_data(machine: &crate::machine::Machine, active_agents: usize) -> Value {
    json!({
        "cpu": machine.cpu,
        "ram": machine.ram,
        "saturation": machine.saturation,
        "agents": { "active": active_agents },
    })
}

/// `orchestrate status` (one run) → the status viewmodel the CLI, web, and MCP all share.
/// Extracted from bn-cli's inline `run_json` (#389) so `bn orchestrate status --json`, the
/// `GET /api/orchestrate` endpoint, and any MCP surface emit one byte-identical shape. Recovery
/// is reversed to newest-first and capped at the last 20 events. `started_at`/`updated_at` carry
/// the run's clock so a monitor UI can render "started N ago" and staleness without a second call.
///
/// `reviews` are the open plan-reviews (#349), passed in (not read here, keeping this pure) so each
/// dispatched item is annotated with its `waiting_for_review` state — a dispatched agent paused on
/// an un-approved plan is surfaced distinctly in `bn orchestrate status`/watch and the web board.
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
/// present. Derived here (not persisted) so the CLI, web board, and MCP inherit one shape.
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

/// The `rollups` monitoring block for `bn orchestrate watch` / `status --json` (#379): the pure
/// throughput / ETA / recovery math from [`crate::orchestrate::Rollups`] (merged/hr, avg cycle,
/// ETA, queue depth, recent recovery/failure counts) plus the cost burn the CLI layers on from the
/// #381 `cost::rollup` surface — which is disk-bound, so it can't live in the pure core. The three
/// cost keys (`cost_today_aic`, the real Copilot bill; `cost_today_usd`, the cache-aware API-equiv
/// benchmark; `tokens_today`) are always present, `null`/`0` when nothing priceable ran today.
/// Shared here so the CLI and web (#390) emit one byte-identical block.
pub fn orchestrate_rollups_data(
    rollups: &crate::orchestrate::Rollups,
    cost_today_aic: Option<f64>,
    cost_today_usd: Option<f64>,
    tokens_today: i64,
) -> Value {
    let mut v = serde_json::to_value(rollups).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("cost_today_aic".to_string(), json!(cost_today_aic));
        obj.insert("cost_today_usd".to_string(), json!(cost_today_usd));
        obj.insert("tokens_today".to_string(), json!(tokens_today));
    }
    v
}

/// `orchestrate answers` → `{ "repo", "answers": [ { id, question, path, harvested_at } ] }` — the
/// scout/ask deliverable surface (#378). Each element is one answered item's id plus its harvested
/// [`crate::orchestrate::Answer`] (the question echoed back, the findings-write-up path, and the
/// harvest time). Extracted from bn-cli's inline `answers` json (mirroring [`orchestrate_status_data`])
/// so `bn orchestrate answers --json`, the web board, and any MCP surface emit one byte-identical
/// shape. `answered` is the already-filtered set of items carrying an answer (an empty slice — no
/// run, or nothing routed back yet — yields an empty `answers` array).
pub fn orchestrate_answers_data(repo: &str, answered: &[&crate::orchestrate::QueueItem]) -> Value {
    let items: Vec<Value> = answered
        .iter()
        .filter_map(|i| {
            let a = i.answer.as_ref()?;
            Some(json!({
                "id": i.id,
                "question": a.question,
                "path": a.path,
                "harvested_at": a.harvested_at,
            }))
        })
        .collect();
    json!({ "repo": repo, "answers": items })
}

/// `handoff write`/`read` → `{ "handoff": {...}, "rendered": "<orientation text>" }`. The packet
/// round-trips through serde; the rendered orientation is the one-read banner.
pub fn handoff_data(h: &crate::handoff::Handoff) -> Value {
    json!({ "handoff": h, "rendered": crate::handoff::render_orientation(h) })
}

/// `add` → `{ "section", "item": { "id", "text", "line", "skipped" } }`.
/// `skipped` is true when an `if_absent` key matched an existing item, so nothing was written.
pub fn add_data(a: &AddOutcome) -> Value {
    json!({
        "section": a.section.label(),
        "item": { "id": a.id, "text": a.text, "line": a.line, "skipped": a.skipped }
    })
}

/// `done` → `{ "item": { "id", "text", "done": true } }`.
pub fn done_data(d: &DoneOutcome) -> Value {
    match d.pct {
        Some(p) => json!({ "item": { "id": d.id, "text": d.text, "done": false, "pct": p } }),
        None => json!({ "item": { "id": d.id, "text": d.text, "done": true, "actual": d.actual } }),
    }
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

/// `bn quarter` (bare / past) → goals with derived status, counts, the elapsed header, and the
/// appended reviews. `today` drives only the elapsed-vs-progress header (the file body is stable).
pub fn quarter_data(q: &Quarter, today: &str) -> Value {
    let goals: Vec<Value> = q
        .front
        .goals
        .iter()
        .map(|g| {
            json!({
                "key": g.key,
                "text": g.text,
                "done": g.done,
                "done_at": g.done_at,
                "metric": g.metric,
                "status": q.status_of(&g.key),
            })
        })
        .collect();
    json!({
        "quarter": q.front.quarter,
        "elapsed_pct": (quarter::elapsed_fraction(&q.front.quarter, today).unwrap_or(0.0) * 100.0).round() as i64,
        "open": q.open().count(),
        "done": q.front.goals.iter().filter(|g| g.done).count(),
        "on_track": q.on_track_count(),
        "total": q.front.goals.len(),
        "goals": goals,
        "reviews": q.front.reviews,
        "header": quarter::header(q, today),
        "rendered": quarter::render(q, today),
    })
}

/// `quarter add` → `{ "item": { "key", "text", "quarter" } }`.
pub fn quarter_add_data(a: &quarter::AddOutcome) -> Value {
    json!({ "item": { "key": a.key, "text": a.text, "quarter": a.quarter } })
}

/// `quarter done` → `{ "item": { "key", "text", "done": true } }`.
pub fn quarter_done_data(d: &quarter::DoneOutcome) -> Value {
    json!({ "item": { "key": d.key, "text": d.text, "done": true } })
}

/// `quarter review` → the impact-agent outcome: appended (with the validated review + attn count),
/// malformed (with the validation reason), or skipped (no goals / agent unavailable).
pub fn quarter_review_data(outcome: &quarter::ReviewOutcome, qid: &str) -> Value {
    match outcome {
        quarter::ReviewOutcome::Appended { review, raised } => json!({
            "outcome": "appended",
            "quarter": qid,
            "review": review,
            "raised": raised,
        }),
        quarter::ReviewOutcome::Malformed { reason } => json!({
            "outcome": "malformed",
            "quarter": qid,
            "reason": reason,
        }),
        quarter::ReviewOutcome::Skipped { reason } => json!({
            "outcome": "skipped",
            "quarter": qid,
            "reason": reason,
        }),
    }
}

/// `bn quarter advise` → the goal-aligned shortlist (#458): the ranked candidates plus the
/// quarter id and the capacity the ranking was shaped to. `shortlist` is the count flagged
/// in-capacity — what the orchestrator would dispatch this pass.
pub fn quarter_advise_data(qid: &str, capacity: usize, advice: &[crate::advise::Advice]) -> Value {
    json!({
        "quarter": qid,
        "capacity": capacity,
        "shortlist": advice.iter().filter(|a| a.within_capacity).count(),
        "advice": advice,
    })
}

fn interval_json(iv: &Interval) -> Value {
    json!({
        "activity": iv.activity,
        "start": iv.start,
        "end": iv.end,
        "secs": iv.secs,
        "open": iv.open,
        "repo": iv.repo,
        "branch": iv.branch,
        "tag": iv.tag,
        "link": iv.link,
    })
}

/// `track` → `{ "date", "intervals": [...], "total_secs", "rendered" }`.
pub fn track_data(date: &str, intervals: &[Interval]) -> Value {
    json!({
        "date": date,
        "intervals": intervals.iter().map(interval_json).collect::<Vec<_>>(),
        "total_secs": track::total_secs(intervals),
        "rendered": track::render_day(date, intervals),
    })
}

/// `track in` / `track out` → the recorded punch and the activity it closed (if any).
pub fn track_punch_data(o: &PunchOutcome) -> Value {
    json!({
        "punch": {
            "ts": o.punch.ts,
            "is_in": o.punch.is_in,
            "activity": o.punch.activity,
            "repo": o.punch.repo,
            "branch": o.punch.branch,
            "tag": o.punch.tag,
            "link": o.punch.link,
        },
        "closed": o.closed,
    })
}

/// `track at` → the activity in progress at a queried time (or null).
pub fn track_at_data(at_ts: i64, iv: Option<&Interval>) -> Value {
    json!({ "at": at_ts, "interval": iv.map(interval_json) })
}

/// `track report` → the rolled-up buckets plus the rendered summary.
pub fn track_report_data(rows: &[ReportRow], by: ReportBy) -> Value {
    let buckets: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "label": r.label, "secs": r.secs }))
        .collect();
    json!({
        "buckets": buckets,
        "total_secs": rows.iter().map(|r| r.secs).sum::<i64>(),
        "rendered": track::render_report(rows, by),
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

/// `standup` → the full structured view plus a `rendered` text field for display.
pub fn standup_data(s: &reports::Standup) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(reports::render_standup(s)));
    }
    v
}

/// `stand` → the full structured view plus a `rendered` text field for display.
pub fn stand_data(s: &reports::Stand) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(reports::render_stand(s)));
    }
    v
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

/// `brag` → the full impact packet plus a `rendered` markdown field for display.
pub fn brag_data(b: &crate::brag::Brag) -> Value {
    let mut v = serde_json::to_value(b).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut m) = v {
        m.insert("rendered".into(), json!(crate::brag::render_markdown(b)));
    }
    v
}

/// `close` → what the close completed: counts + the logged achievement (if any).
pub fn close_data(c: &CloseOutcome) -> Value {
    json!({
        "repo": c.repo,
        "branch": c.branch,
        "todos_completed": c.todos_completed,
        "achievement": c.achievement,
        "unmet_criteria": c.unmet_criteria,
    })
}

/// `set_pr` → `{ "item": { "pr": "<url>" } }`.
pub fn pr_data(p: &PrOutcome) -> Value {
    json!({ "item": { "pr": p.pr } })
}

/// `link`/`pr` resolve → `{ "url": "<url>" }` (the value the CLI would open/copy/show).
pub fn url_data(url: &str) -> Value {
    json!({ "url": url })
}

/// `set_goal` → `{ "goal": "<line>" }`.
pub fn goal_data(g: &GoalOutcome) -> Value {
    json!({ "goal": g.goal })
}

/// `note` → `{ "path": "<note.md>" }`. The plugin edits `path` directly.
pub fn note_data(path: &std::path::Path) -> Value {
    json!({ "path": path.display().to_string() })
}

/// `note` (full detail) → `{ "repo", "branch", "front", "body", "goal" }`: one branch note as the
/// dashboard reads it — the typed frontmatter (PR, work-item, structured item lists), the prose
/// body (markdown), and the derived goal line. Shared by `GET /api/notes/{repo}/{branch}` and the
/// enriched agent-detail read so both surface one identical note shape.
pub fn note_detail_data(repo: &str, branch: &str, note: &crate::model::Note) -> Value {
    json!({
        "repo": repo,
        "branch": branch,
        "front": note.front,
        "body": note.body,
        "goal": crate::render::goal_text(&note.body),
    })
}

/// `files` → `{ "dir", "files": [ { "name", "path" } ] }`.
pub fn files_data(dir: &std::path::Path, files: &[crate::commands::FileEntry]) -> Value {
    json!({
        "dir": dir.display().to_string(),
        "files": files.iter().map(|f| json!({ "name": f.name, "path": f.path })).collect::<Vec<_>>(),
    })
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

/// `recall` (query) → `{ "source", "degraded_reason", "hits": [RecallHit, ...] }`. `source` is
/// `index` or `fallback` (substring search); `degraded_reason` is present only when it fell back.
pub fn recall_data(out: &crate::recall::RecallOutcome) -> Value {
    let source = match out.source {
        crate::recall::RecallSource::Index => "index",
        crate::recall::RecallSource::Fallback => "fallback",
    };
    json!({
        "source": source,
        "degraded_reason": out.degraded_reason,
        "hits": out.hits,
    })
}

/// `recall --index` → the [`BuildStats`](crate::recall::BuildStats) of the build run.
pub fn recall_index_data(stats: &crate::recall::BuildStats) -> Value {
    json!({ "index": stats })
}

/// `related` → `{ "branch", "related": [RelatedBranch, ...] }`. `branch` echoes the target so
/// agents know whose neighbours these are.
pub fn related_data(branch: &str, related: &[crate::related::RelatedBranch]) -> Value {
    json!({ "branch": branch, "related": related })
}

/// `doctor` → `{ "findings": [Finding, ...] }`. Object-wrapped (see `todos_data`).
pub fn doctor_data(findings: &[Finding]) -> Value {
    json!({ "findings": findings })
}

/// `prune` → `{ "closed": [{repo,branch}], "failed": [..] }`.
pub fn prune_data(r: &crate::lifecycle::PruneReport) -> Value {
    json!({ "closed": r.closed, "failed": r.failed })
}

/// `stale` → `{ "stale": [{repo,branch,path,last_activity,idle_days,open_todos}] }`.
pub fn stale_data(entries: &[crate::lifecycle::StaleEntry]) -> Value {
    json!({ "stale": entries })
}

/// `clean` → `{ "cleaned": [{repo,branch,removed}], "removed": <total> }`.
pub fn clean_data(entries: &[crate::lifecycle::CleanEntry]) -> Value {
    let total: usize = entries.iter().map(|e| e.removed).sum();
    json!({ "cleaned": entries, "removed": total })
}

/// `archive` → `{ "archived": [..], "skipped": [..], "failed": [..], "dry_run": bool }`.
pub fn archive_data(r: &crate::lifecycle::ArchiveReport) -> Value {
    json!({
        "archived": r.archived,
        "skipped": r.skipped,
        "failed": r.failed,
        "dry_run": r.dry_run,
    })
}

/// `cron status` → `{ "jobs": [CronJobStatus, ...] }`. Object-wrapped (see `todos_data`).
pub fn cron_status_data(jobs: &[CronJobStatus]) -> Value {
    json!({ "jobs": jobs })
}

/// `GET /api/cron` → `{ "count", "jobs": [CronView, ...] }` — the machine-wide registry of
/// declared crons joined with their last-run health (#483). Object-wrapped with a definitive
/// `count` (see `skills_data`), so an empty registry is `count: 0`, never a missing field.
pub fn crons_view_data(views: &[crate::crons::CronView]) -> Value {
    json!({ "count": views.len(), "jobs": views })
}

/// `skills` → `{ "count", "skills": [SkillInfo, ...] }`. Object-wrapped (see `todos_data`);
/// `count` makes the empty result definitive for agents (`count: 0`, not a missing field).
pub fn skills_data(skills: &[SkillInfo]) -> Value {
    json!({ "count": skills.len(), "skills": skills })
}

/// How many open todos `context_data` inlines before pointing at `bn_cat` for the rest.
const CONTEXT_TOP_TODOS: usize = 5;

/// `bn_context` (MCP) → the common session-start reads in one compact, current-branch payload:
/// goal, open todo/blocker counts, the top open todos, blockers, PR/work item, whether a handoff
/// packet exists, and contextual `help` next-step commands. Empty states are definitive
/// (`note_exists: false`, zeroed counts, `[]` lists) so an agent never has to guess whether a
/// missing field means "none" or "not looked". Deliberately compact: the full note stays behind
/// `bn_cat`, the full orientation behind `bn_handoff_read`.
pub fn context_data(
    note: Option<&crate::model::Note>,
    ctx: &Resolved,
    handoff_available: bool,
) -> Value {
    let goal = note.and_then(|n| crate::render::goal_text(&n.body));
    let open: Vec<&crate::model::Item> = note
        .map(|n| n.front.todos.iter().filter(|t| t.is_open_todo()).collect())
        .unwrap_or_default();
    let blockers: Vec<&crate::model::Item> = note
        .map(|n| n.front.blockers.iter().collect())
        .unwrap_or_default();
    let brief = |i: &&crate::model::Item| json!({ "id": i.id, "text": i.text });

    let mut help: Vec<&str> = Vec::new();
    if goal.is_none() {
        help.push("bn_goal — set the one-line goal first thing in a fresh worktree");
    }
    if handoff_available {
        help.push("bn_handoff_read — load the full orientation from the last session");
    }
    if !blockers.is_empty() {
        help.push("bn_add section=blocker — update blockers as they move; clear stale ones");
    }
    if !open.is_empty() {
        help.push("bn_done — close a todo the moment it's finished, not batched at the end");
    }
    help.push("bn_add — record todos/blockers/decisions as they come up");
    if note.is_some() {
        help.push("bn_cat — render the full note (all todos, decisions, progress)");
    }

    json!({
        "repo": ctx.repo,
        "branch": ctx.branch,
        "note_exists": note.is_some(),
        "goal": goal,
        "counts": { "open_todos": open.len(), "blockers": blockers.len() },
        "todos": open.iter().take(CONTEXT_TOP_TODOS).map(brief).collect::<Vec<_>>(),
        "todos_truncated": open.len() > CONTEXT_TOP_TODOS,
        "blockers": blockers.iter().map(brief).collect::<Vec<_>>(),
        "work_item": note.and_then(|n| n.front.work_item.clone()),
        "pr": note.and_then(|n| n.front.pr.clone()),
        "handoff_available": handoff_available,
        "help": help,
    })
}

/// `skill_new` → `{ "skill": { "name", "scope", "path" } }` for the just-created file.
pub fn skill_new_data(path: &std::path::Path, scope: SkillScope, name: &str) -> Value {
    json!({ "skill": { "name": name, "scope": scope, "path": path.display().to_string() } })
}

/// `attn list` → `{ "items": [AttnItem, ...] }`. Object-wrapped (see `todos_data`).
pub fn attn_list_data(items: &[crate::attention::AttnItem]) -> Value {
    json!({ "items": items })
}

/// `GET /api/actions` → the unified action-center list (#522): every actionable signal folded into
/// one ranked array (`crate::action_center::build_actions`), each carrying its editable
/// suggested prompt + default urgency. `count` is the total; the array is already urgent-first.
/// `cron_activity` (#557) is the observational feed of recent cron runs (ok + failed), its own
/// section in the Command Center — distinct from the actionable `failing-cron` items.
pub fn actions_data(
    actions: &[crate::action_center::ActionItem],
    cron_activity: &[crate::action_center::CronActivity],
) -> Value {
    json!({ "count": actions.len(), "actions": actions, "cron_activity": cron_activity })
}

/// `GET /api/agents/catalog` → the triggerable-agent catalog (#557): every agent/job the automation
/// can fire (declared crons + native jobs + the staff roster), each with the metadata a human needs
/// to decide whether to re-trigger it (`crate::action_center::build_catalog`). Object-wrapped with a
/// definitive `count`, so an empty registry is `count: 0`, never a missing field.
pub fn catalog_data(items: &[crate::action_center::CatalogItem]) -> Value {
    json!({ "count": items.len(), "agents": items })
}

/// `GET /api/completed` → the Completed tab's history (#561): every finished task bn tracks —
/// closed branch-note todos, achieved goals, resolved attention items, and landed/merged work —
/// folded most-recent-first (`crate::completed::build_completed`), each carrying its branch note's
/// local path + remote URL for the "open locally / remotely" jump.
pub fn completed_data(items: &[crate::completed::CompletedItem]) -> Value {
    json!({ "count": items.len(), "completed": items })
}

/// `GET /api/search?q=` → the global command-palette result set (#565): every bn source folded
/// into one ranked, typed, actionable list (`crate::palette::search`), plus the echoed `query` so
/// a slow response can be discarded if the box has moved on. `results` is already best-first.
pub fn palette_data(query: &str, results: &[crate::palette::SearchResult]) -> Value {
    json!({ "query": query, "count": results.len(), "results": results })
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

/// `review list` / `GET /api/reviews` → `{ "reviews": [Review, ...] }` (#349). Each record carries
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

/// `metrics` → the `Metrics` object (already a record).
pub fn metrics_data(m: &Metrics) -> Value {
    json!(m)
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

/// `focus start` → `{ "id", "ends_at", "duration_secs" }`.
pub fn focus_start_data(o: &crate::focus::StartOutcome) -> Value {
    json!({ "id": o.id, "ends_at": o.ends_at, "duration_secs": o.duration_secs })
}

/// `focus status` (and bare `bn focus`) → `{ "active": bool, "block": Block | null }`. The
/// statusline reads `active` + `block.remaining_secs` to render "focus · 47m left".
pub fn focus_status_data(block: Option<&crate::focus::Block>) -> Value {
    json!({ "active": block.is_some(), "block": block })
}

/// `focus end` → `{ "ended": Block | null, "drained": [Action, ...] }`. `ended` is null when no
/// block was active; `drained` is the non-urgent queue the block had been holding back.
pub fn focus_end_data(
    ended: Option<&crate::focus::Block>,
    drained: &[crate::next::Action],
    captures: &[crate::inbox::InboxItem],
) -> Value {
    json!({ "ended": ended, "drained": drained, "captures": captures })
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

/// `sync` → the `Proposals` record (since/commits/progress_line/done_candidates/close). Already
/// a serde record; object-wrapped so the envelope `data` is `{ "proposals": {...} }`.
pub fn sync_data(p: &crate::sync::Proposals) -> Value {
    json!({ "proposals": p })
}
