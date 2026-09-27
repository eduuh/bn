//! `bn-mcp` — the per-worktree Model Context Protocol server for branch notes, spoken over
//! **stdio**. A third interface (after the CLI and nvim plugin) driving the same bn-core engine.
//!
//! This binary is a thin stdio transport over the [`bn_mcp`] library: it pins one fixed
//! [`Scope::launch`](bn_mcp::Scope) to its launch cwd and serves the shared tool-router. Because the
//! scope comes from the directory the process starts in, a plain MCP client needs no per-call scope
//! headers — the branch identity is the launch worktree, the standard MCP pattern. `bn mcp serve`
//! execs this binary; the shared HTTP transport on `bn serve` (bn-web) remains for the dashboard and
//! the cross-worktree `/mcp/orchestrate` tools.

use anyhow::{Context, Result};
use bn_mcp::orchestrator::BnOrchestratorServer;
use bn_mcp::server::BnServer;
use bn_mcp::Scope;
use rmcp::ServiceExt;

mod discovery;
mod forge_call;

#[tokio::main]
async fn main() -> Result<()> {
    // `--forge-call`: a one-shot MCP *client* call to the Forge app for `bn forge-agent`
    // (see forge_call.rs). Checked first because it serves nothing.
    if std::env::args().any(|a| a == "--forge-call") {
        return forge_call::run().await;
    }

    // `--orchestrate` selects the opt-in CROSS-WORKTREE server (the "friend"): it composes the `bn`
    // CLI to manage every worktree from anywhere. Without the flag we serve the default scoped
    // server, locked to the launch worktree — the safety boundary an agent-in-a-repo relies on.
    // (A one-flag check keeps this crate arg-parser-free, matching its minimalism.)
    if std::env::args().any(|a| a == "--orchestrate") {
        let transport =
            discovery::with_legacy_discovery_fallback((tokio::io::stdin(), tokio::io::stdout()));
        let service = BnOrchestratorServer::new()
            .serve(transport)
            .await
            .context("starting bn-mcp --orchestrate stdio server")?;
        service.waiting().await?;
        return Ok(());
    }

    let cfg = bn_core::resolver::Config::load();
    let base_dir = std::env::current_dir().unwrap_or_default();

    // The tier list (personal / global / per-repo) lives in bn-core so the CLI and MCP share
    // one source of truth; here we just need the paths, served low→high precedence as prompts.
    let skill_dirs = bn_core::skills::skill_dirs(&cfg, &base_dir)
        .into_iter()
        .map(|(_, dir)| dir)
        .collect();

    // The stdio transport is fixed to its launch worktree: one scope for the whole session,
    // derived from cwd — so the scoped tools resolve the current branch without any client headers.
    let server = BnServer::new(cfg, Scope::launch(base_dir), skill_dirs);
    let transport =
        discovery::with_legacy_discovery_fallback((tokio::io::stdin(), tokio::io::stdout()));
    let service = server
        .serve(transport)
        .await
        .context("starting bn-mcp stdio server")?;
    service.waiting().await?;
    Ok(())
}
