//! `bn tools`: managed docker-backed services (#653).
//!
//! Split out of [`crate::cli`] under the never-grow ratchet, to make room for `bn forge`.
use crate::*;

/// The `bn tools` subcommands (#653 — managed docker-backed services).
#[derive(Subcommand, Debug)]
pub(crate) enum ToolsCmd {
    /// List every declared tool (opted-in built-ins overlaid by `<config_dir>/tools.toml`) with
    /// its image.
    List,
    /// Report run-state + health for one tool, or all tools when the name is omitted.
    Status {
        /// Tool name (omit for every declared tool).
        name: Option<String>,
    },
    /// Bring a tool up: create the container if absent, start it if stopped (idempotent).
    Deploy {
        /// Tool name.
        name: String,
    },
    /// Re-assert the "up" desired state for one tool, or all tools when the name is omitted — the
    /// keep-alive primitive the `tools-reconcile` cron job calls.
    Reconcile {
        /// Tool name (omit to reconcile every declared tool).
        name: Option<String>,
    },
    /// Tear a tool down: `docker rm -f <name>` (idempotent — a missing container is not an error).
    Down {
        /// Tool name.
        name: String,
    },
}
