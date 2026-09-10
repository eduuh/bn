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
    /// Schedule one pending entry for a time: `bn queue at t42 12:30`, `… "in 2h"`, `… lunch`.
    ///
    /// Sets `not_before`, so the dispatcher holds it until then. A clock time always resolves
    /// forward — `09:00` typed in the afternoon means tomorrow morning, never this morning.
    At {
        /// The entry id.
        id: String,
        /// A time (`12:30`, `5pm`), a delay (`in 2h`, `90m`), a day (`tomorrow`, `tonight`), or a
        /// window name (`lunch`).
        when: String,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Start one pending entry as soon as the machine can: `bn queue now t42`.
    ///
    /// Clears any snooze, marks it foreground and raises it above everything else pending — three
    /// things because raising the priority alone does none of the others. A snoozed entry is held
    /// before ranking ever happens, and an opportunistic one waits for an away window whatever its
    /// number.
    Now {
        /// The entry id.
        id: String,
        /// Its repo (default: the current repo).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Declare yourself away, so work that was waiting for you to leave can start: `bn queue away
    /// --for 45m`. `--back` ends it early.
    ///
    /// The queue normally infers your absence from an idle event log, which means the first ten
    /// minutes of a break are spent deciding whether you have really gone. This says so outright.
    Away {
        /// How long you will be gone (`45m`, `2h`, `1d`).
        #[arg(long = "for", default_value = "60m")]
        for_duration: String,
        /// End the declared absence now — you came back.
        #[arg(long, conflicts_with = "for_duration")]
        back: bool,
    },
    /// What runs when: every configured window, its next firings, and what it applies to.
    Schedule {
        #[command(subcommand)]
        action: Option<ScheduleCmd>,
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
    /// Run the event-driven dispatcher in the foreground. bn starts one in the background on its
    /// own; this is for watching it work, or for running it under your own supervisor.
    Run {
        /// Maximum seconds to wait while idle; filesystem events wake the dispatcher sooner.
        #[arg(long, default_value_t = 15)]
        interval: u64,
    },
    /// Stop this machine's dispatcher. It stays stopped until the next `bn` command starts a new
    /// one — to keep it off, set BN_NO_AUTO_TICK=1 or turn the queue off for this machine.
    Stop,
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
    /// Switch the queue off — stop filing work and run every trigger on the spot.
    ///
    /// The opposite of `pause`, not a stronger version of it. A pause *holds* work: it keeps
    /// accumulating and starts when the pause lifts. This stops work being filed at all, so a cron
    /// firing, a review poll or a verb you press runs its work immediately, as it did before the
    /// queue existed — and there is no backlog to catch up on afterwards.
    ///
    /// The dispatcher keeps running, so whatever is already queued still drains and `bn queue on`
    /// takes effect at once.
    /// bn queue off --for 2h --reason "shipping".
    Off {
        /// Switch back on automatically after this long (e.g. `90m`, `2h`). Omit to stay off until
        /// `bn queue on`.
        #[arg(long = "for", value_name = "DURATION")]
        for_: Option<String>,
        /// Why — shown by `bn queue status`, on the picker's settings row and in `bn queue log`.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Switch the queue back on: triggers file work for the dispatcher again.
    On,
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
    /// The subject handed to `--skill` when the queue's own subject line is not what the workflow
    /// should be pointed at — e.g. a bare task id under a human-readable board entry.
    #[arg(long, requires = "skill")]
    pub(crate) skill_arg: Option<String>,
    /// The fleet task name to launch under, so the run is recognisable on the board and the
    /// already-running guard can tell a duplicate from a fresh start.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Launch the agent without the permission gate. Unattended queue work that stops on a prompt
    /// nobody is watching is work that never finishes.
    #[arg(long)]
    pub(crate) yolo: bool,
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

/// Managing the machine's priority windows — the times of day when some work is preferred, or when
/// opportunistic work is allowed to run at all.
///
/// These live in `[[queue.windows]]` in `config.toml`, and until now the only way to add one was to
/// edit that file by hand: there was no CLI at all, and the single write path (the dashboard's pool
/// form) could set exactly the two built-in pool-warm windows and nothing else.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum ScheduleCmd {
    /// Add or replace a window: `bn queue schedule add lunch --at "Mon..Fri *-*-* 12:30" --for 90m
    /// --mode away`.
    Add {
        /// What to call it. Adding a name that exists replaces it.
        name: String,
        /// When it opens, as an `OnCalendar` spec — the same grammar `[[cron]]` uses.
        #[arg(long)]
        at: String,
        /// How long it stays open (`90m`, `2h`).
        #[arg(long = "for")]
        for_duration: String,
        /// `prefer` (rank matching work higher), `away` (let opportunistic work run), `watched`
        /// (boost foreground work), or `deadline` (both, plus a horizon the fit gate respects).
        #[arg(long, default_value = "prefer")]
        mode: String,
        /// What it applies to: `*`, `kind:<k>`, `repo:<r>`, `source:<prefix>`, `class:<mode>`.
        /// Repeatable.
        #[arg(long = "match")]
        matches: Vec<String>,
        /// How much to raise matching work while open.
        #[arg(long, default_value_t = 100, allow_hyphen_values = true)]
        boost: i64,
    },
    /// Remove a window by name.
    Rm {
        /// The window's name.
        name: String,
    },
}
