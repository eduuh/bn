//! `bn watchdog`, `bn proof`, `bn compact`, `bn context` — agent coordination (see [`bn_core::coord`]).
//!
//! Its own flattened group, last in [`super::Command`], so the coordination commands land at the
//! end of `bn --help` without touching the order of anything above them.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdCoord {
    /// Durable scheduled checks that wake a seat, because a seat cannot wake itself. Stored in the
    /// coordination DB, so they survive a reboot; the `coord-tick` job evaluates the due ones.
    /// Kinds: reminder (deliver a message), keepalive (nudge only if idle), context (warn past a
    /// context-window threshold). Bare `bn watchdog` lists the active ones.
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
    /// goal, readiness, recap, handoff, reference index or sections, skills, file sections) with a
    /// token budget. Declared in `<repo>/.bn/context.toml`; `ship`, `scout`,
    /// `compact-restore` and `orient` are built in. Bare `bn context` lists profiles.
    Context {
        #[command(subcommand)]
        action: Option<ContextCmd>,
    },
    /// Compact session bootstrap without branch-note changes: context and up to three task-relevant
    /// workflows with invocation and next commands. Run at session start or when changing tasks.
    /// Without a task, orient without guessing a workflow. Includes bn workflows and installed
    /// native skills, with native /name versus Claude MCP /bn:name invocation guidance.
    /// Works outside a repo with global skills. --json data: task, context (nullable),
    /// workflows (max 3), workflows_truncated (result cap), workflows_filtered (weak matches
    /// omitted), commands. No branch notes are created or updated.
    Discover {
        /// Task to match against effective workflow names, descriptions, triggers and keywords.
        #[arg(value_name = "TASK", num_args = 0..)]
        task: Vec<String>,
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
        /// reminder | keepalive | context.
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
        /// `context`: fire at or above this percent of the context window (default 80).
        #[arg(long)]
        threshold: Option<u8>,
        /// `keepalive`: quiet time before the seat counts as idle (default 10m).
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

/// The acting seat for a command that registers on someone's behalf.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct AsSeat {
    /// Operate as this seat (`repo:branch` or `human:<name>`). Defaults to `$BN_SEAT`, then this
    /// worktree's `repo:branch`, then `human:$USER`.
    #[arg(long = "as", value_name = "SEAT")]
    pub(crate) as_seat: Option<String>,
}
