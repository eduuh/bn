//! Mount the P1 `bn-mcp` handlers as Streamable-HTTP MCP endpoints on the bn-web router (#639).
//!
//! `bn serve` is the single API exposing bn's branch-note tools over HTTP MCP:
//! `POST /mcp` for the scoped branch-note tools and `POST /mcp/orchestrate` for the cross-worktree
//! orchestrator tools. The scoped server is built with [`Scope::Request`], so it derives its
//! `(repo, branch)` per request from the `X-Bn-Repo`/`X-Bn-Branch` headers (or `?repo=&branch=`
//! query) — one shared server serves every branch and fails closed when no scope is supplied.
//!
//! **Auth.** Like the rest of bn-web, `/mcp` is not token-gated: the web token was removed (#523),
//! so loopback / a trusted LAN / an authenticated dev tunnel is the boundary. `/mcp` inherits that
//! exact posture — no new auth system, and no weaker than the dashboard's REST + terminal routes.
//!
//! **Transport mode.** Stateless with a direct JSON response (`stateful_mode = false`,
//! `json_response = true`): every POST is self-contained and returns one `application/json` body
//! that closes immediately, rather than a long-lived SSE stream. That matches how agents drive the
//! tools and keeps the surface easy to exercise with `tower::oneshot`. DNS-rebinding host checks
//! are disabled so LAN / tailscale `Host` headers reach the service, consistent with the open
//! (untokened) posture above.

use std::sync::Arc;

use bn_core::resolver::Config;
use bn_mcp::orchestrator::BnOrchestratorServer;
use bn_mcp::server::BnServer;
use bn_mcp::Scope;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

/// Shared transport config for both MCP mounts (see the module docs for the mode rationale).
fn server_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts()
}

/// The scoped branch-note MCP service (`POST /mcp`). Each request builds a fresh [`BnServer`] over
/// [`Scope::Request`], so the branch identity is resolved per call from the request headers/query.
/// The session-safe skill prompts are seeded from the global skill dirs
/// ([`bn_core::skills::global_skill_dirs`]) — the shared server has no worktree, so the per-repo
/// skill tiers are out of reach (skills authoring stays a `bn skills`/CLI workflow, #641).
pub fn scoped_service(cfg: Arc<Config>) -> StreamableHttpService<BnServer, LocalSessionManager> {
    StreamableHttpService::new(
        move || {
            Ok(BnServer::new(
                (*cfg).clone(),
                Scope::Request,
                bn_core::skills::global_skill_dirs(),
            ))
        },
        Arc::new(LocalSessionManager::default()),
        server_config(),
    )
}

/// The orchestrator MCP service (`POST /mcp/orchestrate`) — the cross-worktree "friend" toolset,
/// composed from the `bn` CLI and independent of any single branch scope.
pub fn orchestrator_service() -> StreamableHttpService<BnOrchestratorServer, LocalSessionManager> {
    StreamableHttpService::new(
        || Ok(BnOrchestratorServer::new()),
        Arc::new(LocalSessionManager::default()),
        server_config(),
    )
}

#[cfg(test)]
mod tests {
    use crate::routes::router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, Response, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt; // for `oneshot`

