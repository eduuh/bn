//! `bn control` — the long-lived coordination-only controller agents.
//!
//! Split out of `cli.rs` under the never-grow file-size ratchet: the clap declaration file is the
//! one place every new command must touch, so a self-contained sub-enum is what moves out.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum ControlCmd {
    /// Start a controller, or enqueue a request onto an already-running controller with --id.
    Start {
        /// Stable controller id. Omit to generate a new id.
        #[arg(long)]
        id: Option<String>,
        /// Readable task name used by the session registry and tmux window.
        #[arg(long)]
        name: Option<String>,
        /// Harness to launch: copilot or claude. Omit to use the configured default
        /// (`[harness].default` / `$BN_HARNESS`, else copilot).
        #[arg(long)]
        harness: Option<String>,
        /// The agent manifest whose body seeds the controller. Its name lives here rather than in
        /// Rust so the seed can be swapped by editing `config/agents/`, not by a rebuild.
        #[arg(long, default_value = "controller")]
        agent: String,
        /// Initial request to enqueue for this controller.
        #[arg(value_name = "REQUEST")]
        request: Vec<String>,
    },
    /// Show controllers, their durable request queues, and worker lanes joined by controller_id.
    Status {
        /// Show one controller only.
        #[arg(long)]
        id: Option<String>,
    },
    /// Stop one controller and close its tmux window.
    Stop {
        #[arg(long)]
        id: String,
    },
    /// Lease the next durable request. Internal controller-agent protocol.
    #[command(hide = true)]
    Lease {
        #[arg(long)]
        id: String,
    },
    /// Mark a leased request done or failed. Internal controller-agent protocol.
    #[command(hide = true)]
    Complete {
        #[arg(long)]
        id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        failed: bool,
    },
    /// Tail owned-worker events and wake the controller. Internal controller-agent protocol.
    #[command(hide = true)]
    Watch {
        #[arg(long)]
        id: String,
    },
    /// Guard a controller turn from ending while live work has lost its watcher.
    #[command(hide = true)]
    TurnEndGuard {
        #[arg(long)]
        id: Option<String>,
    },
}
