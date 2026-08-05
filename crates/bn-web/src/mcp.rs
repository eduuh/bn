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
//! exact posture — no new auth system, and no weaker than the dashboard's REST routes.
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
/// The skill prompts are seeded from the global skill dirs
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
