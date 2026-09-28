//! Read-only views of the coordination DB for the dashboard: queue rows with their history, and
//! watchdogs with their evaluations. Writes go through the CLI, whose rules they need.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use bn_core::coord::watchdog::parse_wdid;
use bn_core::coord::{self, CoordDb, ListFilter, QState};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct QueueQuery {
    /// Rows for this seat (`repo:branch` or `human:<name>`); every seat when absent.
    pub seat: Option<String>,
    /// Comma-separated states, or `any`; open states when absent.
    pub state: Option<String>,
    pub limit: Option<usize>,
}

fn fail(cmd: &str, status: StatusCode, code: &str, msg: String) -> Response {
    (status, Json(Envelope::failure(cmd, code, msg))).into_response()
}

fn db_error(cmd: &str, e: impl std::fmt::Display) -> Response {
    fail(
        cmd,
        StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        e.to_string(),
    )
}

/// `GET /api/queue?seat=&state=&limit=` — queue rows, oldest first.
pub async fn queue(Query(q): Query<QueueQuery>) -> Response {
    let cmd = "queue";
    let db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => return db_error(cmd, e),
    };
    let seat = match q.seat.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match coord::normalize_seat(s) {
            Ok(s) => Some(s),
            Err(e) => return fail(cmd, StatusCode::BAD_REQUEST, "invalid_seat", e.to_string()),
        },
        None => None,
    };
    let states = match q.state.as_deref().map(str::trim) {
        None | Some("") => vec![],
        Some("any" | "all") => QState::ALL.to_vec(),
        Some(list) => {
            let parsed: Option<Vec<QState>> = list.split(',').map(QState::parse).collect();
            match parsed {
                Some(s) => s,
                None => {
                    return fail(
                        cmd,
                        StatusCode::BAD_REQUEST,
                        "invalid_state",
                        list.to_string(),
                    )
                }
            }
        }
    };
    let filter = ListFilter {
        seat,
        states,
        overdue_at: None,
        limit: q.limit,
    };
    match db.list(&filter) {
        Ok(rows) => {
            let now = bn_core::util::now_epoch_secs();
            let overdue: Vec<&str> = rows
                .iter()
                .filter(|r| r.is_overdue(now))
                .map(|r| r.id.as_str())
                .collect();
            Json(Envelope::success(
                cmd,
                json!({"count": rows.len(), "rows": rows, "overdue": overdue}),
            ))
            .into_response()
        }
        Err(e) => fail(
            cmd,
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            e.to_string(),
        ),
    }
}

/// `GET /api/queue/:id` — one row and its append-only history.
pub async fn queue_row(Path(id): Path<String>) -> Response {
    let cmd = "queue-row";
    let Some(n) = coord::parse_qid(&id) else {
        return fail(cmd, StatusCode::BAD_REQUEST, "invalid_id", id);
    };
    let db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => return db_error(cmd, e),
    };
    match (db.get(n), db.transitions(n)) {
        (Ok(row), Ok(history)) => Json(Envelope::success(
            cmd,
            json!({"row": row, "transitions": history}),
        ))
        .into_response(),
        (Err(e), _) | (_, Err(e)) => fail(cmd, StatusCode::NOT_FOUND, "not_found", e.to_string()),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct WatchdogQuery {
    /// Include stopped, fired and expired ones.
    pub all: Option<bool>,
    pub limit: Option<usize>,
}

/// `GET /api/watchdogs?all=` — watchdogs, newest first.
pub async fn watchdogs(Query(q): Query<WatchdogQuery>) -> Response {
    let cmd = "watchdogs";
    let db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => return db_error(cmd, e),
    };
    match db.watchdogs(q.all.unwrap_or(false), None, q.limit.unwrap_or(100)) {
        Ok(rows) => Json(Envelope::success(
            cmd,
            json!({"count": rows.len(), "watchdogs": rows}),
        ))
        .into_response(),
        Err(e) => fail(
            cmd,
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            e.to_string(),
        ),
    }
}

/// `GET /api/watchdogs/:id/evals` — one watchdog and its recent evaluations.
pub async fn watchdog_evals(Path(id): Path<String>) -> Response {
    let cmd = "watchdog-evals";
    let Some(n) = parse_wdid(&id) else {
        return fail(cmd, StatusCode::BAD_REQUEST, "invalid_id", id);
    };
    let db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => return db_error(cmd, e),
    };
    match (db.watchdog(n), db.watchdog_evals(n, 50)) {
        (Ok(wd), Ok(evals)) => Json(Envelope::success(
            cmd,
            json!({"watchdog": wd, "evals": evals}),
        ))
        .into_response(),
        (Err(e), _) | (_, Err(e)) => fail(cmd, StatusCode::NOT_FOUND, "not_found", e.to_string()),
    }
}