    /// POST a JSON-RPC message at `uri` with the MCP-required `Accept` (both JSON and SSE) and
    /// optional scope headers, driven through the full router via `tower::oneshot`.
    fn mcp_post(uri: &str, headers: &[(&str, &str)], body: Value) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "localhost")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    async fn send(state: AppState, req: Request<Body>) -> Response<Body> {
        router(state).oneshot(req).await.unwrap()
    }

    async fn body_json(resp: Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn init_body(id: u32) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "bn-web-test", "version": "0" }
            }
        })
    }

    #[test]
    fn initialize_returns_the_bn_mcp_server_info() {
        let state = AppState::new(bn_core::resolver::Config::from_env());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = send(state, mcp_post("/mcp", &[], init_body(1))).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert_eq!(body["result"]["serverInfo"]["name"], "bn-mcp");
            assert!(body["result"]["capabilities"]["tools"].is_object());
        });
    }

    #[test]
    fn prompts_list_serves_the_session_safe_skills_over_http() {
        // The shared server has no launch worktree, so it seeds prompts from the GLOBAL skill dirs
        // (BN_MCP_SKILLS_DIR). Point that at the repo's `config/prompts/` and only the session-safe set
        // (start-session, wrap-session, stow, axi, lavish) must surface as MCP prompts (#641).
        let skills_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/prompts");
        let _guard = bn_core::test_support::EnvGuard::set("BN_MCP_SKILLS_DIR", skills_dir);
        let state = AppState::new(bn_core::resolver::Config::from_env());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = send(
                state,
                mcp_post(
                    "/mcp",
                    &[],
                    json!({ "jsonrpc": "2.0", "id": 6, "method": "prompts/list" }),
                ),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            let mut names: Vec<String> = body["result"]["prompts"]
                .as_array()
                .expect("prompts array")
                .iter()
                .map(|p| p["name"].as_str().unwrap().to_string())
                .collect();
            names.sort();
            assert_eq!(
                names,
                vec!["axi", "lavish", "start-session", "stow", "wrap-session"],
                "only session-safe skills are served as prompts; got {names:?}"
            );
        });
    }

    #[test]
    fn tools_list_returns_the_scoped_branch_note_tool_set() {
        let state = AppState::new(bn_core::resolver::Config::from_env());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = send(
                state,
                mcp_post(
                    "/mcp",
                    &[],
                    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
                ),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            let names: Vec<String> = body["result"]["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect();
            for expected in [
                "bn_context",
                "bn_add",
                "bn_done",
                "bn_pr",
                "bn_goal",
                "bn_cat",
                "bn_status",
                "bn_sync",
                "bn_handoff_read",
                "bn_handoff_write",
            ] {
                assert!(
                    names.iter().any(|n| n == expected),
                    "tools/list should expose {expected}; got {names:?}"
                );
            }
        });
    }

    #[test]
    fn bn_context_round_trips_over_http_with_a_request_scope() {
        // The scope headers pick the branch note per request; a seeded goal must round-trip in the
        // structured result — proving the P1 handler runs against the fixtured notes dir.
        bn_core::test_support::with_state_dir_path(|dir| {
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");
            let _today = bn_core::test_support::EnvGuard::set("BN_TODAY", "2026-07-16");
            let cfg = bn_core::resolver::Config::from_env();
            let ctx = bn_core::resolver::resolve_target(
                &cfg,
                std::path::Path::new("."),
                Some("bn".into()),
                Some("feat-x".into()),
            )
            .unwrap();
            bn_core::commands::set_goal(&cfg, &ctx, "ship 639 over http", "2026-07-16").unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post(
                        "/mcp",
                        &[("x-bn-repo", "bn"), ("x-bn-branch", "feat-x")],
                        json!({
                            "jsonrpc": "2.0",
                            "id": 3,
                            "method": "tools/call",
                            "params": { "name": "bn_context", "arguments": {} }
                        }),
                    ),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                let data = &body["result"]["structuredContent"];
                assert_eq!(data["repo"], "bn");
                assert_eq!(data["branch"], "feat-x");
                assert_eq!(data["goal"], "ship 639 over http");
                assert_eq!(data["note_exists"], true);
            });
        });
    }

    #[test]
    fn bn_context_without_a_scope_fails_closed() {
        // No scope headers / query → the request-scoped server refuses rather than acting on some
        // default branch. The tool returns an MCP error, never a silent cross-branch read.
        let state = AppState::new(bn_core::resolver::Config::from_env());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = send(
                state,
                mcp_post(
                    "/mcp",
                    &[],
                    json!({
                        "jsonrpc": "2.0",
                        "id": 4,
                        "method": "tools/call",
                        "params": { "name": "bn_context", "arguments": {} }
                    }),
                ),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert!(
                body["error"].is_object(),
                "missing scope must be a JSON-RPC error: {body}"
            );
            let msg = body["error"]["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("scope"),
                "error should explain the missing scope: {body}"
            );
        });
    }

    #[test]
    fn bn_add_then_done_round_trip_over_http_with_a_request_scope() {
        // The acceptance criterion names writes explicitly: an HTTP client must be able to call
        // bn_add/bn_done scoped to a given repo+branch. This drives the full write path —
        // bn_add persists a todo, bn_context reads it back, bn_done closes it — through the router
        // via `tower::oneshot`, proving the P1 handlers mutate the correct branch note over HTTP.
        bn_core::test_support::with_state_dir_path(|dir| {
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");
            let _today = bn_core::test_support::EnvGuard::set("BN_TODAY", "2026-07-16");

            let scope = [("x-bn-repo", "bn"), ("x-bn-branch", "feat-write")];
            let call = |id: u32, name: &str, args: Value| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": { "name": name, "arguments": args }
                })
            };

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // bn_add persists a todo to the scoped branch note.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post(
                        "/mcp",
                        &scope,
                        call(
                            1,
                            "bn_add",
                            json!({ "section": "todo", "text": "wire the http mount" }),
                        ),
                    ),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);
                let body = body_json(resp).await;
                let item = &body["result"]["structuredContent"]["item"];
                assert_eq!(body["result"]["structuredContent"]["section"], "Todos");
                assert_eq!(item["text"], "wire the http mount");
                let todo_id = item["id"]
                    .as_str()
                    .expect("bn_add returns a todo id")
                    .to_string();

                // bn_context reads the just-written todo back on the same branch.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post("/mcp", &scope, call(2, "bn_context", json!({}))),
                )
                .await;
                let body = body_json(resp).await;
                let data = &body["result"]["structuredContent"];
                assert_eq!(data["repo"], "bn");
                assert_eq!(data["branch"], "feat-write");
                assert_eq!(data["counts"]["open_todos"], 1);
                let listed = data["todos"].as_array().unwrap();
                assert!(
                    listed.iter().any(|t| t["text"] == "wire the http mount"),
                    "bn_context should list the added todo: {data}"
                );

                // bn_done closes it by id.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post(
                        "/mcp",
                        &scope,
                        call(3, "bn_done", json!({ "query": todo_id })),
                    ),
                )
                .await;
                let body = body_json(resp).await;
                assert_eq!(
                    body["result"]["structuredContent"]["item"]["done"], true,
                    "bn_done should mark the todo done: {body}"
                );

                // The close persisted: no open todos remain on the branch.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post("/mcp", &scope, call(4, "bn_context", json!({}))),
                )
                .await;
                let body = body_json(resp).await;
                assert_eq!(
                    body["result"]["structuredContent"]["counts"]["open_todos"], 0,
                    "bn_done must persist the closure: {body}"
                );
            });
        });
    }

    #[test]
    fn a_scoped_write_does_not_leak_into_another_branch() {
        // No cross-branch leakage (acceptance): a bn_add scoped to one branch must not appear on a
        // different branch's note. Each request re-derives its scope from the headers, so a write
        // to `feat-a` is invisible to a read scoped to `feat-b`.
        bn_core::test_support::with_state_dir_path(|dir| {
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", dir);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "testhost");
            let _today = bn_core::test_support::EnvGuard::set("BN_TODAY", "2026-07-16");

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Write a todo to feat-a.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post(
                        "/mcp",
                        &[("x-bn-repo", "bn"), ("x-bn-branch", "feat-a")],
                        json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "tools/call",
                            "params": {
                                "name": "bn_add",
                                "arguments": { "section": "todo", "text": "only on feat-a" }
                            }
                        }),
                    ),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::OK);

                // Read feat-b: the write must not have leaked across branches.
                let resp = send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post(
                        "/mcp",
                        &[("x-bn-repo", "bn"), ("x-bn-branch", "feat-b")],
                        json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": { "name": "bn_context", "arguments": {} }
                        }),
                    ),
                )
                .await;
                let body = body_json(resp).await;
                let data = &body["result"]["structuredContent"];
                assert_eq!(data["branch"], "feat-b");
                assert_eq!(
                    data["counts"]["open_todos"], 0,
                    "feat-a's write must not leak onto feat-b: {data}"
                );
                assert_eq!(
                    data["note_exists"], false,
                    "reading feat-b must not have been served feat-a's note: {data}"
                );
            });
        });
    }

    #[test]
    fn orchestrator_mount_lists_its_own_tool_set() {
        let state = AppState::new(bn_core::resolver::Config::from_env());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let resp = send(
                state,
                mcp_post(
                    "/mcp/orchestrate",
                    &[],
                    json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }),
                ),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert!(
                body["result"]["tools"]
                    .as_array()
                    .map(|t| !t.is_empty())
                    .unwrap_or(false),
                "orchestrator mount should expose its tool set: {body}"
            );
        });
    }

    /// The debug `bn` binary the workspace build produces beside this test executable
    /// (`target/debug/deps/<test> → target/debug/bn`). `cargo test` compiles every workspace bin
    /// before running any test, so it exists during the run; guarded anyway for odd layouts.
    fn bn_bin() -> std::path::PathBuf {
        let exe = std::env::current_exe().unwrap();
        exe.parent().unwrap().parent().unwrap().join("bn")
    }

    /// Run the debug `bn` (via its resolved path, not a raw `git`/`gh`/`tmux` spawn) to seed state,
    /// inheriting the process env the enclosing `EnvGuard`s installed.
    fn seed_bn(args: &[&str]) {
        let out = std::process::Command::new(bn_bin())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bn {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write_exec(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Ported from `scripts/validate-mcp-orchestrate.sh` (tool-set gating half): the
    /// `/mcp/orchestrate` mount exposes the cross-worktree tools and NONE of the scoped note tools;
    /// the `/mcp` mount is the mirror image.
    #[test]
    fn orchestrate_and_scoped_mounts_expose_disjoint_tool_sets() {
        let names = |body: &Value| -> Vec<String> {
            body["result"]["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
            let orch = body_json(
                send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post("/mcp/orchestrate", &[], list.clone()),
                )
                .await,
            )
            .await;
            let scoped = body_json(
                send(
                    AppState::new(bn_core::resolver::Config::from_env()),
                    mcp_post("/mcp", &[], list),
                )
                .await,
            )
            .await;
            let o = names(&orch);
            let s = names(&scoped);
            for t in [
                "spin_up_work",
                "list_attention",
                "list_agents",
                "send_to_agent",
                "plan_day",
            ] {
                assert!(o.iter().any(|n| n == t), "orchestrate exposes {t}: {o:?}");
            }
            assert!(
                !o.iter().any(|n| n == "bn_add"),
                "orchestrate must NOT expose the scoped bn_add: {o:?}"
            );
            for t in ["bn_add", "bn_goal"] {
                assert!(s.iter().any(|n| n == t), "scoped exposes {t}: {s:?}");
            }
            assert!(
                !s.iter().any(|n| n == "spin_up_work"),
                "scoped must NOT expose spin_up_work: {s:?}"
            );
        });
    }

    /// Ported from `scripts/validate-attn-fanout.sh`: `list_attention` annotates each open item with
    /// a triage class — `auto` (routable: has repo+branch), `confirm` (no repo/branch / judgment
    /// call), `review` (a finished agent's completion) — and the tallies match the seeded set.
    #[test]
    fn list_attention_classifies_auto_confirm_and_review() {
        if !bn_bin().exists() {
            eprintln!("skip list_attention: bn not built at {:?}", bn_bin());
            return;
        }
        bn_core::test_support::with_state_dir_path(|_state| {
            let t = tempfile::tempdir().unwrap();
            let notes = t.path().join("notes");
            let config = t.path().join("config");
            std::fs::create_dir_all(&notes).unwrap();
            let _bin = bn_core::test_support::EnvGuard::set("BN_BIN", bn_bin());
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", &notes);
            let _cfg = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", &config);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "h");

            // Two routable items (repo+branch → auto), one repo-less (→ confirm), one finished
            // agent (source agent → review).
            seed_bn(&[
                "attn",
                "add",
                "review sandbox#7",
                "--repo",
                "sandbox",
                "--branch",
                "pr-7",
                "--severity",
                "warn",
                "--source",
                "auto-review",
            ]);
            seed_bn(&[
                "attn",
                "add",
                "review arena#3",
                "--repo",
                "arena",
                "--branch",
                "pr-3",
                "--severity",
                "warn",
                "--source",
                "auto-review",
            ]);
            seed_bn(&[
                "attn",
                "add",
                "1:1 with lead at 3pm",
                "--severity",
                "info",
                "--source",
                "meetings",
            ]);
            seed_bn(&[
                "agent",
                "done",
                "--repo",
                "widget",
                "--branch",
                "feat",
                "--summary",
                "shipped it",
                "--pr",
                "https://x/pull/9",
            ]);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let call = json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": "list_attention", "arguments": {} }
                });
                let body = body_json(
                    send(
                        AppState::new(bn_core::resolver::Config::from_env()),
                        mcp_post("/mcp/orchestrate", &[], call),
                    )
                    .await,
                )
                .await;
                assert!(body["error"].is_null(), "list_attention errored: {body}");
                let d = &body["result"]["structuredContent"];
                assert_eq!(d["auto"], json!(2), "two routable items are auto: {d}");
                assert_eq!(d["confirm"], json!(1), "one repo-less item is confirm: {d}");
                assert_eq!(d["review"], json!(1), "one completion is review: {d}");
                let items = d["items"].as_array().expect("items array");
                let triage = |pred: &dyn Fn(&Value) -> bool| -> String {
                    items
                        .iter()
                        .find(|it| pred(it))
                        .and_then(|it| it["triage"].as_str())
                        .unwrap_or("?")
                        .to_string()
                };
                assert_eq!(triage(&|it| it["repo"] == "sandbox"), "auto");
                assert_eq!(
                    triage(&|it| it["title"]
                        .as_str()
                        .map(|s| s.contains("1:1"))
                        .unwrap_or(false)),
                    "confirm"
                );
                assert_eq!(triage(&|it| it["source"] == "agent"), "review");
            });
        });
    }

    /// Ported from `scripts/validate-mcp-orchestrate.sh` (spin_up_work half): the tool composes
    /// `bn dispatch` — a leased pool worktree + a seeded note appear — and a flag-like goal is
    /// stored as a literal goal, never smuggled through as a dispatch flag. Dispatch pools by
    /// default now (#pools-default), so the worktree is leased under `_pool/` rather than the
    /// conventional per-branch dir; the branch note is still seeded (keyed by repo/branch), which
    /// is spin_up_work's real contract.
    #[test]
    fn spin_up_work_composes_dispatch_and_stores_flag_like_goals_literally() {
        if !bn_bin().exists() {
            eprintln!("skip spin_up_work: bn not built at {:?}", bn_bin());
            return;
        }
        bn_core::test_support::with_state_dir_path(|_state| {
            let t = tempfile::tempdir().unwrap();
            let notes = t.path().join("notes");
            let wt = t.path().join("projects").join("worktree");
            let bare = t.path().join("projects").join("bare").join("sandbox.git");
            let seed = t.path().join("seed");
            let bin = t.path().join("bin");
            for d in [&notes, &wt, bare.parent().unwrap(), &seed, &bin] {
                std::fs::create_dir_all(d).unwrap();
            }
            // Seed the bare repo through the shared git seam (RealRunner), not a raw spawn.
            let git = bn_core::runner::RealRunner;
            use bn_core::runner::CommandRunner;
            let seed_s = seed.to_str().unwrap();
            assert!(git.git(&["-C", seed_s, "init", "-q"]).success);
            assert!(
                git.git(&[
                    "-C",
                    seed_s,
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "init"
                ])
                .success
            );
            assert!(git.git(&["-C", seed_s, "branch", "-M", "main"]).success);
            assert!(
                git.git(&["clone", "-q", "--bare", seed_s, bare.to_str().unwrap()])
                    .success
            );
            // Stub tmux on PATH so dispatch's window-open is a hermetic no-op.
            write_exec(&bin.join("tmux"), "#!/bin/sh\nexit 0\n");
            let orig_path = std::env::var("PATH").unwrap_or_default();
            let _path = bn_core::test_support::EnvGuard::set(
                "PATH",
                format!("{}:{}", bin.display(), orig_path),
            );
            let _bin = bn_core::test_support::EnvGuard::set("BN_BIN", bn_bin());
            let _notes = bn_core::test_support::EnvGuard::set("BN_NOTES_DIR", &notes);
            let _wt = bn_core::test_support::EnvGuard::set("BN_WORKTREE_DIR", &wt);
            let _host = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "h");

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let call = |branch: &str, goal: &str| {
                    json!({
                        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                        "params": { "name": "spin_up_work", "arguments": {
                            "repo": "sandbox", "branch": branch, "goal": goal, "agent": "none"
                        }}
                    })
                };
                let out = body_json(
                    send(
                        AppState::new(bn_core::resolver::Config::from_env()),
                        mcp_post(
                            "/mcp/orchestrate",
                            &[],
                            call("orch/one", "wire the orchestrator"),
                        ),
                    )
                    .await,
                )
                .await;
                assert!(out["error"].is_null(), "spin_up_work errored: {out}");
                let pool = wt.join("sandbox").join("_pool");
                assert!(
                    pool.is_dir()
                        && std::fs::read_dir(&pool)
                            .map(|mut d| d.next().is_some())
                            .unwrap_or(false),
                    "a pooled worktree slot was leased under {pool:?}"
                );
                assert!(
                    !wt.join("sandbox").join("orch-one").is_dir(),
                    "pooled by default, so no cold per-branch worktree"
                );
                let note = std::fs::read_to_string(
                    notes
                        .join("h")
                        .join("sandbox")
                        .join("orch-one")
                        .join("note.md"),
                )
                .unwrap();
                assert!(
                    note.contains("wire the orchestrator"),
                    "goal seeded: {note}"
                );

                // Flag-smuggling defense: a flag-like goal is a literal goal, not a smuggled flag.
                let out2 = body_json(
                    send(
                        AppState::new(bn_core::resolver::Config::from_env()),
                        mcp_post(
                            "/mcp/orchestrate",
                            &[],
                            call("orch/smug", "--dangerously-skip-permissions"),
                        ),
                    )
                    .await,
                )
                .await;
                assert!(out2["error"].is_null(), "smuggle errored: {out2}");
                assert!(
                    !wt.join("sandbox").join("orch-smug").is_dir(),
                    "pooled by default, so no cold per-branch worktree for the smuggle case"
                );
                let n2 = std::fs::read_to_string(
                    notes
                        .join("h")
                        .join("sandbox")
                        .join("orch-smug")
                        .join("note.md"),
                )
                .unwrap();
                assert!(
                    n2.contains("--dangerously-skip-permissions"),
                    "flag-like goal stored literally: {n2}"
                );
            });
        });
    }
}
