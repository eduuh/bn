//! `bn today` — the clap surface for Today's agent and the suggestions it feeds the queue.
//!
//! Its own file rather than another block in `cli.rs`: the accept/sync verbs each need a paragraph
//! to explain that accepting *queues*, and `cli.rs` is over the size ratchet and may not grow.
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum TodayCmd {
    /// Run Today's agent now (the manual trigger): gather the latest cron findings → synthesize
    /// suggested tasks → merge them into the reviewable store. Idempotent. `--json` emits the run
    /// report + current suggestions.
    Run {
        /// Days of findings to look back over (default 7).
        #[arg(long)]
        lookback_days: Option<i64>,
        /// Cap on suggestions synthesized per source's latest run (default 5).
        #[arg(long)]
        per_source: Option<usize>,
    },
    /// List suggested tasks, newest-first. Filter with `--status suggested|accepted|dismissed`.
    List {
        #[arg(long)]
        status: Option<String>,
        /// Most-recent-N cap (default 20, 0 = uncapped).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Accept a suggestion — and queue it. Accepting is the moment a proposal becomes work, so it
    /// mints the governing task, judges it against the SMART gate, and enqueues either the work or
    /// the enhance pass that resolves where the work happens. Idempotent: a second accept reports
    /// the entry already in flight rather than queueing a duplicate.
    Accept { id: String },
    /// Dismiss a suggestion — it stays recorded so the agent never re-proposes it.
    Dismiss { id: String },
    /// Reconcile accepted suggestions against the queue: enqueue any whose work is not in flight.
    /// Idempotent and safe to run repeatedly — this is what picks a suggestion back up after an
    /// enhance pass has resolved its repo, or after it was accepted while no dispatcher was running.
    Sync,
    /// Start, stop, or inspect Today's **hosted** agent — a live harness session in the tmux
    /// `planning` session, seeded with today's findings packet and steerable from the dashboard's
    /// Today page (or `bn agent send today:<date> "..."`).
    Agent {
        #[command(subcommand)]
        action: TodayAgentCmd,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum TodayAgentCmd {
    /// Launch Today's agent into the `planning` tmux session. Idempotent: a live agent for today
    /// is reported, not duplicated. The session is leased and its id recorded, so it is steerable
    /// (`bn agent send today:<date>`) the moment it comes up.
    Start {
        /// Harness to launch: copilot or claude. Omit to use the configured default
        /// (`[harness].default` / `$BN_HARNESS`).
        #[arg(long)]
        harness: Option<String>,
        /// Days of findings to seed the packet with (default 7).
        #[arg(long)]
        lookback_days: Option<i64>,
    },
    /// Stop Today's agent: close its tmux window and release its lease.
    Stop,
    /// Show the recorded agent joined with its live state (health, control channel).
    Status,
}
