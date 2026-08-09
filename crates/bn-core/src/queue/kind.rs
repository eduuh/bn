//! The queue's vocabulary: **what** a piece of work is, **how** it is realized, and **what it
//! needs** from the node that runs it.
//!
//! Three orthogonal axes that were previously one two-valued enum plus a pile of implicit
//! knowledge spread across seven launch sites:
//!
//!   * [`BacklogKind`] — the taxonomy. What shape of work is this?
//!   * [`ExecSpec`] — the executor. What do we actually run?
//!   * [`Requirements`] — the demand. What must the node offer?
//!
//! and [`WorkClass`], the concurrency **lane**, which is a projection of the first. Kind and lane
//! are deliberately separate: there are many kinds of work but only two pools to run them in, and
//! collapsing the two would mean either a lane per kind (fragmenting capacity) or a kind per lane
//! (losing the distinction the timeline and the per-kind caps are built on).
use serde::{Deserialize, Serialize};

/// What kind of work a queue entry represents — the **taxonomy** axis, orthogonal to the
/// concurrency lane ([`WorkClass`]) and to the executor ([`ExecSpec`]).
///
/// Widened from the original `{ship, scout}` pair (#375/#348) so that *every* agent the machine
/// dispatches is expressible as a queue entry rather than a bespoke launch site. Serialized
/// kebab-case.
///
/// `#[serde(other)]` is deliberately **not** used: an unrecognized kind must fail the line parse
/// loudly rather than silently degrade to `ship` and point a coding agent at a research job.
///
/// Backward compatibility: existing `backlog.jsonl` lines carry only `ship`/`scout`, or no `kind`
/// at all (which `#[serde(default)]` on the field reads as `Ship`), so the widening is a pure
/// superset. The reverse — a new line read by an *older* binary — fails to parse and is skipped by
/// `Backlog::from_lines`. That is a downgrade hazard, not an upgrade one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum BacklogKind {
    /// A change that opens and lands a PR — the default and the pre-existing behavior.
    #[default]
    Ship,
    /// A read-only investigation that produces findings, not a merge (#348 scout).
    Scout,
    /// A scoped read-only investigation that writes evidence into a branch note and hands off to
    /// plan (the `investigate` skill). Distinct from [`Scout`](BacklogKind::Scout): it is
    /// task-bound, not exploratory.
    Investigate,
    /// A deep research / answer agent — the ask lane.
    Research,
    /// A quality pass over existing code that opens a PR.
    Enhance,
    /// An automated review of a pull request (the `auto-review-pr` skill).
    ReviewPr,
    /// A scheduled headless agent or job — the `config/agents/*.md` roster, the `staff-*` jobs, and
    /// any native job that opts into the queue. Which one it is stays in `BacklogEntry::source`
    /// (`cron:<job>` / `staff:<name>`), so the kind does not have to enumerate the roster.
    Cron,
    /// Housekeeping that reclaims a resource rather than producing one: handing a pool slot's lease
    /// back, removing a merged worktree, resetting a checkout.
    ///
    /// It is a kind of its own because it is the work you defer. `prefix x` closes the window on the
    /// keystroke and files the reclaim here, so the expensive half — a warm reset, a `gh pr view`, a
    /// `git worktree remove` — waits for a moment the machine has to spare instead of freezing tmux
    /// while you watch. Distinct from [`Cron`](BacklogKind::Cron): nothing schedules it, a human
    /// action does.
    Cleanup,
}

