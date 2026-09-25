//! `bn critic` — the adversarial diff-only review lane (#649 Phase 3).
//!
//! Split out of `cli.rs` under the never-grow file-size ratchet: the clap declaration file is the
//! one place every new command must touch, so a self-contained sub-enum is what moves out.
use crate::*;

/// The `bn critic` subcommands (#649 Phase 3).
#[derive(Subcommand, Debug)]
pub(crate) enum CriticCmd {
    /// Cut a fresh critic worktree off the PR head and launch a `--fresh`, plan-free critic lane
    /// over the branch's diff (base...head) + its done-when criteria.
    Dispatch {
        /// The execution branch under test (its head is the PR head; its note holds the done-when).
        #[arg(long)]
        branch: String,
        /// Target repo by name (default: the current repo). Lets you critique from anywhere.
        #[arg(long)]
        repo: Option<String>,
        /// The PR url under review, embedded in the critic's seed for reference.
        #[arg(long)]
        pr: Option<String>,
        /// Diff base to compare the branch against (default: main) — the seed shows base...head.
        #[arg(long, default_value = "main")]
        base: String,
        /// Critic harness: copilot (default) | claude. Claude is org-disabled here, so the critic
        /// defaults to copilot; the Claude path is designed but inert until the org toggle flips.
        #[arg(long)]
        agent: Option<String>,
        /// Don't open a tmux window / launch the agent — just cut the worktree and seed the note.
        #[arg(long)]
        no_tmux: bool,
    },
    /// Record the critic's ruling and drive the loop-back: PASS → shippable; FAIL → a
    /// `task_type=critique` item + re-enter execution (bounded by `--max-iterations`), then a
    /// `task_type=planning` human-steer escalation on exhaustion.
    Verdict {
        /// The execution repo the verdict is about (default: inferred from the current worktree).
        #[arg(long)]
        repo: Option<String>,
        /// The execution branch the verdict is about (default: inferred from the current worktree).
        #[arg(long)]
        branch: Option<String>,
        /// The diff passes every acceptance criterion — advance to shippable.
        #[arg(long, conflicts_with = "fail")]
        pass: bool,
        /// The diff fails — loop back to execution with the findings (or escalate when exhausted).
        #[arg(long)]
        fail: bool,
        /// The critic's findings text (what's wrong / unmet), forwarded to the re-executed lane.
        #[arg(long)]
        findings: Option<String>,
        /// Read the findings from a file instead of `--findings` (for long, multi-line reports).
        #[arg(long)]
        findings_file: Option<String>,
        /// Max critique rounds before escalating to a human-steer item (default 3).
        #[arg(long, default_value_t = bn_core::pipeline::critic::DEFAULT_MAX_ITERATIONS)]
        max_iterations: u32,
    },
}
