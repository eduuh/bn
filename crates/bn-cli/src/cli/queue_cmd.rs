//! `bn queue` — the machine-wide work queue: what is waiting, what it needs, and whether the
//! dispatcher is allowed to start it.
//!
//! Distinct from `bn orchestrate`, which drives **one repo's run**. This is the machine's whole
//! picture across every repo and every kind of agent.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum QueueCmd {
    /// The machine-wide picture: pending work by kind and lane, and every node
    /// with the skills and harnesses it advertises. The answer to "why is nothing running".
    Status {
        /// Restrict the pending breakdown to one repo.
        #[arg(long)]
        repo: Option<String>,
    },
    /// List queue entries with their kind, priority, executor and requirements.
    List {
        /// Only this kind (`ship`, `scout`, `investigate`, `research`, `enhance`, `review-pr`,
        /// `cron`).
        #[arg(long)]
        kind: Option<String>,
        /// Only this repo.
        #[arg(long)]
        repo: Option<String>,
        /// Only this class (`foreground`, `autonomous`, `opportunistic`, `cron`, `warming`,
        /// `recovery`, `maintenance`).
        #[arg(long = "class")]
        class_filter: Option<String>,
        /// Only this lifecycle state (default: pending).
        #[arg(long, default_value = "pending")]
        status: String,
        /// Every state, overriding `--status`.
        #[arg(long)]
        all: bool,
    },
    /// Enqueue work without dispatching it. The tick starts it when there is capacity and a
    /// capable node.
    ///
    /// Boxed: it carries far more options than any other subcommand, and an enum is as large as its
    /// largest variant — so inline it would pad every other `QueueCmd` to its size.
    Add(Box<AddCmd>),
    /// Take pending work back out of the queue.
    ///
    /// Pending only: a leased entry has an executor running, and marking its row cancelled would
    /// stop the queue describing it without stopping the work. Use `bn agent kill` for that.
    Cancel {
        /// The entry id (as `bn queue list` shows it).
        id: String,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Move queued work up or down the order.
    ///
    /// Sets the priority exactly, in either direction — unlike a producer re-declaring work, which
    /// can only ever raise it.
    Priority {
        /// The entry id.
        id: String,
        /// The new priority. Higher runs first; signed, so work can be pushed below the default 0.
        #[arg(allow_hyphen_values = true)]
        priority: i64,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Snooze one pending task until later, without pausing the queue.
    Snooze {
        /// The entry id.
        id: String,
        /// Duration from now (`30m`, `2h`, `1d`).
        #[arg(long = "for")]
        for_duration: String,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Override one pending task's derived queue class.
    Class {
        /// The entry id.
        id: String,
        /// `foreground`/`watched`, `autonomous`, `opportunistic`/`away`, or `clear`.
        class: String,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Run one dispatcher pass: start what there is capacity and a capable node for, and record
    /// why everything else stayed put.
    Tick {
        /// Decide and print, but start nothing and take no lease. The whole policy, observable
        /// without consequences.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the event-driven dispatcher, with the `queue-tick` cron as the once-a-minute fallback.
    Run {
        /// Maximum seconds to wait while idle; filesystem events wake the dispatcher sooner.
        #[arg(long, default_value_t = 15)]
        interval: u64,
    },
    /// Why the last tick did what it did — the persisted decision, verbatim.
    /// Hold new work without stopping what is running — the drain.
    ///
    /// A pause gates **admission, not execution**: leases already taken finish normally, so
    /// `--scope repo:<name>` empties a repo out rather than killing it. Recovery work
    /// (`agent-supervise`, `fleet-reap`, `agent-observe`, `wt-cleanup`) is never held — pausing
    /// because the machine is unhappy must not switch off the passes that fix it.
    ///
    /// Scopes: `all`, `kind:<k>`, `lane:coding|research`, `node:<id>`, `repo:<name>`. Several
    /// coexist, so one noisy repo can be drained without touching anything else.
    /// bn queue pause --scope repo:bn [--reason "..."] [--for 2h].
    Pause {
        /// What to hold: `all`, `kind:<k>`, `lane:<l>`, `node:<id>`, or `repo:<name>`.
        #[arg(long, default_value = "all")]
        scope: String,
        /// Why — shown wherever the pause is reported, so a stall is never anonymous.
        #[arg(long, default_value = "")]
        reason: String,
        /// Auto-resume after this long (e.g. `90m`, `2h`). Omit to hold until resumed.
        #[arg(long = "for", value_name = "DURATION")]
        for_: Option<String>,
    },
    /// Lift the pause on a scope and start admitting its work again.
    /// bn queue resume --scope repo:bn.
    Resume {
        /// The scope to lift, in the same form `pause` took.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    Why,
    /// Compact tmux/status-bar signal: quiet when only healthy autonomous work is happening.
    Badge,
    /// What the dispatcher has been doing: every start, stall, reclaim and completion, newest last.
    ///
    /// `why` answers "why is nothing running right now". This answers "what happened while I was
    /// away" — the question you have when work you expected never ran and the last pass looks fine.
    Log {
        /// How far back to read (`30m`, `4h`, `2d`).
        #[arg(long, default_value = "4h")]
        since: String,
        /// Most recent N entries, after filtering.
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// Only this event kind (`dispatch`, `tick`, `finish`, `expired`, `reclaimed`,
        /// `skipped`).
        #[arg(long)]
        kind: Option<String>,
        /// Only this repo.
        #[arg(long)]
        repo: Option<String>,
    },
    /// The day as an axis: what ran, what is running, what is scheduled, and — by replaying the
    /// dispatcher forward — roughly when the queue's pending work would start.
    Timeline {
        /// The day to render (default: today).
        #[arg(long)]
        date: Option<String>,
        /// Only this class (`foreground`, `autonomous`, `opportunistic`, `cron`, `warming`,
        /// `recovery`, `maintenance`).
        #[arg(long = "class")]
        class_filter: Option<String>,
    },
    /// Resolve a queue entry — the callback heavy work makes when it finishes.
    ///
    /// Not usually typed by hand: the dispatcher wraps heavy work so it reports its own completion,
    /// because the tick cannot wait for a build that takes minutes.
    Finish {
        /// The entry's repo.
        repo: String,
        /// The entry id.
        id: String,
        /// The command's exit status. Non-zero resolves the entry as failed.
        #[arg(long, default_value_t = 0)]
        exit_code: i32,
    },
    /// Every node and what it offers: status, free slots, heartbeat age, and the skills, harnesses
    /// and MCP servers it advertises. Turns "no capable node" into an answerable question.
    Nodes {
        /// Show only nodes that could take work requiring this skill.
        #[arg(long)]
        skill: Option<String>,
    },
}

/// The options `bn queue add` takes.
#[derive(clap::Args, Debug)]
pub(crate) struct AddCmd {
    /// The dispatch subject: an issue number, a URL, or free text.
    pub(crate) item: String,
    /// Target repo (default: the current repo).
    #[arg(long)]
    pub(crate) repo: Option<String>,
    /// What kind of work this is. Decides the lane, the default executor, and the timeline
    /// colour.
    #[arg(long, default_value = "ship")]
    pub(crate) kind: String,
    /// Higher runs first; ties break FIFO. Signed, so work can be pushed below the default 0.
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    pub(crate) priority: i64,
    /// Run a named bn skill rather than a bare agent. Implies requiring that skill on the
    /// executing node.
    #[arg(long)]
    pub(crate) skill: Option<String>,
    /// Run a native bn job headlessly instead of launching an agent.
    #[arg(long, conflicts_with_all = ["skill", "shell"])]
    pub(crate) job: Option<String>,
    /// Repo context for `--job` when the job needs a target repo.
    #[arg(long, requires = "job")]
    pub(crate) job_repo: Option<String>,
    /// Branch context for `--job` when the job needs a target branch.
    #[arg(long, requires = "job")]
    pub(crate) job_branch: Option<String>,
    /// Run a shell command headlessly instead of launching an agent.
    #[arg(long, conflicts_with_all = ["skill", "job"])]
    pub(crate) shell: Option<String>,
    /// Acceptable harnesses, in preference order. Repeatable; any one will do.
    #[arg(long = "harness", value_name = "NAME")]
    pub(crate) harness: Vec<String>,
    /// A capability label the executing node must advertise. Repeatable.
    #[arg(long = "requires", value_name = "LABEL")]
    pub(crate) requires: Vec<String>,
    /// A capability label that disqualifies a node. Repeatable.
    #[arg(long = "excludes", value_name = "LABEL")]
    pub(crate) excludes: Vec<String>,
    /// Path globs this work is expected to touch — the overlap hint that holds conflicting
    /// work apart.
    #[arg(long = "touches", value_name = "GLOB")]
    pub(crate) touches: Vec<String>,
    /// Estimated wall-clock seconds, for the timeline projection.
    #[arg(long)]
    pub(crate) est_secs: Option<i64>,
    /// What happens if this fires again while this instance is still waiting: `coalesce`
    /// (fold in and carry the newest deadline — the default) or `skip` (leave the waiting
    /// instance alone, so a stale one lapses on its own deadline).
    #[arg(long, default_value = "coalesce")]
    pub(crate) recurrence: String,
}
