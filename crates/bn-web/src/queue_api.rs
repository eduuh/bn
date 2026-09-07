//! The `/api/queue/*` surface — the machine-wide work queue and its day timeline.
//!
//! Its own module rather than more lines in `api.rs`/`actions.rs`: those two are already the
//! largest files in the crate, and the queue's surface will keep growing as the epic lands.
//!
//! Reads fold `bn-core` directly; writes shell the CLI, so the browser and terminal go through one
//! implementation of each queue mutation.
use crate::actions::{parse_body, respond_process, run_bn_env, MaybeAddr};
use crate::state::AppState;
use crate::view_cache::Bounded;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::Json;
use bn_core::render::Envelope;
use serde::Deserialize;
use std::sync::Arc;

/// How many past days of timeline stay cacheable.
///
/// The cache key space has to be finite, and `?date=` is a free string. Rather than cache every
/// date anyone ever asks for — which is how a cache with no eviction becomes a leak — only the
/// window people actually browse is cached; older dates are computed and served uncached. A week
/// covers "what happened yesterday / on Monday" and caps this view at 8 × 10 keys.
const CACHEABLE_DAYS: i64 = 7;

/// Whether `class` is a real queue class rather than an arbitrary string.
///
/// Validated against `bn-core`'s own label sets, not a copy of them, so a class added to the enum
/// becomes cacheable here without anyone remembering to update a list.
fn known_class(class: &str) -> bool {
    use bn_core::queue::classify::{QueueMode, QueueSubtype};
    QueueMode::ALL.iter().any(|m| m.label() == class)
        || QueueSubtype::ALL.iter().any(|s| s.label() == class)
}

/// Whether `status` is a real backlog status.
fn known_status(status: &str) -> bool {
    bn_core::backlog::BacklogStatus::ALL
        .iter()
        .any(|s| s.label() == status)
}

/// The cache key for a timeline request, or `None` when the request falls outside the allowlist and
/// must be served uncached.
///
/// `date` is already normalized to the resolved day by the caller; `class` is the raw query value,
/// where empty means "no filter".
fn timeline_key(date: &str, class: &str, today: &str) -> Option<Bounded> {
    if !class.is_empty() && !known_class(class) {
        return None;
    }
    if date != today && !within_recent_days(date, today, CACHEABLE_DAYS) {
        return None;
    }
    Some(Bounded::new(&[date, class]))
}

/// Whether `date` is within `days` before `today`. Both are `YYYY-MM-DD`; anything unparseable is
/// outside the window by definition.
fn within_recent_days(date: &str, today: &str, days: i64) -> bool {
    let (Some(d), Some(t)) = (
        bn_core::queue::timeline::parse_date(date),
        bn_core::queue::timeline::parse_date(today),
    ) else {
        return false;
    };
    let delta = t.signed_duration_since(d).num_days();
    (0..=days).contains(&delta)
}

/// The cache key for a backlog listing, or `None` for the uncacheable shapes.
///
/// Three parameters are refused rather than keyed. `repo` is free text — the one value here a
/// caller could use to mint unlimited keys. `limit` is clamped to 1..=2000 downstream, so keying on
/// it would multiply the space by two thousand for a parameter the dashboard never sends. `kind` is
/// likewise unused by the UI. Each is still *served*, just computed fresh.
///
/// `all` normalizes to the empty string because the handler already treats them identically; left
/// alone they would be two keys for one answer.
fn entries_key(
    status: &str,
    repo: &str,
    kind: &str,
    class: &str,
    limit: Option<usize>,
) -> Option<Bounded> {
    if !repo.is_empty() || !kind.is_empty() || limit.is_some() {
        return None;
    }
    let status = if status == "all" { "" } else { status };
    let class = if class == "all" { "" } else { class };
    if !status.is_empty() && !known_status(status) {
        return None;
    }
    if !class.is_empty() && !known_class(class) {
        return None;
    }
    Some(Bounded::new(&[status, class]))
}

