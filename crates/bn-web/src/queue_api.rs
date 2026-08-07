//! The `/api/queue/*` surface — the machine-wide work queue and its day timeline.
//!
//! Its own module rather than more lines in `api.rs`/`actions.rs`: those two are already the
//! largest files in the crate, and the queue's surface will keep growing as the epic lands.
//!
//! Reads fold `bn-core` directly; writes shell the CLI, so the browser and the terminal go through
//! one implementation of what a pause scope means instead of two.
use crate::actions::{parse_body, respond_process, run_bn_env, MaybeAddr};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::Json;
use bn_core::render::Envelope;
use serde::Deserialize;

/// `GET /api/queue` — the machine-wide queue: pauses in force, pending work by kind and lane, and
/// what each node offers. The read behind "why is nothing running".
pub async fn queue(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let now = bn_core::util::now_epoch_secs();
        bn_core::payload::queue_data(&cfg, now)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "pending": 0, "kinds": [], "nodes": [] }));
    Json(Envelope::success("queue", data))
}

/// `GET /api/queue/events` — the dispatcher's recent history: every start, stall, reclaim and
/// completion.
///
/// The page behind "what happened while I was away". `/api/queue` answers what is true *now*; a
/// queue's failures are mostly things that already happened and left no trace on the current state,
/// so the current state is exactly where you cannot see them.
///
/// Folds [`bn_core::queue::log`], the same code `bn queue log` renders, so the terminal and the
/// browser cannot tell different stories about the same dispatcher.
pub async fn queue_events(Query(q): Query<QueueEventsQuery>) -> Json<Envelope> {
    let data = tokio::task::spawn_blocking(move || {
        let now = bn_core::util::now_epoch_secs();
        let window = q
            .since
            .as_deref()
            .and_then(bn_core::focus::parse_duration)
            .unwrap_or(4 * 3600);
        // A `kind` that was asked for but is not one of ours matches nothing. Dropping it to
        // `None` would mean "no filter", so a typo in the query string would answer a question
        // nobody asked with the full log — a filter that quietly does not apply is worse than an
        // empty list, because an empty list at least looks wrong.
        let asked = q.kind.as_deref().filter(|k| !k.is_empty());
        let known = asked.filter(|k| bn_core::queue::log::is_kind(k));
        let rejected = asked.is_some() && known.is_none();
        let rows = if rejected {
            Vec::new()
        } else {
            let events = bn_core::events::read_events_at(&bn_core::events::log_path());
            bn_core::queue::log::rows(
                &events,
                &bn_core::queue::log::Query {
                    since: now.saturating_sub(window),
                    limit: q.limit.unwrap_or(200).clamp(1, 2000),
                    kind: known,
                    repo: q.repo.as_deref().filter(|r| !r.is_empty()),
                },
            )
        };
        let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
        let runs = bn_core::queue::log::runs(&rows, backlog.entries(), now);
        let nodes = runs
            .iter()
            .filter(|run| !run.node.is_empty())
            .map(|run| run.node.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        serde_json::json!({
            "now": now,
            "since": now.saturating_sub(window),
            "kinds": bn_core::queue::log::kind_names(),
            "nodes": nodes,
            // Named, so a caller that mistyped a filter is told rather than left reading an empty
            // list and concluding the queue was idle.
            "unknown_kind": rejected.then(|| asked.unwrap_or_default().to_string()),
            "events": rows,
            "runs": runs,
        })
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "events": [], "runs": [], "nodes": [], "kinds": [] }));
    Json(Envelope::success("queue-events", data))
}

/// Filters for [`queue_events`].
#[derive(Debug, Deserialize)]
pub struct QueueEventsQuery {
    /// How far back (`30m`, `4h`, `2d`). Default 4h.
    pub since: Option<String>,
    pub limit: Option<usize>,
    /// A short kind from [`bn_core::queue::log::KINDS`].
    pub kind: Option<String>,
    pub repo: Option<String>,
}

