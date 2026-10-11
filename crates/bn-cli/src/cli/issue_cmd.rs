//! `bn issue` subcommands. Split out of [`super`] under the never-grow ratchet.

use super::IssueKindArg;
use clap::Subcommand;

/// `bn issue` subcommands (#345 proposal 4 — auto-file deduped, labeled bn issues).
#[derive(Subcommand, Debug)]
pub(crate) enum IssueCmd {
    /// File a labeled GitHub issue for an identified problem, deduped against the repo's OPEN
    /// issues by title so re-running never opens a duplicate. Prints the issue URL (new or the
    /// matched existing one). E.g. `bn issue file "watch reactor wedges on a stale lock" \
    /// --kind bug --body "seen on dispatch-345 …"`.
    File {
        /// The issue title (one-line problem statement). The dedup key.
        #[arg(required = true, value_name = "TITLE")]
        title: Vec<String>,
        /// Issue body / details (Markdown). Optional — a title-only issue is fine.
        #[arg(long, short = 'b')]
        body: Option<String>,
        /// Problem kind, which seeds a canonical label: `bug`, `friction`, `blocker`, or
        /// `enhancement` (default). Combined with any explicit `--label`s.
        #[arg(long, value_enum, default_value_t = IssueKindArg::Enhancement)]
        kind: IssueKindArg,
        /// Extra label to apply (repeatable). Added on top of the `--kind` label.
        #[arg(long = "label", short = 'l', value_name = "LABEL")]
        labels: Vec<String>,
        /// Target repo as `owner/name` (default: the current dir's origin, via gh).
        #[arg(long)]
        repo: Option<String>,
        /// File even when an open issue with the same title already exists (skip the dedup guard).
        #[arg(long)]
        allow_duplicate: bool,
    },
    /// Split one oversized GitHub issue into independently shippable, conflict-scoped child
    /// issues, then start an agent on each child (never the parent). An agent first triages the
    /// bundle into repeated `--part "TITLE::GLOB[,GLOB]"` specifications; bn rejects overlapping
    /// touch scopes and links every child to the parent.
    Decompose {
        /// Parent issue number or URL to split.
        issue: String,
        /// One independently shippable child as `TITLE::GLOB[,GLOB]`. Repeat at least twice.
        #[arg(long, required = true, value_name = "TITLE::GLOB[,GLOB]")]
        part: Vec<String>,
        /// Target repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
        /// Validate and print the decomposition without creating issues or starting agents.
        #[arg(long)]
        dry_run: bool,
    },
}
