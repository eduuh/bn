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
use crate::terminal;
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
        .route("/api/agents/catalog", get(api::agents_catalog))
        .route("/api/today/crons", get(api::today_crons))
        .route("/api/today/agents", get(api::today_agents))
        .route("/api/agents/{session_id}", get(api::agent_detail))
        .route("/api/agents/{target}/pane", get(api::agent_pane))
        // Live agent terminal (#491) — a WebSocket streaming the pane + forwarding token-gated input.
        // Not an `/api` path, so it self-gates ([`terminal`]) rather than via the token middleware.
        .route(
            "/ws/agents/{target}/terminal",
            get(terminal::agent_terminal),
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
        // Today's agent (#577) — manual trigger + suggestion review (native in-process store writes):
        .route("/api/today/run", post(actions::today_run))
        .route("/api/suggestions/{id}", post(actions::suggestion_status))
        // Warm-worktree pool (#497) — trigger `bn wt pool warm --repo <repo>` for a pooled repo.
        // Long-running, so the handler spawns it detached and returns a "started" ack (non-blocking).
        .route("/api/repos/{repo}/pool/warm", post(actions::repo_pool_warm))
        // Goal-note connect/disconnect (#564) — native in-process daily-goal writes:
        .route("/api/goals/{id}/notes", post(actions::goal_note_connect))
        .route(
            "/api/goals/{id}/notes/{note_id}",
            delete(actions::goal_note_disconnect),
        )
        // Attention actions (#483) — native append-only store write (ack/remove/snooze/plan):
        .route("/api/attention/{id}", post(actions::attn_action))
        // Skill edit (#483) — native validated file write of the effective skill:
        .route("/api/skills/{name}", put(actions::skill_write))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_payload_carries_the_agent_role() {
        // #500 contract: the `/api/fleet` payload builder emits each agent's `role`, so the UI can
        // pin orchestrators as their own tier. Guards against the field silently dropping from the
        // serialized shape. Built through the same `agent_fleet_data` the `fleet` handler calls.
        use bn_core::agent::{AgentHealth, AgentRole, Fleet, LiveAgent};
        let agent = LiveAgent {
            session_id: "orch-1".into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "wave".into(),
            target: "bn:wave".into(),
            last_activity_ts: 0,
            idle_secs: 0,
            health: AgentHealth::Active,
            role: AgentRole::Orchestrator,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
        };
        let fleet = Fleet {
            agents: vec![agent],
            ..Fleet::default()
        };
        let data = bn_core::payload::agent_fleet_data(&fleet);
        assert_eq!(data["agents"][0]["role"], "orchestrator");
    }

    #[test]
    fn health_envelope_is_ok_and_reports_web_config() {
        let state = AppState::for_test(bn_core::resolver::Config::from_env());
        let env = health_envelope(&state);
        assert!(env.ok);
        assert_eq!(env.command, "health");
        assert_eq!(env.data["status"], "ok");
        // The resolved [web] defaults flow through to the payload.
        assert_eq!(env.data["web"]["port"], 7770);
        assert_eq!(env.data["web"]["bind"], "127.0.0.1");
    }

    #[test]
    fn root_and_unknown_paths_resolve_to_index_html() {
        // build.rs always embeds an index.html (real UI or placeholder), so both the root and an
        // arbitrary SPA route serve HTML rather than 404.
        let (mime_root, root) = resolve_asset("").expect("index for empty path");
        assert!(mime_root.contains("html"));
        assert!(!root.is_empty());
        let (_, spa) = resolve_asset("some/client/route").expect("SPA fallback to index");
        assert_eq!(root, spa);
    }

    // --- Router end-to-end through the real service --------------------------

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`

    fn test_state() -> AppState {
        AppState::new(bn_core::resolver::Config::from_env())
    }

    async fn status_of(state: AppState, req: Request<Body>) -> StatusCode {
        router(state).oneshot(req).await.unwrap().status()
    }

    fn put(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
        let mut b = Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    fn post(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    fn delete(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn goal_notes_connect_read_and_disconnect_round_trip() {
        // Contract for the goal↔note association (#564): connect writes it, GET /api/goals reads it
        // back inline on the goal, and DELETE removes it — all open on loopback without a token
        // (#526). A bad input is a 400; a missing goal a 404.
        bn_core::test_support::with_state_dir_path(|dir| {
            // Point the notes root at the isolated state dir so the day file lands there, and pin
            // "today" so the seeded goal and the handler's day file agree.
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");
            let _today = bn_core::test_support::EnvGuard::set("BN_TODAY", "2026-07-14");
            let cfg = bn_core::resolver::Config::from_env();
            bn_core::daily::add(&cfg, "2026-07-14", "ship the goals feature", None, None).unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Connect a linked branch note to g1.
                let resp = respond(
                    test_state(),
                    post(
                        "/api/goals/g1/notes",
                        None,
                        r#"{"repo":"bn","branch":"feat-x"}"#,
                    ),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["data"]["goal"]["id"], "g1");
                assert_eq!(body["data"]["note"]["id"], "n1");
                assert_eq!(body["data"]["note"]["repo"], "bn");

                // GET /api/goals now carries the connected note inline on the goal.
                let resp = respond(test_state(), get("/api/goals", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                let note = &body["data"]["daily"]["goals"][0]["notes"][0];
                assert_eq!(note["id"], "n1");
                assert_eq!(note["branch"], "feat-x");

                // An input with neither a complete link nor text is a 400.
                let bad = respond(
                    test_state(),
                    post("/api/goals/g1/notes", None, r#"{"repo":"bn"}"#),
                )
                .await;
                assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

                // A note on a goal that doesn't exist is a 404.
                let missing = respond(
                    test_state(),
                    post("/api/goals/g9/notes", None, r#"{"text":"x"}"#),
                )
                .await;
                assert_eq!(missing.status(), StatusCode::NOT_FOUND);

                // A bogus goal id never reaches the store (400 before any write).
                let bad_id = respond(
                    test_state(),
                    post("/api/goals/evil-id/notes", None, r#"{"text":"x"}"#),
                )
                .await;
                assert_eq!(bad_id.status(), StatusCode::BAD_REQUEST);

                // Disconnect removes the note.
                let del = respond(test_state(), delete("/api/goals/g1/notes/n1")).await;
                assert_eq!(del.status(), StatusCode::OK);
                let resp = respond(test_state(), get("/api/goals", None)).await;
                let body = body_json(resp).await;
                assert!(body["data"]["daily"]["goals"][0]["notes"]
                    .as_array()
                    .unwrap()
                    .is_empty());
            });
        });
    }

    #[test]
    fn wrap_read_is_open_on_loopback_and_returns_a_wrap_envelope() {
        // `/api/wrap` is a read: open on loopback without a token, and always resolves to the
        // standard success envelope (git/gh are best-effort; a clean day is still `ok`). Read-only —
        // it must never materialize today's day file (that stays a `bn` CLI action).
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/wrap", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "wrap");
                // The digest always carries its date + a rendered text block.
                assert!(body["data"]["date"].is_string());
                assert!(body["data"]["rendered"].is_string());
            });
        });
    }

    #[tokio::test]
    async fn machine_read_is_open_on_loopback_and_returns_a_machine_envelope() {
        // `/api/machine` (#525) is a read: open on loopback without a token, always a success
        // envelope carrying cpu / ram / saturation / agents{active}. Metrics are best-effort, so an
        // unreadable signal is `null`, never a failure.
        let resp = respond(test_state(), get("/api/machine", None)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["command"], "machine");
        // Shape the dashboard widget + #503 backpressure rely on.
        assert!(body["data"]["cpu"]["cores"].as_u64().unwrap() >= 1);
        assert!(body["data"]["ram"].is_object());
        assert!(body["data"]["saturation"]["level"].is_string());
        assert!(body["data"]["saturation"]["reasons"].is_array());
        assert!(body["data"]["agents"]["active"].is_u64());
    }

    #[tokio::test]
    async fn bearings_read_returns_the_shared_bounded_snapshot() {
        let resp = respond(test_state(), get("/api/bearings", None)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["command"], "bearings");
        assert!(body["data"]["fleet"]["agents"].is_array());
        assert!(body["data"]["orchestration"]["runs"].is_array());
        assert!(body["data"]["attention"]["items"].is_array());
        assert!(
            body["data"]["fleet"]["agents"].as_array().unwrap().len()
                <= bn_core::bearings::MAX_AGENTS
        );
    }

    fn get(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method("GET").uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn pane_rejects_a_malformed_target_before_spawning() {
        // A target without a `:` never reaches tmux — the handler 400s it (loopback read, so auth
        // isn't the gate here; the shape check is).
        let s = status_of(test_state(), get("/api/agents/no-colon/pane", None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn repo_issues_rejects_a_malformed_repo_and_is_best_effort_otherwise() {
        // #556: a repo name with disallowed characters never reaches gh — 400 up front.
        let bad = status_of(test_state(), get("/api/repos/a:b/issues", None)).await;
        assert_eq!(bad, StatusCode::BAD_REQUEST);

        // A well-formed but unknown repo resolves no bare clone / slug, so the list is empty —
        // a 200 success envelope (best-effort), never an error.
        let resp = respond(
            test_state(),
            get("/api/repos/definitely-not-a-real-repo/issues", None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["command"], "repo-issues");
        assert!(body["data"]["issues"].as_array().unwrap().is_empty());
    }

    // --- Plan-review gate endpoints (#349) ----------------------------------

    async fn respond(state: AppState, req: Request<Body>) -> Response {
        router(state).oneshot(req).await.unwrap()
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn search_read_folds_sources_into_ranked_palette_results() {
        // #565 contract: `/api/search` is a loopback read that folds the command-center sources
        // into one ranked, typed, actionable list. A seeded pending plan-review must surface as a
        // startable palette result (the "work on this" action the composer consumes). gh issues are
        // best-effort (like `/api/wrap`), so any gh outcome leaves this assertion intact.
        bn_core::test_support::with_state_dir(|| {
            bn_core::plan_review::submit(
                "bn",
                "dispatch/565",
                None,
                "add the global search palette",
                100,
            )
            .unwrap();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/search?q=palette", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "search");
                assert_eq!(body["data"]["query"], "palette");
                let results = body["data"]["results"].as_array().expect("results array");
                let review = results
                    .iter()
                    .find(|r| r["type"] == "plan-review")
                    .expect("the pending plan-review folds into a result");
                assert!(
                    review["action"].is_object(),
                    "startable result: {review:#?}"
                );
                assert_eq!(review["repo"], "bn");

                // A blank query short-circuits to an empty set (no work, no gh).
                let resp = respond(test_state(), get("/api/search?q=", None)).await;
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["data"]["count"], 0);
            });
        });
    }

    #[test]
    fn reviews_read_lists_from_the_store() {
        bn_core::test_support::with_state_dir(|| {
            bn_core::plan_review::submit("bn", "dispatch/349", None, "the plan", 100).unwrap();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // A read: open on loopback without a token.
                let resp = respond(test_state(), get("/api/reviews", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["data"]["reviews"][0]["repo"], "bn");
                assert_eq!(body["data"]["reviews"][0]["status"], "pending");
                assert_eq!(body["data"]["reviews"][0]["plan"], "the plan");
            });
        });
    }

    #[tokio::test]
    async fn repo_orchestrate_validates_repo_and_harness_before_dispatching() {
        // #556: a repo name with disallowed characters never reaches `bn dispatch` — 400 up front.
        let bad_repo = status_of(
            test_state(),
            post("/api/repos/a:b/orchestrate", Some("secret-token"), "{}"),
        )
        .await;
        assert_eq!(bad_repo, StatusCode::BAD_REQUEST);
        // An unknown harness is rejected before any subprocess spawn.
        let bad_harness = status_of(
            test_state(),
            post(
                "/api/repos/bn/orchestrate",
                Some("secret-token"),
                r#"{"harness":"emacs"}"#,
            ),
        )
        .await;
        assert_eq!(bad_harness, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn review_decide_validates_id_and_action() {
        // A bogus id (contains a non-alphanumeric char) never reaches the store.
        let bad_id = status_of(
            test_state(),
            post(
                "/api/reviews/r1-evil",
                Some("secret-token"),
                r#"{"action":"approve"}"#,
            ),
        )
        .await;
        assert_eq!(bad_id, StatusCode::BAD_REQUEST);
        // An unknown action is rejected before any mutation.
        let bad_action = status_of(
            test_state(),
            post(
                "/api/reviews/r1",
                Some("secret-token"),
                r#"{"action":"yolo"}"#,
            ),
        )
        .await;
        assert_eq!(bad_action, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn review_decide_approves_a_pending_review() {
        bn_core::test_support::with_state_dir(|| {
            let r =
                bn_core::plan_review::submit("bn", "dispatch/349", None, "the plan", 100).unwrap();
            let uri = format!("/api/reviews/{}", r.id);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(
                    test_state(),
                    post(&uri, Some("secret-token"), r#"{"action":"approve"}"#),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["data"]["review"]["status"], "approved");
            });
            // The store transitioned — the agent's poll would now see `approved`.
            assert_eq!(
                bn_core::plan_review::status_of(&r.id),
                Some(bn_core::plan_review::ReviewStatus::Approved)
            );
        });
    }

    #[test]
    fn review_decide_request_changes_needs_notes_then_records_them() {
        bn_core::test_support::with_state_dir(|| {
            let r =
                bn_core::plan_review::submit("bn", "dispatch/349", None, "the plan", 100).unwrap();
            let uri = format!("/api/reviews/{}", r.id);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // No notes → 400.
                let missing = respond(
                    test_state(),
                    post(
                        &uri,
                        Some("secret-token"),
                        r#"{"action":"request_changes"}"#,
                    ),
                )
                .await;
                assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
                // With notes → 200 and the notes are recorded.
                let ok = respond(
                    test_state(),
                    post(
                        &uri,
                        Some("secret-token"),
                        r#"{"action":"request_changes","notes":"split it up"}"#,
                    ),
                )
                .await;
                assert_eq!(ok.status(), StatusCode::OK);
                let body = body_json(ok).await;
                assert_eq!(body["data"]["review"]["status"], "changes-requested");
                assert_eq!(body["data"]["review"]["notes"], "split it up");
            });
        });
    }

    #[test]
    fn review_decide_on_unknown_id_is_404() {
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(
                    test_state(),
                    post(
                        "/api/reviews/r999",
                        Some("secret-token"),
                        r#"{"action":"approve"}"#,
                    ),
                )
                .await;
                // Auth passed (not 401) and the native decide ran → 404 for the missing record.
                assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            });
        });
    }

    // --- Cron registry + skills reads (#483) --------------------------------

    #[test]
    fn cron_read_is_open_on_loopback_and_wraps_jobs() {
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/cron", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "cron");
                assert!(body["data"]["jobs"].is_array());
                assert!(body["data"]["count"].is_number());
            });
        });
    }

    #[test]
    fn today_crons_and_agents_read_wrap_the_catalog_shape() {
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                for (uri, cmd) in [
                    ("/api/today/crons", "today-crons"),
                    ("/api/today/agents", "today-agents"),
                ] {
                    let resp = respond(test_state(), get(uri, None)).await;
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = body_json(resp).await;
                    assert_eq!(body["ok"], true);
                    assert_eq!(body["command"], cmd);
                    assert!(body["data"]["agents"].is_array());
                    assert!(body["data"]["count"].is_number());
                }
            });
        });
    }

    #[test]
    fn actions_read_carries_a_cron_activity_feed() {
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/actions", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "actions");
                assert!(body["data"]["actions"].is_array());
                // #557: the observational cron-activity feed rides alongside the action list.
                assert!(body["data"]["cron_activity"].is_array());
            });
        });
    }

    #[test]
    fn agents_catalog_read_wraps_the_triggerable_roster() {
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/agents/catalog", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "agents-catalog");
                // The static `catalog` segment wins over the `{session_id}` param route, and the
                // payload is the object-wrapped triggerable roster (#557).
                assert!(body["data"]["agents"].is_array());
                assert!(body["data"]["count"].is_number());
            });
        });
    }

    #[test]
    fn skills_read_is_open_on_loopback_and_wraps_skills() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = respond(test_state(), get("/api/skills", None)).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert_eq!(body["ok"], true);
            assert_eq!(body["command"], "skills");
            assert!(body["data"]["skills"].is_array());
            assert!(body["data"]["count"].is_number());
        });
    }

    #[test]
    fn skill_detail_404s_an_unknown_skill() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = respond(test_state(), get("/api/skills/zzz-no-such-skill-xyz", None)).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        });
    }

    // --- Skill edit (#483) --------------------------------------------------

    #[tokio::test]
    async fn skill_write_validates_name_and_content_before_touching_disk() {
        // A name outside the file-stem charset never reaches discovery.
        let bad_name = status_of(
            test_state(),
            put(
                "/api/skills/bad.name",
                Some("secret-token"),
                r#"{"content":"---\ndescription: x\n---\nb\n"}"#,
            ),
        )
        .await;
        assert_eq!(bad_name, StatusCode::BAD_REQUEST);
        // Content that isn't a valid skill (no frontmatter) is rejected before any write.
        let bad_content = status_of(
            test_state(),
            put(
                "/api/skills/wrap",
                Some("secret-token"),
                r#"{"content":"just prose, no frontmatter"}"#,
            ),
        )
        .await;
        assert_eq!(bad_content, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn skill_write_on_valid_content_but_unknown_skill_is_404() {
        // Valid content passes validation, then resolution finds no such skill → 404 (not a write).
        let s = status_of(
            test_state(),
            put(
                "/api/skills/zzz-no-such-skill-xyz",
                Some("secret-token"),
                r#"{"content":"---\ndescription: x\n---\nbody\n"}"#,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    // --- Attention actions (#483) -------------------------------------------

    #[tokio::test]
    async fn attn_action_rejects_bad_id_and_unknown_action() {
        // A non-alphanumeric id never reaches the store.
        let bad_id = status_of(
            test_state(),
            post(
                "/api/attention/a1.evil",
                Some("secret-token"),
                r#"{"action":"ack"}"#,
            ),
        )
        .await;
        assert_eq!(bad_id, StatusCode::BAD_REQUEST);
        // An unknown verb is rejected before any mutation.
        let bad_action = status_of(
            test_state(),
            post(
                "/api/attention/a1",
                Some("secret-token"),
                r#"{"action":"yolo"}"#,
            ),
        )
        .await;
        assert_eq!(bad_action, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn attn_ack_resolves_an_open_item_and_404s_unknown() {
        bn_core::test_support::with_state_dir(|| {
            let today = bn_core::util::today();
            let item = bn_core::attention::add(
                "manual",
                "prod is down",
                "urgent",
                None,
                None,
                None,
                None,
                &today,
            )
            .unwrap();
            let uri = format!("/api/attention/{}", item.id);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Unknown id → 404 (auth passed, store ran, nothing matched).
                let miss = respond(
                    test_state(),
                    post(
                        "/api/attention/a999",
                        Some("secret-token"),
                        r#"{"action":"ack"}"#,
                    ),
                )
                .await;
                assert_eq!(miss.status(), StatusCode::NOT_FOUND);
                // Real id → 200 with ok:true.
                let ok = respond(
                    test_state(),
                    post(&uri, Some("secret-token"), r#"{"action":"ack"}"#),
                )
                .await;
                assert_eq!(ok.status(), StatusCode::OK);
                let body = body_json(ok).await;
                assert_eq!(body["data"]["ok"], true);
            });
            // The item folded out of the open set.
            assert!(bn_core::attention::open_items(&today).is_empty());
        });
    }

    #[test]
    fn attn_remove_dismisses_an_open_item() {
        bn_core::test_support::with_state_dir(|| {
            let today = bn_core::util::today();
            let item =
                bn_core::attention::add("manual", "typo", "warn", None, None, None, None, &today)
                    .unwrap();
            let uri = format!("/api/attention/{}", item.id);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(
                    test_state(),
                    post(&uri, Some("secret-token"), r#"{"action":"remove"}"#),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
            });
            assert!(bn_core::attention::open_items(&today).is_empty());
        });
    }

    #[test]
    fn attn_snooze_requires_a_date() {
        bn_core::test_support::with_state_dir(|| {
            let today = bn_core::util::today();
            let item =
                bn_core::attention::add("manual", "later", "warn", None, None, None, None, &today)
                    .unwrap();
            let uri = format!("/api/attention/{}", item.id);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // No `until` → 400 before any write.
                let missing = respond(
                    test_state(),
                    post(&uri, Some("secret-token"), r#"{"action":"snooze"}"#),
                )
                .await;
                assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
            });
        });
    }

    // --- Handoff read/write (#509) ------------------------------------------

    #[test]
    fn handoff_get_composes_from_note_then_post_writes_it() {
        // GET composes a packet live from the branch note when none is written (mirroring
        // `bn handoff read`); POST persists it as `handoff.json`. Both are open on loopback (#526).
        bn_core::test_support::with_state_dir_path(|dir| {
            // Point the notes root at the isolated state dir so the note + handoff land there.
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");

            let cfg = bn_core::resolver::Config::from_env();
            let ctx = bn_core::resolver::resolve_target(
                &cfg,
                std::path::Path::new("."),
                Some("bn".into()),
                Some("dispatch-web-handoff-509".into()),
            )
            .unwrap();
            // Seed a note so both compose (GET) and write (POST) have a source.
            let mut note = bn_core::model::Note::new(
                bn_core::resolver::RepoBranch::new("bn", "dispatch-web-handoff-509"),
                &bn_core::util::today(),
                "testhost",
            );
            note.body = "## Goal\n\nShip the handoff viewer.\n".into();
            bn_core::store::save(&note, &cfg.note_file(&ctx)).unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // GET with no handoff written yet → composed from the note.
                let resp = respond(
                    test_state(),
                    get(
                        "/api/handoff?repo=bn&branch=dispatch-web-handoff-509",
                        None,
                    ),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "handoff");
                assert_eq!(
                    body["data"]["handoff"]["state"]["goal"],
                    "Ship the handoff viewer."
                );
                assert!(body["data"]["markdown"].is_string());

                // POST writes/updates the packet (open — no token).
                let post_resp = respond(
                    test_state(),
                    post(
                        "/api/handoff",
                        None,
                        r#"{"repo":"bn","branch":"dispatch-web-handoff-509","narrative":"Composed from the web."}"#,
                    ),
                )
                .await;
                assert_eq!(post_resp.status(), StatusCode::OK);
                let pb = body_json(post_resp).await;
                assert_eq!(pb["ok"], true);
                assert_eq!(pb["data"]["handoff"]["narrative"], "Composed from the web.");
            });

            // The write landed a `handoff.json` beside the note.
            let written = bn_core::handoff::read(&cfg, &ctx).expect("handoff.json persisted");
            assert_eq!(written.narrative, "Composed from the web.");
        });
    }

    #[test]
    fn handoff_get_requires_repo_and_branch() {
        // A partial target is a clean 400 before any store read.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = respond(test_state(), get("/api/handoff?repo=bn", None)).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        });
    }

    #[test]
    fn handoff_post_requires_a_target() {
        // POST with an empty body has no repo/branch → 400, no write.
        bn_core::test_support::with_state_dir(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), post("/api/handoff", None, "{}")).await;
                assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            });
        });
    }

    // --- Cron re-run + failing-run log surface (#538) ------------------------

    #[tokio::test]
    async fn cron_run_rejects_a_bogus_job_before_spawning() {
        // A job name with shell/path metacharacters never reaches the runner — a clean 400.
        let bad = status_of(
            test_state(),
            post("/api/cron/run", None, r#"{"job":"evil; rm -rf"}"#),
        )
        .await;
        assert_eq!(bad, StatusCode::BAD_REQUEST);
        // An empty job is rejected too.
        let empty = status_of(test_state(), post("/api/cron/run", None, r#"{"job":""}"#)).await;
        assert_eq!(empty, StatusCode::BAD_REQUEST);
    }

    // --- Warm-worktree pool trigger (#497) ----------------------------------

    #[tokio::test]
    async fn warm_rejects_a_bogus_repo_before_spawning() {
        // A repo name with metacharacters never reaches the runner — a clean 400 (no subprocess).
        let bad = status_of(
            test_state(),
            post("/api/repos/bad!repo/pool/warm", None, ""),
        )
        .await;
        assert_eq!(bad, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn warm_rejects_an_unpooled_repo() {
        // A well-formed repo that isn't in the pools config is a 400 (not_pooled) — the handler must
        // not fire a detached warm that would silently no-op. Force an empty pools config so the
        // check is deterministic regardless of the host's real config.
        let _g = bn_core::test_support::env_lock();
        let _pools =
            bn_core::test_support::EnvGuard::set("WT_POOLS", "/nonexistent/bn-web-test-pools-497");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = respond(test_state(), post("/api/repos/bn/pool/warm", None, "")).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            let body = body_json(resp).await;
            assert_eq!(body["ok"], false);
            assert_eq!(body["error"]["code"], "not_pooled");
        });
    }

    #[test]
    fn cron_read_surfaces_a_failing_runs_output_tail() {
        // The `/api/cron` payload carries `last_output` for a job whose last run failed, so the UI
        // can show *why* without a shell. Seed a failed run with a captured tail; a uniquely-named
        // job stays a deterministic run-observed orphan regardless of the host's real cron jobs.
        bn_core::test_support::with_state_dir(|| {
            bn_core::events::log_cron_run(
                "bn-web-cron-test-538",
                "global",
                None,
                None,
                1000,
                200,
                1,
                "boom: it broke\n",
            )
            .unwrap();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/cron", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["command"], "cron");
                let jobs = body["data"]["jobs"].as_array().unwrap();
                let j = jobs
                    .iter()
                    .find(|j| j["job"] == "bn-web-cron-test-538")
                    .expect("the seeded run-observed orphan is present");
                assert_eq!(j["ok"], false);
                assert!(j["last_output"]
                    .as_str()
                    .unwrap()
                    .contains("boom: it broke"));
            });
        });
    }

    #[test]
    fn completed_read_folds_a_done_todo_with_its_note_links() {
        // #561 contract: `/api/completed` is a loopback read folding every finished signal into a
        // most-recent-first history. A branch-note todo closed with `bn done` must surface as a
        // completed item carrying its local note path (the "open locally" jump). The remote URL is
        // best-effort (the isolated state dir has no git remote), so we only assert the local jump.
        bn_core::test_support::with_state_dir_path(|dir| {
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");
            let cfg = bn_core::resolver::Config::from_env();
            let ctx = bn_core::resolver::Resolved {
                repo: "bn".into(),
                branch: "dispatch-561".into(),
            };
            bn_core::commands::add(&cfg, &ctx, "todo", "ship completed tab", None, "2026-07-14")
                .unwrap();
            bn_core::commands::done(&cfg, &ctx, "ship completed tab", "2026-07-15", None).unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = respond(test_state(), get("/api/completed", None)).await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                assert_eq!(body["ok"], true);
                assert_eq!(body["command"], "completed");
                let items = body["data"]["completed"]
                    .as_array()
                    .expect("completed array");
                let todo = items
                    .iter()
                    .find(|c| c["type"] == "todo" && c["title"] == "ship completed tab")
                    .expect("the closed todo folds into a completed item");
                assert_eq!(todo["repo"], "bn");
                assert_eq!(todo["branch"], "dispatch-561");
                assert!(
                    todo["note"]["local_path"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("testhost/bn/dispatch-561/note.md")),
                    "local note jump present: {todo:#?}"
                );
            });
        });
    }
}