impl BacklogKind {
    /// The kebab-case label — the wire form, the CLI `--kind` value, and the UI colour key.
    pub fn label(self) -> &'static str {
        match self {
            BacklogKind::Ship => "ship",
            BacklogKind::Scout => "scout",
            BacklogKind::Investigate => "investigate",
            BacklogKind::Research => "research",
            BacklogKind::Enhance => "enhance",
            BacklogKind::ReviewPr => "review-pr",
            BacklogKind::Cron => "cron",
            BacklogKind::Cleanup => "cleanup",
        }
    }

    /// Parse a kebab-case label back to a kind (`None` for anything unrecognized).
    pub fn parse(s: &str) -> Option<BacklogKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ship" => Some(BacklogKind::Ship),
            "scout" => Some(BacklogKind::Scout),
            "investigate" => Some(BacklogKind::Investigate),
            "research" => Some(BacklogKind::Research),
            "enhance" => Some(BacklogKind::Enhance),
            "review-pr" => Some(BacklogKind::ReviewPr),
            "cron" => Some(BacklogKind::Cron),
            "cleanup" => Some(BacklogKind::Cleanup),
            _ => None,
        }
    }

    /// Every kind, in display / timeline order.
    ///
    /// The single list every surface iterates, so adding a kind cannot be silently forgotten by one
    /// of them. Ordered coding-lane-first because that is how the day reads.
    pub const ALL: [BacklogKind; 8] = [
        BacklogKind::Ship,
        BacklogKind::Enhance,
        BacklogKind::ReviewPr,
        BacklogKind::Investigate,
        BacklogKind::Research,
        BacklogKind::Scout,
        BacklogKind::Cron,
        BacklogKind::Cleanup,
    ];

    /// The executor to use when an entry declares none — so `bn queue add --kind investigate 412`
    /// needs no `--exec` flag to do the obvious thing.
    ///
    /// Only the two kinds that *are* a named skill get one; the rest stay a plain agent, because
    /// guessing a skill for them would silently constrain placement to nodes that serve it.
    pub fn default_exec(self) -> ExecSpec {
        match self {
            BacklogKind::Investigate => ExecSpec::Skill {
                skill: "investigate".to_string(),
                arg: None,
            },
            BacklogKind::ReviewPr => ExecSpec::Skill {
                skill: "auto-review-pr".to_string(),
                arg: None,
            },
            _ => ExecSpec::Agent,
        }
    }
}

/// **How** a queue entry is realized — the executor, separate from the taxonomy
/// ([`BacklogKind`]) and the lane ([`WorkClass`]).
///
/// This is what makes "the system knows how to dispatch the particular stuff" a data question
/// instead of a chain of `if job == "…"` spread across seven launch sites.
///
/// Internally tagged on `run`, so a legacy entry — which has no `exec` key at all — reads as
/// [`ExecSpec::Agent`] through `#[serde(default)]` on the field: exactly the pre-queue behavior of
/// `bn dispatch <item>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "run", rename_all = "kebab-case")]
pub enum ExecSpec {
    /// Dispatch a worktree + harness agent on the entry's `item` — the existing local `sh_dispatch`
    /// / ssh `dispatch_worker_item` path. The default.
    #[default]
    Agent,
    /// Dispatch an agent that opens on a named bn skill (`investigate`, `auto-review-pr`, `plan`).
    /// `arg` carries the skill's subject when it differs from the entry's `item`.
    Skill {
        skill: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arg: Option<String>,
    },
    /// Run a named bn job headlessly through the native-job registry — how a `staff-*`, native,
    /// script, or Markdown agent enters the queue without its implementation moving.
    Job {
        job: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// Run an explicitly queued headless shell command. Markdown agents use `Job` so dequeueing
    /// re-enters `bn run <name>` and preserves catalog validation, gates, and the run ledger.
    Shell { cmd: String },
}

impl ExecSpec {
    /// Whether realizing this spec needs a worktree + agent lease, and therefore a pool slot and a
    /// RAM-sized concurrency slot.
    ///
    /// The load-bearing distinction for keeping the queue cheap: a headless job or shell command
    /// costs nothing like an agent does, so the tick runs it without charging it against a lane.
    /// Queuing a two-second housekeeping cron behind the research cap would make the machine
    /// slower, which is the opposite of the point.
    pub fn needs_worktree(&self) -> bool {
        matches!(self, ExecSpec::Agent | ExecSpec::Skill { .. })
    }

    /// The skill this spec implies, if any.
    ///
    /// Folded into [`Requirements::skills`] at enqueue time, so an entry that names a skill
    /// automatically requires that skill on whichever node runs it — no separate declaration to
    /// forget.
    pub fn implied_skill(&self) -> Option<&str> {
        match self {
            ExecSpec::Skill { skill, .. } => Some(skill.as_str()),
            _ => None,
        }
    }

