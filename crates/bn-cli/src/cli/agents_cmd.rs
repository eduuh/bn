//! `bn agents ...` clap surface, split from the top-level command catalog.

use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum AgentsCmd {
    /// List every effective Markdown agent with its explicit kind and schedule.
    List,
    /// Strictly load and validate every visible Markdown agent manifest.
    Validate,
    /// Render an agent's Markdown body with a strict JSON argument map.
    Render {
        name: String,
        /// Inline JSON object containing declared template argument values.
        #[arg(long, value_name = "JSON", conflicts_with_all = ["args_file", "stdin"])]
        args: Option<String>,
        /// Read the JSON argument object from this file (`-` reads stdin).
        #[arg(long, value_name = "PATH", conflicts_with_all = ["args", "stdin"])]
        args_file: Option<std::path::PathBuf>,
        /// Read the JSON argument object from stdin.
        #[arg(long, conflicts_with_all = ["args", "args_file"])]
        stdin: bool,
    },
    /// Preview the compiled native agent for `name` (Copilot `.agent.md` by default).
    Show {
        name: String,
        /// Render the Claude Code variant instead of the Copilot one.
        #[arg(long)]
        claude: bool,
    },
    /// Scaffold a new specialist source file: bn agents new <name> --repo <r> [--source <ado-repo>].
    New {
        name: String,
        /// Repository this specialist is gated to (its description/instruction gate).
        #[arg(long, value_name = "REPO")]
        repo: String,
        /// One-line description shown to the harness (synthesized from --repo when omitted).
        #[arg(long = "desc", value_name = "TEXT")]
        description: Option<String>,
        /// ADO repo whose merged-PR review comments `bn agents bootstrap` mines.
        #[arg(long, value_name = "ADO_REPO")]
        source: Option<String>,
        /// Tier to scaffold into: global-personal | global-shared (default global-personal).
        #[arg(long, default_value = "global-personal")]
        scope: String,
        /// Replace an existing specialist of the same name in that tier.
        #[arg(long)]
        overwrite: bool,
    },
    /// (Re)compile the discovered specialists into native Copilot/Claude agent files.
    Install {
        /// Install into the current repo's dirs instead of globally.
        #[arg(long)]
        repo: bool,
        /// Restrict to one world: copilot | claude (default both).
        #[arg(long, value_name = "WORLD")]
        harness: Option<String>,
    },
    /// Remove only bn-managed native agent files.
    Uninstall {
        /// Uninstall from the current repo's dirs instead of the global ones.
        #[arg(long)]
        repo: bool,
        /// Restrict to one world: copilot | claude (default both).
        #[arg(long, value_name = "WORLD")]
        harness: Option<String>,
    },
    /// Mine merged PR review comments into this specialist's review-standards reference.
    Bootstrap {
        /// Specialist whose review standards to mine.
        name: String,
    },
}
