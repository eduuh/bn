//! `bn ref ...` clap surface, split from the top-level command catalog.

use crate::*;

/// The `--repo [NAME]` scope switch every `bn ref` form accepts. Absent means the person-scoped
/// standards store; bare `--repo` means the repo you are standing in; `--repo <name>` targets
/// another repo and works from anywhere, including outside a git repo.
///
/// `num_args(0..=1)` + `default_missing_value` is what makes the bare form legal: clap otherwise
/// demands a value for a flag that takes one.
#[derive(clap::Args, Debug, Clone)]
pub(crate) struct RefScopeArg {
    /// Read/write this repo's reference notes instead of the global standards store. Bare
    /// `--repo` uses the current repo; `--repo <name>` targets another one.
    #[arg(long, value_name = "REPO", num_args = 0..=1, default_missing_value = "")]
    pub repo: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum RefCmd {
    /// Open a reference in $EDITOR, creating it from a template if it doesn't exist yet:
    /// `bn ref edit javascript`, or `bn ref edit architecture/runtime-concepts --repo`.
    Edit {
        /// Reference name to open/create (exact name; the file is `<store>/<name>.md`). A repo
        /// note may nest: `architecture/runtime-concepts`.
        name: String,
        #[command(flatten)]
        scope: RefScopeArg,
    },
    /// Scaffold a new reference file from a small template: `bn ref add react`. Reports if one
    /// already exists (no overwrite).
    Add {
        /// Reference name to create (the file is `<store>/<name>.md`).
        name: String,
        #[command(flatten)]
        scope: RefScopeArg,
    },
    /// Record one durable fact in a repo's reference notes, as a bullet under a section:
    /// `bn ref append build-and-run --repo --section gotchas "port 3001 serves the client
    /// chunks (observed, 2026-09-03)"`. Creates the note if it doesn't exist yet, and reports
    /// (without writing) when that exact fact is already recorded — the write path an agent
    /// uses mid-session, so a discovery lands where the next session will find it.
    Append {
        /// Reference name to write to (created from the template if missing).
        name: String,
        /// The fact, as one line. Include where it came from and when you checked it.
        #[arg(required = true)]
        text: Vec<String>,
        /// Section heading to append under: `facts` (default), `gotchas`, or any other.
        #[arg(long, value_name = "SECTION")]
        section: Option<String>,
        #[command(flatten)]
        scope: RefScopeArg,
    },
}
