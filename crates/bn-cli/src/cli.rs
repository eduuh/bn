//! The clap command surface: the top-level `Command` enum and every `*Cmd`
//! subcommand enum. Split out of `main.rs` (issue #333); `main.rs` keeps the
//! `Cli` parser struct and the dispatch `match`. Glob-re-exported by `main.rs`.
use crate::*;

/// `bn dispatch`'s value enums: under the never-grow ratchet, a command's args move out first.
mod dispatch_args;
pub(crate) use dispatch_args::OnPoolFull;

mod agent;
pub(crate) use agent::AgentCmd;
mod agents_cmd;
pub(crate) use agents_cmd::AgentsCmd;
mod forge_cmd;
pub(crate) use forge_cmd::ForgeCmd;
mod task;
pub(crate) use task::TaskCmd;
mod tracking_group;
pub(crate) use tracking_group::CmdTracking;
mod pipeline_cmd;
pub(crate) use pipeline_cmd::{PipelineCmd, PipelineInvestigateCmd};
mod plan_cmd;
mod ref_cmd;
pub(crate) use plan_cmd::PlanCmd;
pub(crate) use ref_cmd::{RefCmd, RefScopeArg};
mod skills_cmd;
pub(crate) use skills_cmd::SkillsCmd;
mod repos_cmd;
pub(crate) use repos_cmd::ReposCmd;
mod critic_cmd;
pub(crate) use critic_cmd::CriticCmd;
mod on_cmd;
pub(crate) use on_cmd::OnCmd;
mod control_cmd;
pub(crate) use control_cmd::ControlCmd;
mod verify_args;
pub(crate) use verify_args::VerifyArgs;
pub(crate) mod coord_group;
pub(crate) use coord_group::{CmdCoord, CompactCmd, ContextCmd, ProofCmd, WatchdogCmd};
mod issue_kind;
pub(crate) use issue_kind::IssueKindArg;

