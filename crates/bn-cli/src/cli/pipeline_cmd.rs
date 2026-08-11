//! `bn pipeline` — the per-worktree stage machine's clap surface.
//!
//! Its own file rather than another block in `cli.rs`: the sweep verb needs a paragraph to explain
//! that it enqueues one entry per stage and honours the review gate, and `cli.rs` is over the size
//! ratchet and may not grow.
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum PipelineCmd {
    /// Advance every task in the pipeline by one stage: enqueue the next stage's work for each task
    /// whose current stage has finished. One queue entry per stage, each running under that stage's
    /// own model, effort and harness, and all of them pinned to the task's single worktree. A stage
    /// configured `gate = "hold"` is not queued until its plan review is approved. Idempotent — the
    /// backlog is the state, so re-running enqueues only what is actually missing.
    Sweep,
    /// Show the target worktree's current stage + full transition history. Default subcommand.
    Status {
        /// Target repo (default: the current worktree's repo).
        #[arg(long)]
        repo: Option<String>,
        /// Target branch (default: the current worktree's branch).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Transition the target worktree to an explicit stage
    /// (pickup|investigate|plan|consensus|execute|validate|critique|ship|learn).
    Set {
        /// The stage to move to.
        stage: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Advance the target worktree to the next stage in the flow (errors at the terminal stage).
    Advance {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Complete or contribute to the investigate-first stage.
    Investigate {
        #[command(subcommand)]
        action: PipelineInvestigateCmd,
    },
    /// List every worktree's current stage, newest transition first.
    Board,
    /// Print the current worktree's compact `glyph stage` for the tmux status row (legacy
    /// worktrees default to pickup). Takes an optional path (tmux passes `#{pane_current_path}`).
    Badge { path: Option<String> },
}

/// The investigate-stage write protocol (PIPE-B, EPIC #746).
#[derive(Subcommand, Debug)]
pub(crate) enum PipelineInvestigateCmd {
    /// Finish the primary investigation. Remaining unknowns fan out to focused fresh agents;
    /// a human blocker raises planning attention; otherwise the task advances to plan.
    Finish {
        /// Target repo (default: the current worktree's repo).
        #[arg(long)]
        repo: Option<String>,
        /// Target branch (default: the current worktree's branch).
        #[arg(long)]
        branch: Option<String>,
        /// One unresolved focused question. Repeat once per unknown.
        #[arg(long)]
        unknown: Vec<String>,
        /// Why this task cannot be scoped without a person's input.
        #[arg(long, conflicts_with = "unknown")]
        blocked: Option<String>,
        /// Build the fan-out records and seeds without opening tmux agents.
        #[arg(long)]
        no_tmux: bool,
    },
    /// Submit one focused sub-investigator's answer back to the parent task.
    Contribute {
        /// Parent task repo.
        #[arg(long)]
        repo: String,
        /// Parent task branch.
        #[arg(long)]
        branch: String,
        /// Fan-out token printed in the sub-investigator seed.
        #[arg(long)]
        token: String,
        /// Evidence-backed finding. Repeat once per finding.
        #[arg(long)]
        finding: Vec<String>,
        /// Why this focused question still requires a person's input.
        #[arg(long, conflicts_with = "finding")]
        blocked: Option<String>,
    },
}
