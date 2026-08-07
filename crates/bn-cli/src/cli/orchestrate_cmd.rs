//! The `bn orchestrate` clap subcommand tree.
//!
//! Split out of [`crate::cli`] so the orchestrator's flags can keep growing without pushing
//! `cli.rs` further past the 1000-line ratchet (see `.file-size-baseline`). Nothing else moved:
//! the value-enums these variants reference still live in `cli.rs` and resolve through the
//! `use crate::*` glob below.
use crate::*;

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
