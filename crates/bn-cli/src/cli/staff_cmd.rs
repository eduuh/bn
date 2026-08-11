//! `bn staff` — the LLM agents that gather, think, and file: their run pipeline, the packets they
//! read, and the briefing feed they write.
//!
//! Scheduled execution enters through `bn run <name>`, which resolves to the same pipeline these
//! subcommands drive by hand — so what you can inspect here is exactly what a cron runs.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum StaffCmd {
    /// Run one staff-kind agent's full pipeline — gather → dispatch → validate → route. Scheduled
    /// execution enters through `bn run <name>` and resolves to this same pipeline.
    Run { agent: String },
    /// Dry-run: print the gathered packet and the sinks the agent is allowed to route into, with
    /// no dispatch and nothing landed: bn staff plan <agent>.
    Plan { agent: String },
    /// Print an agent's deterministic packet JSON to stdout — the built-in gatherers an
    /// agent `gather` command shells back into (`bn staff gather <agent>`).
    Gather { agent: String },
    /// Read briefing cards, unread-first. Bare `bn staff feed` is the index; a filter matching an
    /// agent name or a `YYYY-MM-DD` date renders the full matching card(s) and marks them read.
    Feed {
        /// Filter to one agent name or one YYYY-MM-DD date.
        filter: Option<String>,
        /// Print just the count of today's unread cards — the cheap tmux 📮-badge source (reads
        /// the feed dir + local read-marks; no git, no network). Ignores any filter.
        #[arg(long)]
        unread_count: bool,
        /// Emit the tmux status-bar badge fragment (`📮N`) for today's unread cards, or nothing
        /// when there are none or a `bn focus` block is active. For tmux `#(…)`.
        #[arg(long)]
        badge: bool,
        /// Interactive fzf briefing popup (prefix-B): cards unread-first, ↵ render + mark read,
        /// m mark read, ctrl-o open link, ctrl-r reload. Plain index when fzf is absent.
        #[arg(long)]
        pick: bool,
        /// Internal: the fzf callback stage a `--pick` session re-invokes (list/preview/…).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<staff_pick::StaffPickStage>,
        /// Internal: a focused row's hidden column, repeated (date, agent, path) for a
        /// `--pick-stage` callback.
        #[arg(long = "pick-col", hide = true)]
        pick_cols: Vec<String>,
    },
    /// Put a briefing card into Gmail as a reviewable draft (never sent), and mark it read:
    /// bn staff mail <agent> [YYYY-MM-DD]. Needs a `bn-gmail` app password in the macOS keychain;
    /// without one it says so and leaves the card in the feed.
    Mail {
        /// The agent whose card to draft.
        agent: String,
        /// The card's date (default: that agent's most recent card).
        date: Option<String>,
    },
    /// Cost ledger rolled up: spend by agent for a month, each vs its per-agent budget, and the
    /// whole-roster total vs the global monthly cap: bn staff cost [<YYYY-MM>].
    Cost {
        /// Month to roll up as `YYYY-MM` (default: the current month).
        month: Option<String>,
    },
    /// Apply the store-retention policy (#275): compact fully-elapsed quarters of feed cards into a
    /// per-quarter headline index (deleting the per-day JSON/MD) and roll up `staff_run` ledger
    /// rows older than a year into monthly aggregates. Idempotent; chronicles and retro transcripts
    /// are exempt. Prints a growth measurement either way: bn staff archive [--dry-run].
    Archive {
        /// Measure and report what would change, without writing or deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
}