// The clap surface is split into flattened group sub-enums purely to bound the per-enum
// stack frame that clap-derive's generated `augment_subcommands` allocates in unoptimized
// (debug/test) builds: one frame for the ~100-variant enum crept past libtest's 2 MiB
// thread stack (issue #359). `#[command(flatten)]` merges the groups back into one flat
// namespace IN ORDER, and the groups are contiguous slices of the original declaration
// order, so the CLI surface is byte-identical: every command stays top-level `bn <cmd>`,
// the group names never appear, and help/completions list commands in the same order.
// The split is mechanical (by position, no semantic meaning); to keep ordering stable,
// append a new command to the LAST group (or start a new one) rather than inserting mid-list.
#[derive(Subcommand, Debug)]
pub(crate) enum CmdIntake {
    /// Add an item to a section: bn add <section> <text...>
    /// Sections: todo, blocker, decision, research, collab, ask, done-when (acceptance criteria).
    Add {
        section: String,
        #[arg(required = false)]
        text: Vec<String>,
        /// Interactively prompt for the item text on the tty (one line; empty cancels). Used by
        /// the tmux quick-capture popups (prefix a/b); replaces workflow/bin/bn-capture.sh.
        #[arg(long)]
        prompt: bool,
        /// Target a specific note instead of the current dir's (pair with --branch).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Idempotent add: skip if any existing item in the section contains this key.
        #[arg(long, value_name = "KEY")]
        if_absent: Option<String>,
        /// Attach a time estimate (`90m`, `2h`, `1h30m`) for calibration (`bn calib`). Stamped on
        /// the item; the actual is derived from the punch log when you close it.
        #[arg(long, value_name = "DUR")]
        est: Option<String>,
        /// Tag this item to a quarter goal by its key (`bn add todo … --q ship-v1`), linking
        /// shipped work to a `bn quarter` goal so the impact agent can score it.
        #[arg(long, value_name = "GOAL_KEY")]
        q: Option<String>,
    },
    /// Capture an issue/PR URL as a branch-note item (default: a todo), carrying its resolved
    /// title + URL so it flows into `bn next` and becomes dispatchable. GitHub issues/PRs resolve
    /// their title via `gh`; anything else falls back to a readable tail of the URL.
    #[command(visible_alias = "cap")]
    Capture {
        /// The issue or PR URL to capture.
        url: String,
        /// Section to file it under (default: todo).
        #[arg(long, short = 's', default_value = "todo")]
        section: String,
    },
    /// Zero-friction global capture: bn q "<thought>" banks it in the inbox with a timestamp
    /// and repo/branch hint — no questions, no git ops. Bare `bn q` lists the inbox; routing
    /// happens later via `bn q drain` (also run by daily rollover and shown at focus end).
    #[command(visible_alias = "quick", args_conflicts_with_subcommands = true)]
    Q {
        #[command(subcommand)]
        action: Option<QCmd>,
        /// The thought to bank.
        text: Vec<String>,
    },
    /// Dispatch an item into an isolated worktree — from anywhere: resolve a todo id / issue-PR URL
    /// / free text, create a worktree off the base branch, seed that branch's note with the item as
    /// its goal, and open a window on it in the repo's tmux session. The execution end of `bn next`
    /// — what to do → doing it. Target another repo with `--repo <name>` (works outside any repo).
    Dispatch {
        /// A todo id (e.g. t3), an issue/PR URL, or free text describing the work.
        item: Vec<String>,
        /// Readable task name used by the session registry and tmux window.
        #[arg(long)]
        name: Option<String>,
        /// Target repo by name (a bare repo under the bare root). Lets you spin up work on any repo
        /// from anywhere — no need to be inside it, or inside any repo. Defaults to the current one.
        #[arg(long)]
        repo: Option<String>,
        /// Base branch to fork the new worktree from (default: main).
        #[arg(long)]
        base: Option<String>,
        /// Branch to create (default: derived from the item as dispatch/<slug>).
        #[arg(long)]
        branch: Option<String>,
        /// Don't open a tmux window even when inside tmux.
        #[arg(long)]
        no_tmux: bool,
        /// Launch an AI agent in the worktree's window, seeded with the note to deliver the task end
        /// to end in one `ship-solo` session (investigate → implement → review, asking you when
        /// unclear). `--agent` alone lets the launch stage's harness decide (falling back to copilot);
        /// `--agent claude` (aka claudecode) or `--agent copilot` pins it. Opens the repo's tmux session.
        #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
        agent: Option<String>,
        /// Scout task: investigate / reproduce / audit and write findings to the branch note
        /// first. Once the evidence makes the change clear, the agent may land a bounded fix in
        /// the same session and open a PR; otherwise it finishes report-only.
        #[arg(long)]
        scout: bool,
        /// Mark this a fleet **orchestrator** (#500): a long-lived supervisory agent that dispatches
        /// and monitors the wave itself and ships no PR. Attributes the run as `orchestration` work,
        /// so the dashboard pins it in the distinct Orchestrators tier (role comes from the agent
        /// record, never a UI guess). For the monitor role; a normal ship agent is a worker.
        #[arg(long)]
        orchestrator: bool,
        /// Workflow skill to seed the agent with (see `bn skills`): its rendered body becomes the
        /// agent's process, closing with a `bn reflect "skill:<name> — …"` line so evolve-skills
        /// learns from the run. Defaults: `ship-solo` (ship) / `investigate` (--scout); pick
        /// `debug` for a repro-first bugfix. `--skill investigate` implies a scout task.
        #[arg(long)]
        skill: Option<String>,
        /// Gate "done" on verification: instruct the agent to run `bn verify` (the repo's
        /// build/test/lint gate) and only report done once it passes. Ignored for --scout; the
        /// scout workflow itself decides whether a code-changing outcome must verify before it
        /// ships.
        #[arg(long)]
        verify: bool,
        /// Trusted-repo autonomy: launch the agent WITHOUT approval prompts (claude yolo / copilot
        /// allow-all) so a dispatched agent runs clean instead of wedging on first-run trust/MCP
        /// prompts. Your explicit opt-in — only for repos you trust, since the seed runs unconfirmed.
        #[arg(long)]
        yolo: bool,
        /// Plan-review gate: independent reviewers vote on the proposed plan. Low-risk majority
        /// auto-advances; sensitive or no-consensus plans wait for `bn review approve <id>`.
        /// Opt in per-dispatch; `[dispatch].plan_review = true` makes it the default. `--yolo`
        /// also uses this gate so trusted autonomy cannot bypass sensitive-plan review.
        #[arg(long)]
        plan: bool,
        /// Suppress the plan-review gate for this dispatch, including the one `--yolo` and
        /// `[dispatch].plan_review` would otherwise raise. For self-contained mechanical work whose
        /// whole job is already stated in the prompt — repo setup is the built-in case — where a
        /// plan to review adds a round trip and reviews nothing anyone would change. Refused with
        /// `--plan`: asking for the gate and its opposite in one command is a mistake, not a
        /// preference.
        #[arg(long = "no-plan-review", conflicts_with = "plan")]
        no_plan_review: bool,
        /// The pipeline stage this dispatch runs as (pickup|investigate|plan|consensus|execute|
        /// validate|critique|ship|learn). Selects `[pipeline.stages.<stage>]` — its model, effort
        /// and harness. Omit and bn infers it from `--scout`/`--skill`, which can only ever mean
        /// investigate, plan or execute; pass it explicitly so the six other stages run under their
        /// own profile instead of borrowing execute's.
        #[arg(long)]
        stage: Option<bn_core::pipeline::stage::Stage>,
        /// Attach specific references (coding standards, see `bn ref`) to the agent's seed, so it
        /// writes code to your standards — repeatable (`--ref javascript --ref commit-style`) or
        /// comma-separated. Omit to attach ALL references by default; pass `--ref -` (or an empty
        /// value) to attach none. Each name is listed with a hint to run `bn ref <name>` for the
        /// full text, keeping the seed compact.
        #[arg(long = "ref", value_name = "NAME")]
        refs: Vec<String>,
        /// Force a brand-new harness session, ignoring any recorded one for this worktree. By
        /// default `--agent` resumes a worktree's prior session (a human returning to it continues
        /// where they left off); on a recovery re-dispatch that prior session is idle/dead, so
        /// resuming it just restores an empty prompt (#383). `--fresh` re-seeds a new session that
        /// actually resumes the work — used by `bn orchestrate`'s hard-restart.
        #[arg(long)]
        fresh: bool,
        /// Keep the repo's *project* MCP servers enabled for a dispatched Copilot agent. By
        /// default a dispatched Copilot boots with project MCP servers suppressed (each passed to
        /// `copilot --disable-mcp-server`), so a failing or auth-requiring repo MCP can't wedge the
        /// agent on "Loading: … MCP servers" before it reaches its prompt (#384) — bn's own server
        /// is always kept. Pass this when the repo's MCPs are known-good and the task needs them.
        /// No effect on `--agent claude` (Claude has native strict-MCP handling).
        #[arg(long)]
        keep_project_mcps: bool,
        /// Opt out of pooling: cut a fresh cold worktree instead of leasing a warm `bn wt pool`
        /// slot. Leasing a reap-safe pool slot is the default dispatch path — every repo with a
        /// bn-managed bare clone gets a warm slot (deps/build cache intact), sized from
        /// the slots that exist, floored at four and grown on demand. Pass `--no-pool` for
        /// a throwaway one-off, or when you specifically want an isolated cold checkout. A repo with
        /// no bare clone (or a full pool) already falls back to cold automatically.
        #[arg(long)]
        no_pool: bool,
        /// Reserve a second warm pool slot detached at the latest remote primary branch.
        /// The feature and comparison slots are leased and returned together. Requires pooling.
        #[arg(long, conflicts_with = "no_pool")]
        with_main: bool,
        /// What to do when every warm pool slot is leased. `queue` (default) schedules the item on
        /// the repo's orchestrate run so it starts as soon as a slot frees; `cold` cuts a fresh
        /// worktree anyway (the old behaviour — unbounded, since nothing ever reclaims those);
        /// `fail` reports `pool_full` and does nothing, for scripted callers that want to decide.
        /// A repo with no bn-managed bare clone is not "full" — it was never poolable — and always
        /// goes cold.
        #[arg(long, value_name = "MODE", default_value = "queue")]
        on_pool_full: OnPoolFull,
        /// Bind this dispatch to a task id (`t42`) from the global store, stamping the task's
        /// `repo:`/`branch:` so the fleet can show which task each running agent is on and link
        /// through to it. Without it the worktree is ungoverned — the agent runs, but nothing
        /// records what it is for.
        #[arg(long, value_name = "ID")]
        task: Option<String>,
        /// Deprecated no-op: leasing a warm pool slot is now the default, so `--pool` no longer does
        /// anything (kept hidden so existing scripts/callers that still pass it don't break). Use
        /// `--no-pool` to opt out.
        #[arg(long, hide = true)]
        pool: bool,
        /// Run the agent in THIS pane instead of a background tmux lane (#claude-wrap): the harness
        /// takes over the current terminal, and the moment it exits any leased `--pool` slot is
        /// handed straight back. Powers the `claude`/`copilot` shell wrappers (`bn shell-init`) —
        /// an interactive, throwaway investigation. Requires `--agent`; local-only (no tmux window,
        /// agent-lease, or controller event), so it conflicts with `--no-tmux`/`--orchestrator`.
        #[arg(
            long,
            requires = "agent",
            conflicts_with_all = ["no_tmux", "orchestrator"]
        )]
        attach: bool,
    },
    /// Promote a scout worktree to a ship: record the flip in the branch note and tell the agent it
    /// may now implement (and open a PR). Run inside the scout's worktree, or target it with
    /// --repo/--branch. The follow-through after a scout's report checks out.
    Promote {
        /// Target repo (with --branch) instead of the current worktree.
        #[arg(long)]
        repo: Option<String>,
        /// Target branch (with --repo).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Run this worktree's verification gate (all available `build`, `test`, and `lint` jobs) and record the
    /// PASS/FAIL to the branch note's Progress. Exits non-zero on failure, so an agent can gate
    /// "done" on it (see `bn dispatch --verify`). Runs in the current worktree, like `bn build`.
    Verify(VerifyArgs),
    /// The no-mistake pre-land gate: rebase this worktree's HEAD onto a freshly-fetched
    /// `origin/main` in a *disposable* worktree, then run the repo's aggregate verify gate on THAT
    /// rebased head. Exits non-zero unless green, so an agent self-gates before
    /// reporting done and `bn wt merge` refuses to land a red or stale-base PR. Kills the "green on
    /// a stale base" class: the suite runs against current main + your changes, in an isolated
    /// checkout — your own worktree is never touched.
    Gate(VerifyArgs),
    /// Why does this code exist? Blame a file (or inspect a commit) and resolve its `BN-Note:`
    /// trailers back to the branch notes that produced it — goal, decisions, matching done
    /// todos — including closed and archived notes. The read side of `bn hooks install`.
    Why {
        /// A file path, or a commit-ish (sha, HEAD~2, a branch).
        target: String,
        /// Limit blame to a line range: `-L 120` or `-L 100,140` (file targets only).
        #[arg(short = 'L', value_name = "START[,END]")]
        lines: Option<String>,
    },
    /// Git-hook integration: stamp commits with `BN-Note:` trailers linking them to this
    /// branch's note — the provenance `bn why` reads back.
    Hooks {
        #[command(subcommand)]
        action: HooksCmd,
    },
    /// Pre-grant an agent harness's folder-trust for a path (default: cwd), so a dispatched agent
    /// boots without the interactive "trust this folder?" prompt. Your opt-in for worktrees you
    /// made from your own repos — it rides on `bn dispatch --yolo`. Edits ~/.claude.json /
    /// ~/.copilot/config.json.
    TrustFolder {
        /// Folder to trust (default: current directory).
        path: Option<String>,
        /// Harness to trust for: claude | copilot | both (default).
        #[arg(long, default_value = "both")]
        harness: String,
    },
    /// Git worktree manager (bare+worktree layout): clone, add, list, remove, go, fetch, remote,
    /// prune, cleanup. Port of the `wt` script. `wt go` prints a path for the shell wrapper to cd
    /// into; `wt remote` creates worktrees from your open PRs opened on other machines.
    Wt {
        #[command(subcommand)]
        action: wt::WtCmd,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdReview {
    ReviewPr {
        #[command(subcommand)]
        action: review_pr::ReviewPrCmd,
    },
    /// Adversarial diff-only review gate (#607): before a dispatched ship PR may land, ≥N
    /// independent reviewers inspect the DIFF ONLY (no plan/reasoning) and must approve the current
    /// head with every blocking finding resolved. `bn review-gate record` is the reviewer's write
    /// path; `resolve`/`waive` clear findings; `status`/`list` read; `prompt` emits the diff-only
    /// reviewer seed. Enforced on the ship-lane land path when `[review].diff_gate` is on.
    #[command(name = "review-gate")]
    ReviewGate {
        #[command(subcommand)]
        action: ReviewGateCmd,
    },
    /// Mark a todo done by id or substring: bn done <id|substring...> [--pct N]
    Done {
        #[arg(required = true)]
        query: Vec<String>,
        /// Record partial progress (1-99) instead of closing; 100 completes. Shown in
        /// `bn todo`/`bn --cat` so long-running items surface incremental progress.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
        pct: Option<u8>,
    },
    /// Pull this branch's session result back into its note + metrics: summarize the commits and
    /// diffstat vs the base (default main) into the note's Progress, and log an `ingest` event.
    /// Closes the dispatch loop — what got done flows back to where it was planned.
    Ingest {
        /// Branch to ingest (default: the current worktree's branch).
        #[arg(long)]
        branch: Option<String>,
        /// Base ref to compare against (default: main).
        #[arg(long, default_value = "main")]
        base: String,
    },
    /// Read the note's Goal, or set it. No args reads (with --json: `{ "goal" }`);
    /// `bn goal <text...>` sets it.
    Goal { text: Vec<String> },
    /// Record something learned today (codebase, tooling): bn learned <text...>
    Learned { text: Vec<String> },
    /// Record a reflection — one thing to improve next time: bn reflect <text...>
    Reflect { text: Vec<String> },
    /// Inspect what the dispatched fleet has learned (#322): aggregate every `bn learned` /
    /// `bn reflect` entry across the daily history — each stamped with the repo/branch/issue/
    /// harness it came from — grouped by theme (skill > repo > general), with agent-vs-mine and
    /// review state. Because learnings live on the date axis they survive `bn wt cleanup`, so an
    /// archived note's insights stay reviewable. Filters narrow the window/provenance;
    /// `bn learnings promote|dismiss <key|substring>` is the human-in-the-loop keep/drop. Bare
    /// `bn learnings` shows the anchored weekly window; read-only and network-free.
    Learnings {
        #[command(subcommand)]
        action: Option<LearningsCmd>,
        /// Rolling window of N days instead of the configured weekly anchor.
        #[arg(long)]
        since: Option<i64>,
        /// Widen to the entire daily history (overrides the window).
        #[arg(long)]
        all: bool,
        /// Only learnings recorded in this repo (basename, e.g. `bn`).
        #[arg(long)]
        repo: Option<String>,
        /// Only skill reflections whose `skill:<name>` tag contains this substring.
        #[arg(long)]
        skill: Option<String>,
        /// Only entries I recorded at a plain shell (no harness driving bn).
        #[arg(long, conflicts_with = "agent")]
        mine: bool,
        /// Only entries a dispatched agent recorded (a harness was driving bn).
        #[arg(long)]
        agent: bool,
    },
    /// Print the current branch note's path (creates it if missing).
    Note,
    /// Print the current branch note (action view) — the subcommand form of `bn --cat`, so the
    /// standard resume prompt's `bn cat` works on the CLI, not just the MCP `bn_cat` tool.
    Cat,
    /// Plan-review gate (#349): the human-in-the-loop checkpoint on a dispatched agent's plan. A
    /// `bn dispatch --plan` agent proposes a plan, registers it here, and pauses before implementing
    /// / opening a PR. Bare `bn review` lists; `approve` unblocks
    /// the agent, `request-changes --notes` loops it back, `reject` stops it. `submit`/`status` are
    /// the agent's write/poll side of the gate. Distinct from `bn reviews` (the PR review queue).
    Review {
        #[command(subcommand)]
        action: Option<ReviewCmd>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdPlanning {
    /// Manage branch-scoped plan artifacts under the bn note directory.
    Plan {
        #[command(subcommand)]
        action: Option<PlanCmd>,
    },
    /// List investigation files alongside the current note.
    Files,
    /// Active branch notes with their on-disk worktree dirs (JSON for the nvim picker;
    /// text delegates to the richer bash rollup).
    Worktrees,
    /// List the configured managed repos (clone URLs from `[managed_repos]`), one per line —
    /// the refresh-all job clones/refreshes these under the managed root. `--dir` prints that
    /// root instead (`~/.bn/repos`), so a shell job can pin `WT_ROOT` to it.
    ManagedRepos {
        /// Print the managed-repos root dir instead of the URL list.
        #[arg(long)]
        dir: bool,
    },
    /// List open todos across all branches.
    Todo,
    /// The task list — one global store for every repo (`<work_root>/tasks.txt`, todo.txt
    /// format). Bare `bn task` shows what's actionable today: work dated today or earlier, plus
    /// anything unscheduled. `--all` is the whole backlog; `--repo .` (or a name) narrows to one
    /// repo. No git repo needed — it works from anywhere. Distinct from `bn todo` (the
    /// branch-note rollup), which stays per-branch.
    #[command(visible_alias = "t")]
    Task {
        #[command(subcommand)]
        action: Option<TaskCmd>,
        /// Show the whole backlog, not just what's actionable today (applies to the listing).
        #[arg(long, global = true)]
        all: bool,
        /// Only this repo. `.` means the repo you're standing in.
        #[arg(long, global = true, value_name = "REPO")]
        repo: Option<String>,
    },
    /// Per-branch rollup (open-todo + blocker counts).
    List,
    /// Per-branch rollup with the open todos and blockers.
    Summary,
    /// Daily goals — a day-scoped, cross-branch todo list. Bare `bn daily` shows today
    /// (carrying forward yesterday's unfinished goals); `bn daily add/done/edit` manage it.
    #[command(visible_alias = "day")]
    Daily {
        #[command(subcommand)]
        action: Option<DailyCmd>,
        /// View a specific past day (YYYY-MM-DD) instead of today. Only for the bare view.
        #[arg(value_name = "DATE")]
        date: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdInsights {
    /// Search all notes and design docs (active + archived): bn search <query...>
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        /// Max hits to return (default 50).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// PR review queue: the open PRs awaiting your review and your own, from a local cache; `--fetch` refreshes that cache from GitHub, the write side the `pr-queue` agent runs on a schedule.
    Reviews {
        /// Print the path to the review cache file, then exit.
        #[arg(long)]
        path: bool,
        /// Refresh the cache from GitHub instead of reading it. Needs an authenticated `gh`; a
        /// missing one is reported and exits 0, since a machine without it is not broken.
        #[arg(long)]
        fetch: bool,
    },
    /// Informational-cron inbox: items surfaced by feed jobs (PRs to review, today's meetings,
    /// …), grouped by source. Bare `bn feed` renders; `bn feed ingest <source>` is the write path
    /// a feed cron's script pipes JSON into.
    Feed {
        #[command(subcommand)]
        action: Option<FeedCmd>,
    },
    /// Productivity metrics over the event log: PR completion rate, cycle time, throughput.
    Metrics {
        /// Trailing window in days (default 30).
        #[arg(long, default_value_t = bn_core::metrics::DEFAULT_WINDOW_DAYS)]
        since: i64,
        /// Write today's scorecard to the snapshot store (the weekly trend job runs this).
        #[arg(long)]
        snapshot: bool,
        /// Print the snapshot history (close-out lag, WIP, throughput over time) instead.
        #[arg(long)]
        trend: bool,
    },
    /// Append a productivity event to the synced metrics log: bn event <kind> [--field k=v]...
    /// The write path for job scripts (e.g. pr-observe → `bn event pr_merged --field number=52`).
    Event {
        /// Event kind (e.g. pr_opened, pr_merged, pr_closed, deploy, pr_review).
        kind: String,
        /// Event fields as key=value (repeatable). All-digit values are stored as numbers.
        #[arg(long = "field", value_name = "K=V")]
        fields: Vec<String>,
        /// Epoch-seconds timestamp; defaults to now.
        #[arg(long)]
        ts: Option<i64>,
    },
    /// Attention queue: notes with stale/blocked/no-goal unfinished work.
    Doctor {
        /// Flag notes untouched for at least this many days.
        #[arg(long, default_value_t = bn_core::doctor::DEFAULT_STALE_DAYS)]
        stale_days: i64,
        /// Print a single summary line (for hooks); nothing when all clear.
        #[arg(long)]
        brief: bool,
        /// Interactive fzf attention popup (prefix C-a): the `doctor` queue with a live preview —
        /// ↵ open · ctrl-a ack · ctrl-s snooze · ctrl-r reload. Plain list when fzf is absent.
        #[arg(long)]
        pick: bool,
        /// Internal: the fzf callback stage a `--pick` session re-invokes (list/preview/…).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<picker::PickStage>,
        /// Internal: the focused row's hidden columns, passed to a `--pick-stage` callback.
        #[arg(hide = true, allow_hyphen_values = true)]
        pick_fields: Vec<String>,
    },
    /// What to do now: one ranked action list folding cron failures, attention items, PRs
    /// awaiting your review, your stale in-flight PRs, today's open goals, and blocked notes.
    Next {
        /// How many actions to show (the full ranked list always goes to --json).
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Print a single summary line (for the SessionStart hook); nothing when all clear.
        #[arg(long)]
        brief: bool,
        /// Read the precomputed attention cache instead of recomputing — an instant open for the
        /// prefix-J picker. Self-heals on a cache miss (computes once, then caches).
        #[arg(long)]
        cached: bool,
        /// Recompute and rewrite the attention cache, then exit silently. Drives the
        /// `attention-sync` cron; nothing is printed.
        #[arg(long)]
        refresh: bool,
        /// Delete the precomputed attention cache, then exit. The next cached read recomputes it.
        #[arg(long)]
        clear_cache: bool,
        /// Pure priority/staleness order — skip the momentum tiebreak that ranks actions on the
        /// currently-punched-in repo/branch first within each band (#246). Also the effective
        /// behavior whenever no activity is punched in.
        #[arg(long)]
        no_momentum: bool,
        /// Size the plan to the gap until your next meeting (#247): show only daily goals whose
        /// expected duration (`est` × your `bn calib` multiplier) fits the time left, minus a
        /// short wrap-up buffer. Un-estimated goals move to an "unsized" tail rather than hide;
        /// broken/waiting bands are never filtered. A no-op when no meeting is upcoming.
        #[arg(long)]
        fit: bool,
    },
    /// The gap until your next meeting, read from the offline meetings cache the `meetings` cron
    /// maintains (#247) — no live calendar call. `bn gap` prints `38m until standup (10:30)`, or
    /// `no upcoming events` when there's no fresh upcoming event (a stale cache reads as none).
    /// `bn gap ingest` is the cron's write path: JSON/JSONL of `{start,title,url}` on stdin.
    Gap {
        #[command(subcommand)]
        action: Option<GapCmd>,
    },
    /// The cron findings store (#576): durable, queryable records of what crons *gather*, not just
    /// transient log output. Where the feed mirrors current state (a run replaces its source), the
    /// findings store keeps history — every run appends `{source, run_id, gathered_at, items,
    /// raw?}`. `bn findings record <source>` is the write path (a script cron pipes JSON in, an
    /// agent calls the same path); `bn findings query`/`bn findings sources` read it back.
    Findings {
        #[command(subcommand)]
        action: FindingsCmd,
    },
}

/// Automation and orchestration commands — see [`crate::cli::automation_cmd`].
pub(crate) mod automation_cmd;
pub(crate) mod cron_cmd;
pub(crate) mod forge_app;
pub(crate) mod links_cmd;
pub(crate) mod machine_cmd;
pub(crate) mod shell_ui_cmd;
pub(crate) mod staff_group;
pub(crate) mod worker_cmd;
pub(crate) use automation_cmd::CmdAutomation;
pub(crate) use cron_cmd::CronCmd;
pub(crate) use forge_app::ForgeAppCmd;
pub(crate) use links_cmd::{LinksArgs, LinksCmd};
pub(crate) use machine_cmd::MachineCmd;
pub(crate) use shell_ui_cmd::CmdShellUi;
pub(crate) use staff_group::{CmdStaff, HandoffCmd};
pub(crate) use worker_cmd::WorkerCmd;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdGitPr {
    /// Convert legacy notes (split/md-only) to the single-file format.
    Migrate,
    /// Relocate legacy <repo>/<branch> notes into the <machine>/<repo>/<branch> subtree.
    MigrateLayout {
        /// Report what would move without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
        /// Reconcile collisions by recency (newer note.md wins) instead of skipping them.
        #[arg(long)]
        force: bool,
    },
    /// Link/open the PR for the current branch. `bn pr <url>` records it; bare `bn pr`
    /// resolves the stored PR (or `gh pr view`) and opens it (--copy / --show to copy/print).
    Pr {
        /// PR URL to record. Omit to read/open the stored (or gh-discovered) PR.
        url: Option<String>,
        /// Copy the URL to the clipboard instead of opening it.
        #[arg(long, short = 'c')]
        copy: bool,
        /// Print the URL instead of opening it.
        #[arg(long, short = 's')]
        show: bool,
    },
    /// Link the work item for the current branch. `bn link <id>` records it; bare
    /// `bn link` prints the stored work-item id.
    #[command(visible_alias = "wi")]
    Link {
        /// Work-item id to record. Omit to read the stored work item.
        id: Option<String>,
    },
    /// Append to (or, with no text, list) the note's free-text `## Notes` section.
    #[command(visible_alias = "n")]
    Notes { text: Vec<String> },
    /// Append to (or, with no text, list) the note's `## Test Cases` section.
    #[command(visible_aliases = ["testcase", "testcases"])]
    Tc { text: Vec<String> },
    /// Append a timestamped bullet to the note's `## Progress` section (SessionEnd hook).
    LogProgress {
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Move the screenshot(s) you just captured into the branch note's folder and embed them
    /// under `## Screenshots`. Grabs every file newer than the freshness window; `--gif` grabs
    /// ScreenToGif recordings instead. Source folders come from `[capture]` in config.toml.
    Shot {
        /// Grab GIF recordings (from `[capture].gif_dir`) instead of screenshots.
        #[arg(long)]
        gif: bool,
        /// Freshness window in seconds — only files newer than this are grabbed.
        #[arg(long, default_value_t = 120)]
        fresh: u64,
    },
    /// Git diff stat vs the branch's base, its note's open todos + blockers, and the commit-count
    /// ahead. A quick "where am I" — for this branch, or for any other worktree via `--path` /
    /// `--repo`+`--branch`, which is what makes it usable as a picker preview.
    #[command(visible_alias = "d")]
    Diff {
        /// Describe a different worktree: any directory inside it.
        #[arg(long, value_name = "DIR", conflicts_with_all = ["repo", "branch"])]
        path: Option<std::path::PathBuf>,
        /// Describe another repo's branch by name (with `--branch`). Resolved through the note
        /// rollup, so a pooled slot reports its real checkout.
        #[arg(long, requires = "branch")]
        repo: Option<String>,
        /// The branch to describe (with `--repo`).
        #[arg(long, requires = "repo")]
        branch: Option<String>,
        /// Base ref to diff against. Default: `origin/HEAD`, else the branch's upstream, else the
        /// first of origin/main, main, origin/master, master that resolves.
        #[arg(long)]
        base: Option<String>,
        /// Use `<base>...HEAD` — changes on this branch since it diverged — instead of the
        /// historical `<base>..HEAD`, which reports the base's own newer commits as yours.
        #[arg(long)]
        three_dot: bool,
    },
    /// Open a URL (or file) in the system browser, cross-platform (WSL → Windows). Prints the
    /// URL as a fallback when no opener works, so it's safe as a `$BROWSER` command. Replaces the
    /// `workflow/bin/wf-open` helper.
    Open { url: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdMisc {
    /// Remove empty-text placeholder items from the current note (or --all notes).
    Clean {
        /// Clean every active note instead of just the current branch's.
        #[arg(long)]
        all: bool,
    },
    /// Close every active note whose checked-out worktree was removed, then archive old
    /// closed notes (worktree-managed repos only). Housekeeping for the cross-branch views.
    Prune,
    /// Active notes untouched for at least N days (by note activity date), most-idle first.
    Stale {
        /// Idle-days threshold (default 14).
        #[arg(long, default_value_t = 14)]
        days: i64,
    },
    /// Move closed notes older than N days into the root-level archive/ tree (recoverable).
    Archive {
        /// Age threshold in days against the note's created date (default 30).
        #[arg(long, default_value_t = 30)]
        days: i64,
        /// Report what would move without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a shell completion script: bn completion <bash|zsh|fish|elvish|powershell>.
    /// Source it from your shell rc, e.g. `source <(bn completion zsh)`.
    Completion {
        /// Shell to generate completions for.
        shell: Shell,
    },
    /// Print the shell wrappers that make agent placement enforceable: inside a git repo, `claude`
    /// and `copilot` route through `bn agent launch`, which keeps the main worktree agent-free
    /// (asking what the work is and dispatching into a warm pool slot instead) and refuses a second
    /// agent in a worktree that already has one. Outside a repo they refuse to start, so every
    /// session — including planning — is backed by a repository branch. `agency claude` /
    /// `agency copilot` remain the explicit pooled-scout form. Source from your shell rc, e.g.
    /// `source <(bn shell-init zsh)` (bash|zsh).
    ShellInit {
        /// Shell to emit wrappers for (bash or zsh).
        shell: Shell,
    },
    /// Tear down every non-main worktree across all bare repos and close their notes. Bare `bn
    /// reset` is a dry run; `bn reset --force` rebases each repo's main, removes the worktrees,
    /// closes the notes, prunes stale refs, and archives. Port of the bash `reset`.
    Reset {
        /// Actually remove the worktrees (default: dry run).
        #[arg(long)]
        force: bool,
    },
    /// Rename the current branch and move its note folder atomically: `git branch -m`, move the
    /// note dir, update the note's `branch` field, and repush (deleting the old upstream ref).
    /// Port of the bash `rename`/`mv`.
    #[command(visible_alias = "mv")]
    Rename {
        /// The new branch name.
        new_branch: String,
    },
    /// Run a worktree's build script — `<note>/jobs/<name>.sh` resolved branch → repo → global
    /// (`wt pool warm` warms each slot by invoking THIS command, and the cron resolves the same
    /// way). Defaults to `build`. Runs in the current dir with `~/.local/bin:~/.bin` prepended to
    /// PATH. `--repo`/`--branch` name the note to resolve from explicitly, for callers whose cwd
    /// can't answer it — a detached pool slot, or a cron.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Build {
        /// Script name (default `build` → `build.sh`).
        name: Option<String>,
        /// Resolve the script for this repo instead of the current dir (with `--branch`).
        #[arg(long)]
        repo: Option<String>,
        /// Target branch (with `--repo`).
        #[arg(long)]
        branch: Option<String>,
        /// Extra arguments forwarded to the resolved script unchanged.
        argv: Vec<String>,
    },
    /// Unified token usage + **$ cost** across all agent work (#381), categorized by work-class
    /// (Orchestration / Coding / Scheduled / …). Two clearly-labeled units: **AIC** (the real
    /// Copilot bill) and **$ (API-equiv)** (a cache-aware price-table benchmark). Defaults to a
    /// unified month-to-date report: `bn cost [--by category|repo|model|harness|machine|day]
    /// [--window <days>] [--live] [--json]`.
    Cost {
        /// Breakdown dimension: category, repo, model, harness, machine, day (omitted = unified).
        #[arg(long)]
        by: Option<String>,
        /// Trailing window in days (default: current month-to-date).
        #[arg(long, value_name = "DAYS")]
        window: Option<i64>,
        /// Show in-flight usage of currently-running agents instead of recorded history.
        #[arg(long)]
        live: bool,
    },
    /// Auto-file a bn issue on an identified problem (#345 proposal 4): the orchestrator or an
    /// agent captures a bug / friction / blocker as a labeled, **deduped** GitHub issue instead of
    /// relying on a human to file it. `bn issue file "<title>"` checks the repo's OPEN issues first
    /// and reports the existing one when a title already matches — so re-running is idempotent.
    Issue {
        #[command(subcommand)]
        action: IssueCmd,
    },
    /// Links worth going back to: a PR, a work item, a portal blade, a docs site, a dashboard.
    /// Bare `bn links` opens the fzf picker (this repo's links first, then most used); `bn links
    /// <query>` opens the one match. `add` stores one, `capture` the note's PR and work item, `copy`
    /// puts one on the clipboard as a Teams-ready hyperlink. Agent sessions save the PRs they
    /// mention, marked `•new` until `bn links new`; `check` reports dead and merged links, and
    /// `prune` deletes the ones whose `--ttl` ran out. In the picker ctrl-y copies and tab selects
    /// several. Stored cross-machine as `<work_root>/links.md`, synced with your notes.
    #[command(args_conflicts_with_subcommands = true)]
    Links(LinksArgs),
    /// Reusable **reference** docs, in two scopes. Without `--repo` (#623): per-topic coding
    /// standards / preferences that shape how agents write code and how reviews check diffs — the
    /// sibling of `skills` (reusable *workflow* docs) for reusable *standards*, stored
    /// cross-machine as `<work_root>/references/<name>.md`. With `--repo`: this repo's
    /// **reference notes** — what past sessions discovered about it (how it builds, where the
    /// seams are, which failures mislead), stored beside its branch notes as
    /// `<notes>/<machine>/<repo>/reference/<name>.md` and outliving any one branch.
    ///
    /// Bare `bn ref` lists (name + one-line summary); `bn ref <name>` prints one (fuzzy/prefix
    /// match); `bn ref edit <name>` opens it in $EDITOR (creating from a template if missing);
    /// `bn ref add <name>` scaffolds a new one; `bn ref append <name> <text>` records one fact.
    /// Repo notes may nest (`architecture/runtime-concepts`) and keep an `images/` sibling.
    #[command(args_conflicts_with_subcommands = true)]
    Ref {
        #[command(subcommand)]
        action: Option<RefCmd>,
        /// Reference name to print (fuzzy/prefix match). Omit to list all references.
        name: Option<String>,
        #[command(flatten)]
        scope: RefScopeArg,
    },
    /// Fresh-context CRITIC stage (#649): dispatch a leak-proof critic over a PR's diff, then
    /// drive the execute↔critique loop. `bn critic dispatch --branch <b> --pr <url>` cuts a
    /// **fresh worktree off the PR head** with a plan-free note and launches a `--fresh` critic
    /// lane seeded with ONLY the diff + done-when criteria (never the approved plan) — so the
    /// critic starts with zero prior turns and can't rationalize the code. The critic reports via
    /// `bn critic verdict --pass` / `--fail --findings-file <p>`: PASS advances to shippable; FAIL
    /// raises a `task_type=critique` item and re-enters execution, bounded by `--max-iterations`
    /// (default 3), then escalates to a `task_type=planning` human-steer item on exhaustion.
    Critic {
        #[command(subcommand)]
        action: CriticCmd,
    },
    /// A durable, worker-local record of dispatch attempts, queryable over SSH (#719, epic #707
    /// M1). `bn worker status [--json]` reports this node's capabilities/labels, a capacity
    /// snapshot (RAM/swap/load/disk), its READY/PRESSURE admission verdict, and the SQLite attempt
    /// registry (4-state QUEUED→RUNNING→SUCCEEDED|FAILED; LOST derived at reconcile). `bn worker
    /// finish <request_id>` closes a running attempt — the tail of the fire-and-forget spawn line.
    Worker {
        #[command(subcommand)]
        action: WorkerCmd,
    },
    /// Work Forge activities assigned to an agent member. On automatically when ~/.claude.json has
    /// a Forge MCP server with an agent `fam_` key (`[forge_agent] enabled = false` turns it off);
    /// the `forge-agent` job runs a tick every five minutes: heartbeat or report held runs, then
    /// claim and dispatch at most one task. `bn forge-agent status|run-once|release <id>`.
    ForgeAgent {
        #[command(subcommand)]
        action: crate::cmd::forge_agent::ForgeAgentCmd,
    },
    /// A task's place in the pipeline. **Bare** `bn pipeline` infers it read-only from notes,
    /// plan reviews, attention, verify/PR markers, and live agents (#657); `--all` lists the
    /// fleet, `--repo`/`--branch` targets another worktree. **Subcommands** drive the explicit
    /// per-worktree stage machine (PIPE-A, EPIC #746) through the observable stages
    /// (`pickup → investigate → plan → consensus → execute → validate → critique → ship → learn`):
    /// `set`/`advance` record it, `status` shows the recorded stage + history, `board` lists every
    /// worktree's stage, `badge` renders the compact tmux status glyph.
    Pipeline {
        /// Target a repository together with --branch instead of the current worktree.
        #[arg(long, requires = "branch", conflicts_with = "all")]
        repo: Option<String>,
        /// Target a branch together with --repo instead of the current worktree.
        #[arg(long, requires = "repo", conflicts_with = "all")]
        branch: Option<String>,
        /// List every active local task instead of showing one detailed tracker.
        #[arg(long)]
        all: bool,
        #[command(subcommand)]
        action: Option<PipelineCmd>,
    },
    /// Long-lived coordination-only agents. Controllers capture and route requests, dispatch
    /// workers, and own follow-through; they never implement code themselves.
    Control {
        #[command(subcommand)]
        action: ControlCmd,
    },
    /// Print the running binary's crate version and compile-time build provenance.
    Version,
    /// Your repo fleet as one addressable thing: the inventory of every bare clone, and a
    /// parallel fan-out that runs one command in all of them and reports each repo's own exit
    /// code. `bn list` already spans repos on the notes side; this is the git side.
    #[command(args_conflicts_with_subcommands = true)]
    Repos {
        #[command(subcommand)]
        action: Option<ReposCmd>,
    },
    /// The `--json` contract: the envelope's fields, its version, and what each script-facing
    /// command puts in `data`. `bn schema <command>` prints one command's shape.
    Schema {
        /// A command tag from `bn schema` (e.g. `repos`, `wt-list`).
        command: Option<String>,
    },
    /// React to the event log with your own commands: `bn on add pr_merged -- <cmd>`. Bare
    /// `bn on` lists the rules; `bn on tick` is the pass the `reactor` cron runs.
    #[command(args_conflicts_with_subcommands = true)]
    On {
        #[command(subcommand)]
        action: Option<OnCmd>,
    },
}

/// Top-level command: a thin `#[command(flatten)]` union of the thematic command-group enums above.
/// Flatten keeps every subcommand top-level and in order, so the surface is unchanged.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    #[command(flatten)]
    Intake(CmdIntake),
    #[command(flatten)]
    Review(CmdReview),
    #[command(flatten)]
    Planning(CmdPlanning),
    #[command(flatten)]
    Tracking(CmdTracking),
    #[command(flatten)]
    Insights(CmdInsights),
    #[command(flatten)]
    Automation(CmdAutomation),
    #[command(flatten)]
    Staff(CmdStaff),
    #[command(flatten)]
    ShellUi(CmdShellUi),
    #[command(flatten)]
    GitPr(CmdGitPr),
    #[command(flatten)]
    Misc(CmdMisc),
    #[command(flatten)]
    Coord(CmdCoord),
}

#[derive(Subcommand, Debug)]
pub(crate) enum DailyCmd {
    /// Add a goal/activity to today (or schedule it for a future day with --on/--in):
    /// bn daily add <text...> [--task <id>] [--link <url>] [--on <date> | --in <days>].
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        /// Work-item id recorded verbatim as the goal's link.
        #[arg(long)]
        task: Option<String>,
        /// Attach a raw URL (wins over --task).
        #[arg(long)]
        link: Option<String>,
        /// Schedule for a specific future day (YYYY-MM-DD) instead of today.
        #[arg(long)]
        on: Option<String>,
        /// Schedule N days from today instead of today.
        #[arg(long = "in")]
        in_days: Option<i64>,
        /// Attach a time estimate (`90m`, `2h`, `1h30m`) for calibration (`bn calib`). The actual
        /// is derived from punch-log intervals linked to this goal when you close it.
        #[arg(long, value_name = "DUR")]
        est: Option<String>,
        /// Tag this goal to a quarter goal by its key (`bn daily add … --q ship-v1`).
        #[arg(long, value_name = "GOAL_KEY")]
        q: Option<String>,
    },
    /// Mark today's goal done by id (g3) or text substring: bn daily done <id|substring...>.
    Done {
        #[arg(required = true)]
        query: Vec<String>,
        /// Record partial progress (1-99) instead of closing; 100 completes.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
        pct: Option<u8>,
    },
    /// Remove a goal from today outright: bn daily drop <id|substring...>. For a goal that should
    /// never have been raised (a stale agent ping, a duplicate) — unlike `done` it records no
    /// completion, so it stays out of standup, brag, and the metrics.
    Drop {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Move an open goal to a future day: bn daily defer <id|substring...> --on <date> | --in <days>.
    Defer {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long)]
        on: Option<String>,
        #[arg(long = "in")]
        in_days: Option<i64>,
    },
    /// Roll today forward: carry prior open goals + drain due follow-ups (idempotent).
    Rollover,
    /// Open today's day file in $EDITOR.
    Edit,
}

#[derive(Subcommand, Debug)]
pub(crate) enum FeedCmd {
    /// Replace a source's items from JSON on stdin — the write path for feed cron scripts.
    /// Accepts a JSON array or JSONL (one `{"key","text","url"}` object per line):
    /// `az repos pr list ... | jq -c '.[] | {...}' | bn feed ingest ado-reviews`.
    Ingest {
        source: String,
        /// Also mirror items onto the attention list, reconciling against current state: raise
        /// new items, ack ones whose feed item dropped off (e.g. the PR merged). Idempotent.
        #[arg(long)]
        attn: bool,
        /// Severity for mirrored attention items (with --attn): info | warn | urgent.
        #[arg(long, default_value = "warn")]
        severity: String,
    },
    /// Drop a source's items (e.g. retire a feed cron): bn feed clear <source>.
    Clear { source: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ReviewGateCmd {
    /// Record one reviewer's diff-only verdict (and findings) for a PR at head `--sha`. The
    /// reviewer's write path — the ONLY write a diff-only reviewer agent should make.
    Record {
        repo: String,
        number: i64,
        /// This independent reviewer's label (distinct per reviewer so the approval count is N
        /// genuinely-separate reviewers).
        #[arg(long)]
        reviewer: String,
        /// `approve` | `block` (aliases `reject`/`lgtm`…).
        #[arg(long)]
        verdict: String,
        /// The head sha reviewed — approvals are pinned to it. Defaults to the current
        /// worktree's HEAD (the checkout the reviewer just read).
        #[arg(long)]
        sha: Option<String>,
        /// A substantive summary of the review.
        #[arg(long)]
        summary: Option<String>,
        /// The PR url, stamped on first sight (optional).
        #[arg(long)]
        url: Option<String>,
        /// The PR's source branch, stamped on first sight (optional).
        #[arg(long)]
        branch: Option<String>,
        /// One concrete defect, repeatable: `'blocking: <text>'` / `'advisory: <text>'` (bare text
        /// defaults to blocking). A blocking finding holds the gate closed until resolved/waived.
        #[arg(long = "finding")]
        findings: Vec<String>,
    },
    /// Mark a finding resolved (the author fixed it) — no longer holds the gate closed.
    Resolve {
        repo: String,
        number: i64,
        finding_id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Waive a finding (deliberately accepted without a fix) — requires a reason.
    Waive {
        repo: String,
        number: i64,
        finding_id: String,
        #[arg(long)]
        reason: String,
    },
    /// The gate verdict for a PR: approvals vs required + open blocking findings. `--sha` (the PR's
    /// current head) gives the true land-eligibility answer; omitted uses the last-reviewed head.
    Status {
        repo: String,
        number: i64,
        #[arg(long)]
        sha: Option<String>,
    },
    /// Every PR tracked by the gate with its verdict.
    List,
    /// Emit the canonical diff-only reviewer seed prompt for a PR (dispatch an independent reviewer
    /// with exactly this — its context provably excludes the implementer's reasoning).
    Prompt {
        repo: String,
        number: i64,
        #[arg(long)]
        reviewer: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum FindingsCmd {
    /// Record one run's findings for a source from JSON on stdin — the write path a cron (script
    /// or headless agent) pipes into. Accepts a JSON array or JSONL (one `{"key","text","url"}`
    /// object per line), same as `feed ingest`:
    /// `gh pr list ... | jq -c '.[] | {...}' | bn findings record gh-prs`.
    Record {
        source: String,
        /// Stable id for this run; defaults to a timestamp-derived id when omitted.
        #[arg(long)]
        run_id: Option<String>,
        /// Keep the untransformed payload alongside the parsed items (for later re-parsing).
        #[arg(long)]
        raw: Option<String>,
    },
    /// Query recorded findings, newest-first — by source and/or date. `--json` emits the raw
    /// records for #577 (Today's agent) and the dashboard to consume.
    Query {
        /// Restrict to one source (else every source is unioned).
        #[arg(long)]
        source: Option<String>,
        /// Only runs gathered on/after this local date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Only runs gathered on exactly this local date (YYYY-MM-DD). Overrides --since.
        #[arg(long)]
        date: Option<String>,
        /// Most-recent-N cap (default 20, 0 = uncapped).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List the sources that have recorded findings.
    Sources,
    /// Snapshot the local morning-planning caches (#897) into the findings store: the PR review
    /// queue and your own open PRs, the inbound feed (meetings/email/teams/owe), stale/blocked
    /// branches, last night's PR-ready dispatches, failing crons, the sentinel patrol, the current
    /// quarter's open goals, and mined dojo cards. Additive — records one durable run per non-empty
    /// source and touches no feed/attention/daily side effect. Reuses the existing caches (no
    /// network, no re-fetch).
    Gather,
}

#[derive(Subcommand, Debug)]
pub(crate) enum GapCmd {
    /// Replace the meetings cache from JSON on stdin — the `meetings` cron's write path. Accepts a
    /// JSON array or JSONL (one `{"start","title","url"}` object per line), so it drops straight
    /// out of `jq`: `... | jq -c '{start, title: .subject, url: .joinUrl}' | bn gap ingest`.
    /// A run REPLACES the cache, so finished meetings and prior days drop off on the next ingest.
    Ingest {
        /// The local date (`YYYY-MM-DD`) the events belong to, so each `HH:MM` resolves to an
        /// unambiguous instant. Defaults to today.
        #[arg(long)]
        date: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum McpCmd {
    /// Wire the current repo for the bn MCP server: write a project-scoped MCP config and an
    /// instruction block for the chosen harness. `[mcp] extras` in config.toml (or
    /// `--with-extras`) also wires the curated planning/productivity MCP servers.
    Init {
        /// Target harness: claude | copilot.
        harness: String,
        /// Also wire the curated planning/productivity MCP extras (sequential-thinking, memory,
        /// time — or the `[mcp] extras` subset when configured). They run via `npx -y` / `uvx`,
        /// which install the server package on first launch.
        #[arg(long)]
        with_extras: bool,
    },
    /// Serve the branch-note MCP tools over **stdio**, pinned to the current worktree. This is the
    /// per-worktree launcher an MCP client (`{command:"bn", args:["mcp","serve"]}`) spawns: it
    /// execs the sibling `bn-mcp` binary, which derives its `(repo, branch)` from the launch cwd —
    /// so no per-call scope headers are needed. `--orchestrate` serves the cross-worktree
    /// "friend" toolset instead of the scoped branch-note tools.
    Serve {
        /// Serve the cross-worktree orchestrator tools instead of the scoped branch-note tools.
        #[arg(long)]
        orchestrate: bool,
    },
    /// Register the **user-scoped** MCP servers declared in `[mcp.servers.<name>]` into every MCP
    /// client on this machine (Claude Code, Claude Desktop, VS Code Copilot, Copilot CLI).
    ///
    /// The counterpart to `init`: where that wires one repo with a committable config, this wires
    /// one machine with your personal servers. Because the declarations live in the shared
    /// `<notes_root>/config.toml`, which syncs between machines, declaring a server once and
    /// running this on each machine is what makes it exist everywhere. install.sh runs it too, so
    /// a new machine gets them on install.
    ///
    /// Credentials never live in config.toml: a server may declare a `secret_file` (a path plus
    /// the field names it holds), and this prompts for those fields once per machine and writes
    /// the file `0600`. Idempotent — an already-provisioned secret is left untouched.
    SyncUser {
        /// Never prompt for a missing credential; report it and skip that server instead. Implied
        /// when stdin is not a terminal, so cron and piped installs behave this way already.
        #[arg(long)]
        no_prompt: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum LearningsCmd {
    /// Promote a learning to durable — mark it worth keeping (the human-in-the-loop "keep"). The
    /// query is a learning key from the view (e.g. `l3f9a12bc`) or a unique case-insensitive text
    /// substring; an ambiguous substring lists the candidates so you can rerun with a key.
    Promote {
        /// Learning key (from the view) or a unique text substring.
        query: Vec<String>,
        /// Optional rationale recorded alongside the decision.
        #[arg(long)]
        note: Option<String>,
    },
    /// Dismiss a one-off learning so it stops surfacing as unreviewed (the human-in-the-loop
    /// "drop"). Same key-or-substring resolution as `promote`.
    Dismiss {
        /// Learning key (from the view) or a unique text substring.
        query: Vec<String>,
        /// Optional rationale recorded alongside the decision.
        #[arg(long)]
        note: Option<String>,
    },
}

/// The plan-review gate actions (#349). `submit`/`status` are the dispatched agent's write/poll
/// side; `list`/`show`/`approve`/`request-changes`/`reject` are the human's review side (also
/// reachable from the dashboard).
#[derive(Subcommand, Debug)]
pub(crate) enum ReviewCmd {
    /// Register (or re-submit) a plan for review — the agent-facing write. Creates a pending
    /// plan-review for --repo/--branch, reusing the open one on a change-request re-submit, and
    /// prints its id. The plan comes from --plan "<text>", --plan-file <path>, or stdin (default).
    Submit {
        /// Repo the plan is for.
        #[arg(long)]
        repo: String,
        /// Dispatched branch the plan is for (e.g. dispatch/349).
        #[arg(long)]
        branch: String,
        /// The agent/harness session id, for the trail.
        #[arg(long)]
        session: Option<String>,
        /// Plan text inline. Omit to use --plan-file, else stdin.
        #[arg(long)]
        plan: Option<String>,
        /// Read the plan (markdown) from a file. `-` reads stdin.
        #[arg(long)]
        plan_file: Option<String>,
        /// Number of independent agent reviewers. Omit for the original human-only gate.
        #[arg(long, default_value_t = 0)]
        reviewers: usize,
        /// Approval threshold; defaults to a strict majority of --reviewers.
        #[arg(long)]
        quorum: Option<usize>,
        /// Issue/plan label used by the high-risk heuristic. Repeat for multiple labels.
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Planned touched path or glob used by the high-risk heuristic. Repeat as needed.
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// Print a plan-review's current status (pending | approved | changes-requested | rejected) —
    /// the agent's poll read while paused. `--json` prints the full record (status + notes).
    Status { id: String },
    /// List plan-reviews, newest first (pending ones need you). Bare `bn review` runs this.
    List,
    /// Show one plan-review in full: the plan text, status, and any reviewer notes.
    Show {
        id: String,
        /// Reviewer-scoped view: includes the plan and policy but never peer ballots.
        #[arg(long)]
        reviewer: Option<String>,
    },
    /// Launch fresh headless reviewer agents for every unfilled ballot, then attach their
    /// structured verdicts to the review. `--add`/`--kind` scale the panel up on demand.
    Agents {
        id: String,
        /// Headless agent harness (copilot | claude).
        #[arg(long, default_value = "copilot")]
        harness: String,
        /// Optional harness model override.
        #[arg(long)]
        model: Option<String>,
        /// Grow the panel by N more reviewers before fanning out — on-demand scale-up for a
        /// deadlocked review. Defaults to the number of --kind values when only --kind is given.
        #[arg(long)]
        add: Option<usize>,
        /// Specialty for a scaled-up reviewer: rubber-duck | code-review. Repeat for several;
        /// pairs with --add to request typed critics (defaults to rubber-duck).
        #[arg(long = "kind")]
        kind: Vec<String>,
    },
    /// Record one independent agent verdict. Exactly one of --approve/--reject is required.
    Vote {
        id: String,
        /// Stable fresh reviewer/session identity.
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        reject: bool,
        /// Reviewer confidence percentage (0-100).
        #[arg(long)]
        confidence: Option<u8>,
        /// Optional rationale/findings for the human audit trail.
        #[arg(long)]
        notes: Option<String>,
        /// Concrete risk found by the reviewer. Repeat for multiple risks.
        #[arg(long = "risk")]
        risks: Vec<String>,
    },
    /// Open a plan in $EDITOR (nvim), save the edits back to the review, then approve / request
    /// changes. With no id, edits the newest open (pending/changes-requested) review. Bound to
    /// `prefix E` in tmux — a pending plan opens in an nvim popup. `--approve` /
    /// `--request-changes <notes>` skip the interactive prompt.
    Edit {
        /// Which review to edit; omit for the newest open (pending/changes-requested) one.
        id: Option<String>,
        /// After editing, approve immediately (skip the prompt).
        #[arg(long)]
        approve: bool,
        /// After editing, request changes with these notes (skip the prompt).
        #[arg(long = "request-changes")]
        request_changes: Option<String>,
    },
    /// Approve a pending plan-review — the agent may now implement + open the PR.
    Approve { id: String },
    /// Request changes on a pending plan-review — the agent revises the plan and re-submits.
    #[command(name = "request-changes")]
    RequestChanges {
        id: String,
        /// What to change — sent back to the agent (required).
        #[arg(long)]
        notes: String,
    },
    /// Reject a pending plan-review — the agent stops and ships nothing.
    Reject { id: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AttnCmd {
    /// List open (to-start) attention items, urgent-first. Bare `bn attn` shows the fuller
    /// three-lane board (needs-you / in-progress / to-start).
    List {
        /// Filter to these task types (repeatable or comma-separated): coding | planning |
        /// critique | review. Untyped legacy items count as `coding`.
        #[arg(long = "type", value_delimiter = ',')]
        type_filter: Vec<String>,
    },
    /// Compact one-line status fragment for the tmux badge: `2 working · 1 stuck · 1 done · 3
    /// to-start`. Empty when the list is clear.
    Status,
    /// What the local automation noticed today, as JSON: the cron findings packet plus the crons
    /// currently failing. Raises nothing — it is the read side the `attention-agent` ranks. Prints
    /// a `skip` when there is genuinely nothing to look at.
    Gather,
    /// Attention lifecycle rollup over a trailing window: raised (by mode), started/done/closed,
    /// and time-in-flight. bn attn metrics [--window 30].
    Metrics {
        /// Trailing window in days.
        #[arg(long, default_value_t = 30)]
        window: i64,
    },
    /// Raise an item: bn attn add "<text>" [--severity warn] [--link URL] [--key K].
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        /// info | warn | urgent (default warn).
        #[arg(long, default_value = "warn")]
        severity: String,
        /// Optional URL to attach.
        #[arg(long)]
        link: Option<String>,
        /// Dedup key: a re-add with the same key is a no-op while the item is open.
        #[arg(long)]
        key: Option<String>,
        /// Locator the item is about.
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Who raised it (default manual). Cron scripts pass their job name.
        #[arg(long, default_value = "manual")]
        source: String,
        /// How it may be actioned: manual | assist | auto. assist/auto need a --prompt.
        #[arg(long)]
        mode: Option<String>,
        /// Agent prompt for an assist/auto item (what to do when started). `-` reads stdin.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Batch-add attention items from stdin as JSON array/object or JSONL. Fields: title/text,
    /// key, severity, link/url, repo, branch, detail, source. Keys are namespaced by source.
    Ingest {
        /// Default source for rows without `source` (usually the job/script name).
        #[arg(long, default_value = "script")]
        source: String,
        /// Default severity for rows without `severity`: info | warn | urgent.
        #[arg(long, default_value = "warn")]
        severity: String,
        /// Treat stdin as the source's complete current set: resolve previously-keyed items from
        /// this source that are absent from the batch.
        #[arg(long)]
        replace: bool,
    },
    /// Resolve an item: bn attn ack <id>.
    Ack { id: String },
    /// Edit an open item in place: bn attn update <id> [--title T] [--severity S] [--link URL]
    /// [--repo R] [--branch B] [--detail TEXT]. Only the flags you pass change.
    Update {
        id: String,
        /// New title text.
        #[arg(long)]
        title: Option<String>,
        /// info | warn | urgent.
        #[arg(long)]
        severity: Option<String>,
        /// Replacement URL.
        #[arg(long)]
        link: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Replacement rich/multi-line detail.
        #[arg(long)]
        detail: Option<String>,
        /// New mode: manual | assist | auto.
        #[arg(long)]
        mode: Option<String>,
        /// Replacement agent prompt. `-` reads stdin.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Remove an item: bn attn remove <id>. Dismiss as cancelled or raised in error — distinct
    /// from `ack` (resolved/done) so metrics can tell the two apart.
    #[command(alias = "rm")]
    Remove { id: String },
    /// Mark an in-flight item done — awaiting your review: bn attn done <id> [--notes TEXT|-].
    /// The work is finished but the item stays visible until you `ack` it (the human gate on
    /// autonomous work). An `auto` agent runs this to report completion.
    Done {
        id: String,
        /// Completion summary shown in the detail view. `-` reads stdin.
        #[arg(long)]
        notes: Option<String>,
        /// URL the work shipped as (the draft PR) — lands on the item, so review = open the diff.
        #[arg(long)]
        link: Option<String>,
    },
    /// Start an item: open/switch to its repository worktree tmux window and mark it in-flight (the
    /// IN PROGRESS lane). Repo-less items must be enhanced or assigned first. With --agent, launch
    /// the harness: `auto` submits it (runs), `assist` stages it in the pane for you to run.
    Start {
        id: String,
        /// Launch the coding agent with the item's prompt (auto runs it, assist stages it).
        #[arg(long)]
        agent: bool,
    },
    /// Promote a branch todo into a loop-ready attention item (the inverse of `plan`): bn attn
    /// promote <todo-id> [--mode auto] [--repo R --branch B]. The prompt is seeded from the note's
    /// goal + todo text (the launched agent reads the full note itself); re-promoting an
    /// already-queued todo is a no-op. The todo stays open — the agent checks it off as the work.
    Promote {
        /// The todo's id in the branch note (e.g. t3).
        todo_id: String,
        /// How the item may be actioned: auto (default — feed the loop) | assist | manual.
        #[arg(long, default_value = "auto")]
        mode: String,
        /// info | warn | urgent (default warn).
        #[arg(long, default_value = "warn")]
        severity: String,
        /// Target note (defaults to the current worktree's).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Show the item the autonomous loop would start next (visibility / dry-run) — the top eligible
    /// `auto` item (self-authored, prompted, with a repo/branch). Prints nothing when the loop is at
    /// its concurrency cap (`BN_ATTN_AUTO_MAX`, default 1) or nothing is eligible. The
    /// `attention-auto-trigger` cron applies the idle gate and actually starts them.
    NextAuto,
    /// Promote an item to a branch todo, then resolve it off the list: bn attn plan <id>
    /// [--section todo]. Lands on the item's repo/branch if it carries them, else the current
    /// branch.
    Plan {
        id: String,
        /// Note section for the new item (default todo).
        #[arg(long, default_value = "todo")]
        section: String,
    },
    /// Start an agent on an item through `bn dispatch`, then resolve it off the list (#460):
    /// bn attn dispatch <id> [--repo R]. Targets the item's own repo when it carries one, else
    /// `--repo` or the current worktree's.
    Dispatch {
        id: String,
        /// Target repo for the agent (defaults to the item's repo, else the current one).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Hide an item until a date: bn attn snooze <id> <YYYY-MM-DD>.
    Snooze { id: String, until: String },
    /// Styled status-bar fragment of raised attention (cron failures + open items), urgent in
    /// red. Empty when nothing's raised. For tmux `#(...)` — like `bn cron running`.
    Badge,
    /// Urgent open items not yet pushed as a notification — the `notify` job's work queue.
    /// `--json` for scripting.
    ToNotify,
    /// Record that an item was pushed as a notification (so it never re-fires): the notify
    /// job calls this after toasting. bn attn notified <id>.
    Notified { id: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum NotifyCmd {
    /// Today's toast budget, the open quiet window, and what the gate held back. Same as bare
    /// `bn notify`.
    Status,
}

#[derive(Subcommand, Debug)]
pub(crate) enum QCmd {
    /// Triage the inbox: route each capture to a branch todo/research, today's goals, the
    /// attention queue, or drop it. Interactive (needs a tty) unless --auto.
    Drain {
        /// Non-interactive: route only unambiguous captures (hinted at a still-active branch
        /// note → that branch's todo) and leave the rest.
        #[arg(long)]
        auto: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum SessionsCmd {
    /// Resume a recorded harness session in its worktree.
    Resume {
        /// Exact task name, full session id, or unambiguous session-id prefix.
        name_or_id: String,
    },
}

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

#[derive(Subcommand, Debug)]
pub(crate) enum HooksCmd {
    /// Install the prepare-commit-msg hook for this repo (idempotent; chains any existing
    /// hook; lands in core.hooksPath or the git common dir, so it covers all worktrees).
    Install,
    /// Hook entry point: stamp BN-Note trailers onto the commit-message file. Called by the
    /// installed prepare-commit-msg shim, not by hand.
    #[command(hide = true, name = "run-prepare-commit-msg")]
    RunPrepareCommitMsg {
        msg_file: std::path::PathBuf,
        source: Option<String>,
        /// The commit sha for -c/-C/--amend sources; unused, accepted for hook-arg parity.
        sha: Option<String>,
    },
}
