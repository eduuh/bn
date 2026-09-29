//! `bn links` — the store of links worth going back to: a PBI, a PR, a portal blade, a docs
//! site, a dashboard.
//!
//! Bare `bn links` is the picker and `bn links <query>` opens a match directly, so the
//! subcommands here are only the things that are not "open one" — which is why the parent
//! variant carries `args_conflicts_with_subcommands` (the `bn ref` shape).
use crate::*;

/// `bn links`' own arguments: bare, a query, a tag filter, or one of [`LinksCmd`].
#[derive(clap::Args, Debug)]
pub(crate) struct LinksArgs {
    #[command(subcommand)]
    pub(crate) action: Option<LinksCmd>,
    /// Enough of a link's name, tag or URL to identify it. Omit to open the picker.
    pub(crate) query: Option<String>,
    /// Show only links carrying this tag in the picker, e.g. `--tag icm`.
    #[arg(long)]
    pub(crate) tag: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum LinksCmd {
    /// Store a link. The name defaults to the host plus the last path segment; re-adding a URL
    /// you already have updates it in place rather than storing it twice. Nothing is fetched —
    /// the name is yours to write. An unlisted tag warns and is still saved.
    Add {
        url: String,
        /// What to call it in the picker. Defaults to host + last path segment.
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated tags. See `bn links tags` for the configured vocabulary.
        #[arg(long)]
        tags: Option<String>,
        /// Repository this link belongs to — its links sort first when you are in that repo.
        /// Defaults to the repo you are standing in; `--repo ""` stores it repo-agnostic.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Copy links to the clipboard as hyperlinks, ready to paste into Teams or Outlook: the name
    /// shows, the URL is behind it. A paste into plain text gets `name url` instead. Several
    /// links copy as a bulleted list.
    Copy {
        /// Id or enough of a link's name, tag or URL. Omit with `--tag` to copy every link
        /// carrying that tag.
        needle: Option<String>,
        /// Copy every link with this tag, in picker order.
        #[arg(long)]
        tag: Option<String>,
        /// Copy plain `name url` text only, no HTML.
        #[arg(long)]
        plain: bool,
    },
    /// Drop a link by id or by enough of its name.
    Rm { needle: String },
    /// Print the store in picker order — the current repo's links first, then by frecency.
    List {
        /// Only links carrying this tag.
        #[arg(long)]
        tag: Option<String>,
        /// Only links belonging to this repo.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Store the branch note's PR and work item, tagged with the repo — the one command that
    /// makes "capture the links before the worktree goes away" cost nothing.
    Capture,
    /// The configured tag vocabulary, and how many stored links carry each tag.
    Tags,
    /// Open the store in `$EDITOR`. Fixing a name by hand is a supported workflow.
    Edit,
}
