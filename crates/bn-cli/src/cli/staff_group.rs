//! `bn briefing`/`week`/`quarter`/`staff`/`dojo`/… — the staff-agent command group.
//!
//! Split out of [`super`] under the never-grow ratchet: `cli.rs` is the clap declaration for
//! every command, so a whole flattened group is the largest thing that can move without
//! separating a command from its own flags. The group is still `#[command(flatten)]`-merged
//! back in the same position, so the CLI surface is byte-identical.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdStaff {
    /// Where the day stands, folded from every store into one packet: the night shift's draft PRs,
    /// the top `bn next` actions, the calendar's shape, the quarter, the `q` inbox, the daily
    /// rollup, the standup line and the wrap. `bn briefing gather` prints it as JSON — offline
    /// reads only, every source degrading on its own — and judges none of it. That is how the
    /// `staff-chief-am` and `staff-chief-pm` agents get their input.
    Briefing {
        #[command(subcommand)]
        action: BriefingCmd,
    },
    /// The week's raw material, gathered from every store: the staff feed's activity, merges and
    /// PR cycle time against the preceding week, daily-goal completion, logged achievements, the
    /// week's wins, the retro summary, and where the quarter stands. `bn week gather` prints it as
    /// JSON — offline reads only — and writes no narrative; that is the `staff-archivist` agent's
    /// job.
    Week {
        #[command(subcommand)]
        action: WeekCmd,
    },
    /// The evidence for what to practise next: the recurring themes a post-merge review kept
    /// naming, the dojo area never drilled, what the code is actually written in, the learn notes
    /// that already exist, and what the last few weeks chose (and whether those topics were ever
    /// finished). `bn weakness gather` prints it as JSON and picks nothing — choosing the week's
    /// one topic is the `staff-curriculum` agent's job.
    Weakness {
        #[command(subcommand)]
        action: WeaknessCmd,
    },
    /// Your own pull requests merged yesterday, with the material needed to talk about them: each
    /// one's diff (capped, largest-first) plus the branch note's goal and decisions — the *why* a
    /// post-hoc reader never otherwise has. `bn merged-prs gather` prints the packet as JSON and
    /// forms no opinion about it; that is how the `staff-mentor` agent gets its input.
    MergedPrs {
        #[command(subcommand)]
        action: MergedPrsCmd,
    },
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
        /// Run it here and now instead of queueing it — for when you are waiting on the result.
        #[arg(long)]
        now: bool,
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
    /// The outside world's week: upstream issues in subsystems this person actually works in,
    /// dependency releases and advisories, tooling changelogs. `bn radar gather` prints the packet
    /// as JSON — the one gather that touches the network, with each of its three collectors
    /// degrading independently and saying so. Ranking and filing it is the `scout` agent's job.
    Radar {
        #[command(subcommand)]
        action: RadarCmd,
    },
    /// Command Center activity against the daily and quarter goals: what the week actually went
    /// into, and what it was supposed to. `bn goals-align gather` prints the packet as JSON;
    /// judging the gap is the `goals-align` agent's job.
    GoalsAlign {
        #[command(subcommand)]
        action: GoalsAlignCmd,
    },
    /// The offline estate sweep: aging TODO/FIXME markers, stale dependencies, flaky verify
    /// history, worktree/branch cruft, and note hygiene across the `[sentinel] repos` allowlist
    /// (falling back to `[night] repos`). `bn estate gather` prints the packet as JSON and lands
    /// nothing — it reports what the estate looks like and leaves the judgment to whoever reads
    /// it, which is how the `sentinel` agent gets its nightly input.
    Estate {
        #[command(subcommand)]
        action: EstateCmd,
    },
    /// Agent staff framework: scheduled agents that gather a deterministic packet → dispatch a
    /// pinned skill → validate a briefing card → route its items into your existing surfaces
    /// (attention, daily goals, note sections, the dojo deck). Bare `bn staff` shows the roster
    /// and each agent's last-run health; `bn staff feed` reads the briefing cards.
    Staff {
        #[command(subcommand)]
        action: Option<StaffCmd>,
    },
    /// The Interviewer (#261) — the weekly 10-minute subjective retro. A *pull* agent: the
    /// `staff-interviewer` cron only raises the Friday attention item (never dispatches), and you
    /// run the session when ready. `bn retro chat` asks three fixed questions plus one adaptive
    /// follow-up derived from the week's defer/punch/daily history; `--voice` records spoken
    /// answers; `bn retro skip` records an explicit skip. Answers land as a private transcript in
    /// the notes repo plus a validated summary card in the feed. Bare `bn retro` runs the chat.
    Retro {
        #[command(subcommand)]
        action: Option<RetroCmd>,
    },
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