/// Drop every cached view a backlog write can change.
///
/// One helper rather than five copies of the same two lines: "which views does this write touch"
/// is exactly the question that gets answered inconsistently once it is asked at five call sites,
/// and the failure mode — a cancelled entry that keeps rendering — looks like the write failing.
async fn invalidate_queue_views(state: &AppState) {
    state
        .views
        .invalidate(crate::view_cache::QUEUE_ENTRIES)
        .await;
    state
        .views
        .invalidate(crate::view_cache::QUEUE_TIMELINE)
        .await;
}

/// `GET /api/queue` — the machine-wide queue: pending work by kind and lane, and what each node
/// offers. The read behind "why is nothing running".
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
pub async fn queue_events(
    State(state): State<AppState>,
    Query(q): Query<QueueEventsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
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
            .into_iter()
            .filter(|row| {
                row.repo.is_empty()
                    || row.repo.eq_ignore_ascii_case("machine")
                    || cfg.repo_allowed_here(&row.repo)
            })
            .collect()
        };
        let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
        let entries: Vec<_> = backlog
            .entries()
            .iter()
            .filter(|entry| cfg.repo_allowed_here(&entry.repo))
            .cloned()
            .collect();
        let runs = bn_core::queue::log::runs(&rows, &entries, now);
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
    let now = bn_core::util::now_epoch_secs();
    let today = bn_core::util::local_date_of(now);
    let date = q
        .date
        .filter(|d| bn_core::queue::timeline::parse_date(d).is_some())
        .unwrap_or_else(|| today.clone());
    let class = q.class.unwrap_or_default();

    let cfg = state.cfg.clone();
    let (key_date, key_class) = (date.clone(), class.clone());
    let build = move || {
        let outcome = bn_core::queue::timeline::build_live(
            &cfg,
            &key_date,
            now,
            Some(key_class.as_str()).filter(|c| !c.is_empty()),
        )
        .map_err(|error| error.to_string())
        .and_then(|t| serde_json::to_value(t).map_err(|e| e.to_string()));
        match outcome {
            Ok(data) => serde_json::json!({ "ok": data }),
            Err(error) => serde_json::json!({ "err": error }),
        }
    };

    // Outside the allowlist (an old date, an unknown class) the answer is still correct — it is
    // just computed every time rather than becoming a permanent cache entry.
    let view = match timeline_key(&date, &class, &today) {
        Some(key) => {
            state
                .views
                .get_or_compute_with(crate::view_cache::QUEUE_TIMELINE, &key, build)
                .await
        }
        None => tokio::task::spawn_blocking(build).await.ok().map(Arc::new),
    };

    let cmd = "queue-timeline";
    let Some(view) = view.as_deref() else {
        return Json(Envelope::failure(
            cmd,
            "task_failed",
            "timeline build failed",
        ));
    };
    match (view.get("ok"), view.get("err")) {
        (Some(data), _) => Json(Envelope::success(cmd, data.clone())),
        (None, Some(err)) => Json(Envelope::failure(
            cmd,
            "invalid_catalog",
            err.as_str().unwrap_or("invalid timeline").to_string(),
        )),
        (None, None) => Json(Envelope::failure(
            cmd,
            "task_failed",
            "timeline build failed",
        )),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct QueueTimelineQuery {
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default, rename = "class")]
    pub class: Option<String>,
}