    /// The kebab-case executor label for status surfaces (`agent` / `skill` / `job` / `shell`).
    pub fn label(&self) -> &'static str {
        match self {
            ExecSpec::Agent => "agent",
            ExecSpec::Skill { .. } => "skill",
            ExecSpec::Job { .. } => "job",
            ExecSpec::Shell { .. } => "shell",
        }
    }
}

/// What a queue entry needs from the node that runs it.
///
/// One struct for the whole question, so a dispatch site declares its needs once instead of
/// spreading them over a capability list, a harness flag, and an undocumented assumption about
/// which box has which skill installed.
///
/// Every field is an **AND** except [`harness`](Requirements::harness), which is an **OR** set —
/// any acceptable harness will do. Empty everywhere means "runs anywhere", which is what every
/// pre-queue entry deserializes to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    /// bn skill names the node must serve (`investigate`, `auto-review-pr`, `plan`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Acceptable harnesses (`claude`, `copilot`) — an OR set. Empty accepts any node, which then
    /// runs the executor's configured default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harness: Vec<String>,
    /// MCP servers the agent must be able to reach.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcps: Vec<String>,
    /// Free-form node capability labels — the pre-existing #728 axis, carried here so the whole
    /// requirement is one value. Mirrors `QueueItem::required_capabilities`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Capability labels that DISQUALIFY a node. Mirrors `QueueItem::excluded_capabilities`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
}

impl Requirements {
    /// Whether this asks for nothing.
    ///
    /// Also the serde skip predicate, so an unconstrained entry's JSON line stays byte-identical to
    /// a pre-queue one — the property that lets the widening land without rewriting the store.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
            && self.harness.is_empty()
            && self.mcps.is_empty()
            && self.capabilities.is_empty()
            && self.excludes.is_empty()
    }
}

/// The **concurrency lane** an item is admitted against (#385) — a projection of
/// [`BacklogKind`], not a second taxonomy.
///
/// Defaults to [`Coding`](WorkClass::Coding) (the `ship` default), so a pre-#385 run's state — and
/// every `--queue` item, which has no kind — reads as a coding lane. Serialized kebab-case for the
/// status surface.
///
/// Lives here rather than in `orchestrate` because it is defined *by* the kind: keeping the enum
/// next to the mapping is what makes [`from_kind`](WorkClass::from_kind) exhaustive, so a new kind
/// cannot be added without deciding which pool pays for it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum WorkClass {
    /// A heavy coding agent that opens and lands a PR — the default lane.
    #[default]
    Coding,
    /// A light, read-only research/scout agent that produces findings, not a merge.
    Research,
}

impl WorkClass {
    /// The kebab-case label persisted on the run state + shown in status (`coding` / `research`).
    pub fn label(self) -> &'static str {
        match self {
            WorkClass::Coding => "coding",
            WorkClass::Research => "research",
        }
    }

    /// The lane a queue entry belongs to.
    ///
    /// The single mapping that turns the durable [`BacklogKind`] into a schedulable work-class.
    /// Anything that opens a PR is a coding agent and costs a coding slot; everything else is
    /// read-only and costs a research slot. Exhaustive on purpose — see the type's doc comment.
    pub fn from_kind(kind: BacklogKind) -> WorkClass {
        match kind {
            BacklogKind::Ship | BacklogKind::Enhance | BacklogKind::ReviewPr => WorkClass::Coding,
            BacklogKind::Scout
            | BacklogKind::Investigate
            | BacklogKind::Research
            | BacklogKind::Cron
            // Cleanup is realized as a headless shell/job, so `is_express` runs it inline and it
            // never actually charges a lane — but the mapping is exhaustive, and if one ever did
            // need a slot it is the cheap read-only pool that should pay, not the coding one.
            | BacklogKind::Cleanup => WorkClass::Research,
        }
    }

    /// Every lane, in display order — the sibling of [`BacklogKind::ALL`].
    pub const ALL: [WorkClass; 2] = [WorkClass::Coding, WorkClass::Research];
}
