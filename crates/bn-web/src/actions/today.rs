//! Today's agent write actions — the manual re-synthesis trigger, the hosted agent's
//! start/stop/status, and suggestion review.
//!
//! Split out of `actions.rs` when accepting stopped being a store write. Accepting a suggestion is
//! what feeds the queue, so it routes through `bn today accept` the way the dispatch button routes
//! through `bn dispatch` — one accept rule for the CLI and the page. Everything else here stays a
//! native in-process write, because it is only bookkeeping.
use super::{
    audit, bad_request, normalize_harness, not_found_json, parse_body, respond_process, run_bn,
    MaybeAddr,
};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct TodayRunBody {
    #[serde(default)]
    pub lookback_days: Option<i64>,
    #[serde(default)]
    pub per_source: Option<usize>,
}

/// `POST /api/today/run` — the Today page's manual trigger. Runs Today's agent in-process (gather
/// the cron findings → synthesize suggested tasks → merge idempotently), then returns the run
/// report plus the refreshed open suggestions so the UI can update without a second fetch.
pub async fn today_run(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "today-run";
    // Body is optional — an empty POST uses the defaults.
    let body: TodayRunBody = if raw.is_empty() {
        TodayRunBody::default()
    } else {
        match parse_body(cmd, &raw) {
            Ok(b) => b,
            Err(resp) => return resp,
        }
    };
    let lookback = body
        .lookback_days
        .unwrap_or(bn_core::today_agent::DEFAULT_LOOKBACK_DAYS);
    let per_source = body
        .per_source
        .unwrap_or(bn_core::today_agent::DEFAULT_PER_SOURCE);
    let res = tokio::task::spawn_blocking(move || {
        bn_core::today_agent::run(&bn_core::util::today(), lookback, per_source)
    })
    .await;
    match res {
        Ok(Ok(report)) => {
            audit::record(&state, cmd, "today-agent", addr, true);
            let open = bn_core::suggestions::list(
                Some(bn_core::suggestions::SuggestionStatus::Suggested),
                0,
            );
            Json(Envelope::success(
                cmd,
                json!({ "report": report, "suggestions": open }),
            ))
            .into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, "today-agent", addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "run_failed", e.to_string())),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, "today-agent", addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_failed", e.to_string())),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TodayAgentStartBody {
    /// `claude` | `copilot`. Absent ⇒ the machine's configured default.
    #[serde(default)]
    pub harness: Option<String>,
}

/// `POST /api/today/agent/start {harness?}` → `bn today agent start`.
///
/// Shelled rather than run in-process, like every other launch this server offers (see
/// [`dispatch`]): opening a tmux window, reserving an agent lease and pinning a harness session are
/// bn-cli's job, and duplicating that ordering here is how the two copies drift. Idempotent at the
/// CLI — a live agent for today is reported, not duplicated — so a double-click cannot fork the day.
pub async fn today_agent_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "today-agent-start";
    let body: TodayAgentStartBody = if raw.is_empty() {
        TodayAgentStartBody::default()
    } else {
        match parse_body(cmd, &raw) {
            Ok(b) => b,
            Err(resp) => return resp,
        }
    };
    let mut args = vec![
        "today".to_string(),
        "agent".to_string(),
        "start".to_string(),
    ];
    if let Some(h) = body
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let Some(harness) = normalize_harness(Some(h)) else {
            return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
        };
        args.push("--harness".to_string());
        args.push(harness.to_string());
    }
    let result = run_bn(args, None).await;
    respond_process(&state, "today-agent-start", "today-agent", addr, result)
}

/// `POST /api/today/agent/stop` → `bn today agent stop`. Idempotent: stopping an agent that is
/// already gone succeeds, because the state the caller asked for is the state they get.
pub async fn today_agent_stop(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
) -> Response {
    let args = vec!["today".to_string(), "agent".to_string(), "stop".to_string()];
    let result = run_bn(args, None).await;
    respond_process(&state, "today-agent-stop", "today-agent", addr, result)
}

#[derive(Debug, Default, Deserialize)]
pub struct SuggestionStatusBody {
    pub status: String,
}

/// `POST /api/suggestions/{id} {status}` — review one suggestion (accepted | dismissed | suggested
/// | done).
///
/// Accepting is **not** a store write. It is what feeds the queue: it mints the governing task, runs
/// the SMART gate, and enqueues either the work or the pass that resolves where the work happens. So
/// accept goes through `bn today accept`, exactly as the dashboard's dispatch button goes through
/// `bn dispatch` — one accept rule for the CLI and the page, rather than a second in-process copy
/// that would flip a status and quietly queue nothing.
///
/// Every other status stays a native in-process write: they are only bookkeeping.
pub async fn suggestion_status(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "suggestion-status";
    let body: SuggestionStatusBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    // Ids are `<source>-<key>` — a filename-safe token; keep the allowlist tight.
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return bad_request(cmd, "invalid_id", "suggestion id has invalid characters");
    }
    let Some(status) = bn_core::suggestions::SuggestionStatus::parse(&body.status) else {
        return bad_request(
            cmd,
            "bad_status",
            "status must be suggested | accepted | dismissed | done",
        );
    };
    // Accept is a producer, not a store write — see this function's docs. `id` is already validated
    // against a tight allowlist above, so it is safe as a single argv element.
    if status == bn_core::suggestions::SuggestionStatus::Accepted {
        let result = run_bn(
            vec!["today".to_string(), "accept".to_string(), id.clone()],
            None,
        )
        .await;
        return respond_process(&state, cmd, &id, addr, result);
    }
    let id_call = id.clone();
    let res =
        tokio::task::spawn_blocking(move || bn_core::suggestions::set_status(&id_call, status))
            .await;
    match res {
        Ok(Ok(true)) => {
            audit::record(&state, cmd, &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "status": status.as_str() }),
            ))
            .into_response()
        }
        Ok(Ok(false)) => {
            audit::record(&state, cmd, &id, addr, false);
            not_found_json(cmd, "no such suggestion")
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, &id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "write_failed", e.to_string())),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, &id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_failed", e.to_string())),
            )
                .into_response()
        }
    }
}