/// `GET /api/queue/entries` — every backlog entry, filterable, not just what is waiting.
///
/// Separate from `/api/queue` on purpose. That payload rides the SSE frame to every connected
/// browser on every change, so it carries only what the live view needs — the pending set. Browsing
/// history is a different question asked far less often, and answering it in the frame would put a
/// growing list on the wire every time a single job started.
///
/// Folded by `backlog::load`, so an entry appears once in its current state rather than once per
/// line ever written about it.
pub async fn queue_entries(
    State(state): State<AppState>,
    Query(q): Query<QueueEntriesQuery>,
) -> Json<Envelope> {
    let key = entries_key(
        q.status.as_deref().unwrap_or_default(),
        q.repo.as_deref().unwrap_or_default(),
        q.kind.as_deref().unwrap_or_default(),
        q.class.as_deref().unwrap_or_default(),
        q.limit,
    );

    let cfg = state.cfg.clone();
    let build = move || {
        let bl = bn_core::backlog::load(&bn_core::backlog::backlog_path());
        let want = q.status.as_deref().filter(|s| !s.is_empty() && *s != "all");
        let repo = q.repo.as_deref().filter(|r| !r.is_empty());
        let kind = q.kind.as_deref().filter(|k| !k.is_empty());
        let class = q.class.as_deref().filter(|c| !c.is_empty() && *c != "all");
        let mut rows: Vec<_> = bl
            .entries()
            .iter()
            .filter(|e| cfg.repo_allowed_here(&e.repo))
            .filter(|e| want.is_none_or(|s| e.status.label() == s))
            .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
            .filter(|e| kind.is_none_or(|k| e.kind.label() == k))
            .filter(|e| class.is_none_or(|c| queue_class_matches(e, c)))
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
        for e in bl
            .entries()
            .iter()
            .filter(|e| cfg.repo_allowed_here(&e.repo))
        {
            *counts.entry(e.status.label().to_string()).or_default() += 1;
        }
        serde_json::json!({
            "entries": rows,
            "total": total,
            "counts": counts,
            "unreadable": bl.unreadable,
        })
    };

    let view = match key {
        Some(key) => {
            state
                .views
                .get_or_compute_with(crate::view_cache::QUEUE_ENTRIES, &key, build)
                .await
        }
        None => tokio::task::spawn_blocking(build).await.ok().map(Arc::new),
    };
    let data = view
        .map(|d| (*d).clone())
        .unwrap_or_else(|| serde_json::json!({ "entries": [], "total": 0, "counts": {} }));
    Json(Envelope::success("queue-entries", data))
}

/// Filters for [`queue_entries`].
#[derive(Debug, Default, Deserialize)]
pub struct QueueEntriesQuery {
    /// A `BacklogStatus` label, or `all`.
    pub status: Option<String>,
    pub repo: Option<String>,
    pub kind: Option<String>,
    pub class: Option<String>,
    pub limit: Option<usize>,
}

