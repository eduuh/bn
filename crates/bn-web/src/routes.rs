//! The axum router: the JSON API and the embedded-UI static fallback.
//!
//! Skeleton surface (#388): `GET /api/health` returns a `bn_core::render::Envelope`, and every
//! other path falls back to the embedded single-page app (`ui/dist`, or the "UI not built"
//! placeholder). Later issues hang the read API, SSE stream, and write layer off this same router
//! and the shared [`AppState`].

use crate::actions;
use crate::api;
use crate::assets;
use crate::mcp;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use bn_core::render::Envelope;
use tower_http::trace::TraceLayer;

/// Build the application router with all routes, layers, and shared state wired in.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        // Reads (design §2.1) — every response is a `render::Envelope`.
        .route("/api/bearings", get(api::bearings))
        .route("/api/fleet", get(api::fleet))
        .route("/api/machine", get(api::machine))
        .route("/api/nodes", get(api::nodes))
        .route("/api/agents/catalog", get(api::agents_catalog))
        .route("/api/today/crons", get(api::today_crons))
        .route("/api/today/agents", get(api::today_agents))
        // Singular: Today's one *hosted* agent, not the scheduled roster above.
        .route("/api/today/agent", get(api::today_agent))
        .route("/api/agents/{session_id}", get(api::agent_detail))
        .route("/api/agents/{target}/pane", get(api::agent_pane))
        .route(
            "/api/agents/{session_id}/activity",
            get(api::agent_activity),
        )
        .route("/api/orchestrate", get(api::orchestrate))
        .route("/api/orchestrate/recovery", get(api::orchestrate_recovery))
        .route("/api/cost", get(api::cost))
        .route("/api/metrics", get(api::metrics))
        .route("/api/metrics/trend", get(api::metrics_trend))
        .route("/api/insights", get(api::insights))
        .route("/api/weekly", get(api::weekly))
        .route("/api/brag", get(api::brag))
        .route("/api/repos", get(api::repos))
        .route("/api/repos/{repo}/issues", get(api::repo_issues))
        .route("/api/notes", get(api::notes))
        .route("/api/notes/{repo}/{branch}", get(api::note_detail))
        .route(
            "/api/handoff",
            get(api::handoff).post(actions::handoff_write),
        )
        .route("/api/learnings", get(api::learnings))
        .route("/api/goals", get(api::goals))
        .route("/api/wrap", get(api::wrap))
        .route("/api/attn", get(api::attn))
        // The task-first read surface (#1005, EPIC #993).
        .route("/api/tasks", get(api::tasks))
        .route("/api/tasks/{id}", get(api::task_detail))
        .route("/api/issues", get(api::issues))
        .route("/api/pipeline/board", get(api::pipeline_board))
        // Registered before the `{repo}/{branch}` wildcard so `stages` stays a literal segment.
        .route("/api/pipeline/stages", get(api::pipeline_stages))
        .route("/api/pipeline/{repo}/{branch}", get(api::pipeline_status))
        .route("/api/actions", get(api::actions))
        .route("/api/completed", get(api::completed))
        .route("/api/search", get(api::search))
        .route("/api/cron", get(api::cron))
        .route("/api/skills", get(api::skills))
        .route("/api/skills/{name}", get(api::skill_detail))
        .route("/api/reviews", get(api::reviews))
        .route("/api/events", get(api::events))
        .route("/api/findings", get(api::findings))
        .route("/api/suggestions", get(api::suggestions))
        // Live stream (design §2.2) — SSE fed by the shared watcher broadcast.
        .route("/api/stream", get(api::stream))
        // Writes (design §2.3) — audited per handler. The server no longer gates on a web token
        // (#523); a loopback bind and a trusted LAN are the trust boundary.
        // Native state writes (in-process `orchestrate::mutate`):
        .route("/api/orchestrate/queue", post(actions::queue_add))
        .route(
            "/api/orchestrate/queue/reorder",
            post(actions::queue_reorder),
        )
        .route("/api/orchestrate/queue/{id}", delete(actions::queue_remove))
        .route("/api/orchestrate/scale", post(actions::scale))
        .route("/api/orchestrate/stop", post(actions::stop))
        .route("/api/orchestrate/start", post(actions::orchestrate_start))
        .route(
            "/api/repos/{repo}/orchestrate",
            post(actions::repo_orchestrate_start),
        )
        // Process writes (subprocess `bn --json` via the runner):
        .route("/api/dispatch", post(actions::dispatch))
        .route("/api/agents/{target}/send", post(actions::agent_send))
        .route(
            "/api/agents/{target}/relaunch",
            post(actions::agent_relaunch),
        )
        .route("/api/agents/{target}/kill", post(actions::agent_kill))
        .route("/api/land", post(actions::land))
        // Plan-review gate decision (#349) — native in-process store write:
        .route("/api/reviews/{id}", post(actions::review_decide))
        // Cron re-run (#538) — fire a job on demand via the same `bn run <job>` code path a timer
        // uses; returns the runner's envelope (exit + captured output tail).
        .route("/api/cron/run", post(actions::cron_run))
        // Today's agent (#577) — offline re-synthesis + suggestion review (native in-process
        // store writes), then the hosted agent's lifecycle (shelled, like every other launch).
        .route("/api/today/run", post(actions::today_run))
        .route("/api/suggestions/{id}", post(actions::suggestion_status))
        .route("/api/today/agent/start", post(actions::today_agent_start))
        .route("/api/today/agent/stop", post(actions::today_agent_stop))
        // Warm-worktree pool (#497) — trigger `bn wt pool warm --repo <repo>` for a pooled repo.
        // Long-running, so the handler spawns it detached and returns a "started" ack (non-blocking).
        .route("/api/repos/{repo}/pool/warm", post(actions::repo_pool_warm))
        .route("/api/repos/{repo}/pool", post(actions::repo_pool_set))
        // Goal-note connect/disconnect (#564) — native in-process daily-goal writes:
        .route("/api/goals/{id}/notes", post(actions::goal_note_connect))
        .route(
            "/api/goals/{id}/notes/{note_id}",
            delete(actions::goal_note_disconnect),
        )
        // Attention actions (#483) — native append-only store write (ack/remove/snooze/plan):
        .route("/api/attention/{id}", post(actions::attn_action))
        // Task writes (#1006), split by latency: the first two are file writes answered inline;
        // the third launches an LLM agent and returns a handle rather than holding a worker.
        .route("/api/tasks", post(actions::task_create))
        .route("/api/tasks/{id}", post(actions::task_action))
        .route("/api/tasks/{id}/doc", put(actions::task_doc_edit))
        .route("/api/tasks/{id}/doc/append", post(actions::task_doc_append))
        .route("/api/tasks/{id}/agent", post(actions::task_agent))
        // Skill CRUD (#483) — native validated file writes of the effective skill, each followed
        // by a Claude/Copilot native-skill sync so the harnesses never serve a stale copy:
        .route("/api/skills", post(actions::skill_create))
        .route("/api/skills/{name}", put(actions::skill_write))
        .route("/api/skills/{name}", delete(actions::skill_delete))
        // MCP-in-API (#639): the P1 bn-mcp tool-router served over Streamable HTTP alongside the
        // REST API. `/mcp/orchestrate` (cross-worktree) is nested first so its longer prefix wins
        // over the scoped `/mcp` branch-note tools. `/mcp` derives its (repo, branch) per request
        // from headers/query (`Scope::Request`). Like every other route, these are untokened
        // (#523) — no weaker than the dashboard.
        .nest_service("/mcp/orchestrate", mcp::orchestrator_service())
        .nest_service("/mcp", mcp::scoped_service(state.cfg.clone()))
        // Anything not matched above is an embedded UI asset (or the SPA index fallback).
        .fallback(static_handler)
        // Request tracing (outer): logs every request. There is no token gate (#523) — the server
        // never requires a web token, on loopback or `--lan`.
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// `GET /api/health` — liveness + a little build/runtime metadata, in the standard bn envelope so
/// the web API, CLI `--json`, and MCP all speak one shape.
async fn health(State(state): State<AppState>) -> Json<Envelope> {
    Json(health_envelope(&state))
}

