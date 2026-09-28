//! `bn briefing`/`week`/`quarter`/`staff`/`dojo`/… — the staff-agent command group.
//!
//! Split out of [`super`] under the never-grow ratchet: `cli.rs` is the clap declaration for
//! every command, so a whole flattened group is the largest thing that can move without
//! separating a command from its own flags. The group is still `#[command(flatten)]`-merged
//! back in the same position, so the CLI surface is byte-identical.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdStaff {
    /// Away mode — batched fleet supervision. While away, routine agent activity is absorbed and
    /// only captain-relevant events (an agent that needs you / is stuck, an urgent attention item)
    /// count. `bn afk end` prints ONE batched digest of what needs you. Bare `bn afk` shows status.
    Afk {
        #[command(subcommand)]
        action: Option<AfkCmd>,
    },
    /// The wake watcher — a cheap (no-LLM) event reactor. Each pass gathers what needs you (agents
    /// Reacts to newly waiting/blocked/stuck agents, completions, and urgent attention with a toast.
    /// Filesystem events wake it; `--interval` is the fallback for time-based changes, and `--once`
    /// retains the single-pass cron path.
    Watch {
        /// Single pass then exit (schedule it as a cron), instead of looping.
        #[arg(long)]
        once: bool,
        /// Loop interval in seconds (ignored with --once).
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// Minutes of quiet before an agent counts as *stuck* (a wake reason).
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
    },
    /// Structured session resume packet (#120): the five layers — state snapshot, narrative,
    /// decisions made/deferred, open questions, priority queue — composed from the note and
    /// stored beside it. `bn handoff write` captures it (markdown + JSON); bare `bn handoff`
    /// (or `read`) prints the session-start orientation. Wire it at session end so the next
    /// session is oriented in one read.
    Handoff {
        #[command(subcommand)]
        action: Option<HandoffCmd>,
    },
    /// Switch harnesses mid-task without losing context: capture the session you're leaving (a
    /// live one if it's still running, else the last recorded), write the handoff resume packet,
    /// and print the orientation plus the command to start the target harness in this worktree.
    /// Run it when you hit a limit in one harness and want to finish in another. `bn switch
    /// copilot` / `bn switch claude`; bare `bn switch` picks the other harness from the one
    /// currently driving bn.
    Switch {
        /// Target harness to resume in: claude | copilot. Omit to pick the opposite of the
        /// harness currently driving bn (falling back to the review harness, else copilot).
        harness: Option<String>,
        /// Override the first prompt handed to the target harness (default: load the handoff
        /// packet, then continue the priority queue).
        #[arg(long)]
        prompt: Option<String>,
        /// Perform the swap instead of printing the launch command: grant the target harness
        /// folder trust, then relaunch it shell-backed in the worktree's tmux window, seeded to
        /// read the handoff. Falls back to printing when no tmux window is found.
        #[arg(long)]
        in_place: bool,
        /// Why you're switching, recorded to the metrics log so `bn metrics` can tell a
        /// rate-limit-driven hop from a manual one: limit | manual | stuck (default: manual).
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Run a persisted job script, recording its outcome to the event log: bn run <job>.
    /// Resolves branch → repo → global; pass --repo/--branch to target a branch from cron.
    Run {
        job: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Catch-up guard: skip (clean no-op, exit 0) when this job already had a successful
        /// run today, per the event log. Lets a job fire once on the first bn event of the day
        /// without double-running when its systemd timer already covered it.
        #[arg(long = "if-stale")]
        if_stale: bool,
    },
    /// Hosts this notes tree holds state for, and repairing a split between two of them.
    Machine {
        #[command(subcommand)]
        action: MachineCmd,
    },
    /// Cron observability and job scripts. Bare `bn cron` shows the unified dashboard.
    Cron {
        #[command(subcommand)]
        action: Option<CronCmd>,
        /// Target a specific note's jobs (else resolved from the current directory).
        #[arg(long, global = true)]
        repo: Option<String>,
        #[arg(long, global = true)]
        branch: Option<String>,
    },
    /// Fire every `trigger: startup` agent once when the tmux server boots (deduped on the tmux
    /// server PID). Wired from one `run-shell -b` line in `tmux.conf`; a no-op off tmux.
    Startup {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// The Interviewer (#261) — the weekly 10-minute subjective retro. A *pull* agent: the
    /// `staff-interviewer` cron only raises the Friday attention item (never dispatches), and you
    /// run the session when ready. `bn retro chat` asks three fixed questions plus one adaptive
    /// follow-up derived from the week's defer/punch/daily history; `--voice` records spoken
    /// answers; `bn retro skip` records an explicit skip. Answers land as a private transcript in
    /// the notes repo plus a validated summary card in the feed. Bare `bn retro` runs the chat.
    /// Workflow prompts (config/prompts): served to Claude Code as MCP prompts (`/bn:<name>`) and
    /// installed for Copilot CLI as native skills referred to by name. Bare `bn skills` lists them
    /// across every tier (global shared/personal, per-repo shared/personal); `bn skills new <name>`
    /// scaffolds one.
    Skills {
        #[command(subcommand)]
        action: Option<SkillsCmd>,
        /// Pick a skill in an fzf popup and launch it (`bn agent run`), instead of listing.
        /// Bare `bn skills` stays a plain list, because crons and scripts parse it.
        #[arg(long)]
        pick: bool,
    },
    /// Strict Markdown agent catalog. Bare `bn agents` lists every explicit kind and schedule;
    /// `validate` checks every visible manifest, `render` resolves a prompt body, and specialist
    /// show/install/bootstrap commands compile repo-gated reviewers for Copilot CLI / Claude Code.
    Agents {
        #[command(subcommand)]
        action: Option<AgentsCmd>,
        /// Pick an agent in an fzf popup and launch it (`bn agent run`), instead of listing.
        /// Bare `bn agents` stays a plain list, because crons and scripts parse it.
        #[arg(long)]
        pick: bool,
    },
    /// MCP server helpers. `bn mcp init <claude|copilot>` wires the current repo: the
    /// project-scoped MCP config(s) plus an instruction block in the harness's memory file
    /// (copilot wires both the VS Code extension and the Copilot CLI). Idempotent and
    /// committable — the per-repo counterpart to install.sh's global registration.
    Mcp {
        #[command(subcommand)]
        action: McpCmd,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum HandoffCmd {
    /// Capture the resume packet (markdown + JSON) beside the note from its current state, plus
    /// any author-supplied forward-looking layers: bn handoff write.
    Write {
        /// The narrative (layer 2): a few sentences on why it looks this way. Repeatable words
        /// are joined.
        #[arg(long, value_name = "TEXT", num_args = 1..)]
        narrative: Vec<String>,
        /// A decision deferred to the next session (layer 3). Repeat the flag for several.
        #[arg(long = "defer", value_name = "TEXT")]
        defer: Vec<String>,
        /// An extra open question beyond the note's asks (layer 4). Repeat for several.
        #[arg(long = "question", value_name = "TEXT")]
        question: Vec<String>,
        /// An explicit priority-queue entry (layer 5), in order. Repeat for the queue; when
        /// omitted the queue defaults to the note's open todos.
        #[arg(long = "next", value_name = "TEXT")]
        next: Vec<String>,
        /// Drop the previous packet's author layers instead of carrying forward the ones this
        /// write leaves empty (narrative, deferred decisions, questions, an explicit queue).
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Print the session-start orientation from the stored packet (default — same as bare
    /// `bn handoff`). Falls back to composing one from the note when none was written.
    Read {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
}
