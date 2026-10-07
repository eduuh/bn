//! `bn-mcp` — the per-worktree Model Context Protocol server for branch notes, spoken over
//! **stdio**. A third interface (after the CLI and nvim plugin) driving the same bn-core engine.
//!
//! This binary pins the [`bn_mcp`] tool-router to its launch cwd. `bn mcp serve` execs it;
//! `--orchestrate` selects the separate, opt-in cross-worktree server.

use anyhow::{Context, Result};
use bn_mcp::orchestrator::BnOrchestratorServer;
use bn_mcp::server::BnServer;
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
    let base_dir = std::env::current_dir().context("resolving bn-mcp launch worktree")?;

    // The tier list (personal / global / per-repo) lives in bn-core so the CLI and MCP share
    // one source of truth; here we just need the paths, served low→high precedence as prompts.
    let skill_dirs = bn_core::skills::skill_dirs(&cfg, &base_dir)
        .into_iter()
        .map(|(_, dir)| dir)
        .collect();

    // The stdio transport is fixed to its launch worktree: one scope for the whole session,
    // derived from cwd — so the scoped tools resolve the current branch without any client headers.
    let server = BnServer::new(cfg, base_dir, skill_dirs);
    let transport =
        discovery::with_legacy_discovery_fallback((tokio::io::stdin(), tokio::io::stdout()));
    let service = server
        .serve(transport)
        .await
        .context("starting bn-mcp stdio server")?;
    service.waiting().await?;
    Ok(())
}