/// Pure builder for the health envelope (unit-testable without spinning up HTTP).
fn health_envelope(state: &AppState) -> Envelope {
    let uptime = (bn_core::util::now_epoch_secs() - state.started_at).max(0);
    Envelope::success(
        "health",
        serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "machine": state.cfg.machine(),
            "uptime_secs": uptime,
            "ui_built": assets::ui_built(),
            "web": {
                "port": state.cfg.web.port,
                "bind": state.cfg.web.bind,
                "fleet_poll_secs": state.cfg.web.fleet_poll_secs,
            },
        }),
    )
}

/// Serve the embedded UI. An exact asset path wins; any other path falls back to `index.html` so
/// client-side (SPA) routes resolve. `/api/*` never reaches here — those are real routes, so an
/// unknown `/api/...` path correctly 404s rather than returning the SPA shell.
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Unknown API paths must 404 as JSON-ish, not silently serve the HTML shell.
    if path.starts_with("api/") || path == "api" {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    match resolve_asset(path) {
        Some((mime, bytes)) => ([(header::CONTENT_TYPE, mime)], Body::from(bytes)).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Resolve a request path to an embedded asset's `(mime, bytes)`, applying the SPA fallback: an
/// empty path or an unknown path yields `index.html`. Returns `None` only if even `index.html`
/// is missing (which build.rs guarantees never happens).
fn resolve_asset(path: &str) -> Option<(String, Vec<u8>)> {
    let lookup = if path.is_empty() { "index.html" } else { path };
    let file = assets::get(lookup).or_else(|| assets::get("index.html"))?;
    let mime = file.metadata.mimetype().to_string();
    Some((mime, file.data.into_owned()))
}
