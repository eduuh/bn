//! `bn queue` — durable work rows handed between agents (see [`bn_core::coord`]).
//!
//! Its own flattened group, last in [`super::Command`], so the coordination commands land at the
//! end of `bn --help` without touching the order of anything above them.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdCoord {
    /// Work rows handed between agents. A row (`q<N>`) is addressed to a seat — `repo:branch` for
    /// an agent, `human:<name>` for a person — and moves pending → in-progress → closed, keeping
    /// every transition. Unlike `bn agent msg` (which only informs), a row is owned, has a deadline,
    /// and cannot be closed without saying where the work went. A row for an agent also appears as
    /// a `[q<N>]` todo in that branch's note. Bare `bn queue` lists your open rows.
    Queue {
        #[command(subcommand)]
        action: Option<QueueCmd>,
    },
    /// Durable scheduled checks that wake a seat, because a seat cannot wake itself. Stored in the
    /// coordination DB, so they survive a reboot; the `coord-tick` job evaluates the due ones.
    /// Kinds: reminder (deliver a message), keepalive (nudge only if idle), idle-queue (nudge only
    /// if idle with pending rows), context (warn past a context-window threshold), wake-row (wake a
    /// parked queue row). Bare `bn watchdog` lists the active ones.
    Watchdog {
        #[command(subcommand)]
        action: Option<WatchdogCmd>,
    },
    /// Evidence for this branch's done-when criteria, kept beside its note in `proof/`. A proof
    /// (`p<N>`) says what was checked, at which commit, by whom; a verdict from someone other
    /// than its author accepts or rejects it; readiness per criterion is derived from both, never
    /// written by hand. `bn verify --for w1` records one for you. Bare `bn proof` shows readiness.
    Proof {
        #[command(subcommand)]
        action: Option<ProofCmd>,
    },
    /// Compaction discipline. Compaction is not an unblock: what comes back believes it knows
    /// everything and does not. So plan it (`plan` shows every live agent's context use), prepare
    /// for it (a recap and handoff written first), and audit what comes back. Bare `bn compact`
    /// is `plan`.
    Compact {
        #[command(subcommand)]
        action: Option<CompactCmd>,
    },
    /// Context profiles: the standing context a seat is handed, composed from atoms (directives,
    /// goal, readiness, recap, handoff, reference index or sections, skills, file sections, queue
    /// rows) with a token budget. Declared in `<repo>/.bn/context.toml`; `ship`, `scout`,
    /// `compact-restore` and `orient` are built in. Bare `bn context` lists profiles.
    Context {
        #[command(subcommand)]
        action: Option<ContextCmd>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ContextCmd {
    /// Every profile, with its atoms and budget.
    List,
    /// Compose a profile for this branch: what each atom costs, the total against the budget.
    /// `--render` prints the composed text instead.
    Profile {
        name: String,
        /// Override the profile's token budget.
        #[arg(long)]
        budget: Option<usize>,
        /// Drop atoms from the end until it fits, instead of only warning.
        #[arg(long)]
        fit: bool,
        #[arg(long)]
        render: bool,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// Serve one piece: `profile:<name>`, `atom:<name>`, `ref:<name>[#section]`,
    /// `skill:<name>`, or `file:<path>[#section]`.
    Get {
        what: String,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// Say you read served context (by the digest it was served with). Serving is not reading;
    /// this is the reading half.
    Ack {
        digest: String,
        /// Who read it (default: this worktree's `repo:branch`).
        #[arg(long = "as", value_name = "WHO")]
        as_who: Option<String>,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// What context this branch was served, and which of it was acknowledged. Audit only.
    Served {
        #[command(flatten)]
        target: CompactTarget,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CompactCmd {
    /// Every live agent's context-window use and what to do about it. Read-only.
    Plan {
        /// Only agents at or above this percent.
        #[arg(long)]
        threshold: Option<u8>,
    },
    /// Write this branch's recap (`RECAP.md`): your own words, then a state block bn composes
    /// from the note. Write it at a boundary — before a compaction, a handover, a long break.
    Recap {
        /// What you know that the note doesn't: where you are, what you were about to do, what
        /// you suspect. Several words are joined.
        #[arg(num_args = 0..)]
        text: Vec<String>,
        /// Read the words from stdin instead.
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// Prepare for a compaction: write the recap and a handoff (keeping earlier author notes),
    /// and log it. `--dry-run` shows the recap and writes nothing. Wired as the PreCompact hook.
    Prep {
        /// Your own words for the recap.
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Called by a harness hook: read its JSON from stdin and never fail.
        #[arg(long, hide = true)]
        hook: bool,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// After a compaction: print what the note, the recap and the proof say, so the session checks
    /// its beliefs against them before acting, and log how it came back. Wired as the
    /// SessionStart hook for `compact`.
    Audit {
        #[arg(long, hide = true)]
        hook: bool,
        #[command(flatten)]
        target: CompactTarget,
    },
    /// Compact another live Claude agent deliberately: prep its recap and handoff, type
    /// `/compact` into its pane, and wait for it to come back and audit itself. Reports the
    /// honest outcome, or `waiting` if it hasn't come back in time.
    Run {
        /// The agent: `repo:branch`.
        seat: String,
        /// What the compaction should keep in focus (passed to `/compact`).
        #[arg(long)]
        focus: Option<String>,
        /// Seconds to wait for the restore.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Say what would happen; do nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// This branch's compaction log: each prep and restore, with its outcome.
    Log {
        #[command(flatten)]
        target: CompactTarget,
    },
}

/// Another branch's note, instead of this worktree's.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct CompactTarget {
    #[arg(long)]
    pub(crate) repo: Option<String>,
    #[arg(long)]
    pub(crate) branch: Option<String>,
}

/// Which branch a proof command acts on, and as whom.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct ProofTarget {
    /// Another branch's note (with --branch) instead of this worktree's.
    #[arg(long)]
    pub(crate) repo: Option<String>,
    #[arg(long)]
    pub(crate) branch: Option<String>,
    /// Operate as this identity (`repo:branch` or `human:<name>`). Defaults to `$BN_SEAT`, then — under
    /// an agent harness — this worktree's `repo:branch`, else `human:$USER`.
    #[arg(long = "as", value_name = "WHO")]
    pub(crate) as_who: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ProofCmd {
    /// Register evidence: bn proof add --kind test --for w1 --title "…" [--file log.txt].
    Add {
        /// verify | gate | test | screenshot | log | review | manual.
        #[arg(long)]
        kind: String,
        /// The done-when criteria it speaks to (comma-separated `w<N>`).
        #[arg(long = "for", value_delimiter = ',')]
        for_: Vec<String>,
        #[arg(long)]
        title: String,
        /// What it shows, in words (the proof's body).
        #[arg(long)]
        text: Option<String>,
        /// A file to copy in as the attachment (copied, never moved).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// The command that produced it.
        #[arg(long)]
        cmd: Option<String>,
        /// That command's exit code. Non-zero evidence can never prove a criterion done.
        #[arg(long)]
        exit: Option<i32>,
        /// The commit it was taken at (default: the branch's HEAD).
        #[arg(long)]
        sha: Option<String>,
        #[command(flatten)]
        target: ProofTarget,
    },
    /// Judge a proof: accept or reject it (not your own), or withdraw your own.
    Judge {
        id: String,
        /// accept | reject | withdraw.
        verdict: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        target: ProofTarget,
    },
    /// Point an existing proof at more criteria. Author only.
    Link {
        id: String,
        #[arg(required = true)]
        criteria: Vec<String>,
        #[command(flatten)]
        target: ProofTarget,
    },
    /// Readiness per criterion (read-only), or one proof (`p<N>`) or criterion (`w<N>`) in full.
    Show {
        id: Option<String>,
        /// exact (default: evidence must be at HEAD) | ancestor | any.
        #[arg(long, default_value = "exact")]
        freshness: String,
        #[command(flatten)]
        target: ProofTarget,
    },
    /// Every proof, with its latest verdict.
    List {
        #[command(flatten)]
        target: ProofTarget,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum WatchdogCmd {
    /// Register a watchdog: bn watchdog register --kind reminder --at 30m --text "check CI".
    Register {
        /// reminder | keepalive | idle-queue | context | wake-row.
        #[arg(long)]
        kind: String,
        /// The seat it watches and wakes (default: the seat you act as).
        #[arg(long)]
        target: Option<String>,
        /// First check after this long (`90s`, `30m`, `2h`). Default: at the first interval, or
        /// now for a one-shot.
        #[arg(long)]
        at: Option<String>,
        /// Check again this often (at least 1m). Omit for a one-shot.
        #[arg(long)]
        every: Option<String>,
        /// The message to deliver when it fires.
        #[arg(long)]
        text: Option<String>,
        /// `wake-row`: the parked queue row (`q<N>`).
        #[arg(long)]
        row: Option<String>,
        /// `context`: fire at or above this percent of the context window (default 80).
        #[arg(long)]
        threshold: Option<u8>,
        /// `keepalive` / `idle-queue`: quiet time before the seat counts as idle (default 10m).
        #[arg(long)]
        idle: Option<String>,
        /// Stop after firing this many times.
        #[arg(long)]
        max_fires: Option<i64>,
        /// Retire it after this long, fired or not.
        #[arg(long)]
        expires: Option<String>,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Active watchdogs, newest first.
    List {
        /// Include stopped, fired and expired ones.
        #[arg(long)]
        all: bool,
        /// Only those watching this seat.
        #[arg(long)]
        target: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// One watchdog.
    Show { id: String },
    /// One watchdog plus its recent evaluations.
    Status {
        id: String,
        #[arg(long, default_value_t = 20)]
        evals: usize,
    },
    /// Stop a watchdog; the tick skips it from now on.
    Stop {
        id: String,
        #[arg(long, default_value = "stopped by hand")]
        reason: String,
    },
    /// Evaluate every due watchdog now (what `coord-tick` runs).
    Tick {
        /// Show what is due; evaluate nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Evaluate one watchdog if it is due — the target of an exact-time timer. Idempotent: a slot
    /// the tick already took is not fired twice.
    Fire { id: String },
}

/// The acting seat, shared by every verb that changes a row.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct AsSeat {
    /// Operate as this seat (`repo:branch` or `human:<name>`). Defaults to `$BN_SEAT`, then this
    /// worktree's `repo:branch`, then `human:$USER`.
    #[arg(long = "as", value_name = "SEAT")]
    pub(crate) as_seat: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum QueueCmd {
    /// Open a row for a seat: bn queue create <seat> <body…>.
    Create {
        /// Who it is for: `repo:branch` or `human:<name>`.
        seat: String,
        /// What needs doing. Several words are joined; flags may come before or after.
        #[arg(required = true, num_args = 1..)]
        body: Vec<String>,
        /// fast (30m) | routine (4h, default) | deep (24h) | critical (1h) — how long a claimed row
        /// may stay open before it is overdue.
        #[arg(long, default_value = "routine")]
        tier: String,
        /// A key that makes create idempotent: creating again with it returns the first row.
        #[arg(long)]
        key: Option<String>,
        /// A link that goes with the work (issue, PR, doc).
        #[arg(long = "ref")]
        ref_: Option<String>,
        /// Don't type a heads-up into the seat's live pane (the row and its todo still land).
        #[arg(long)]
        no_nudge: bool,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Take a pending row addressed to you: pending → in-progress, with a deadline from its tier.
    Claim {
        id: String,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Give a claimed row back (in-progress → pending), saying why.
    Unclaim {
        id: String,
        #[arg(long)]
        note: String,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Hand a row to another seat: closes yours and opens theirs in one step.
    Handoff {
        id: String,
        /// The seat that takes it next.
        #[arg(long)]
        to: String,
        /// What they are getting and why — recorded on both rows.
        #[arg(long)]
        note: String,
        /// Close yours as done (your part is finished) rather than handed-off.
        #[arg(long)]
        complete: bool,
        /// Tier for the new row (defaults to this row's).
        #[arg(long)]
        tier: Option<String>,
        #[arg(long = "ref")]
        ref_: Option<String>,
        /// Don't type a heads-up into the next seat's live pane.
        #[arg(long)]
        no_nudge: bool,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Close a row as done, saying where the work went.
    Done(CloseArgs),
    /// Close a row as failed, saying why.
    Fail(CloseArgs),
    /// Refuse a row addressed to you, saying why.
    Deny(CloseArgs),
    /// Withdraw a row, saying why.
    Cancel(CloseArgs),
    /// Park a row on something, with what will wake it. Exactly one of `--wake-after`,
    /// `--watchdog`, or (for `--on human:<name>`) `--summary` + `--evidence`: a park with nothing to
    /// wake it looks like a crash.
    Block {
        id: String,
        /// What it waits on: a seat, a row (`q<N>`), or a few words.
        #[arg(long)]
        on: String,
        /// Wake it after this long (`30m`, `2h`) — creates the row's wake watchdog.
        #[arg(long, conflicts_with_all = ["watchdog", "summary"])]
        wake_after: Option<String>,
        /// Wake it with an existing watchdog (`wd<N>`).
        #[arg(long, conflicts_with = "summary")]
        watchdog: Option<String>,
        /// For a person: what they need to decide, in plain language.
        #[arg(long, requires = "evidence")]
        summary: Option<String>,
        /// For a person: a link or path they can judge by.
        #[arg(long, requires = "summary")]
        evidence: Option<String>,
        /// A note for the row's history.
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Unpark a blocked row (blocked → in-progress), saying what changed.
    Resolve {
        id: String,
        #[arg(long)]
        note: String,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Add a note to a row's history without changing its state.
    Update {
        id: String,
        #[arg(long)]
        note: String,
        #[arg(long = "ref")]
        ref_: Option<String>,
        #[command(flatten)]
        who: AsSeat,
    },
    /// Open rows for you (default), a seat, or everyone.
    List {
        /// Rows for this seat instead of yours.
        #[arg(long, conflicts_with = "all")]
        seat: Option<String>,
        /// Rows for every seat.
        #[arg(long)]
        all: bool,
        /// Only these states (comma-separated, e.g. `pending,blocked`; `any` for every state).
        #[arg(long)]
        state: Option<String>,
        /// Only in-progress rows past their deadline.
        #[arg(long)]
        overdue: bool,
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        who: AsSeat,
    },
    /// One row in full.
    Show { id: String },
    /// A row's append-only history.
    Transitions { id: String },
    /// One pass of what the `coord-tick` job does every five minutes: nudge unclaimed rows at live
    /// agents, raise attention for overdue claims, unclaimed rows, rows waiting on a person and
    /// parks with nothing to wake them, clear alerts whose row has moved on, and repair note
    /// mirrors.
    Sweep {
        /// Print what it would do; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Which seat you act as, and why.
    Whoami {
        #[command(flatten)]
        who: AsSeat,
    },
}

#[derive(clap::Args, Debug)]
pub(crate) struct CloseArgs {
    pub(crate) id: String,
    /// Where the work went, or why it stopped. Required: closing silently is what a crash
    /// looks like.
    #[arg(long)]
    pub(crate) note: String,
    #[arg(long = "ref")]
    pub(crate) ref_: Option<String>,
    #[command(flatten)]
    pub(crate) who: AsSeat,
}
