//! The clap command surface: the top-level `Command` enum and every `*Cmd`
//! subcommand enum. Split out of `main.rs` (issue #333); `main.rs` keeps the
//! `Cli` parser struct and the dispatch `match`. Glob-re-exported by `main.rs`.
use crate::*;

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
        /// A todo id (e.g. t3), an issue/PR URL, or free text describing the work. Optional with
        /// `--issue N --remote` (the issue itself is the task); required otherwise.
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
        /// Launch an AI agent in the worktree's window, seeded with the note and asked to propose a
        /// plan first. `--agent` alone lets the launch stage's configured harness decide (falling
        /// back to copilot); `--agent claude` (aka claudecode) or `--agent copilot` pins the harness.
        /// Opens (or reuses) the repo's tmux session, so it works from outside tmux too.
        #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
        agent: Option<String>,
        /// Scout task: investigate / reproduce / audit and write findings to the branch note — the
        /// agent changes no code (vs the default "ship" task, which delivers a change). Promote a
        /// proven scout to a ship with `bn promote`.
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
        /// learns from the run. Defaults: `plan-work` (ship) / `investigate` (--scout); pick
        /// `debug` for a repro-first bugfix. `--skill investigate` implies a scout task.
        #[arg(long)]
        skill: Option<String>,
        /// Gate "done" on verification: instruct the agent to run `bn verify` (the repo's build/test/lint
        /// gate) and only report done once it passes. Ignored for --scout (a scout ships nothing).
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
        /// `~/.config/wt/pools` (explicit or `default`) or a built-in fallback. Pass `--no-pool` for
        /// a throwaway one-off, or when you specifically want an isolated cold checkout. A repo with
        /// no bare clone (or a full pool) already falls back to cold automatically.
        #[arg(long)]
        no_pool: bool,
        /// Deprecated no-op: leasing a warm pool slot is now the default, so `--pool` no longer does
        /// anything (kept hidden so existing scripts/callers that still pass it don't break). Use
        /// `--no-pool` to opt out.
        #[arg(long, hide = true)]
        pool: bool,
        /// Run the agent in THIS pane instead of a background tmux lane (#claude-wrap): the harness
        /// takes over the current terminal, and the moment it exits any leased `--pool` slot is
        /// handed straight back. Powers the `claude`/`copilot` shell wrappers (`bn shell-init`) —
        /// an interactive, throwaway investigation. Requires `--agent`; local-only (no tmux window,
        /// agent-lease, or controller event), so it conflicts with `--no-tmux`/`--host`/`--remote`/
        /// `--orchestrator`.
        #[arg(
            long,
            requires = "agent",
            conflicts_with_all = ["no_tmux", "host", "remote", "orchestrator"]
        )]
        attach: bool,
        /// Dispatch on a remote machine: an ssh-config Host alias registered under config.toml
        /// `[hosts]`. Runs `bn dispatch <task> --agent --yolo` ON the target — worktree, note, and
        /// agent all created there, in a detached tmux window; the note flows home via `bn sync`.
        /// Watch it with `bn connect <alias>`. Requires --repo (or a cwd inside one).
        #[arg(long)]
        host: Option<String>,
        /// Dispatch to a GitHub-hosted agent instead of a local/remote worktree: triggers the
        /// `claude-dispatch.yml` workflow with the free-text seed as its `prompt` (an owner/collab
        /// `gh workflow run`), captures the run-id, and records it in the branch note. Prints the
        /// run url + a `gh run watch` hint. Requires `gh` authed and the workflow installed
        /// (`bn remote init`). The free-text seed is user-authored — never pass a gathered issue/PR
        /// title here; use `--issue N` for issue-linked work.
        #[arg(long, conflicts_with = "host")]
        remote: bool,
        /// With `--remote`: post the seed as an `@claude` comment on issue N (the issue-linked path)
        /// instead of triggering the workflow. The issue's own title/body is the task context, read
        /// natively by the agent and framed as untrusted input; any free-text `item` is appended as
        /// a user-authored instruction.
        #[arg(long, requires = "remote")]
        issue: Option<u64>,
        /// Stable correlation token for an idempotent remote replay. Internal orchestrator plumbing:
        /// the workflow echoes it into the run name; issue dispatch embeds it in the comment body.
        #[arg(long, requires = "remote", hide = true)]
        corr: Option<String>,
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
    Verify {
        /// Job to run instead of the aggregate default gate.
        #[arg(long)]
        job: Option<String>,
        /// Skip the bn-web UI gate (npm ci + build + vitest) even when `crates/bn-web/ui` changed.
        #[arg(long)]
        no_ui: bool,
    },
    /// The no-mistake pre-land gate: rebase this worktree's HEAD onto a freshly-fetched
    /// `origin/main` in a *disposable* worktree, then run the repo's aggregate verify gate on THAT
    /// rebased head. Exits non-zero unless green, so an agent self-gates before
    /// reporting done and `bn wt merge` refuses to land a red or stale-base PR. Kills the "green on
    /// a stale base" class: the suite runs against current main + your changes, in an isolated
    /// checkout — your own worktree is never touched.
    Gate {
        /// Job to run instead of the aggregate default gate.
        #[arg(long)]
        job: Option<String>,
        /// Skip the bn-web UI gate (npm ci + build + vitest) even when `crates/bn-web/ui` changed.
        #[arg(long)]
        no_ui: bool,
    },
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
    /// Native, event-driven fleet orchestration (#338/#340): declare a queue of issues and bn drives
    /// each one dispatch → self-gate → merge-queue → next until the queue drains — recovering wedged
    /// agents (api-error / dns-outage / idle-with-WIP / dead) with exponential backoff, no
    /// scheduled-prompt polling. Bare `bn orchestrate` shows the run; `--queue <items…>` starts one
    /// (daemonized via launchd so it survives the launching session going away); `run` is the daemon
    /// loop; `stop` ends it. The supervision state machine is codified + unit-tested in bn-core.
    #[command(args_conflicts_with_subcommands = true)]
    Orchestrate {
        #[command(subcommand)]
        action: Option<OrchestrateCmd>,
        /// Declare the wave: each issue number (`338`), issue/PR URL, or free-text item is
        /// dispatched, gated, merged, and the next pulled until the queue drains. Prefix an item
        /// with `repo:` (e.g. `wira360:456`) to route it to another repo — one `--queue` can then
        /// seed several repos at once (#343). Unprefixed items go to `--repo`/the current repo.
        #[arg(long, num_args = 1.., value_name = "ITEM")]
        queue: Vec<String>,
        /// Default repo for unprefixed queue items (default: the current repo). Per-item
        /// `repo:item` prefixes override this, so a single wave can span multiple repos.
        #[arg(long)]
        repo: Option<String>,
        /// Max agents dispatched at once (default 2). `--solo` overrides this to 1. Also the
        /// coding-lane cap unless `--coding` is given; a bare `--concurrency N` (no typed flags) is
        /// the single pool every lane shares — the pre-#385 behavior.
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
        /// Typed concurrency lane (#385): the max **coding** (ship) agents in flight at once.
        /// Overrides `--concurrency` for the coding lane. Pair with `--research` for `5 coding + 2
        /// research`-style independent budgets; the daemon caps each lane separately.
        #[arg(long)]
        coding: Option<usize>,
        /// Typed concurrency lane (#385): the max **research** (scout) agents in flight at once,
        /// scaling independently of the coding lane. Omit for the single shared pool.
        #[arg(long)]
        research: Option<usize>,
        /// Refactor mode — one agent at a time (e.g. a main.rs-touching change like #333), no overlap.
        #[arg(long)]
        solo: bool,
        /// The harness the daemon dispatches every agent under: `claude` | `copilot` (aliases
        /// `claudecode` / `cc` / `gh`). Omit to use `[orchestrate].agent` (`$BN_ORCHESTRATE_AGENT`),
        /// else `copilot`. The #382 bias: on a usage-limit stall the daemon auto-switches that lane
        /// to the *other* harness in place (`bn switch --in-place`) so the work continues.
        #[arg(long)]
        agent: Option<String>,
        /// Run the loop in the foreground (blocking) instead of daemonizing via launchd.
        #[arg(long)]
        foreground: bool,
    },
    /// Idle-triggered auto-review of queued PRs: create the worktree in the PR's own repo,
    /// favourite the branch (best-effort), review, and surface an Approve/Reject recommendation.
    #[command(name = "review-pr")]
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
    /// / opening a PR. Review it in the dashboard or here: bare `bn review` lists; `approve` unblocks
    /// the agent, `request-changes --notes` loops it back, `reject` stops it. `submit`/`status` are
    /// the agent's write/poll side of the gate. Distinct from `bn reviews` (the PR review queue).
    Review {
        #[command(subcommand)]
        action: Option<ReviewCmd>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdPlanning {
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
    /// The task list — one global store for every repo (`<personal_root>/tasks.txt`, todo.txt
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
    /// Quarter goals + a recurring impact agent (#252) — the layer above daily/weekly. Bare
    /// `bn quarter` prints the current quarter (goals + status + elapsed/progress header,
    /// auto-creating on first use); `bn quarter <YYYY-Qn>` reads a past quarter; add/done/edit
    /// manage goals; `bn quarter review` runs the impact agent (what the weekly cron calls);
    /// `bn quarter advise` ranks the backlog into a goal-aligned shortlist for task selection.
    Quarter {
        #[command(subcommand)]
        action: Option<QuarterCmd>,
        /// Read a specific quarter (YYYY-Qn, e.g. 2026-Q2) instead of the current one. Bare view only.
        #[arg(value_name = "QUARTER")]
        quarter: Option<String>,
    },
    /// Standup view: yesterday's done/carried goals, today's plan, blockers, upcoming follow-ups.
    Standup {
        /// Print only the copy-paste-ready one-liner (the standup-autopilot push uses this).
        #[arg(long)]
        copy: bool,
        /// Cron gate: exit silently (0) unless standup autopilot is enabled in config. The
        /// `standup-push` job passes this; an interactive `bn standup` always renders.
        #[arg(long, hide = true)]
        cron: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdTracking {
    /// What got done: completed goals + logged achievements across yesterday and today.
    Stand,
    /// End-of-day wrap digest (#98): branches touched, un-noted commits, PRs merged today,
    /// still-open todos, and tomorrow's carry-forward. Silent when the day is clean.
    Wrap {
        /// Cron gate: exit silently (0) unless the wrap digest is enabled in config. The
        /// `wrap-digest` job passes this; an interactive `bn wrap` always runs on demand.
        #[arg(long, hide = true)]
        cron: bool,
    },
    /// Personal DXI micro-prompt (#110): record today's <10s self-rating (focus 1-5, CI/tooling
    /// friction 1-5, one-word mood) beside the daily files. `bn weekly` plots the trend.
    Rate {
        /// Focus / flow-time score, 1 (fragmented) .. 5 (deep focus).
        #[arg(long)]
        focus: u8,
        /// CI / tooling friction, 1 (smooth) .. 5 (fought it all day).
        #[arg(long)]
        friction: u8,
        /// Optional one-word mood tag.
        #[arg(long)]
        mood: Option<String>,
    },
    /// Start (or switch to) the activity you're working on now — a time-tracking punch. The
    /// previously-running activity ends the instant this lands: bn in <activity...>
    /// [--task <id>] [--link <url>] [--tag <tag>] [--at <HH:MM> | --ago <dur>]. Auto-stamps the
    /// current repo/branch.
    In {
        #[arg(required = true)]
        activity: Vec<String>,
        /// Work-item id recorded verbatim as the entry's link (rolls up in `bn track report`).
        #[arg(long)]
        task: Option<String>,
        /// Attach a raw URL (wins over --task).
        #[arg(long)]
        link: Option<String>,
        /// Categorize the activity (dev/meeting/review/break…) for clean report rollups.
        #[arg(long)]
        tag: Option<String>,
        /// Backdate the punch to a wall-clock time today (HH:MM), e.g. the morning you actually
        /// started. Mutually exclusive with --ago; must not be in the future.
        #[arg(long, value_name = "HH:MM", conflicts_with = "ago")]
        at: Option<String>,
        /// Backdate the punch by a duration before now (45m, 2h, 1h30m). Mutually exclusive
        /// with --at.
        #[arg(long, value_name = "DUR")]
        ago: Option<String>,
    },
    /// End the current activity / your tracked day — a closing punch: bn out.
    Out,
    /// Show today's tracked timeline with durations (or query/report): bn track [at <HH:MM>]
    /// [report [--by activity|tag|branch|task]] [--day <date>].
    Track {
        #[command(subcommand)]
        action: Option<TrackCmd>,
        /// View a specific past day (YYYY-MM-DD) instead of today. Only for the bare timeline.
        #[arg(long)]
        day: Option<String>,
    },
    /// Estimation calibration: your personal median actual÷est multiplier per tag/repo, joined
    /// from estimated todos/goals (`--est`) and the punch log. Reads every closed item carrying
    /// both stamps across branches + daily history. `--json` for the envelope.
    Calib,
    /// Weekly 1-1 review: goals completed since the anchor weekday (or `--since N` days),
    /// PRs merged, still-open goals, and blockers.
    Weekly {
        /// Rolling window of N days instead of the configured weekly anchor.
        #[arg(long)]
        since: Option<i64>,
    },
    /// Weekly auto-retro: mine the telemetry bn already collects (punch log, focus blocks, PR
    /// metrics, todo ages, daily goals) into a metric table + one heuristic suggestion. Anchored
    /// to the weekly window like `bn weekly`; read-only and network-free. This is the *objective*
    /// retro; the subjective weekly session is `bn retro` (the Interviewer, #261).
    Insights {
        /// Rolling window of N days instead of the configured weekly anchor.
        #[arg(long)]
        since: Option<i64>,
        /// Write the rendered retro to `<notes>/<machine>/insights/<YYYY-Www>.md` (the cron
        /// snapshot path) instead of printing it, and report where it landed.
        #[arg(long)]
        write: bool,
        /// Record the current scoreboard numbers as the fixed baseline anchor (#276) — the
        /// pre-staff comparison point every later run renders against. Overwrites any prior
        /// anchor and reports where it landed.
        #[arg(long)]
        set_baseline: bool,
    },
    /// Long-horizon impact packet for reviews: logged achievements + your merged PRs + metric
    /// highlights + completed goals over a window, grouped by theme into review-ready markdown,
    /// plus impactful merged PRs you never `bn achieve`d.
    Brag {
        /// Window start: a `YYYY-MM-DD` date, `review` (default 6-month review horizon), or an
        /// `N`/`Nmo` month count. Default: 6 months.
        #[arg(long)]
        since: Option<String>,
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
    /// Semantic recall over ALL notes (active + closed) and investigation docs, ranked by
    /// meaning — the decision log that finally pays interest. `bn recall "<query>"`.
    /// Degrades to `bn search` semantics (with a notice) when the index is cold; build/refresh
    /// the machine-local index with `bn recall --index` (also a cron job).
    Recall {
        /// The query. Omit only with `--index` (to just (re)build the index).
        query: Vec<String>,
        /// Scope to one repo (matched on the trailing path segment: `bn` matches `owner/bn`).
        #[arg(long)]
        repo: Option<String>,
        /// (Re)build the machine-local index incrementally, then exit. Re-embeds only changed
        /// files. Fetches nothing at query time; this is the one place a model would bootstrap.
        #[arg(long)]
        index: bool,
        /// Max hits to return (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Related branches: past branches that touched the same files (and work-item area),
    /// ranked by overlap. Offline, derived from git. `bn related [--branch <name>]`.
    Related {
        /// Branch to find neighbours for (default: the current branch).
        #[arg(long)]
        branch: Option<String>,
        /// Max related branches to return (default 10, 0 = uncapped).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// PR review queue: open PRs from followed authors (cached by the pr-queue job).
    Reviews {
        /// Print the path to the review cache file (where the pr-queue job writes), then exit.
        #[arg(long)]
        path: bool,
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
        #[arg(long, default_value_t = 30)]
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
    /// Today's agent (#577): synthesize the cron findings store (#576) into a reviewable list of
    /// **suggested tasks** — proposals, each with its source finding + rationale — for you to
    /// review on Today. Bare `bn today` (or `bn today run`) runs the agent: gather the latest
    /// findings per source → synthesize proposals → merge into the suggestions store (idempotent,
    /// so a dismissed proposal never returns). `bn today list` reads them back; `bn today
    /// accept|dismiss <id>` reviews one. Generalizes the `bn staff`/`bn chief` pattern; promotion
    /// of an accepted suggestion to a Command Center item is #578.
    Today {
        #[command(subcommand)]
        action: Option<TodayCmd>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdFleet {
    /// Jump to whatever needs you (prefix `j`): an fzf popup stacking live tmux windows that need
    /// you (waiting / done / blocker badges) over the cross-repo `bn next` queue. ↵ switches to the
    /// window or opens the link; ctrl-r reloads. Plain list when fzf is absent.
    Jump {
        /// Fast-path (prefix Tab): jump straight to the top-ranked window, no picker.
        #[arg(long)]
        next: bool,
        /// Internal: the fzf callback stage a `jump` session re-invokes (list/preview).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<jump::JumpStage>,
        /// Internal: the focused row's hidden columns, passed to a `--pick-stage` callback.
        #[arg(hide = true, allow_hyphen_values = true)]
        pick_fields: Vec<String>,
    },
    /// Project + worktree picker (prefix `C-o`): an fzf popup over your repos (bare + flat clones)
    /// and live tmux sessions, frecency-sorted, with a git-status preview. ↵ opens the project (a
    /// bare repo opens a window/worktree sub-picker); type `repo/branch` to create that worktree,
    /// or paste a git URL to clone it; ctrl-x kills a session. Plain list when fzf is absent.
    Tat {
        /// Open this project/worktree directly (`<name>`, `<repo>/<branch>`, or a git URL). Omit
        /// for the interactive picker. Also carries the hidden `ref` for `--pick-stage`.
        #[arg(allow_hyphen_values = true)]
        name: Option<String>,
        /// Scope the picker to an alternate root. `--root win` targets the Windows-filesystem repos
        /// (the `tatw` replacement); omit to span the primary root plus the auto-loaded `win` root.
        #[arg(long)]
        root: Option<String>,
        /// Internal: the fzf callback stage a `tat` session re-invokes (list/preview/kill).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<tat::TatStage>,
        /// Internal: an extra `--pick-stage` payload (the focused row's `kind~payload`), used by the
        /// `rmworktree` action bind.
        #[arg(long, hide = true)]
        pick_arg: Option<String>,
    },
    /// Remote connect (the cross-machine `tat`): an fzf picker over your `~/.ssh/config` hosts
    /// (enriched by config.toml `[hosts]`), preview shows the remote's live `bn summary`. ↵ or
    /// `bn connect <alias>` opens `ssh -t` into `tmux new-session -A -s bn` on the target, so a
    /// dropped pipe re-attaches instead of killing work. Degrades to a plain ssh shell when the
    /// remote has no tmux, and to a numbered list when fzf is absent. `--window` wraps that
    /// connection in a local `host:<alias>` tmux session; `--read-only` observes an existing
    /// remote session without creating one.
    Connect {
        /// Host alias to connect to (as named in ~/.ssh/config). Omit for the picker.
        host: Option<String>,
        /// Open the remote in a dedicated local `host:<alias>` tmux session. Inside tmux, create
        /// or switch to that session; outside tmux, attach to it.
        #[arg(long)]
        window: bool,
        /// Attach to the remote `bn` tmux session read-only. If it does not exist, degrade to the
        /// remote login shell instead of creating it.
        #[arg(long)]
        read_only: bool,
        /// Internal: the fzf callback stage a `connect` session re-invokes (list/preview).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<connect::ConnectStage>,
    },
    /// Print a phone-friendly mosh recipe for a known SSH host (#633 P4). The command connects
    /// through the host alias from `~/.ssh/config` and attaches to the persistent remote `bn`
    /// tmux session, so roaming, sleep, and reconnects do not kill the work.
    Mobile {
        /// Host alias to connect to (as named in ~/.ssh/config).
        host: String,
    },
    /// commits since the note was last touched, `bn done` for todos a commit closed, and (on a
    /// MERGED PR) `bn close` + `wt cleanup`. Interactive confirm by default; nothing is written
    /// without confirm or `--yes`. Silent when already in sync.
    Sync {
        /// Apply every proposal without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Attention items: hand-, script-, or agent-raised entries on the attention list.
    /// Bare `bn attn` lists the open set; they also fold into `bn doctor`. Resolve with `ack`
    /// (done), `remove` (cancelled), or `plan` (promote to a todo); edit with `update`.
    Attn {
        #[command(subcommand)]
        action: Option<AttnCmd>,
        /// Filter the board to these task types (repeatable or comma-separated): coding |
        /// planning | critique | review. Untyped legacy items count as `coding`.
        #[arg(long = "type", value_delimiter = ',')]
        type_filter: Vec<String>,
    },
    /// Focus mode — a declared deep-work block that suppresses + queues non-urgent surfacing.
    /// `bn focus start 90m` opens a quiet window; while active `bn next` lets only urgent
    /// signals (failing cron, a true blocker) through and queues the rest. Bare `bn focus`
    /// shows the active block (same as `status`); `bn focus end` closes it and drains the queue.
    Focus {
        #[command(subcommand)]
        action: Option<FocusCmd>,
    },
    /// Desktop-notification policy: what toasted today, what was held back, and why. A toast
    /// means work has stopped and needs your decision; everything else stays in `bn next`.
    Notify {
        #[command(subcommand)]
        action: Option<NotifyCmd>,
    },
    /// AI-agent session telemetry. Bare `bn agent` lists this branch's recent Claude/Copilot
    /// sessions; `bn agent ingest` folds new sessions into the metrics log (the poller's call).
    Agent {
        #[command(subcommand)]
        action: Option<AgentCmd>,
    },
    /// Persistent cross-repo registry of controller and worker agent sessions.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsCmd>,
    },
    /// Resume the latest recorded session for this worktree, or a named session from anywhere.
    Resume {
        /// Exact task name, full session id, or unambiguous session-id prefix. Omit to select the
        /// latest Copilot or Claude session recorded for the current git worktree.
        name_or_id: Option<String>,
    },
    /// The at-a-glance fleet board: every live agent across all worktrees and both harnesses
    /// (Copilot + Claude), ranked by who needs you, each with a short live preview of its pane —
    /// so you see what they're all doing without attaching. One-shot. `bn fleet pick` is the
    /// interactive jump-in (an fzf picker with a live pane preview, orchestrators included).
    Fleet {
        /// Interactive picker to jump to an agent — `bn fleet pick` (absent → the read board).
        #[command(subcommand)]
        action: Option<FleetCmd>,
        /// Minutes of quiet before an agent is *idle*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_IDLE_MINS)]
        idle: i64,
        /// Minutes of quiet before an agent is flagged *stuck*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
        /// Fix the usage-limited agents: for every agent sitting at its harness's usage-limit
        /// prompt, run `bn switch --in-place` to the other harness (handoff written, pane
        /// respawned) so the work continues instead of waiting for the limit to reset.
        #[arg(long)]
        fix: bool,
        /// Per-run token budget: flag any agent whose cumulative tokens exceed it — spot a
        /// runaway (e.g. a rebase loop that won't converge) early.
        #[arg(long)]
        budget: Option<i64>,
        /// Show live in-flight token usage by work-class category instead of the fleet board.
        #[arg(long)]
        cost: bool,
        /// Compact one-line-per-agent list — the scannable default (`status · target · harness ·
        /// tokens`, no pane preview). Already the default; the flag (and its `--compact` /
        /// `--no-preview` aliases) is here for explicitness and to override a `--preview`.
        #[arg(long, visible_aliases = ["compact", "no-preview"])]
        oneline: bool,
        /// Opt into the multi-line live pane preview beneath each agent (a `tmux capture-pane`
        /// peek of what it's doing) — the richer, less scannable board. Overridden by `--oneline`.
        #[arg(long)]
        preview: bool,
        /// Narrow-screen reflow (#633): render one compact line per agent that never wraps at
        /// ~40 cols — a short state badge, the truncated `repo:branch` target, and the token
        /// burn — so the board stays readable in a phone SSH client. Forces `--oneline`.
        #[arg(long)]
        mobile: bool,
    },
    /// The night shift — the nightly fleet planner. Picks the top promoted todos (ranked by
    /// `bn next`), isolates each in its own worktree, dispatches an agent, and leaves you a morning
    /// review queue of draft PRs. `plan` dry-runs the pick; `run` executes it (what the cron calls);
    /// `report` is the morning digest; `off`/`on` are the kill switch. Opt-in per repo via
    /// `[night] repos` and gated by `[night] enabled` + a run window.
    Night {
        #[command(subcommand)]
        action: Option<NightCmd>,
    },
    /// The learning dojo (#253) — gamified, spaced-repetition drills derived from your codebase and
    /// its history: bug mysteries from real `fix:` commits, feature walkthroughs, decision recall,
    /// and architecture tours. Bare `bn dojo` runs today's session (~a handful of cards) from
    /// pre-mined material, self-graded Anki-style; your streak, XP and per-repo mastery accrue.
    /// Degrades to a live architecture tour when no cards are ready. Cards are mined nightly by the
    /// `dojo-cards` cron. Rotation honors `[dojo] repos`.
    Dojo {
        #[command(subcommand)]
        action: Option<DojoCmd>,
        /// Limit this session to one repo (overrides the `[dojo] repos` rotation).
        #[arg(long)]
        repo: Option<String>,
        /// Cards to present this session (overrides `[dojo] daily_target`).
        #[arg(long)]
        max: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdStaff {
    /// The Chief of Staff — the one card you read. Twice a day it synthesizes what the system
    /// already knows (the night shift, `bn next`, the calendar cache, the quarter, the `q` inbox,
    /// your daily) into a single briefing card: `bn chief am` is the 05:30 morning brief (start the
    /// day with the decision made; adds a review goal per night PR), `bn chief pm` is the 16:45
    /// evening shutdown (what moved, what to defer, and tomorrow's first task pre-chosen). Bare
    /// `bn chief` picks the slot by the time of day. Offline reads only; the card is never urgent.
    /// The `staff-chief-{am,pm}` crons run it on a schedule (opt-in via `[chief].enabled`).
    Chief {
        #[command(subcommand)]
        action: Option<ChiefCmd>,
    },
    /// The Archivist — the Friday private weekly chronicle: the *story* of the week. Once a week it
    /// folds what the system already knows (the staff's feed cards, merged-PR + goal metrics vs the
    /// prior week, achievements logged this week, the quarter's status) into a ~300-word, five-
    /// section chronicle written to `<notes>/<machine>/chronicle/<YYYY-Www>.md` — strictly private,
    /// never published (it replaces the rejected "Publicist"). Thin weeks degrade to a two-line
    /// chronicle, not filler. Unlogged wins are surfaced as *draft* brag entries on the feed only —
    /// `bn achieve` stays a human act. Offline reads only. The `staff-archivist` cron runs it
    /// Fridays 17:30 (opt-in via `[archivist].enabled`); `bn archivist` runs it on demand.
    Archivist,
    /// The Curriculum Director (#260) — the Sunday weekly learning plan. It reads *where you're
    /// weak* (recurring reflection themes + the #253 dojo mastery map, falling back to a tech
    /// inventory when signals are thin) and turns it into ONE topic a week with a single
    /// time-boxed resource, 2–3 dojo drills, a Monday 30-minute goal, and a seeded learn note —
    /// honestly carrying or dropping last week's topic (a topic never rides unfinished for a
    /// month). Bare `bn curriculum` (or `plan`) previews the card read-only; `bn curriculum run`
    /// applies its idempotent side-effects; `bn curriculum gather` prints the packet JSON. The
    /// `staff-curriculum` cron runs it Sunday 18:00.
    Curriculum {
        #[command(subcommand)]
        action: Option<CurriculumCmd>,
    },
    /// The Mentor — post-merge review-to-teach on your *own* merged PRs (#258). For each of your
    /// PRs merged yesterday it reviews the diff like a staff engineer whose only job is to teach:
    /// the stronger design, one idiom worth stealing, one genuine strength, and a recurring-theme
    /// tag. It appends a craft note to the merged branch's note, reinforces a dojo deck card per
    /// theme, and files one info card per PR in `bn staff feed`. Never gates, never posts anywhere.
    /// The `staff-mentor` cron runs it Mon..Fri 06:30 (opt-in via `[mentor].enabled`); bare
    /// `bn mentor` runs it now regardless.
    Mentor,
    /// Away mode — batched fleet supervision. While away, routine agent activity is absorbed and
    /// only captain-relevant events (an agent that needs you / is stuck, an urgent attention item)
    /// count. `bn afk end` prints ONE batched digest of what needs you. Bare `bn afk` shows status.
    Afk {
        #[command(subcommand)]
        action: Option<AfkCmd>,
    },
    /// The wake watcher — a cheap (no-LLM) event reactor. Each pass gathers what needs you (agents
    /// waiting/blocked/stuck, agent completions, urgent attention), diffs against the last-seen set,
    /// and fires a desktop toast for anything NEW — so you're pinged the moment it happens instead
    /// of polling. `--once` is a single pass (for cron); otherwise it loops every `--interval`s.
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
    },
    /// Review **specialists**: repo-scoped PR-review experts bn compiles and installs as native
    /// Copilot CLI / Claude Code custom agents, so a running Copilot/Claude session discovers them
    /// under `/agent` and can delegate to them. A specialist is an `agents/<name>.md` with a
    /// `repo:` gate whose `references:` docs are mined from real PR review comments. Bare
    /// `bn agents` lists them; `bn agents install` (re)compiles them into `~/.copilot/agents` +
    /// `~/.claude/agents`; `bn agents bootstrap <name>` mines its review standards.
    Agents {
        #[command(subcommand)]
        action: Option<AgentsCmd>,
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
pub(crate) enum CmdShellUi {
    /// Scaffold ~/.config/bn/config.toml (if absent) and regenerate the legacy plaintext
    /// file (work-repos) config loading still falls back to, from the loaded config.
    Setup,
    /// Remote GitHub agent dispatch (#280): scaffold the `claude-dispatch.yml` workflow into the
    /// current repo and verify the `ANTHROPIC_API_KEY` secret. The repo-side setup that lets a
    /// later `bn dispatch --remote` (#281) hand a task to a Claude agent running in GitHub Actions.
    Remote {
        #[command(subcommand)]
        action: RemoteCmd,
    },
    /// One-line status badge for the tmux status bar (replaces branch-note-status.sh).
    /// Resolves from the given path (tmux passes #{pane_current_path}) or cwd; prints
    /// nothing when there's no note or it's closed.
    TmuxStatus { path: Option<String> },
    /// Managed bn MCP lifecycle and health helpers for tmux. `ensure` idempotently starts
    /// `bn serve --daemon` when configured and unreachable; `badge` emits the green/red
    /// `bnweb:<port>` status segment.
    TmuxMcp {
        #[command(subcommand)]
        action: TmuxMcpCmd,
    },
    /// Themed git segment for the tmux status bar (replaces workflow/bin/bn-git-status.sh):
    /// `⎇ <branch> ↑ahead ↓behind ●dirty` from one `git status --porcelain=v2 --branch`.
    /// Resolves from the given path (tmux passes #{pane_current_path}) or cwd; silent outside
    /// a git repo so the segment collapses.
    TmuxGit { path: Option<String> },
    /// Focus segment for the tmux status bar (replaces workflow/bin/bn-goal-status.sh): the
    /// linked PR's title (preferred, cached 5 min via gh) or the branch goal, truncated. Prints
    /// a dim "set a goal" nudge when there's a note but no goal/PR; silent outside a repo.
    TmuxGoal { path: Option<String> },
    /// Global attention tally for the tmux status bar (replaces
    /// workflow/bin/bn-attention-count.sh): counts windows `waiting` (an agent needs you) and
    /// `ready`/done across ALL sessions, amber→red once something has waited 15+ min. Silent
    /// when all quiet so the bar stays clean.
    TmuxAttnCount,
    /// Mark a watched window done when its output goes quiet (replaces
    /// workflow/bin/tmux-done-badge.sh): invoked from the global alert-silence hook with the
    /// window id. Sets @bn_done ✓, clears monitor-silence, fires a best-effort desktop toast.
    TmuxDoneBadge {
        /// The tmux window id (`#{hook_window}`) to badge.
        window: String,
    },
    /// Orchestration roll-up popup (bound to prefix S; replaces workflow/bin/tmux-cockpit.sh):
    /// in-flight windows across every session (agent state / silence-watch), fleet health
    /// (`agent supervise`), and the bn cross-branch summary. Waits for Enter before closing.
    TmuxCockpit,
    /// Curated keybinding cheat-sheet popup (bound to prefix ?; replaces
    /// workflow/bin/tmux-help.sh). The workflow bindings worth remembering, grouped. Waits for
    /// Enter before closing.
    TmuxHelp,
    /// Open the current dir's branch note in $EDITOR (bound to prefix n; replaces
    /// workflow/bin/tmux-note.sh). Execs the editor so closing it closes the popup.
    TmuxNote,
    /// Icon-vocabulary cheatsheet: every Nerd Font glyph bn uses (name, codepoint, meaning)
    /// plus a mock of the two tmux status rows. If a row shows tofu/blanks, your font is
    /// missing the glyph. The mirrors listed in bn-core/src/glyph.rs must match this set.
    Glyphs,
    /// Push the current note's counts into per-window tmux options (@bn_todos,
    /// @bn_blockers, @bn_state) so the window list can decorate cheaply. Runs
    /// automatically after `bn add` / `bn done` inside tmux; this is the manual entry.
    TmuxSync,
    /// Set the live agent-activity decoration on the calling pane's window via the
    /// @bn_agent option. Driven by Claude Code hooks: UserPromptSubmit -> working,
    /// Notification -> waiting, Stop -> done. `clear` blanks it. No-op outside tmux,
    /// I/O-free (no note read) so hooks stay fast. Takes precedence over @bn_done (the
    /// silence-watch fallback) in the status line and cockpit — see workflow/tmux.conf.
    TmuxAgent {
        /// working | waiting | done | clear
        state: String,
    },
    /// Status for the current branch.
    Status,
    /// Lean context snapshot (goal, blockers, open todos, recent progress/decisions).
    Brief,
    /// Complete and close the branch note: mark open todos done, log an achievement (from the
    /// note's Goal + PR, or `--achievement`), and set status closed. Run when work ships; then,
    /// when run interactively inside a feature worktree, it reclaims that worktree the same way
    /// `wt cleanup` does — removes it, deletes the branch, and kills its tmux window — provided
    /// the PR is merged and the tree is clean (use `--force` to reclaim regardless).
    Close {
        /// Achievement line to log instead of the one derived from the Goal + PR.
        #[arg(long)]
        achievement: Option<String>,
        /// Reclaim the worktree even if the PR isn't merged or the tree is dirty.
        #[arg(long)]
        force: bool,
    },
    /// Reopen a closed branch note.
    Reopen,
    /// Cut a GitHub release end-to-end (#627): bump the semver from the last tag (default patch,
    /// `--bump major|minor|patch`), create + push the tag, and `gh release create` with notes
    /// rolled up from the PRs merged since the last tag. Gated + idempotent — refuses on a dirty or
    /// behind-`origin/main` tree, and never releases the same commit twice. Runs the local `bn
    /// verify` gate first to confirm `main` is green (`--skip-verify` trusts the caller); `--dry-run`
    /// prints the tag + notes without touching git/GitHub.
    Release {
        /// Which semver component to bump: major | minor | patch (default: patch).
        #[arg(long)]
        bump: Option<String>,
        /// Print the tag + rolled-up notes without creating/pushing the tag or the release.
        #[arg(long)]
        dry_run: bool,
        /// Skip the local `bn verify` gate (trust the caller that `main` is green).
        #[arg(long)]
        skip_verify: bool,
    },
}

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
    /// Stage tracked changes, commit (default `wip: …`), and push if an upstream is set.
    #[command(visible_alias = "ap")]
    Autopush { message: Vec<String> },
    /// Git diff stat vs origin/main, the current note's open todos + blockers, and the
    /// commit-count ahead. A quick "where am I" for the branch.
    #[command(visible_alias = "d")]
    Diff,
    /// Open a URL (or file) in the system browser, cross-platform (WSL → Windows). Prints the
    /// URL as a fallback when no opener works, so it's safe as a `$BROWSER` command. Replaces the
    /// `workflow/bin/wf-open` helper.
    Open { url: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CmdMisc {
    /// SIGTERM (then SIGKILL after a grace period) every process listening on a TCP port.
    Kill {
        /// TCP port to free.
        port: String,
    },
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
    /// agent in a worktree that already has one. Outside a repo they fall through to the real
    /// binary. `agency claude` / `agency copilot` remain the explicit pooled-scout form. Source
    /// from your shell rc, e.g. `source <(bn shell-init zsh)` (bash|zsh).
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
    /// (the SAME script `wt pool warm` runs in each slot, and the cron resolves). Defaults to
    /// `build`. Runs in the current dir with `~/.local/bin:~/.bin` prepended to PATH.
    Build {
        /// Script name (default `build` → `build.sh`).
        name: Option<String>,
    },
    /// Start the bn-web dashboard server (#386/#388): a thin launcher that execs the sibling
    /// `bn-web` binary (native axum server over bn-core), the same way `bn-mcp` sits beside `bn`.
    /// `--daemon` registers a managed, auto-restarting service (launchd on macOS, systemd `--user`
    /// on Linux, else a detached child) so the dashboard survives this shell, a crash, and a reboot
    /// (#499). `bn serve status` reports running/down/not-installed; `bn serve stop` unloads it.
    /// Bare `bn serve` binds loopback:7770; `--lan` exposes it on 0.0.0.0.
    #[command(args_conflicts_with_subcommands = true)]
    Serve {
        #[command(subcommand)]
        action: Option<ServeCmd>,
        /// Port to bind (default: config `[web].port`, else 7770).
        #[arg(long)]
        port: Option<u16>,
        /// Bind all interfaces (`0.0.0.0`) so the dashboard is reachable on the LAN.
        #[arg(long)]
        lan: bool,
        /// Run in the background via a managed auto-restarting service (launchd/systemd, else a
        /// detached child) instead of the foreground.
        #[arg(long)]
        daemon: bool,
    },
    /// Unified token usage + **$ cost** across all agent work (#381), categorized by work-class
    /// (Orchestration / Coding / Scheduled / …). Two clearly-labeled units: **AIC** (the real
    /// Copilot bill) and **$ (API-equiv)** (a cache-aware price-table benchmark). Defaults to a
    /// unified month-to-date report: `bn cost [--by category|repo|model|machine|day]
    /// [--window <days>] [--live] [--json]`.
    Cost {
        /// Breakdown dimension: category, repo, model, machine, or day (omitted = unified report).
        #[arg(long)]
        by: Option<String>,
        /// Trailing window in days (default: current month-to-date).
        #[arg(long, value_name = "DAYS")]
        window: Option<i64>,
        /// Show in-flight usage of currently-running agents instead of recorded history.
        #[arg(long)]
        live: bool,
    },
    /// Reset tmux to a clean slate: back up the tmux-resurrect saved state to a tarball, wipe it
    /// so tmux-continuum won't auto-restore the old sessions, then kill the tmux server (tmux
    /// starts fresh next launch). Run from inside tmux and it ends the current session too.
    /// `--no-backup` skips the tarball; `--yes` (`--force`) skips the confirmation.
    KillTmux {
        /// Skip the tarball backup of the resurrect state before wiping it.
        #[arg(long)]
        no_backup: bool,
        /// Skip the confirmation prompt.
        #[arg(long, visible_alias = "force")]
        yes: bool,
    },
    /// Auto-file a bn issue on an identified problem (#345 proposal 4): the orchestrator or an
    /// agent captures a bug / friction / blocker as a labeled, **deduped** GitHub issue instead of
    /// relying on a human to file it. `bn issue file "<title>"` checks the repo's OPEN issues first
    /// and reports the existing one when a title already matches — so re-running is idempotent.
    Issue {
        #[command(subcommand)]
        action: IssueCmd,
    },
    /// Reusable **reference** docs (#623): per-topic coding standards / preferences that shape how
    /// agents write code and how reviews check diffs — the sibling of `skills` (reusable *workflow*
    /// docs) for reusable *standards*. Bare `bn ref` lists them (name + one-line summary); `bn ref
    /// <name>` prints one (fuzzy/prefix match, e.g. `bn ref javascript`); `bn ref edit <name>` opens
    /// it in $EDITOR (creating from a template if missing); `bn ref add <name>` scaffolds a new one.
    /// Stored cross-machine as `<personal_root>/references/<name>.md` (synced with your notes).
    #[command(args_conflicts_with_subcommands = true)]
    Ref {
        #[command(subcommand)]
        action: Option<RefCmd>,
        /// Reference name to print (fuzzy/prefix match). Omit to list all references.
        name: Option<String>,
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
    /// Managed "tools" (services) subsystem (#653): declaratively deploy and keep long-running
    /// local developer tools alive. Backed by docker (via `docker`, falling back to `docker.exe`
    /// on WSL). Built-in specs (pilot: **n8n**) are opt-in via `[tools].builtins` /
    /// `$BN_TOOLS_BUILTINS`; user tools live in `<config_dir>/tools.toml`.
    /// `bn tools list` shows declared tools; `bn tools status [name]` reports run-state + health;
    /// `bn tools deploy <name>` brings one up (create/start, idempotent); `bn tools reconcile
    /// [name]` re-asserts the desired "up" state (the keep-alive primitive the cron loop calls);
    /// `bn tools down <name>` tears one down.
    Tools {
        #[command(subcommand)]
        action: ToolsCmd,
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
    /// Opt-in keratoconus protective-behaviour coach. `check-in` records today's habits, bare
    /// `bn eye` shows the no-rubbing streak + weekly adherence, and `review` records monitoring.
    /// The `staff-eye-daily` / `staff-eye-weekly` jobs use the same deterministic packet.
    Eye {
        #[command(subcommand)]
        action: Option<EyeCmd>,
    },
    /// Routine-aware personal schedule coach (#801): a timezone-aware `[routine]` weekly recurring
    /// schedule (work/learning, family/recovery/exercise/sleep, …). Bare `bn routine` shows status;
    /// `now`/`next` report the current/upcoming block; `check-in`/`snooze`/`skip`/`override` record
    /// durable, non-destructive runtime state; `tick` (native `routine-tick` job, `notify` alias)
    /// delivers due reminders at-least-once with a bounded catch-up horizon; `watch` is a
    /// deadline-driven foreground loop sharing the same engine; `service` manages `watch` as a
    /// launchd/systemd daemon. Feeds `bn next`/`bn daily`: a protected block pauses non-urgent
    /// guidance but always keeps urgent/breaking items visible, with a transparent explanation.
    Routine {
        #[command(subcommand)]
        action: Option<RoutineCmd>,
    },
    /// Long-lived coordination-only agents. Controllers capture and route requests, dispatch
    /// workers, and own follow-through; they never implement code themselves.
    Control {
        #[command(subcommand)]
        action: ControlCmd,
    },
    /// Print the running binary's crate version and compile-time build provenance.
    Version,
    /// Print one bounded current-state snapshot of live agents, orchestration, and attention.
    Bearings,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ControlCmd {
    /// Start a controller, or enqueue a request onto an already-running controller with --id.
    Start {
        /// Stable controller id. Omit to generate a new id.
        #[arg(long)]
        id: Option<String>,
        /// Readable task name used by the session registry and tmux window.
        #[arg(long)]
        name: Option<String>,
        /// Harness to launch: copilot or claude. Omit to use the configured default
        /// (`[harness].default` / `$BN_HARNESS`, else copilot).
        #[arg(long)]
        harness: Option<String>,
        /// Initial request to enqueue for this controller.
        #[arg(value_name = "REQUEST")]
        request: Vec<String>,
    },
    /// Show controllers, their durable request queues, and worker lanes joined by controller_id.
    Status {
        /// Show one controller only.
        #[arg(long)]
        id: Option<String>,
    },
    /// Stop one controller and close its tmux window.
    Stop {
        #[arg(long)]
        id: String,
    },
    /// Lease the next durable request. Internal controller-agent protocol.
    #[command(hide = true)]
    Lease {
        #[arg(long)]
        id: String,
    },
    /// Mark a leased request done or failed. Internal controller-agent protocol.
    #[command(hide = true)]
    Complete {
        #[arg(long)]
        id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        failed: bool,
    },
    /// Tail owned-worker events and wake the controller. Internal controller-agent protocol.
    #[command(hide = true)]
    Watch {
        #[arg(long)]
        id: String,
    },
    /// Guard a controller turn from ending while live work has lost its watcher.
    #[command(hide = true)]
    TurnEndGuard {
        #[arg(long)]
        id: Option<String>,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub(crate) enum EyeMode {
    Daily,
    Weekly,
}

impl EyeMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum EyeCmd {
    /// Record one date-scoped protective-behaviour check-in.
    CheckIn {
        #[arg(long)]
        date: Option<String>,
        /// Eye rubbing happened today (omitting means a clean no-rubbing day).
        #[arg(long)]
        rubbed: bool,
        #[arg(long)]
        drops: bool,
        #[arg(long)]
        greens: bool,
        #[arg(long = "omega-3")]
        omega_3: bool,
        #[arg(long)]
        sunglasses: bool,
        #[arg(long = "screen-breaks")]
        screen_breaks: bool,
        #[arg(long = "slept-back")]
        slept_back: bool,
    },
    /// Show the current no-rubbing streak, weekly adherence/misses, and monitoring status.
    Status {
        #[arg(long)]
        date: Option<String>,
    },
    /// Print the deterministic daily or weekly staff packet JSON.
    Gather {
        #[arg(value_enum, default_value_t = EyeMode::Daily)]
        mode: EyeMode,
        #[arg(long)]
        date: Option<String>,
    },
    /// Run the pinned eye staff skill now and route its validated card.
    Run {
        #[arg(value_enum, default_value_t = EyeMode::Daily)]
        mode: EyeMode,
        #[arg(long)]
        date: Option<String>,
    },
    /// Record the date of a completed topography/ophthalmology monitoring review.
    Review {
        #[arg(long)]
        date: Option<String>,
    },
}

/// The `bn routine` subcommands (#801) — the routine-aware personal schedule coach.
#[derive(Subcommand, Debug)]
pub(crate) enum RoutineCmd {
    /// The block covering right now, if any (default when no subcommand is given).
    Now,
    /// The next upcoming block (today or later).
    Next,
    /// Full status: current + next block, remaining/until-next minutes, alignment, timezone,
    /// UTC offset, DST adjustment, active skip/snooze/override.
    Status,
    /// Record that you checked in with the routine right now (an adherence acknowledgment;
    /// never touches the recurring schedule).
    CheckIn {
        /// Optional free-form note (e.g. what you actually did instead).
        #[arg(long)]
        note: Option<String>,
    },
    /// Silence notifications until `--minutes` from now (status/`now`/`next` stay unaffected —
    /// only outbound reminders are gated).
    Snooze {
        #[arg(long, default_value_t = 30)]
        minutes: i64,
    },
    /// Skip just today's occurrence of a block (future occurrences are unaffected).
    Skip {
        /// Block id (e.g. `b0`) — defaults to whichever block is current right now.
        #[arg(long)]
        block: Option<String>,
        /// Calendar date (routine timezone, `YYYY-MM-DD`) — defaults to today.
        #[arg(long)]
        date: Option<String>,
    },
    /// Declare a temporary status that supersedes the recurring schedule until it expires —
    /// without ever rewriting `[routine.blocks]`.
    Override {
        /// Free-form label (e.g. `"family emergency"`).
        name: String,
        /// Category (e.g. `family`, `work`) — informational only, doesn't affect suppression.
        #[arg(long, default_value = "override")]
        kind: String,
        /// Minutes until the override expires and the recurring schedule resumes.
        #[arg(long, default_value_t = 60)]
        minutes: i64,
    },
    /// End the active override early, reverting to the recurring schedule immediately.
    ClearOverride,
    /// Run one shared evaluation: deliver any due reminder/start events (at-least-once, deduped
    /// by stable block id + resolved instant + event kind, bounded by `catchup_horizon_minutes`),
    /// persist health, and report evaluated_at/active/due/sent/deduped/expired/backend/next-wake.
    /// This is the native `routine-tick` cron/timer job; `bn routine watch` and the managed
    /// `routine` service call the exact same engine on a deadline-driven loop instead of a fixed
    /// interval.
    Tick,
    /// Compatibility alias for [`RoutineCmd::Tick`] — identical delivery, kept for existing
    /// muscle memory / scripts.
    Notify,
    /// Foreground deadline-driven reconciliation loop: run the shared tick, then sleep until the
    /// earliest reminder/start/end/snooze/override deadline (or a bounded reconciliation cap)
    /// instead of polling. Re-resolves `timezone = "local"`/config on every wake, so suspend/
    /// resume, clock jumps, DST transitions, and an invalid-then-fixed config are all handled by
    /// the same reconciliation. Exits cleanly on SIGTERM/Ctrl-C.
    Watch,
    /// Manage the long-running `bn routine watch` as a machine-managed service (launchd on
    /// macOS, systemd `--user` on Linux) — starts at login/reboot and restarts on crash.
    Service {
        #[command(subcommand)]
        action: RoutineServiceCmd,
    },
}

/// The `bn routine service` subcommands (#801 amendment) — managed lifecycle for
/// `bn routine watch`.
#[derive(Subcommand, Debug)]
pub(crate) enum RoutineServiceCmd {
    /// Register + start the managed watch service (launchd/systemd), or report that no
    /// supported service manager is available — never a detached fallback claiming reboot/crash
    /// coverage it doesn't have; use `bn cron register` for the `routine-tick` timer instead.
    Install,
    /// Stop and unregister the managed watch service.
    Stop,
    /// Restart the managed watch service (stop, then install again).
    Restart,
    /// Report installed/active state, health freshness, and coverage (covered/degraded/
    /// uncovered).
    Status,
}

/// The `bn pipeline` subcommands (PIPE-A, EPIC #746) — read/drive a worktree's pipeline stage.
#[derive(Subcommand, Debug)]
pub(crate) enum PipelineCmd {
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

/// The `bn worker` subcommands (#719 — the attempt-registry slice of epic #707 M1).
#[derive(Subcommand, Debug)]
pub(crate) enum WorkerCmd {
    /// Report this worker's capabilities/labels, a capacity snapshot (RAM/swap/load/disk), its
    /// derived READY/PRESSURE admission status, and the durable attempt registry. `--json` (the
    /// global flag) emits the versioned machine payload; the plain form prints a human summary.
    Status,
    /// Close a running attempt: RUNNING → SUCCEEDED (default) or FAILED (`--fail`). Called by the
    /// spawn wrapper (`tmux ... 'agent-cmd ; bn worker finish <request_id>'`). Records the exit
    /// code so the outcome is retrievable by id. Resolves the request's most recent RUNNING
    /// attempt.
    Finish {
        /// The request whose running attempt is finishing.
        request_id: String,
        /// Mark the attempt FAILED instead of SUCCEEDED.
        #[arg(long)]
        fail: bool,
        /// The agent's exit status to record on the attempt.
        #[arg(long)]
        exit_code: Option<i64>,
    },
    /// Worker-authoritative atomic admission (#720, epic #707 M1). Opens the registry with
    /// `BEGIN IMMEDIATE`, re-reads `MemAvailable` inside the txn, self-heals dead reservations,
    /// enforces the memory guard (`estimate + 512MB`) and the single M1 slot, then records the
    /// `RUNNING` reservation BEFORE any worktree/tmux side effect. Prints the verdict
    /// (`accepted` / `already_exists` / `rejected_capacity`); exit 0 accepted or already_exists,
    /// 1 rejected, 2 internal error.
    Start {
        /// The dispatch request id — the idempotency/dedupe key `(repo, id)` maps onto.
        #[arg(long)]
        request_id: String,
        /// This attempt's unique id; a retried dispatch reuses the exact id → `already_exists`.
        #[arg(long)]
        attempt_id: String,
        /// A human/scheduler label for what the attempt runs (e.g. the issue number).
        #[arg(long)]
        task: String,
        /// Repository the worker-local dispatch should create the lane in.
        #[arg(long)]
        repo: String,
        /// Agent harness passed to the canonical worker-local dispatch.
        #[arg(long, default_value = "copilot")]
        harness: String,
        /// The agent's estimated memory ceiling (MB); admission needs `MemAvailable >= this + 512`
        /// and the launch is wrapped in a cgroup cap at this ceiling.
        #[arg(long)]
        memory_ceiling_mb: u64,
    },
    /// Internal monitor process kept inside the attempt's `tmux -L orch` liveness session.
    #[command(hide = true)]
    Run {
        #[arg(long)]
        attempt_id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        memory_ceiling_mb: u64,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Sweep RUNNING reservations and clear the orphaned ones — a manual counterpart to the
    /// self-heal step of `start`. Run after a reboot to release attempts whose tmux session/pid is
    /// gone. Prints the cleared attempt ids.
    Reconcile,
}

/// The `bn tools` subcommands (#653 — managed docker-backed services).
#[derive(Subcommand, Debug)]
pub(crate) enum ToolsCmd {
    /// List every declared tool (opted-in built-ins overlaid by `<config_dir>/tools.toml`) with
    /// its image.
    List,
    /// Report run-state + health for one tool, or all tools when the name is omitted.
    Status {
        /// Tool name (omit for every declared tool).
        name: Option<String>,
    },
    /// Bring a tool up: create the container if absent, start it if stopped (idempotent).
    Deploy {
        /// Tool name.
        name: String,
    },
    /// Re-assert the "up" desired state for one tool, or all tools when the name is omitted — the
    /// keep-alive primitive the `tools-reconcile` cron job calls.
    Reconcile {
        /// Tool name (omit to reconcile every declared tool).
        name: Option<String>,
    },
    /// Tear a tool down: `docker rm -f <name>` (idempotent — a missing container is not an error).
    Down {
        /// Tool name.
        name: String,
    },
}

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
    Fleet(CmdFleet),
    #[command(flatten)]
    Staff(CmdStaff),
    #[command(flatten)]
    ShellUi(CmdShellUi),
    #[command(flatten)]
    GitPr(CmdGitPr),
    #[command(flatten)]
    Misc(CmdMisc),
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
pub(crate) enum QuarterCmd {
    /// Add a quarter goal: bn quarter add <text...> [--key <key>] [--metric <hint>].
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        /// Stable, greppable key the goal lives under (for --q tagging + reviews). Derived from
        /// the text when omitted.
        #[arg(long)]
        key: Option<String>,
        /// Optional metric hint for the impact agent (e.g. `pr_cycle_time<-30%`). Not enforced.
        #[arg(long)]
        metric: Option<String>,
    },
    /// Mark a quarter goal done by key or text substring: bn quarter done <key|substring...>.
    Done {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Open the current quarter file in $EDITOR.
    Edit,
    /// Run the impact agent now: score the window's shipped work against the quarter goals, append
    /// a validated review, and flag at-risk/off-track goals as attention items.
    Review {
        /// Cron gate: exit silently (0) unless `[quarter].enabled`. The quarter-review job passes
        /// this; an interactive `bn quarter review` always runs on demand.
        #[arg(long, hide = true)]
        cron: bool,
        /// Harness to drive (copilot / claude); overrides `[quarter].harness` / `[review].harness`.
        #[arg(long)]
        harness: Option<String>,
    },
    /// Advise next (#458): rank the pending backlog into a goal-aligned shortlist — each candidate
    /// scored by which quarter goal it advances (slipping goals first), lifted by open attention on
    /// its repo, with a rationale. `--json` is what the orchestrator consumes for task selection.
    Advise {
        /// Shortlist size — how many free slots to fill (the top `N` ranks are flagged in-capacity).
        /// Lower-ranked candidates still print/serialize below the cut.
        #[arg(long, default_value_t = 3)]
        capacity: usize,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum TrackCmd {
    /// What was I doing at a given clock time today: bn track at <HH:MM>.
    At {
        #[arg(value_name = "HH:MM")]
        time: String,
    },
    /// Roll today's tracked time up by bucket: bn track report [--by activity|tag|branch|task].
    Report {
        /// Group by: activity (default), tag, branch, or task/link.
        #[arg(long, default_value = "activity")]
        by: String,
    },
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
    /// React to a staff briefing card: bn feed rate <card> up|down [--note "…"]. One-keystroke
    /// feedback that tells an agent (e.g. the Mentor) whether its card earned its cost. `<card>`
    /// is an agent name (its newest card), a YYYY-MM-DD date, or an exact <date>/<agent>. The
    /// rating is stored beside the read-marks and feeds the `bn insights` staff performance review;
    /// a down-vote note also accrues as prompt-tuning material.
    Rate {
        /// Which card: an agent name, a YYYY-MM-DD date, or <date>/<agent>.
        card: String,
        /// The reaction: up | down.
        rating: String,
        /// Why — optional. Down-vote notes accumulate as tuning material for the agent's prompt.
        #[arg(long)]
        note: Option<String>,
    },
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
        /// The head sha reviewed (`git rev-parse HEAD`). Required — approvals are pinned to it.
        #[arg(long)]
        sha: String,
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
    /// Mark a suggestion accepted (you intend to act on it; promotion to a work item is #578).
    Accept { id: String },
    /// Dismiss a suggestion — it stays recorded so the agent never re-proposes it.
    Dismiss { id: String },
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

#[derive(Subcommand, Debug)]
pub(crate) enum RemoteCmd {
    /// Scaffold `.github/workflows/claude-dispatch.yml` into the current repo and verify the
    /// `ANTHROPIC_API_KEY` secret. Idempotent (rewrites the bn-managed workflow on drift); prints
    /// the `gh secret set` instruction when the API key isn't set yet.
    Init,
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
}

#[derive(Subcommand, Debug)]
pub(crate) enum FleetCmd {
    /// Jump to any active agent's tmux window via an fzf picker — the interactive companion to the
    /// read-only `bn fleet` board (#347). Lists every live agent window across all sessions **and**
    /// the orchestrator cockpit (`planning`, #345); the fzf `--preview` is a live `tmux
    /// capture-pane` of the highlighted agent. Select → jump focus there (`switch-client` inside
    /// tmux, `attach` from outside). Reads tmux as the source of truth. Degrades to the board when
    /// fzf is absent.
    Pick {
        /// Scope the picker to *dispatched* agents only — the `dispatch/*` worktrees `bn dispatch`
        /// / `bn orchestrate` launched — dropping manually-run agents and the orchestrator cockpit.
        #[arg(long)]
        dispatched: bool,
        /// Internal: the fzf preview callback stage a `pick` session re-invokes.
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<agent_live::FleetPickStage>,
        /// Internal: the focused row's tmux window id, passed to `--pick-stage preview`.
        #[arg(hide = true)]
        target: Option<String>,
    },
    /// Single-key tmux jumper (#633): a native `display-menu` popup, one entry per live agent
    /// window plus the orchestrator cockpit, each with an accelerator key (1-9, a-z) and a short
    /// state badge (needs-you / working / idle …), plus a `/` entry that opens the fzf `bn fleet
    /// pick` in a `display-popup` search overlay (#633 P2). Press the key → jump focus to that
    /// window (`switch-client`). Pass `<issue|repo>` to jump straight to the one window it names
    /// (numeric issue# or repo/branch fragment) with no menu. Zero typing, big tap targets — the
    /// mobile/one-handed companion to the fzf-based `bn fleet pick`. Prints the read board when
    /// tmux is unavailable.
    Jump {
        /// Optional issue#/repo/branch fragment: jump directly to the single window it matches,
        /// skipping the menu. Omit it to raise the single-key `display-menu` popup.
        query: Option<String>,
    },
    /// Quick-steer an agent from a fleet row (#633 P3): a native `display-menu` of the live fleet
    /// (one entry per agent window + orchestrator cockpit, needs-you first); pick one to open a
    /// single-key action submenu — Approve (1/2), Continue, Send text, Interrupt (Ctrl-C), Kill —
    /// that steers the agent **without** navigating into its pane. The common answers agents block
    /// on, one-handed on a phone. Text steers reuse `bn agent send --keys`. Pass `<target>` (a tmux
    /// `window_id` or `repo:branch`) to jump straight to the action menu for that one window. Prints
    /// the board when tmux is unavailable.
    Steer {
        /// Optional tmux `window_id` (or `repo:branch`): open the action submenu for that one
        /// window directly. Omit it to raise the fleet-row picker first.
        target: Option<String>,
    },
    /// Garbage-collect the fleet (#503): find orphaned/stale agents — a process still alive whose
    /// tmux window is gone, worktree removed, or PR merged/closed — and terminate the **full
    /// process tree**, then prune the dead window. Also prunes `remain-on-exit` dead panes/windows.
    /// Dry-run + `--json` by default (shows what *would* be reaped); pass `--kill` (`--yes`) to act.
    /// Never touches an actively-working agent, the orchestrator/`--resume` session, or an agent
    /// with uncommitted work unless `--force`.
    Reap {
        /// Act: kill the stale process trees and prune the dead windows. Without it, a dry run.
        #[arg(long, visible_alias = "yes")]
        kill: bool,
        /// Override the safety gates — reap even a working / uncommitted / just-quiet stale agent.
        /// Never overrides the orchestrator guard (the supervisor is never reaped).
        #[arg(long)]
        force: bool,
        /// Grace seconds: a stale agent quieter than this is spared (launch/teardown race guard).
        #[arg(long, default_value_t = bn_core::fleet_gc::DEFAULT_MIN_IDLE_SECS)]
        min_idle: i64,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum NightCmd {
    /// Dry-run: show what tonight's shift would pick and why each candidate is skipped. No side
    /// effects — no worktrees, no agents, no audit entries.
    Plan,
    /// Execute the shift now (what the `night-shift` cron calls): plan, then dispatch each pick
    /// into its worktree with an agent, recording an audit entry per item. Respects the kill switch
    /// and the run window (declines outside it unless `--force`).
    Run {
        /// Override `[night] max_dispatch` for this run (the hard cap on picks).
        #[arg(long)]
        max: Option<usize>,
        /// Run even outside the configured window (still honors the kill switch + allowlist).
        #[arg(long)]
        force: bool,
    },
    /// The morning digest: what the shift dispatched last night and where it landed. Adds a
    /// `bn daily` goal per PR waiting on review ("review night PR bn#245").
    Report {
        /// How far back to read the audit log, in hours (default 24).
        #[arg(long, default_value_t = 24)]
        hours: i64,
    },
    /// Kill switch — disable the shift. The `night-shift` cron checks this first and no-ops while
    /// it's set, so you can pause autonomy without touching config.
    Off,
    /// Clear the kill switch — re-enable the shift (the inverse of `off`).
    On,
    /// Show the shift's configuration and current state: enabled, window, cap, allowlist, and
    /// whether the kill switch is engaged.
    Status,
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

#[derive(Subcommand, Debug)]
pub(crate) enum DojoCmd {
    /// Quick-fire recall: quiz cards from your deck (spaced-repetition scheduled).
    Quiz {
        /// Limit to one repo (overrides the `[dojo] repos` rotation).
        #[arg(long)]
        repo: Option<String>,
        /// Cards to present (overrides `[dojo] daily_target`).
        #[arg(long)]
        max: Option<usize>,
    },
    /// A debugging drill: a real past `fix:` bug re-posed as a mystery — symptom + pre-fix code
    /// shown, the actual fix hidden until you answer.
    Bug {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        max: Option<usize>,
    },
    /// Guided architecture tour of a subsystem, then a drill. `<area>` is a module name (e.g.
    /// `store`, `agent`); bare `bn dojo map` lists the areas you can tour.
    Map {
        /// The subsystem/module to tour.
        area: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Streak, XP + level per repo, and your weakest areas.
    Stats,
    /// Which repos are in rotation and how many cards each holds.
    Config,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ChiefCmd {
    /// The 05:30 morning brief — start the day with the decision made.
    Am,
    /// The 16:45 evening shutdown — close out with tomorrow's first task pre-chosen.
    Pm,
}

#[derive(Subcommand, Debug)]
/// The plan-review gate actions (#349). `submit`/`status` are the dispatched agent's write/poll
/// side; `list`/`show`/`approve`/`request-changes`/`reject` are the human's review side (also
/// reachable from the dashboard).
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
/// The Curriculum Director actions (#260): `plan` previews the weekly card read-only, `run`
/// applies its idempotent side-effects, `gather` prints the packet JSON.
pub(crate) enum CurriculumCmd {
    /// Preview this week's plan as a briefing card, read-only (no goals/notes/drills seeded).
    Plan,
    /// Run the full pipeline and apply its idempotent side-effects (Monday goal, learn note,
    /// dojo drills), write the card, and mirror it to the feed.
    Run {
        /// Ask a harness to find the one best resource (a network read); otherwise a
        /// deterministic self-study resource is used.
        #[arg(long)]
        enrich: bool,
    },
    /// Print the deterministic packet JSON — the agent-facing gather output.
    Gather,
}

#[derive(Subcommand, Debug)]
/// The Interviewer's weekly retro actions (#261).
pub(crate) enum RetroCmd {
    /// Run the interactive weekly retro: three fixed questions plus one adaptive follow-up.
    Chat {
        /// Speak your answers — record + transcribe each instead of typing (needs voice tooling
        /// configured via `$BN_VOICE_TRANSCRIBE`).
        #[arg(long)]
        voice: bool,
    },
    /// Explicitly skip this week's retro — recorded as skipped, not silently missed.
    Skip,
    /// Print a recorded week's transcript (defaults to the current ISO week).
    Show {
        /// ISO week label (e.g. `2026-W28`); omit for the current week.
        week: Option<String>,
    },
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
    /// Start an item: open/switch to its worktree tmux window and mark it in-flight (the IN
    /// PROGRESS lane). Repo-less items open a planning-session scratch window that scopes and
    /// dispatches the real worktree. With --agent, launch the harness: `auto` submits it (runs),
    /// `assist` stages it in the pane for you to run.
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
    /// Promote an item into the orchestrate backlog as a new task, then resolve it off the list
    /// (#460): bn attn dispatch <id> [--repo R]. The pure policy picks the lane — start-new (a
    /// fresh task), reprioritize (bump an already-queued twin, never duplicate), or interrupt (an
    /// urgent item onto the #378 priority lane). Targets the item's own repo when it carries one,
    /// else `--repo` or the current worktree's.
    Dispatch {
        id: String,
        /// Target repo for the backlog task (defaults to the item's repo, else the current one).
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
pub(crate) enum TaskCmd {
    /// List open tasks (default — same as bare `bn task`): today's work first, then unscheduled.
    List,
    /// Add a task: bn task add "<text>" [-p A] [--due YYYY-MM-DD] [--src S]. The text may inline
    /// +project, @context, and any `key:value` meta — so `bn task add "fix it repo:atlas day:
    /// 2026-08-03 est:2h"` sets those directly. `repo:` defaults to the repo you're standing in.
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        /// Priority A | B | C (= severity urgent | warn | info).
        #[arg(short = 'p', long)]
        priority: Option<char>,
        /// Due date, YYYY-MM-DD.
        #[arg(long)]
        due: Option<String>,
        /// Who's adding it (default manual). Agents/cron pass their name.
        #[arg(long, default_value = "manual")]
        src: String,
    },
    /// Mark a task done: bn task do <id>.
    Do { id: String },
    /// Mark a task in progress (@wip): bn task wip <id>.
    Wip { id: String },
    /// Hide a task until a date: bn task snooze <id> <YYYY-MM-DD>.
    Snooze { id: String, until: String },
    /// Close a task as cancelled (distinct from done): bn task cancel <id> [reason…].
    Cancel {
        id: String,
        #[arg(trailing_var_arg = true)]
        reason: Vec<String>,
    },
    /// Batch-add todos from stdin (JSON array/object or JSONL). Fields: text/title, priority
    /// (A|B|C) or severity, key, due, snooze, branch, src. Idempotent by key (namespaced by
    /// source); with --replace, open keyed todos from the source(s) absent from the batch close.
    Ingest {
        /// Default source for rows without `src` (usually the job/script name).
        #[arg(long, default_value = "script")]
        source: String,
        /// Default branch tag for rows without `branch` (else the current branch).
        #[arg(long)]
        branch: Option<String>,
        /// Treat stdin as the source's complete current set: mark done any open keyed todo from
        /// this source that's absent from the batch.
        #[arg(long)]
        replace: bool,
    },
    /// Open a task's context doc — the long form the one-line task can't hold: why it matters,
    /// the `## Done When` checklist the SMART gate reads, investigation findings, open questions.
    /// Created from a template on first use. Lives in the notes tree, beside the branch note once
    /// the task is picked up. Prints the path; `--open` launches $EDITOR.
    Doc {
        /// Task id (e.g. t42).
        id: String,
        /// Open it in $EDITOR instead of just printing the path.
        #[arg(long)]
        open: bool,
    },
    /// Refill the task list from the GitHub-issue reservoir, bounded by a low-water mark: it tops
    /// each repo back up to N actionable tasks and no further, so the store stays readable.
    /// Idempotent — an issue already pulled is never minted twice. Pulled tasks carry no `smart:`
    /// verdict, so the gate routes them to the improve agent before anything executes them.
    Pull {
        /// Tasks to keep actionable per repo (the low-water mark).
        #[arg(long, short = 'n', default_value_t = bn_core::orchestrate::DEFAULT_KEEP_WARM_LOW_WATER)]
        low_water: usize,
        /// Show what would be pulled and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Make a task executable: fill in why it matters, the `## Done When` the SMART gate reads,
    /// an estimate and a day. Prints the agent seed with --dry-run. `--steer` passes your own
    /// "things to look out for" straight into the seed.
    Improve {
        /// Task id (e.g. t42). Omit with --due to take the next task needing work.
        id: Option<String>,
        /// Things to look out for, passed to the agent verbatim.
        #[arg(long)]
        steer: Option<String>,
        /// Take the highest-ranked task that failed the gate, rather than a named one.
        #[arg(long)]
        due: bool,
        /// Print the seed instead of launching an agent.
        #[arg(long)]
        dry_run: bool,
    },
    /// Decide what is genuinely doable today: rank every open task deterministically, then let an
    /// agent reorder the shortlist and stamp `day:`. Writes its reasoning to the day's triage note.
    Triage {
        /// The day to triage for (default today).
        #[arg(long, value_name = "DATE")]
        day: Option<String>,
        /// Print the ranking and the seed instead of launching an agent.
        #[arg(long)]
        dry_run: bool,
    },
    /// Absorb a legacy repo-local `TODO.txt` into the global store: bn task import <path>.
    /// Idempotent — re-running imports only what's new. Ids are reminted globally, and `repo:` is
    /// stamped from `--repo` or the source file's directory name. The original is left in place.
    Import {
        /// The legacy todo.txt file to read.
        path: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum FocusCmd {
    /// Open a deep-work block: bn focus start <dur>. Duration accepts 90m, 2h, 1h30m, or a bare
    /// number of minutes (90). Starting while one is active extends the quiet window.
    Start { dur: String },
    /// Close the active block early and drain the queue — "while you were heads-down: …".
    End,
    /// Show the active block (remaining time), or that there's none. Same as bare `bn focus`.
    Status,
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
pub(crate) enum AfkCmd {
    /// Enter away mode: routine fleet activity is absorbed until you return.
    Start,
    /// Leave away mode and print ONE batched digest of what needs you.
    End,
    /// Show away status (and a quick count of what needs you). Same as bare `bn afk`.
    Status,
}

#[derive(Subcommand, Debug)]
pub(crate) enum AgentCmd {
    /// Recent agent sessions for a branch (default — same as bare `bn agent`). Resolves the
    /// current worktree, or pass --repo/--branch to target another.
    Sessions {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Fold not-yet-seen Claude + Copilot sessions into the metrics log: bn agent ingest.
    /// Idempotent by session id; called by the `agent-observe` job.
    Ingest,
    /// Mine this branch's recorded sessions into proposed note lines — a progress bullet, and
    /// (for code-changing sessions) a decision stub + follow-up todo — for work that never
    /// landed in the note. Propose-only, mirroring `bn sync`: `--json` emits and writes nothing;
    /// otherwise each line is confirmed (or `--yes`-applied). bn agent digest.
    Digest {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Apply every proposed line without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Live-agent fleet: every *running* Claude session with its health (active / idle / stuck),
    /// flagging any idle past `--stuck` minutes. The supervisor pass behind the FLEET cockpit
    /// panel and `--json`. bn agent supervise.
    Supervise {
        /// Minutes of quiet before an agent is *idle*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_IDLE_MINS)]
        idle: i64,
        /// Minutes of quiet before an agent is flagged *stuck*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
    },
    /// Joined live fleet across BOTH harnesses: every agent seen by its tmux window (@bn_agent
    /// working/waiting/done) and/or the session-log health (active/idle/stuck), one row per
    /// repo:branch. The machine-readable "what are all my agents doing" behind the fleet board;
    /// pairs with `bn fleet`. bn agent live [--json].
    Live {
        /// Minutes of quiet before an agent is *idle*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_IDLE_MINS)]
        idle: i64,
        /// Minutes of quiet before an agent is flagged *stuck*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
    },
    /// Self-schedule a check-in nudge: in `<mins>` minutes, deliver `<note>` to this worktree's
    /// agent pane (the `schedule_with_note.sh` analogue). Uses a transient systemd `--user` timer
    /// so it survives the agent going idle. bn agent checkin <mins> "<note>".
    Checkin {
        /// Minutes from now to fire the nudge.
        mins: i64,
        /// The message to deliver at the check-in.
        note: String,
        /// Target tmux window `repo:branch` (default: the current worktree).
        #[arg(long)]
        target: Option<String>,
    },
    /// Recover an interrupted ship turn: a worktree with commits ahead of base but no PR (its agent
    /// died before pushing). Dry-run by default (reports candidates); `--yes` pushes the branch and
    /// opens a PR (`gh pr create --fill`), recording it on the note. Targets the current worktree,
    /// `--repo/--branch`, or `--all` (sweep every worktree). bn agent recover.
    Recover {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Sweep every worktree, not just one target.
        #[arg(long)]
        all: bool,
        /// Base branch the work forked from (for the commits-ahead check). Default: main.
        #[arg(long)]
        base: Option<String>,
        /// Actually push + open the PR (default: dry-run report).
        #[arg(long)]
        yes: bool,
    },
    /// Signal completion back to the orchestrator (the agent runs this when it finishes) — the
    /// event-driven push. Raises a durable, keyed attention item (source `agent`, carrying repo/
    /// branch/PR link) that surfaces in `bn next` / the orchestrator's attention read, sets the
    /// window's `@bn_agent done` badge, and records it in the note. `--notify` also fires a desktop
    /// toast. Run inside the worktree, or target it with --repo/--branch. bn agent done.
    Done {
        /// One-line summary of what was accomplished.
        #[arg(long)]
        summary: Option<String>,
        /// PR URL, if one was opened (becomes the attention item's link).
        #[arg(long)]
        pr: Option<String>,
        /// Fire a desktop toast too (the immediate "wake" nudge).
        #[arg(long)]
        notify: bool,
        /// This finding is report-only — a recommendation or "nothing to fix", NOT a code change to
        /// ship. Marks the note so `bn promote` won't try to ship it (no empty PR). Scouts whose
        /// finding isn't code-shaped should pass this.
        #[arg(long)]
        report_only: bool,
        /// Report terminal worker failure instead of successful completion.
        #[arg(long)]
        failed: bool,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Steer a running agent with a message and get its reply. Default: the reliable `-p --resume`
    /// channel (harness + session id from the branch note) — works for both harnesses even against a
    /// live session, and returns the agent's answer. `--keys` instead pastes into the live tmux
    /// window and submits with a raw CR, verifying the pane took it (visible there; use when the
    /// agent is idle). `<target>` is `repo:branch`. bn agent send.
    Send {
        /// `repo:branch` of the agent (its note holds the recorded session id).
        target: String,
        /// The message to send.
        msg: String,
        /// Force the visible tmux path (paste + verified raw-CR submit) instead of `-p --resume`.
        #[arg(long)]
        keys: bool,
        /// Fire-and-forget: run the `-p --resume` turn DETACHED (own process group, survives the
        /// caller exiting) and return immediately, instead of blocking for the whole turn. For long
        /// ship tasks — the agent reports back with `bn agent done` when finished. Output is logged.
        #[arg(long)]
        detach: bool,
    },
    /// Kill a running agent by closing its tmux window (`repo:branch`, the `target` from
    /// `bn agent live`). Frees the slot when an agent is wedged past recovery — the hard stop the
    /// web dashboard's kill button and the orchestrator's give-up path call. Idempotent: a target
    /// with no window is reported as already gone. bn agent kill <target>.
    Kill {
        /// `repo:branch` of the agent — its tmux window name (slashes already dashed).
        target: String,
    },
    /// Capture the last N lines of a running agent's tmux pane (read-only). Powers the web
    /// dashboard's live-pane view; `<target>` is `repo:branch` (the `target` from `bn agent live`).
    /// bn agent pane <target> [--lines N].
    Pane {
        /// `repo:branch` of the agent — its tmux window name (slashes already dashed).
        target: String,
        /// How many trailing pane lines to return (default 40).
        #[arg(long, default_value_t = 40)]
        lines: usize,
        /// Include ANSI/SGR escape sequences (`tmux capture-pane -e`) so a terminal renderer (the
        /// web dashboard's live terminal) shows the pane's real colours/attributes instead of
        /// plain text. Off by default — the plain tail stays escape-free.
        #[arg(long)]
        ansi: bool,
    },
    /// Read a session's transcript as a readable timeline: your prompts, the agent's prose, and
    /// consecutive tool calls collapsed into one summary row. Powers the web dashboard's agent
    /// activity feed — unlike `pane`, it works for a session whose tmux window has already closed,
    /// because the transcript is on disk. `<session>` is the harness session id (the `session_id`
    /// from `bn agent live`). bn agent activity <session> [--limit N].
    Activity {
        /// The harness session id — a Claude transcript or a Copilot session directory.
        session: String,
        /// How many trailing timeline entries to return (default 40).
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Claude Code SessionStart hook: point this worktree's auto-memory at a per-worktree
    /// `.claude/memory` (written idempotently to the gitignored `.claude/settings.local.json`,
    /// since the checked-in settings ignore `autoMemoryDirectory`), then print `bn handoff read`
    /// to orient the session. Replaces `scripts/wt-memory.sh`. Best-effort, quiet on no note.
    MemoryHook,
    /// Launch an AI coding agent in the current directory, resuming this directory's recorded
    /// session if one exists, else starting fresh — the single home of the resume-or-fresh rule
    /// (tmux keybinds, `bn dispatch`, and a plain `cd <wt> && bn agent launch` all behave
    /// identically). Replaces the `workflow/bin/agent` zsh launcher. Execs the harness (this
    /// process is replaced), so it never returns on success. bn agent launch [copilot|claude].
    Launch {
        /// Harness to launch: `copilot` (default) or `claude`.
        #[arg(value_name = "HARNESS")]
        harness: Option<String>,
        /// Seed the first turn of a FRESH session with this prompt (ignored when resuming).
        #[arg(long)]
        seed: Option<String>,
        /// Read the seed prompt from a file (keeps the launch command short). A missing/unreadable
        /// file degrades to an unseeded launch rather than erroring.
        #[arg(long = "seed-file")]
        seed_file: Option<String>,
        /// Trusted-repo autonomy: run without approval prompts even when seeded.
        #[arg(long)]
        yolo: bool,
        /// Skip resume-detection and start a FRESH session even if this worktree has a recorded one
        /// (recovery re-seed, #383).
        #[arg(long)]
        fresh: bool,
        /// Pin the harness session id (so `bn agent send` can steer this exact session via resume).
        #[arg(long)]
        session: Option<String>,
        /// Prefer this exact recorded session before falling back to a fresh launch.
        #[arg(long, hide = true)]
        resume_session: Option<String>,
        /// Extra args passed through to the harness verbatim (everything after `--`).
        #[arg(last = true)]
        passthrough: Vec<String>,
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

/// `bn serve` subcommands (#499). Bare `bn serve` (no subcommand) runs/daemonizes the dashboard;
/// these manage the daemon's lifecycle.
#[derive(Subcommand, Debug)]
pub(crate) enum ServeCmd {
    /// Report an honest managed-service status: is a service installed (launchd/systemd) and is
    /// something listening on the port → running / down / not-installed (`--json` for the envelope).
    Status {
        /// Port to probe (default: the resolved `[web].port`, else 7770).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop and unregister the managed bn-web service (launchd on macOS / systemd on Linux),
    /// best-effort — the way to take down a crashed or looping daemon.
    Stop,
}

#[derive(Subcommand, Debug)]
pub(crate) enum TmuxMcpCmd {
    /// Ensure the configured managed bn-web/MCP service is reachable, starting it if needed.
    Ensure,
    /// Emit the configured tmux status-bar health segment.
    Badge,
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
}

/// The problem kind for `bn issue file`, each mapping to a canonical label.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssueKindArg {
    Bug,
    Friction,
    Blocker,
    Enhancement,
}

/// `bn orchestrate` subcommands. Bare `bn orchestrate` (no subcommand, no `--queue`) shows status.
#[derive(Subcommand, Debug)]
pub(crate) enum OrchestrateCmd {
    /// Diagnose orchestrator layout drift and stuck state: repo/jobs anchors, stale locks,
    /// referenced worktrees, resident runs, queue holds, wedged panes, retries, and saturation.
    /// Exits nonzero when a warning or error needs attention.
    Doctor {
        /// Diagnose only this repo's persisted run (machine-wide anchors and locks are always checked).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show runs: bare lists every live run; `--repo` shows one in full (queue / in-flight /
    /// merged / recovery events). Same as bare `bn orchestrate`.
    Status {
        /// Show only this repo's run, in full detail.
        #[arg(long)]
        repo: Option<String>,
        /// Explain the latest dispatch decision with lane capacity, held/skipped candidates, and
        /// queue wait times.
        #[arg(long)]
        why: bool,
        /// Show only active orchestrator degradations (tmux, daemon fallback, backlog maintenance).
        #[arg(long, conflicts_with = "why")]
        degraded: bool,
    },
    /// Show structured orchestrator events, newest first. Alias: `events`.
    #[command(visible_alias = "events")]
    Logs {
        /// Show only events for this repo (machine-wide `*` events are included).
        #[arg(long)]
        repo: Option<String>,
        /// Filter by severity.
        #[arg(long)]
        level: Option<OrchestrateLogLevelArg>,
        /// Maximum events to print.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Live monitoring view (#379): a self-refreshing rollup header (queue depth, in-flight with
    /// their tmux targets, merged/hr, ETA, cost today) over one row per in-flight agent. `--json`
    /// emits the enriched contract once and exits; `--plain` prints one static frame (wrap it in
    /// `watch -n` or pipe it) instead of looping.
    Watch {
        /// Watch only this repo's run (default: the only live run, or every run when several).
        #[arg(long)]
        repo: Option<String>,
        /// Refresh cadence in seconds while looping (ignored under `--json`/`--plain`).
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Print one static frame and exit instead of self-refreshing — for `watch -n`/pipes.
        #[arg(long)]
        plain: bool,
    },
    /// The daemon loop entry — supervise → land → dispatch-next until the queue drains. launchd
    /// invokes this; run it yourself with `--once` for a single pass (cron fallback / manual poke).
    Run {
        /// Run exactly one supervision pass and exit, instead of looping.
        #[arg(long)]
        once: bool,
        /// Drive this repo's run (default: the only live run; errors when several are live).
        #[arg(long)]
        repo: Option<String>,
        /// Drive the #455 cross-repo sweep instead — one loop over every configured repo's session.
        #[arg(long, conflicts_with = "repo")]
        all_repos: bool,
    },
    /// Reconcile durable dispatch claims after a crash: promote claims whose local worktree,
    /// GitHub run/comment, or worker attempt exists; return definitively unlaunched claims to
    /// pending; leave unreachable/ambiguous claims reserved.
    Reconcile {
        /// Reconcile this repo's run (default: the current/only run).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Force a wedged local agent lane through a fresh restart while preserving its worktree and
    /// recoverable work. The lane may be addressed by queue item id or dispatched branch.
    Reset {
        /// Queue item id or dispatched branch identifying the lane.
        lane: String,
        /// Reset a lane in this repo's run (default: the current/only run).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Start a standing, session-independent **supervisor** for the repo (#426): a daemon that, with
    /// nobody at the keyboard, lands every ready PR and recovers dead/stalled standalone `bn dispatch`
    /// lanes — the fix for the drain stalling when unattended. Keeps standing after the queue drains
    /// (only `stop` ends it); survives the shell exiting, a crash, and a reboot (launchd RunAtLoad +
    /// KeepAlive). Idempotent: upgrades a live run in place, else seeds an empty supervisor.
    /// `--all-repos` (#455) supervises EVERY configured repo in one invocation — one loop that
    /// sweeps every repo's tmux session, so agents in any repo (not just the current one) get landed.
    Supervise {
        /// Supervise this repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
        /// Supervise every configured repo in one cross-repo sweep loop (#455).
        #[arg(long, conflicts_with = "repo")]
        all_repos: bool,
        /// Max agents dispatched at once when refilling from the backlog (default 2).
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
        /// Keep owned repos supplied from eligible open issues when backlog + in-flight is low.
        #[arg(long)]
        keep_warm: bool,
        /// Minimum backlog + in-flight depth maintained per owned repo.
        #[arg(long, requires = "keep_warm")]
        low_water: Option<usize>,
        /// Repos this box may feed; comma-delimited or repeatable. Defaults to `[orchestrate].own`.
        #[arg(long, value_delimiter = ',', requires = "keep_warm")]
        own: Vec<String>,
        /// Case-insensitive title substring to exclude; repeatable.
        #[arg(long, requires = "keep_warm")]
        exclude_title: Vec<String>,
        /// Case-insensitive label to exclude; repeatable.
        #[arg(long, requires = "keep_warm")]
        exclude_label: Vec<String>,
        /// Ideation scouts launched when no eligible issues remain (0 disables brainstorming).
        #[arg(long, requires = "keep_warm")]
        brainstorm: Option<usize>,
        /// Run the loop in the foreground (blocking) instead of daemonizing via launchd.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop a run: mark it stopped (the daemon's next tick exits) and unload its launchd daemon.
    /// Bare stop targets the only live run (or the current repo's); `--repo` picks one explicitly;
    /// `--all-repos` stops the #455 cross-repo supervisor (unloads its daemon + ends every run).
    Stop {
        /// Stop this repo's run.
        #[arg(long)]
        repo: Option<String>,
        /// Stop the cross-repo supervisor: unload its daemon and end every supervised run (#455).
        #[arg(long, conflicts_with = "repo")]
        all_repos: bool,
    },
    /// Live-adjust the typed concurrency lanes on a **running** run without stop/re-declare (#385):
    /// set the coding and/or research lane caps in place. The daemon picks the new caps up on its
    /// next tick — no restart. E.g. `bn orchestrate scale --coding 5 --research 2`.
    Scale {
        /// Scale this repo's run (default: the only live run, or the current repo's).
        #[arg(long)]
        repo: Option<String>,
        /// New coding-lane (ship) cap.
        #[arg(long)]
        coding: Option<usize>,
        /// New research-lane (scout) cap — opens a typed research lane if the run had none.
        #[arg(long)]
        research: Option<usize>,
    },
    /// Append items to the durable prioritized backlog — any repo, any time, run active or not.
    /// The daemon pulls the highest-priority pending entries into the run as concurrency slots
    /// free (within one poll, no restart). Deduped by `(repo, id)`; survives restarts and reboots.
    Add {
        /// The work items: issue numbers (`338`), issue/PR URLs, or free text. Deduped by
        /// `(repo, id)` via the same id-derivation the run queue uses.
        #[arg(required = true, value_name = "ITEM")]
        items: Vec<String>,
        /// Target repo (default: the current repo). Half of the `(repo, id)` dedup key.
        #[arg(long)]
        repo: Option<String>,
        /// Priority — higher is pulled first (default 0). Ties break FIFO by add time.
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i64,
        /// Work kind: `ship` (open a PR, default) or `scout` (read-only investigation).
        #[arg(long, value_enum, default_value_t = BacklogKindArg::Ship)]
        kind: BacklogKindArg,
        /// A path glob this work is expected to touch (overlap-aware scheduling hint). Repeatable.
        #[arg(long, value_name = "GLOB")]
        touches: Vec<String>,
        /// Where it should run: `auto` (default), `local`, or `remote`.
        #[arg(long, value_enum, default_value_t = SubstrateArg::Auto)]
        substrate: SubstrateArg,
        /// Free-form provenance recorded on each entry (e.g. `triage`, `scout:341`).
        #[arg(long)]
        source: Option<String>,
        /// After appending, ensure a daemon is running for the repo so the backlog starts draining
        /// (starts an empty run if none is live). Without it, `add` only enqueues.
        #[arg(long)]
        start: bool,
        /// Max agents at once when `--start` seeds a fresh run (default 2).
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
        /// Solo mode (one agent at a time) when `--start` seeds a fresh run.
        #[arg(long)]
        solo: bool,
    },
    /// Ask the orchestrator a question — the ad-hoc priority lane (#378). Appends a read-only
    /// `scout` backlog entry at the top priority band, so the daemon dispatches it on the next
    /// active poll (≤15s) without preempting a running ship: the scout runs the `investigate` skill,
    /// writes findings to its branch note, and opens **no PR**. The answer routes back — read it
    /// with `bn orchestrate answers`, `--wait` here, or the notification that links the findings.
    Ask {
        /// The question to investigate — free text (joined). E.g. `where is retry backoff handled?`.
        #[arg(required = true, value_name = "QUESTION")]
        question: Vec<String>,
        /// Target repo (default: the current repo). Where the scout investigates.
        #[arg(long)]
        repo: Option<String>,
        /// Block until the answer is harvested, then print it inline (polls the state file the
        /// daemon writes — no new IPC). Without it, `ask` returns immediately after enqueuing.
        #[arg(long)]
        wait: bool,
    },
    /// Show the answers routed back from scout/ask items (#378) — the harvested findings for this
    /// repo's run, newest last. `--id` shows just one item's answer. Reads the state file the daemon
    /// writes (no new IPC), so it works whether or not the daemon is mid-poll.
    Answers {
        /// Show answers for this repo's run (default: the only live run, or the current repo's).
        #[arg(long)]
        repo: Option<String>,
        /// Show only the answer for this item id (the scout/ask id, e.g. a question slug).
        #[arg(long)]
        id: Option<String>,
    },
    /// Inspect the durable prioritized backlog (#373/#375) — the read view that complements `add`.
    /// Lists the pending work the daemon pulls from, in pull order (priority desc, then FIFO), so an
    /// operator who loaded hundreds of items can see queue depth, priorities, kinds, and provenance.
    /// `--status` filters (default `pending`; `all` includes leased/terminal); `--all-repos` lists
    /// every repo's backlog; `--limit` caps rows (0 = unlimited); `--json` emits the entries.
    Backlog {
        /// List this repo's backlog (default: the current repo). Ignored with `--all-repos`.
        #[arg(long)]
        repo: Option<String>,
        /// List every repo present in the backlog, grouped by repo (#343).
        #[arg(long, conflicts_with = "repo")]
        all_repos: bool,
        /// Which lifecycle states to show (default: only pending — the pullable work).
        #[arg(long, value_enum, default_value_t = BacklogStatusArg::Pending)]
        status: BacklogStatusArg,
        /// Max rows per repo (0 = unlimited). Guards against dumping a 100s-deep backlog.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Build the backlog from a failing check instead of hand-listing issues (#609). Runs `check`
    /// (a shell command — e.g. `cargo clippy --workspace --all-targets`), parses its failures,
    /// groups them by file (or `--group-by dir`) into ship work items, and appends each as a
    /// backlog entry carrying a `touches` glob so the conflict-aware scheduler never co-schedules
    /// two agents on the same file. `--dry-run` prints the shards without enqueuing; `--start`
    /// begins draining. Grouping-by-crate/module ≈ `--group-by dir`.
    QueueFrom {
        /// The check to run — a shell command whose failures seed the queue. Joined if multi-word.
        #[arg(required = true, value_name = "CHECK")]
        check: Vec<String>,
        /// Target repo (default: the current repo). Half of the `(repo, id)` dedup key.
        #[arg(long)]
        repo: Option<String>,
        /// Shard axis: `file` (default, one lane per file) or `dir` (one per directory ≈ module).
        #[arg(long, default_value = "file")]
        group_by: String,
        /// Priority stamped on every generated item — higher is pulled first (default 0).
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i64,
        /// Parse and print the shards that would be enqueued, but don't touch the backlog.
        #[arg(long)]
        dry_run: bool,
        /// After appending, ensure a daemon is draining the repo (seeds an empty run if none live).
        #[arg(long)]
        start: bool,
        /// Max agents at once when `--start` seeds a fresh run (default 2).
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
    },
    /// Split one oversized GitHub issue into independently shippable, conflict-scoped child
    /// issues, then enqueue only those children. An agent first triages the bundle into repeated
    /// `--part "TITLE::GLOB[,GLOB]"` specifications; bn rejects overlapping touch scopes, links
    /// every child to the parent, and carries each scope into the backlog scheduler.
    Decompose {
        /// Parent issue number or URL to split.
        issue: String,
        /// One independently shippable child as `TITLE::GLOB[,GLOB]`. Repeat at least twice.
        #[arg(long, required = true, value_name = "TITLE::GLOB[,GLOB]")]
        part: Vec<String>,
        /// Target repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
        /// Priority stamped on every generated child backlog entry.
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i64,
        /// Validate and print the decomposition without creating or enqueueing issues.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `--status` for `bn orchestrate backlog`, mapped to an optional [`bn_core::backlog::BacklogStatus`]
/// filter (`All` → no filter). bn-core is clap-free, so the value-enum lives here.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum BacklogStatusArg {
    /// Pending — pullable work waiting for a slot (the default).
    Pending,
    /// Leased — already handed to a run.
    Leased,
    /// Done — a leased item that merged (present until compaction).
    Done,
    /// Failed — a leased item that failed (present until compaction).
    Failed,
    /// Every lifecycle state.
    All,
}

/// Severity filter for `bn orchestrate logs`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum OrchestrateLogLevelArg {
    Info,
    Warn,
    Error,
}

impl OrchestrateLogLevelArg {
    pub(crate) fn to_core(self) -> bn_core::events::EventLevel {
        match self {
            Self::Info => bn_core::events::EventLevel::Info,
            Self::Warn => bn_core::events::EventLevel::Warn,
            Self::Error => bn_core::events::EventLevel::Error,
        }
    }
}

impl BacklogStatusArg {
    /// The [`bn_core::backlog::BacklogStatus`] filter this selects — `None` means "every status".
    pub(crate) fn to_filter(self) -> Option<bn_core::backlog::BacklogStatus> {
        use bn_core::backlog::BacklogStatus;
        match self {
            BacklogStatusArg::Pending => Some(BacklogStatus::Pending),
            BacklogStatusArg::Leased => Some(BacklogStatus::Leased),
            BacklogStatusArg::Done => Some(BacklogStatus::Done),
            BacklogStatusArg::Failed => Some(BacklogStatus::Failed),
            BacklogStatusArg::All => None,
        }
    }
}

/// `--kind` for `bn orchestrate add`, mapped to [`bn_core::backlog::BacklogKind`] (bn-core is
/// clap-free, so the value-enum lives here).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum BacklogKindArg {
    /// Open and land a PR.
    Ship,
    /// A read-only investigation that produces findings, not a merge.
    Scout,
}

impl BacklogKindArg {
    pub(crate) fn to_core(self) -> bn_core::backlog::BacklogKind {
        match self {
            BacklogKindArg::Ship => bn_core::backlog::BacklogKind::Ship,
            BacklogKindArg::Scout => bn_core::backlog::BacklogKind::Scout,
        }
    }
}

/// `--substrate` for `bn orchestrate add`, mapped to [`bn_core::backlog::Substrate`].
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum SubstrateArg {
    /// Let the scheduler decide.
    Auto,
    /// Prefer a local worktree + agent.
    Local,
    /// Prefer a remote/cloud runner.
    Remote,
}

impl SubstrateArg {
    pub(crate) fn to_core(self) -> bn_core::backlog::Substrate {
        match self {
            SubstrateArg::Auto => bn_core::backlog::Substrate::Auto,
            SubstrateArg::Local => bn_core::backlog::Substrate::Local,
            SubstrateArg::Remote => bn_core::backlog::Substrate::Remote,
        }
    }
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

#[derive(Subcommand, Debug)]
pub(crate) enum CronCmd {
    /// Per-job: next scheduled run + last-run status. Same as bare `bn cron`.
    Status,
    /// Show a job's last run — exit, when, duration, and its captured output (including the
    /// error tail of a failed run): bn cron show <job>.
    Show { job: String },
    /// Recent failed runs with their error output: bn cron failures [--limit N].
    Failures {
        /// Max failures to show (newest first).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Stop and unregister the systemd timer driving a job: bn cron unregister <job>.
    Unregister { job: String },
    /// Generate + install platform timers from the cron manifests (crons.toml), idempotently:
    /// adds new timers, updates changed schedules, and removes bn timers no longer present in any
    /// manifest. The data-driven replacement for hand-editing jobs/systemd/install.sh.
    Register {
        /// Print the add/update/remove plan without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Compact badge of jobs running right now, for the tmux bar: bn cron running.
    /// Prints nothing when idle (keeps the bar clean).
    Running,
    /// Internal: refresh the Windows-task cache (spawned detached by the dashboard).
    #[command(hide = true)]
    RefreshWindows,
}

#[derive(Subcommand, Debug)]
pub(crate) enum StaffCmd {
    /// Run one agent's full pipeline — gather → dispatch → validate → route. This is exactly what
    /// the `staff-<name>` cron calls: bn staff run <agent>.
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

#[derive(Subcommand, Debug)]
pub(crate) enum RefCmd {
    /// Open a reference in $EDITOR, creating it from a template if it doesn't exist yet:
    /// `bn ref edit javascript`.
    Edit {
        /// Reference name to open/create (exact name; the file is `references/<name>.md`).
        name: String,
    },
    /// Scaffold a new reference file from a small template: `bn ref add react`. Reports if one
    /// already exists (no overwrite).
    Add {
        /// Reference name to create (the file is `references/<name>.md`).
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AgentsCmd {
    /// List review specialists visible for the current repo (repo-gated) plus global ones.
    List,
    /// Preview the compiled native agent for `name` (Copilot `.agent.md` by default).
    Show {
        name: String,
        /// Render the Claude Code variant instead of the Copilot one.
        #[arg(long)]
        claude: bool,
    },
    /// Scaffold a new specialist source file: bn agents new <name> --repo <r> [--source <ado-repo>].
    New {
        name: String,
        /// Repository this specialist is gated to (its description/instruction gate).
        #[arg(long, value_name = "REPO")]
        repo: String,
        /// One-line description shown to the harness (synthesized from --repo when omitted).
        #[arg(long = "desc", value_name = "TEXT")]
        description: Option<String>,
        /// ADO repo whose merged-PR review comments `bn agents bootstrap` mines.
        #[arg(long, value_name = "ADO_REPO")]
        source: Option<String>,
        /// Tier to scaffold into: global-personal | global-shared (default global-personal).
        #[arg(long, default_value = "global-personal")]
        scope: String,
        /// Replace an existing specialist of the same name in that tier.
        #[arg(long)]
        overwrite: bool,
    },
    /// (Re)compile the discovered specialists into native Copilot/Claude agent files. Default
    /// target is a global install (`~/.copilot/agents` + `~/.claude/agents`); idempotent, guarded,
    /// and prunes stale bn-managed files. Run by `install.sh`.
    Install {
        /// Install into the current repo's `.github/agents` + `.claude/agents` instead of globally
        /// (only specialists gated to this repo).
        #[arg(long)]
        repo: bool,
        /// Restrict to one world: copilot | claude (default both).
        #[arg(long, value_name = "WORLD")]
        harness: Option<String>,
    },
    /// Remove only bn-managed native agent files (user-authored agents are preserved).
    Uninstall {
        /// Uninstall from the current repo's dirs instead of the global ones.
        #[arg(long)]
        repo: bool,
        /// Restrict to one world: copilot | claude (default both).
        #[arg(long, value_name = "WORLD")]
        harness: Option<String>,
    },
    /// Mine `source_repo`'s 50 most-recent merged PRs for real reviewer comments and (re)write the
    /// specialist's `references:` review-standards doc, then reinstall. Prints the dispatch plan.
    Bootstrap {
        /// Specialist whose review standards to mine.
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum SkillsCmd {
    /// List skills across all tiers (default).
    List,
    /// Rank skills by relevance to a task: bn skills find wrap up my work.
    Find {
        /// Free-text task/intent to match against skill names, keywords, when, and descriptions.
        #[arg(value_name = "QUERY", required = true)]
        query: Vec<String>,
    },
    /// Create a new skill: bn skills new <name> --desc <text> [--scope <tier>].
    New {
        name: String,
        /// One-line description shown in the prompt list.
        #[arg(long = "desc", value_name = "TEXT")]
        description: String,
        /// Tier: global-shared | global-personal | repo-shared | repo-personal-notes |
        /// repo-personal-local (default global-personal).
        #[arg(long, default_value = "global-personal")]
        scope: String,
        /// Replace an existing skill of the same name in that tier.
        #[arg(long)]
        overwrite: bool,
    },
    /// Render a skill's prompt body to stdout (headless — no MCP server needed), with
    /// `{{name}}` placeholders filled from repeatable `--arg key=value`. For cron/scripts:
    /// `prompt=$(bn skills render notes-commit)`.
    Render {
        name: String,
        /// Fill a `{{key}}` placeholder: repeatable, e.g. `--arg branch=main`.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
    },
    /// Open a skill in $EDITOR, or the skills folder itself when no name is given:
    /// `bn skills edit` opens the global-shared skills dir; `bn skills edit debug` opens that
    /// skill's file. `--scope <tier>` picks a different tier's folder.
    Edit {
        /// Skill to open (its effective definition). Omit to open the skills folder.
        name: Option<String>,
        /// Which tier's folder to open when no name is given: global-shared | global-personal |
        /// repo-shared | repo-personal-notes | repo-personal-local (default global-shared).
        #[arg(long, default_value = "global-shared")]
        scope: String,
    },
    /// Internal (run by install.sh): (re)generate the native Agent Skills under `~/.copilot/skills`
    /// and `~/.claude/skills` from the global bn skills, so the shipped prompts + personal skills
    /// appear as `/name` in Copilot CLI and Claude Code — matching what the dashboard's skill CRUD
    /// writes on every edit. Idempotent; only ever writes or removes bn-managed files, and a
    /// harness whose root holds a same-named skill bn doesn't own is reported, not fatal.
    #[command(hide = true)]
    Install,
}