/// `GET /api/queue/timeline?date=YYYY-MM-DD` — the day as an axis.
///
/// Built from the same pure fold `bn queue timeline` renders, so the browser and the terminal
/// cannot tell different stories about the same day.
pub async fn queue_timeline(
    State(state): State<AppState>,
    Query(q): Query<QueueTimelineQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        let now = bn_core::util::now_epoch_secs();
        let date = q
            .date
            .filter(|d| bn_core::queue::timeline::parse_date(d).is_some())
            .unwrap_or_else(|| bn_core::util::local_date_of(now));
        bn_core::queue::timeline::build_live(&cfg, &date, now).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(timeline)) => match serde_json::to_value(timeline) {
            Ok(data) => Json(Envelope::success("queue-timeline", data)),
            Err(error) => Json(Envelope::failure(
                "queue-timeline",
                "serialize_failed",
                error.to_string(),
            )),
        },
        Ok(Err(error)) => Json(Envelope::failure(
            "queue-timeline",
            "invalid_catalog",
            error,
        )),
        Err(error) => Json(Envelope::failure(
            "queue-timeline",
            "task_failed",
            error.to_string(),
        )),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct QueueTimelineQuery {
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct QueuePauseBody {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

/// `POST /api/queue/pause {scope?, reason?, until?}` — stop the dispatcher starting new work.
///
/// Shells `bn queue pause` so the browser and the terminal write the same events through the same
/// validation, rather than the dashboard growing its own idea of what a scope string means.
/// `GET /api/queue/entries` — every backlog entry, filterable, not just what is waiting.
///
/// Separate from `/api/queue` on purpose. That payload rides the SSE frame to every connected
/// browser on every change, so it carries only what the live view needs — the pending set. Browsing
/// history is a different question asked far less often, and answering it in the frame would put a
/// growing list on the wire every time a single job started.
///
/// Folded by `backlog::load`, so an entry appears once in its current state rather than once per
/// line ever written about it.
pub async fn queue_entries(Query(q): Query<QueueEntriesQuery>) -> Json<Envelope> {
    let data = tokio::task::spawn_blocking(move || {
        let bl = bn_core::backlog::load(&bn_core::backlog::backlog_path());
        let want = q.status.as_deref().filter(|s| !s.is_empty() && *s != "all");
        let repo = q.repo.as_deref().filter(|r| !r.is_empty());
        let kind = q.kind.as_deref().filter(|k| !k.is_empty());
        let mut rows: Vec<_> = bl
            .entries()
            .iter()
            .filter(|e| want.is_none_or(|s| e.status.label() == s))
            .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
            .filter(|e| kind.is_none_or(|k| e.kind.label() == k))
            .cloned()
            .collect();
        // Most recently touched first: for anything terminal that is the order you care about, and
        // for pending it still surfaces what just arrived. Priority order is the *dispatch* view,
        // and the live list already shows that.
        rows.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        let total = rows.len();
        rows.truncate(q.limit.unwrap_or(200).clamp(1, 2000));
        // Counts over the *unfiltered* set, so the chips can show what is behind each one rather
        // than only what the current filter already selected.
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for e in bl.entries() {
            *counts.entry(e.status.label().to_string()).or_default() += 1;
        }
        serde_json::json!({
            "entries": rows,
            "total": total,
            "counts": counts,
            "unreadable": bl.unreadable,
        })
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "entries": [], "total": 0, "counts": {} }));
    Json(Envelope::success("queue-entries", data))
}

/// Filters for [`queue_entries`].
#[derive(Debug, Default, Deserialize)]
pub struct QueueEntriesQuery {
    /// A `BacklogStatus` label, or `all`.
    pub status: Option<String>,
    pub repo: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
}

/// `POST /api/queue/add` — enqueue work from the dashboard.
///
/// The dashboard could pause the whole machine but could not queue a single item, which left the
/// browser able to stop work and not start it. Every producer routes through the queue now; this is
/// what lets the UI be one.
///
/// Priority is the point of the form rather than an advanced option: "run this next" is the request
/// people actually have, and without it the only way to jump the order was to re-add the item with
/// a higher number and know that a re-add raises.
pub async fn queue_add(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-add";
    let body: QueueAddBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let mut args = vec!["queue".to_string(), "add".to_string(), body.item];
    if let Some(repo) = body.repo.filter(|r| !r.trim().is_empty()) {
        args.extend(["--repo".to_string(), repo]);
    }
    args.extend([
        "--kind".to_string(),
        body.kind.unwrap_or_else(|| "ship".to_string()),
    ]);
    if let Some(p) = body.priority {
        args.extend(["--priority".to_string(), p.to_string()]);
    }
    if let Some(job) = body.job.filter(|j| !j.trim().is_empty()) {
        args.extend(["--job".to_string(), job]);
    }
    if let Some(skill) = body.skill.filter(|s| !s.trim().is_empty()) {
        args.extend(["--skill".to_string(), skill]);
    }
    let result = run_bn_env(args, None, Vec::new()).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// `POST /api/queue/cancel` — take pending work back out.
pub async fn queue_cancel(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-cancel";
    let body: QueueEntryBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let args = vec![
        "queue".to_string(),
        "cancel".to_string(),
        body.id,
        "--repo".to_string(),
        body.repo,
    ];
    let result = run_bn_env(args, None, Vec::new()).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// `POST /api/queue/priority` — move queued work up or down the order.
pub async fn queue_priority(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-priority";
    let body: QueuePriorityBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let args = vec![
        "queue".to_string(),
        "priority".to_string(),
        body.id,
        body.priority.to_string(),
        "--repo".to_string(),
        body.repo,
    ];
    let result = run_bn_env(args, None, Vec::new()).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// Body for [`queue_add`].
#[derive(Debug, Default, Deserialize)]
pub struct QueueAddBody {
    pub item: String,
    pub repo: Option<String>,
    pub kind: Option<String>,
    pub priority: Option<i64>,
    pub job: Option<String>,
    pub skill: Option<String>,
}

/// Body for [`queue_cancel`] — one entry, named the way `bn queue list` names it.
#[derive(Debug, Default, Deserialize)]
pub struct QueueEntryBody {
    pub repo: String,
    pub id: String,
}

/// Body for [`queue_priority`].
#[derive(Debug, Default, Deserialize)]
pub struct QueuePriorityBody {
    pub repo: String,
    pub id: String,
    pub priority: i64,
}

pub async fn queue_pause(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-pause";
    let body: QueuePauseBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let mut args = vec![
        "queue".to_string(),
        "pause".to_string(),
        "--scope".to_string(),
        body.scope.unwrap_or_else(|| "all".to_string()),
    ];
    if let Some(reason) = body.reason.filter(|r| !r.trim().is_empty()) {
        args.extend(["--reason".to_string(), reason]);
    }
    if let Some(until) = body.until.filter(|u| !u.trim().is_empty()) {
        args.extend(["--until".to_string(), until]);
    }
    let result = run_bn_env(args, None, Vec::new()).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// `POST /api/queue/resume {scope?}` — lift a pause.
pub async fn queue_resume(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-resume";
    let body: QueuePauseBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let args = vec![
        "queue".to_string(),
        "resume".to_string(),
        "--scope".to_string(),
        body.scope.unwrap_or_else(|| "all".to_string()),
    ];
    let result = run_bn_env(args, None, Vec::new()).await;
    respond_process(&state, cmd, "queue", addr, result)
}
