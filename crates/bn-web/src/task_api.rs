//! The task-list read endpoint, split from `api.rs` to keep the read surface under its size ratchet.

use crate::api::{bad_request, nonempty};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::{json, Value};

/// Query params for `GET /api/tasks`.
#[derive(Debug, Deserialize, Default)]
pub struct TasksQuery {
    /// `today` (the default view) or a `YYYY-MM-DD` date.
    pub day: Option<String>,
    pub repo: Option<String>,
    /// Filter on the SMART verdict: `ok`, `fail`, `stuck`, or `unjudged`.
    pub smart: Option<String>,
    pub stage: Option<String>,
    /// `1` to include the whole backlog rather than just what's actionable now.
    pub all: Option<String>,
    /// `open` (the default), `done` (the finished record for a day), or `all` (the complete
    /// paginated task ledger).
    pub status: Option<String>,
    /// Lifecycle filter within `status=all`: open, wip, snoozed, done, or cancelled.
    pub state: Option<String>,
    /// Case-insensitive all-terms search over the task's complete durable line.
    pub q: Option<String>,
    /// Bounded ledger page size. Defaults to 50 and is capped at 200.
    pub limit: Option<usize>,
    /// Zero-based ledger page offset.
    pub offset: Option<usize>,
}

/// `GET /api/tasks` — the task store (#996), filtered.
///
/// The normal path is the open actionable view, `status=done` is one day's immutable completion
/// record, and `status=all` is the bounded all-time ledger.
pub async fn tasks(State(state): State<AppState>, Query(q): Query<TasksQuery>) -> Response {
    let cmd = "tasks";
    let today = bn_core::util::today();
    let all = q.all.as_deref().is_some_and(|v| v == "1" || v == "true");
    let repo = nonempty(q.repo).map(|r| r.to_ascii_lowercase());
    let smart = nonempty(q.smart).map(|s| s.to_ascii_lowercase());
    let stage = nonempty(q.stage).map(|s| s.to_ascii_lowercase());
    let day = nonempty(q.day);
    let status = nonempty(q.status).map(|s| s.to_ascii_lowercase());
    let state_filter = nonempty(q.state).map(|s| s.to_ascii_lowercase());
    let search = nonempty(q.q);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or_default();

    if let Some(d) = day.as_deref() {
        if d != "today" && bn_core::util::epoch_day(d).is_none() {
            return bad_request(cmd, "invalid_day", "day must be `today` or YYYY-MM-DD");
        }
    }
    if let Some(s) = status.as_deref() {
        if s != "open" && s != "done" && s != "all" {
            return bad_request(
                cmd,
                "invalid_status",
                "status must be `open`, `done`, or `all`",
            );
        }
    }
    if let Some(s) = state_filter.as_deref() {
        if !matches!(s, "open" | "wip" | "snoozed" | "done" | "cancelled") {
            return bad_request(
                cmd,
                "invalid_state",
                "state must be open, wip, snoozed, done, or cancelled",
            );
        }
    }
    let day_filter = day.as_deref().map(|d| {
        if d == "today" {
            today.clone()
        } else {
            d.to_string()
        }
    });

    let file = match bn_core::task_store::task_store(&state.cfg).load() {
        Ok(file) => file,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    cmd,
                    "store_unreadable",
                    format!("could not read the task store: {error}"),
                )),
            )
                .into_response()
        }
    };

    if status.as_deref() == Some("all") {
        return Json(Envelope::success(
            cmd,
            bn_core::payload::task_ledger_data(
                &file,
                &today,
                bn_core::payload::tasks::TaskLedgerFilter {
                    query: search.as_deref(),
                    repo: repo.as_deref(),
                    smart: smart.as_deref(),
                    stage: stage.as_deref(),
                    day: day_filter.as_deref(),
                    state: state_filter.as_deref(),
                    offset,
                    limit,
                },
            ),
        ))
        .into_response();
    }

    if status.as_deref() == Some("done") {
        let want_day = day_filter.unwrap_or_else(|| today.clone());
        let mut done = 0usize;
        let mut cancelled = 0usize;
        let mut rows: Vec<Value> = Vec::new();

        for task in file.tasks() {
            if task.completion.as_deref() != Some(want_day.as_str()) {
                continue;
            }
            if repo.as_deref().is_some_and(|want| {
                !task
                    .meta("repo")
                    .is_some_and(|value| value.eq_ignore_ascii_case(want))
            }) {
                continue;
            }
            match task.status(&today) {
                bn_core::todo::Status::Cancelled => cancelled += 1,
                bn_core::todo::Status::Done => done += 1,
                _ => continue,
            }
            let mut row = task.to_json(&today);
            row["reason"] = json!(task.meta("reason"));
            rows.push(row);
        }

        return Json(Envelope::success(
            cmd,
            json!({
                "today": today,
                "day": want_day,
                "repo": repo,
                "status": "done",
                "count": rows.len(),
                "done": done,
                "cancelled": cancelled,
                "tasks": rows,
            }),
        ))
        .into_response();
    }

    let rows: Vec<Value> = file
        .open_sorted(&today, None)
        .into_iter()
        // The board answers "what could I do today", so machine bookkeeping is folded out the way
        // the CLI's day view already folds it (#1151, `task_view::select_tasks`). Without this the
        // dashboard shows every `run wt-cleanup` and reclaim chore as work waiting on you — 99 rows
        // of it against 20 real tasks — and each one sits in the enhance queue that will never
        // touch it, because the gate does not judge machine work. `?status=all` is the ledger and
        // still shows everything.
        .filter(|task| !bn_core::task_owner::is_machine(task))
        .filter(|task| {
            repo.as_deref().is_none_or(|want| {
                task.meta("repo")
                    .is_some_and(|value| value.eq_ignore_ascii_case(want))
            })
        })
        .filter(|task| {
            stage.as_deref().is_none_or(|want| {
                task.meta("stage")
                    .is_some_and(|value| value.eq_ignore_ascii_case(want))
            })
        })
        .filter(|task| match smart.as_deref() {
            None => true,
            Some("unjudged") => task.meta("smart").is_none(),
            Some(want) => task
                .meta("smart")
                .is_some_and(|value| value.eq_ignore_ascii_case(want)),
        })
        .filter(|task| match (&day_filter, all) {
            (Some(day), _) => task.meta("day") == Some(day.as_str()),
            (None, true) => true,
            (None, false) => match task
                .meta("day")
                .filter(|day| bn_core::util::epoch_day(day).is_some())
            {
                Some(day) => day <= today.as_str(),
                None => true,
            },
        })
        .map(|task| task.to_json(&today))
        .collect();

    Json(Envelope::success(
        cmd,
        json!({
            "today": today,
            "all": all,
            "day": day_filter,
            "repo": repo,
            "smart": smart,
            "stage": stage,
            "count": rows.len(),
            "tasks": rows,
        }),
    ))
    .into_response()
}
