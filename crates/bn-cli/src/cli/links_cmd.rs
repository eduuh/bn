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
    /// A value for a link's `{name}` placeholder, e.g. `--fill id=11047593`. Repeatable. A
    /// placeholder left unfilled is asked for on the terminal.
    #[arg(long, value_name = "NAME=VALUE")]
    pub(crate) fill: Vec<String>,
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
        /// Hide the link from every listing after this long: `7d`, `2w`, or bare days. For
        /// incident bridges and one-off meeting links. `bn links prune` deletes expired ones.
        #[arg(long)]
        ttl: Option<String>,
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
    /// Put a link in or take it out of groups without restating its other tags:
    /// `bn links tag <link> +standup -dri`. A bare tag adds, like `+`.
    Tag {
        /// Id or enough of a link's name, tag or URL.
        needle: String,
        /// `+tag` or `tag` to add, `-tag` to drop.
        #[arg(required = true, allow_hyphen_values = true)]
        changes: Vec<String>,
    },
    /// Print the store in picker order — the current repo's links first, then by frecency.
    List {
        /// Only links whose name, tags or URL contain this (case-insensitive) — how an agent
        /// checks for a saved URL before searching for it.
        query: Option<String>,
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
    /// Delete every expired link (one whose `--ttl` ran out) and say what went.
    Prune,
    /// Check every link and report it as ok, merged, closed, dead, sign-in or unknown. GitHub
    /// PRs and issues go through `gh api`; everything else is a request with a 5s timeout. A
    /// page that wants you signed in is reported as sign-in, never dead. Changes nothing.
    Check {
        /// Only links carrying this tag.
        #[arg(long)]
        tag: Option<String>,
    },
    /// Review the links the session hook saved without asking: keep, tag or drop each one.
    /// They are marked `•new` in the picker and on the note until you do.
    New,
    /// Open every link carrying a tag at once — a tag is a group. Tag links into one with
    /// `bn links add <url> --tags <group>`. The picker offers each tag on two or more links as
    /// a group row too.
    Group { tag: String },
    /// Save the PRs, issues and work items an agent session mentioned. Run by the Claude Stop and
    /// Copilot agentStop hooks, which name the session on stdin; `--transcript` or
    /// `--copilot-session` reads one instead. Silent
    /// and always exits 0, so it can never break a turn.
    #[command(hide = true)]
    Harvest {
        #[arg(long)]
        transcript: Option<std::path::PathBuf>,
        /// Read this Copilot session's `events.jsonl` instead of the hook's stdin.
        #[arg(long)]
        copilot_session: Option<String>,
        /// Print the URLs that would be saved instead of saving them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Open the store in `$EDITOR`. Fixing a name by hand is a supported workflow.
    Edit,
}
