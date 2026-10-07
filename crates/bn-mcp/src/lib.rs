//! bn-mcp — the branch-notes Model Context Protocol surface. A third interface (after the CLI and
//! nvim plugin) driving the same bn-core engine.
//!
//! The [`server::BnServer`] tool-router is pinned to its launch worktree and served over stdio
//! by `bn mcp serve`. The separate [`orchestrator`] surface remains explicitly opt-in.

pub mod orchestrator;
mod prompts;
pub mod server;
