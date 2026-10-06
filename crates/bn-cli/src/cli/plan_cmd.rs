//! `bn plan ...` clap surface, split from the top-level command catalog.

use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum PlanCmd {
    /// Create the plan artifact directories and active plan file.
    Init,
    /// Print a plan artifact path.
    Path {
        /// Print the active plan path (default).
        #[arg(long)]
        active: bool,
        /// Print/create this agent session's draft plan path.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Open the active plan in $EDITOR.
    Open,
    /// Open this branch's newest plan HTML in the local browser. With `--host`, the one an agent on
    /// that machine made (copied over ssh); with `--port`, a live page forwarded from it instead.
    View {
        /// The machine the plan is on, as `ssh <machine>` knows it (see `bn tunnels`).
        #[arg(long)]
        host: Option<String>,
        /// Forward this port from `--host` and open http://localhost:<port> (a live page).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Show plan artifact status for the current branch.
    Status,
    /// Promote a draft relative path or session id to plans/active.md.
    Promote { draft: String },
    /// Create plans/active.md from legacy note sections when no plan file exists.
    Migrate,
}