fn queue_class_matches(entry: &bn_core::backlog::BacklogEntry, want: &str) -> bool {
    let class = bn_core::queue::classify::classify(entry);
    class.mode.label() == want || class.subtype.label() == want
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
    invalidate_queue_views(&state).await;
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
    invalidate_queue_views(&state).await;
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
    invalidate_queue_views(&state).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// `POST /api/queue/snooze` — defer one pending task without pausing the queue.
pub async fn queue_snooze(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-snooze";
    let body: QueueSnoozeBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let args = vec![
        "queue".to_string(),
        "snooze".to_string(),
        body.id,
        "--for".to_string(),
        body.for_duration,
        "--repo".to_string(),
        body.repo,
    ];
    let result = run_bn_env(args, None, Vec::new()).await;
    invalidate_queue_views(&state).await;
    respond_process(&state, cmd, "queue", addr, result)
}

/// `POST /api/queue/class` — override one pending task's derived class.
pub async fn queue_class(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "queue-class";
    let body: QueueClassBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let args = vec![
        "queue".to_string(),
        "class".to_string(),
        body.id,
        body.class,
        "--repo".to_string(),
        body.repo,
    ];
    let result = run_bn_env(args, None, Vec::new()).await;
    invalidate_queue_views(&state).await;
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

#[derive(Debug, Default, Deserialize)]
pub struct QueueSnoozeBody {
    pub repo: String,
    pub id: String,
    #[serde(rename = "for")]
    pub for_duration: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct QueueClassBody {
    pub repo: String,
    pub id: String,
    pub class: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use bn_core::resolver::{Config, MachineProfile};

    // The cache never evicts, so "the key space is finite" is not a nicety — it is the property
    // that stops a query string from growing the process without bound. These tests are where that
    // claim is actually checked.

    #[test]
    fn a_timeline_key_covers_today_and_the_recent_week() {
        let today = "2026-08-09";
        assert!(timeline_key(today, "", today).is_some());
        assert!(
            timeline_key("2026-08-02", "", today).is_some(),
            "7 days back"
        );
        assert!(timeline_key(today, "cron", today).is_some());
    }

    #[test]
    fn a_timeline_key_refuses_everything_outside_the_window() {
        let today = "2026-08-09";
        // An old date, a future date, and an unparseable one are all served — just not cached.
        assert!(
            timeline_key("2026-08-01", "", today).is_none(),
            "8 days back"
        );
        assert!(timeline_key("1998-01-01", "", today).is_none());
        assert!(timeline_key("2026-08-10", "", today).is_none(), "tomorrow");
        assert!(timeline_key("not-a-date", "", today).is_none());
        // An unknown class is the other way in, and is refused the same way.
        assert!(timeline_key(today, "'; DROP TABLE", today).is_none());
        assert!(timeline_key(today, "nonsense", today).is_none());
    }

    #[test]
    fn a_thousand_hostile_dates_mint_no_keys() {
        // The shape of the attack the bound exists for: many distinct values, one per request.
        let today = "2026-08-09";
        let minted = (0..1000)
            .filter(|i| timeline_key(&format!("attacker-{i}"), "", today).is_some())
            .count();
        assert_eq!(minted, 0);
    }

    #[test]
    fn an_entries_key_covers_the_filters_the_dashboard_actually_sends() {
        assert!(
            entries_key("", "", "", "", None).is_some(),
            "the default view"
        );
        assert!(entries_key("pending", "", "", "", None).is_some());
        assert!(entries_key("done", "", "", "cron", None).is_some());
    }

    #[test]
    fn an_entries_key_refuses_the_unbounded_parameters() {
        // A repo filter is free text, and `limit` spans 1..=2000 — both would blow the key space.
        assert!(entries_key("", "bn", "", "", None).is_none());
        assert!(entries_key("", "", "", "", Some(50)).is_none());
        assert!(entries_key("", "", "agent", "", None).is_none());
        assert!(entries_key("not-a-status", "", "", "", None).is_none());
        assert!(entries_key("", "", "", "not-a-class", None).is_none());
    }

    #[test]
    fn all_and_empty_are_one_key_not_two() {
        // The handler treats them identically, so keying them apart would cache one answer twice.
        assert_eq!(
            entries_key("all", "", "", "all", None),
            entries_key("", "", "", "", None)
        );
    }

    #[test]
    fn the_known_sets_track_bn_core_rather_than_a_local_copy() {
        // Every label bn-core defines is accepted; that is what keeps this from drifting when a
        // variant is added to either enum.
        for mode in bn_core::queue::classify::QueueMode::ALL {
            assert!(known_class(mode.label()), "mode {}", mode.label());
        }
        for sub in bn_core::queue::classify::QueueSubtype::ALL {
            assert!(known_class(sub.label()), "subtype {}", sub.label());
        }
        for status in bn_core::backlog::BacklogStatus::ALL {
            assert!(known_status(status.label()), "status {}", status.label());
        }
        assert!(!known_class(""));
        assert!(!known_status(""));
    }

    #[tokio::test]
    async fn queue_endpoint_reports_not_enabled_without_work_rows() {
        let cfg = Config {
            hostname: "web-only".to_string(),
            machine_profile: Some(MachineProfile {
                repos: vec!["acme/api".to_string()],
                queue: false,
                cron: false,
                web: true,
            }),
            ..Config::default()
        };
        let state = AppState::with_live_addr(cfg, "127.0.0.1".to_string(), 7770);

        let Json(envelope) = queue(State(state)).await;

        assert!(envelope.ok);
        assert_eq!(envelope.data["state"], "not_enabled");
        assert_eq!(envelope.data["accepting"], false);
        assert_eq!(envelope.data["entries"], serde_json::json!([]));
        assert_eq!(envelope.data["active"], serde_json::json!([]));
        assert_eq!(envelope.data["nodes"], serde_json::json!([]));
    }
}
