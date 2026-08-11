//! The staff surface: the roster health board, the briefing-card feed, the read mark, and the
//! schedule write.
//!
//! Split out of `api.rs` under the file-size ratchet. They belong together because they share one
//! backing fold — `bn_core::staff`, the same view `bn staff board --json` prints, so the dashboard
//! and the CLI cannot drift — and one cost shape: directory scans, all on the blocking pool.

use super::{bad_request, nonempty, not_found};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;

// ---------------------------------------------------------------------------
// Staff (#269/#273) — the health board, the briefing-card feed, and the read-mark write. All
// three delegate to the shared `bn_core::staff` views (the same fold `bn staff board --json`
// prints), so the dashboard and the CLI cannot drift. Directory-scan reads on a blocking thread.

/// `GET /api/staff/board` — the roster health board: per-agent schedule, last-run taxonomy word,
/// missed-run flag, unread-card mark, and the backpressure state (`unread_streak`/`paused`) that
/// explains a quiet agent.
pub async fn staff_board(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || bn_core::staff::board_view(&cfg, None)).await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("staff-board", data)),
        Ok(Err(error)) => Json(Envelope::failure("staff-board", "invalid-catalog", error)),
        Err(join) => Json(Envelope::failure(
            "staff-board",
            "internal",
            join.to_string(),
        )),
    }
}

/// `GET /api/staff/feed` — the briefing-card index, unread-first then newest-first.
pub async fn staff_feed(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || bn_core::staff::feed_view(&cfg))
        .await
        .unwrap_or(serde_json::Value::Null);
    Json(Envelope::success("staff-feed", data))
}

#[derive(serde::Deserialize)]
pub struct StaffCardQuery {
    pub date: String,
    pub agent: String,
}

/// The same two fields, but optional, so a missing one is *this handler's* error to report.
///
/// A required field in a `Query<T>` is rejected by the extractor before the handler runs, and
/// axum's default rejection is `text/plain` — which broke the one promise the whole API makes, that
/// every response is a `{ok, command, data, error}` envelope. A client calling
/// `/api/staff/card` with no params got `Failed to deserialize query string: missing field
/// \`date\`` as bare text, so `r.json()` threw a parse error instead of yielding a structured
/// failure it could show.
#[derive(serde::Deserialize)]
pub struct StaffCardParams {
    pub date: Option<String>,
    pub agent: Option<String>,
}

/// `GET /api/staff/card?date=&agent=` — one card rendered to Markdown.
///
/// Both params are required; a missing one is answered with a 400 *envelope* rather than the
/// extractor's plain-text rejection. See [`StaffCardParams`].
pub async fn staff_card(
    State(state): State<AppState>,
    Query(q): Query<StaffCardParams>,
) -> Response {
    let (Some(date), Some(agent)) = (nonempty(q.date), nonempty(q.agent)) else {
        return bad_request(
            "staff-card",
            "missing_params",
            "date and agent are both required, e.g. /api/staff/card?date=2026-08-11&agent=chief-pm",
        );
    };
    let cfg = state.cfg.clone();
    let result =
        tokio::task::spawn_blocking(move || bn_core::staff::card_view(&cfg, &date, &agent)).await;
    match result {
        Ok(Some(data)) => Json(Envelope::success("staff-card", data)).into_response(),
        Ok(None) => not_found("staff-card", "no such card"),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                "staff-card",
                "internal",
                join.to_string(),
            )),
        )
            .into_response(),
    }
}

/// `POST /api/staff/card/read` — mark one card read. This is the lever that resumes a
/// backpressure-paused agent (#269): reading a card ends the unread streak, so the next
/// scheduled run fires again.
pub async fn staff_card_read(
    State(_state): State<AppState>,
    Json(q): Json<StaffCardQuery>,
) -> Json<Envelope> {
    match bn_core::staff::mark_read(&q.date, &q.agent) {
        Ok(newly) => Json(Envelope::success(
            "staff-card-read",
            serde_json::json!({ "marked": newly, "date": q.date, "agent": q.agent }),
        )),
        Err(error) => Json(Envelope::failure(
            "staff-card-read",
            "io",
            error.to_string(),
        )),
    }
}

#[derive(serde::Deserialize)]
pub struct StaffScheduleBody {
    pub agent: String,
    /// The systemd `OnCalendar` spec to set; `null`/absent removes the schedule (the agent
    /// becomes trigger/manual-only).
    #[serde(default)]
    pub on_calendar: Option<String>,
}

/// `POST /api/staff/schedule` — set or clear one agent's `on_calendar:` in its source manifest.
/// Validated against the shared OnCalendar parser before writing; the agent-scheduler picks the
/// change up on its next tick.
pub async fn staff_schedule(
    State(state): State<AppState>,
    Json(body): Json<StaffScheduleBody>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let expr = body
        .on_calendar
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let agent = body.agent.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::agents::set_schedule(&cfg, None, &agent, expr.as_deref())
    })
    .await;
    match result {
        Ok(Ok(path)) => Json(Envelope::success(
            "staff-schedule",
            serde_json::json!({
                "agent": body.agent,
                "on_calendar": body.on_calendar,
                "path": path.display().to_string(),
            }),
        )),
        Ok(Err(error)) => Json(Envelope::failure("staff-schedule", "invalid", error)),
        Err(join) => Json(Envelope::failure(
            "staff-schedule",
            "internal",
            join.to_string(),
        )),
    }
}
