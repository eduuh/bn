//! bn-mcp — the branch-notes Model Context Protocol surface. A third interface (after the CLI and
//! nvim plugin) driving the same bn-core engine.
//!
//! This crate is a *library* exposing the rmcp [`ServerHandler`](rmcp::ServerHandler) tool-router
//! ([`server::BnServer`]) plus the per-request [`Scope`] that maps a call onto a `(repo, branch)`
//! note context. `bn serve` (bn-web) mounts the handler over Streamable HTTP with a
//! [`Scope::Request`] server that re-derives each call's scope from the request headers/query
//! (epic #637). The sibling `bn-mcp` binary remains the per-worktree stdio transport used by
//! `bn mcp serve`; it pins the same handler to the launch worktree with [`Scope::launch`].

pub mod orchestrator;
mod prompts;
mod scope;
pub mod server;

pub use scope::Scope;
