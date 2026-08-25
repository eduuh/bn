//! What a queue entry *is* — the record, its axes, and the spec that creates one.
//!
//! Split from [`super`], which owns the log: how lines are folded into a view, leased, resolved and
//! compacted. The two change for different reasons — a new axis on an entry is a vocabulary change,
//! while the fold is a storage concern — and keeping them together pushed the file past the
//! 1000-line ratchet.
use crate::orchestrate::derive_id;
use crate::queue::kind::{BacklogKind, ExecSpec, Requirements};
use serde::{Deserialize, Serialize};

/// Where an entry should run — the substrate hint the selection brain (#348) later honors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Substrate {
    /// Let the scheduler decide — the default.
    #[default]
    Auto,
    /// Prefer a local worktree + agent.
    Local,
    /// Prefer a remote/cloud runner.
    Remote,
}

impl Substrate {
    pub fn label(self) -> &'static str {
        match self {
            Substrate::Auto => "auto",
            Substrate::Local => "local",
            Substrate::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Substrate> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Substrate::Auto),
            "local" => Some(Substrate::Local),
            "remote" => Some(Substrate::Remote),
            _ => None,
        }
    }
}

/// **Where — on which physical node — an item executes** (#717, M1 foundation). This is a
/// *separate axis* from [`Substrate`] and must not be conflated with it: `Substrate` chooses
/// *how* an item runs (a local worktree+agent vs. a GitHub-hosted fan-out that occupies no local
/// slot), whereas `ExecutionTarget` chooses *the node* a locally-substrated item lands on — the
/// implicit local box, or a chosen SSH-reachable worker. Making execution location first-class
/// lets dispatch target a picked node instead of always the box the daemon runs on.
///
/// Orthogonality: a `Substrate::Remote` item is GitHub fan-out and ignores this axis; a
/// `Substrate::Local`/`Auto` item is placed by the scheduler among `{Local, SshHost(..)}` (the
/// [`GitHubRunner`](ExecutionTarget::GitHubRunner) variant names the cloud runner for later
/// slices and is treated as always-eligible by the pure placement filter). Defaults to `Local`,
/// so a pre-#717 run's state — which carries no target — reads as the implicit local box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionTarget {
    /// The implicit local box the daemon runs on — the default and pre-#717 behavior.
    #[default]
    Local,
    /// A chosen SSH-reachable worker node, addressed by its `~/.ssh/config` alias / node id.
    SshHost(String),
    /// A GitHub-hosted runner (the cloud-execution target named for later M1 slices).
    GitHubRunner,
}

impl ExecutionTarget {
    /// The kebab-case label for status surfaces: `local`, `ssh:<alias>`, or `github-runner`.
    pub fn label(&self) -> String {
        match self {
            ExecutionTarget::Local => "local".to_string(),
            ExecutionTarget::SshHost(alias) => format!("ssh:{alias}"),
            ExecutionTarget::GitHubRunner => "github-runner".to_string(),
        }
    }

    /// Whether this is the default implicit-local-box target — the serde skip predicate, so a
    /// `Local` item omits the `target` key entirely (keeping pre-#717 state files byte-identical).
    pub fn is_local(&self) -> bool {
        matches!(self, ExecutionTarget::Local)
    }

    /// Parse a label back to a target (`None` for anything unrecognized). Accepts `local`,
    /// `ssh:<alias>` (alias must be non-empty), and `github-runner`.
    pub fn parse(s: &str) -> Option<ExecutionTarget> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "local" => Some(ExecutionTarget::Local),
            "github-runner" => Some(ExecutionTarget::GitHubRunner),
            _ => s
                .strip_prefix("ssh:")
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(|a| ExecutionTarget::SshHost(a.to_string())),
        }
    }
}

/// The lifecycle of a backlog entry: pending → leased → done|failed. Distinct from the run state's
/// [`crate::orchestrate::ItemStatus`] — `leased` means "handed to a run", which then owns the
/// dispatch → merge lifecycle and reconciles the terminal outcome back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BacklogStatus {
    /// Available to be pulled into a run — the default and the only pullable state.
    #[default]
    Pending,
    /// Pulled into a run (extended onto the run state's queue) — not pullable again.
    Leased,
    /// Terminal success — the leased run item merged. Dropped by [`compact`].
    Done,
    /// Terminal failure — the leased run item failed. Dropped by [`compact`].
    Failed,
    /// Terminal, and asked for — someone took this back out of the queue.
    ///
    /// Distinct from `Failed` because the difference is the whole point of recording it: failed
    /// means the machine tried and could not, cancelled means a person decided it should not. A
    /// queue that can only be added to is not a queue you can run a machine from, and collapsing
    /// the two would make "why did this never run" unanswerable for the case where the answer is
    /// "you said so".
    Cancelled,
}

impl BacklogStatus {
    /// Every status, in lifecycle order, so a caller can enumerate or validate the label set
    /// instead of hardcoding its own copy (`bn-web` validates `?status=` against this).
    pub const ALL: [BacklogStatus; 5] = [
        BacklogStatus::Pending,
        BacklogStatus::Leased,
        BacklogStatus::Done,
        BacklogStatus::Failed,
        BacklogStatus::Cancelled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BacklogStatus::Pending => "pending",
            BacklogStatus::Leased => "leased",
            BacklogStatus::Done => "done",
            BacklogStatus::Failed => "failed",
            BacklogStatus::Cancelled => "cancelled",
        }
    }

    /// Whether this entry has reached a terminal state (done or failed) and can be compacted away.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            BacklogStatus::Done | BacklogStatus::Failed | BacklogStatus::Cancelled
        )
    }

    /// Whether this entry still occupies the backlog as live work — pending or leased. Used for
    /// dedup: re-adding a `(repo, id)` that is already active is a no-op.
    pub fn is_active(self) -> bool {
        matches!(self, BacklogStatus::Pending | BacklogStatus::Leased)
    }
}

/// One durable backlog entry, keyed `(repo, id)`. `id` is [`derive_id`] of `item` (issue number,
/// issue/PR url, or a slug), so the same subject declared two ways dedups to one entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklogEntry {
    /// Stable id within `repo` — [`derive_id`] of `item`.
    pub id: String,
    /// The repo this work targets. Half of the `(repo, id)` key (#343 multi-repo).
    pub repo: String,
    /// The raw dispatch subject as declared (`338`, a url, or free text).
    pub item: String,
    #[serde(default)]
    pub kind: BacklogKind,
    /// Higher is pulled first; ties break FIFO by `added_at` then `id`. Signed so a de-prioritized
    /// item can sit below the default-`0` floor.
    #[serde(default)]
    pub priority: i64,
    /// Path globs this work is expected to touch — the overlap-aware scheduling hint #348 uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
    #[serde(default)]
    pub substrate: Substrate,
    /// Epoch secs when first added — the FIFO tiebreak and the "added N ago" clock.
    #[serde(default)]
    pub added_at: i64,
    /// Free-form provenance (e.g. `"triage"`, `"scout:341"`). Empty when unset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// The task this work came from (#1001) — the id of a line in the global store.
    ///
    /// **The invariant that makes "work only ever starts from a task" checkable.** Without it,
    /// `bn orchestrate add` could inject work with no durable record, so the promise was a
    /// convention rather than something a test could assert.
    ///
    /// Empty only on entries that predate #1001; those are grandfathered read-only and reported by
    /// `bn doctor` rather than deleted, so an in-flight run is never orphaned by the upgrade.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task: String,
    /// Controller-agent ownership for work routed through `bn orchestrate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(default)]
    pub status: BacklogStatus,
    /// Epoch secs of the last status change (lease / terminal reconcile).
    #[serde(default)]
    pub updated_at: i64,
    /// **How** this work is executed. Absent on every pre-queue line, which reads as
    /// [`ExecSpec::Agent`] — the historical `bn dispatch <item>` behavior.
    #[serde(default)]
    pub exec: ExecSpec,
    /// **What the executing node must offer.** Absent or empty means "runs anywhere".
    #[serde(default, skip_serializing_if = "Requirements::is_empty")]
    pub requires: Requirements,
    /// The node this entry was leased *to*, once a tick placed it.
    ///
    /// Distinct from `status: Leased`, which only says *some* consumer took it. Naming the node is
    /// what lets a crashed box's leases be identified and released rather than stranded pending
    /// forever behind a machine that is never coming back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// Estimated wall-clock seconds, for the timeline's projection. `0` = use the per-kind default.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub est_secs: i64,
    /// Epoch secs before which this work should not start.
    ///
    /// This is the task-level replacement for queue-wide pause: one entry can be snoozed without
    /// stopping the dispatcher or unrelated work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<i64>,
    /// Epoch secs after which this work is pointless and should be dropped rather than started.
    ///
    /// Time-sensitive work loses its meaning when it slips: a 09:00 digest run at 14:00 is worse
    /// than no digest, and by the time tomorrow's is due, today's has been superseded. Queueing
    /// something otherwise strips it of its schedule — it becomes just another entry, run whenever
    /// capacity appears. This is how a scheduled job keeps its time inside a queue.
    ///
    /// `None` for work with no deadline, which is most of it: a PR review or a code change is not
    /// less valuable an hour later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
    /// What happens when this work's schedule fires again while this instance is still waiting.
    /// Absent on every pre-existing line, which reads as [`Recurrence::Coalesce`].
    #[serde(default, skip_serializing_if = "Recurrence::is_default")]
    pub recurrence: Recurrence,
    /// Work that keeps the *machine* healthy rather than doing anything for you: recovering stuck
    /// agents, reaping dead leases, freeing pool slots.
    ///
    /// Exempt from pause, and the reason is the whole point. You pause when the machine is unhappy
    /// — which is exactly when the recovery pass matters most. Pausing it would disable the thing
    /// that would have fixed the situation, silently, at the moment you most needed it.
    ///
    /// Pause means "start no new work". Recovery is not new work; it is what makes stopping safe.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovery: bool,
    /// Work that costs like an agent even though it launches none.
    ///
    /// The express path exists because a two-second housekeeping job should not queue behind the
    /// research cap. It keys off "needs no worktree", which is a *proxy* for cheap — and the proxy
    /// is wrong at both ends of the range it was never asked about. Pool warming and the pre-land
    /// merge gate own no worktree and are the two most expensive things the machine does: minutes
    /// of full build each.
    ///
    /// Expressing them would be wrong twice over. They would skip every cap an operator set — and
    /// they would run *inline inside the tick*, holding the dispatcher for minutes while every pass
    /// behind it skipped on the lock. Losing the dispatcher is worse than losing the throttle.
    ///
    /// So weight is declared. Heavy work is admitted like an agent and spawned like one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heavy: bool,
    /// Work the machine generated for itself, rather than work a person chose to do.
    ///
    /// A repo setup agent, a pool warm, a worktree reclaim: the *click* may be a human's, but the
    /// work is bookkeeping, and it should not be booked against the person the way a typed task is.
    /// Without the distinction, every producer's enqueue minted a person-scoped task (#1001) and
    /// today's list came back 96 items deep with three of them the human's own — a list that says
    /// everything says nothing.
    ///
    /// Declared by the producer rather than inferred, because the same door serves both: `bn queue
    /// add --job pool-warm` is machine work and `bn queue add "fix the header"` is not, and only the
    /// caller knows which it just built. [`crate::queue::classify::is_machine`] is where the answer
    /// is read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub machine: bool,
    /// Manual classification override when durable fields are not enough.
    ///
    /// Outranks [`machine`](BacklogEntry::machine): that is the producer's statement of provenance,
    /// this is a person's later opinion, and a person who says `bn queue class <id> foreground`
    /// means it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_override: Option<ClassOverride>,
    /// The identity a worktree-bearing entry keeps across the queue — see [`AgentSpec`]. Read when
    /// [`ExecSpec::needs_worktree`] and ignored otherwise; empty on every line written before it
    /// existed, which reads as the historical "let dispatch derive it".
    #[serde(default, skip_serializing_if = "AgentSpec::is_empty")]
    pub agent: AgentSpec,
    /// Consecutive launches of this entry that never got off the ground, reset by the first that
    /// does — see [`crate::queue::tick::launch_retry_delay`].
    ///
    /// A launch failure returns the entry to pending, which is what makes it retryable and also
    /// what made it immortal: nothing counted the attempts, so an entry whose blocker never lifts
    /// retried for as long as the machine was up. This is the count that lets the tick eventually
    /// stop. It is *consecutive* rather than lifetime, because a lease reclaimed after a healthy
    /// launch says nothing about whether the next one will start.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub launch_failures: u32,
}

/// The identity a queued agent must keep in order to run as the same piece of work it was declared
/// to be.
///
/// **Why this exists.** `bn dispatch` takes identity as launch *flags* — `--branch`, `--name`,
/// `--yolo` — and for years the only consumer was the launcher, which read them immediately. When
/// dispatch learned to enqueue instead, the capture path carried the subject and the harness and
/// silently dropped the rest. A repo-setup agent declared as `--branch repo-setup --name repo-setup
/// --yolo` came back out of the queue as a bare agent on a branch slugified from its own prompt,
/// unable to be recognised by the guard that was supposed to stop a second one starting.
///
/// The loss was invisible: nothing failed, the work simply became a different piece of work. So the
/// identity is durable now, and [`crate::queue::kind::ExecSpec::needs_worktree`] decides when it is
/// read. Empty means "let dispatch derive it", which is the historical behavior of a bare
/// `bn dispatch <item>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// The branch the agent works on. `None` lets dispatch slugify one from the item — right for a
    /// free-text dispatch, wrong for anything that must be found again by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The fleet task name, which is how a running agent is *recognised* — the dashboard's
    /// already-running guards match on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the agent runs without stopping for approvals. An autonomous job that loses this
    /// does not fail; it starts and then waits forever for a human who was never told.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yolo: bool,
    /// Whether this lane declines the plan-review gate. Durable for the same reason [`yolo`] is:
    /// dropping it between the click and the tick does not fail, it silently reinstates a gate the
    /// caller asked to skip and leaves the agent waiting on a review nobody expected.
    ///
    /// [`yolo`]: AgentSpec::yolo
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_plan_review: bool,
    /// Reserve a second warm pool slot, detached at the repo's primary branch, for comparison
    /// testing alongside the feature worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_main: bool,
    /// Which pipeline stage this entry **is**.
    ///
    /// The stage decides the model, the effort, the harness and the workflow a launch runs under, so
    /// guessing it is guessing all four. Dispatch's fallback infers it from `(scout, skill)`, which
    /// can only ever yield `investigate`, `plan` or `execute` — the other six stages are
    /// unreachable, and a `critique` entry would silently launch on the execute stage's profile.
    ///
    /// So a producer that knows its stage says so and the entry carries it, for exactly the reason
    /// `branch`/`name`/`yolo` are here: work should run as the thing that was declared, not as
    /// something rebuilt from its subject line. `None` keeps the inference, so every line written
    /// before this field existed behaves as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<crate::pipeline::stage::Stage>,
}

impl AgentSpec {
    /// Whether this adds nothing to a bare dispatch — the skip-serializing test, so an entry that
    /// declares no identity is byte-identical to every line written before this field existed.
    pub fn is_empty(&self) -> bool {
        self == &AgentSpec::default()
    }
}

/// Manual queue class override on an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClassOverride {
    Foreground,
    Autonomous,
    Opportunistic,
}

impl ClassOverride {
    pub fn parse(s: &str) -> Option<ClassOverride> {
        match s.trim().to_ascii_lowercase().as_str() {
            "foreground" | "watched" => Some(ClassOverride::Foreground),
            "autonomous" | "auto" => Some(ClassOverride::Autonomous),
            "opportunistic" | "away" => Some(ClassOverride::Opportunistic),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ClassOverride::Foreground => "foreground",
            ClassOverride::Autonomous => "autonomous",
            ClassOverride::Opportunistic => "opportunistic",
        }
    }
}

/// Serde skip predicate for an unset [`BacklogEntry::est_secs`], so an entry that carries no
/// estimate keeps a byte-identical JSON line.
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// What to do when scheduled work fires again while its last firing is still waiting.
///
/// Deduping by id was never in question — the queue has always kept one entry per `(repo, id)`.
/// The policy is what happens to the entry that is already there, and the two kinds of scheduled
/// work want opposite answers:
///
/// * **Refresh work** — warm a pool, sync a cache, reconcile a registry. Its value does not decay;
///   running it late is fine and running it twice is waste. It wants the newest firing to keep the
///   waiting instance alive.
/// * **Time-critical work** — a 09:00 digest, an end-of-day summary. Its value *is* its hour. A
///   later firing must not keep a stale instance alive, because running it at the wrong time is
///   worse than not running it.
///
/// There is deliberately no `QueueBehind`. Two instances of a scheduled job running back to back
/// almost never does more than one does — the second reads the state the first just wrote — and it
/// would need each firing to carry its own identity, which is precisely the duplicate the dedupe
/// exists to prevent. If a job really does have per-occurrence work, that work belongs in distinct
/// entries with distinct ids, not in a recurrence policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Recurrence {
    /// Fold into the waiting instance and carry the newest deadline forward. The default, because
    /// most scheduled work is refresh work, and the failure it prevents — a job that silently stops
    /// running whenever the machine is busy for one interval — is the worse of the two.
    #[default]
    Coalesce,
    /// Leave the waiting instance exactly as it was, so a stale one lapses on its own deadline.
    Skip,
}

impl Recurrence {
    pub fn label(&self) -> &'static str {
        match self {
            Recurrence::Coalesce => "coalesce",
            Recurrence::Skip => "skip",
        }
    }

    /// Whether this is the default, for `skip_serializing_if` — so an entry that never expressed a
    /// preference serializes exactly as it did before the field existed.
    pub fn is_default(&self) -> bool {
        *self == Recurrence::default()
    }

    pub fn parse(s: &str) -> Option<Recurrence> {
        match s.trim().to_ascii_lowercase().as_str() {
            "coalesce" => Some(Recurrence::Coalesce),
            "skip" => Some(Recurrence::Skip),
            _ => None,
        }
    }
}

/// The mutable fields carried on a fresh `add` (everything not derived from `item`/clock). Lets the
/// CLI declare one set of options and stamp it onto every item in a batch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddSpec {
    pub kind: BacklogKind,
    pub priority: i64,
    /// What happens when this fires again while the last firing is still waiting.
    pub recurrence: Recurrence,
    pub touches: Vec<String>,
    pub substrate: Substrate,
    pub source: String,
    pub controller_id: Option<String>,
    /// The task this work comes from (#1001). Required by [`BacklogEntry::checked`]; the
    /// unchecked [`BacklogEntry::new`] leaves it empty for the migration path only.
    pub task: String,
    /// How to realize the work. [`Default`] is [`ExecSpec::Agent`], so a producer that does not
    /// care keeps the pre-queue behavior; [`BacklogEntry::new`] substitutes
    /// [`BacklogKind::default_exec`] when this is left at the default.
    pub exec: ExecSpec,
    /// What the executing node must offer. The entry's [`ExecSpec::implied_skill`] is folded in
    /// automatically, so naming a skill is enough to require it.
    pub requires: Requirements,
    /// Estimated wall-clock seconds; `0` uses the per-kind default.
    pub est_secs: i64,
    /// Epoch secs after which the work is pointless — see [`BacklogEntry::not_after`].
    pub not_after: Option<i64>,
    /// Epoch secs before which this work should not start — see [`BacklogEntry::not_before`].
    pub not_before: Option<i64>,
    /// Machine-health work, exempt from pause — see [`BacklogEntry::recovery`].
    pub recovery: bool,
    /// Costs like an agent though it launches none — see [`BacklogEntry::heavy`].
    pub heavy: bool,
    /// Machine bookkeeping rather than work a person chose — see [`BacklogEntry::machine`].
    pub machine: bool,
    /// Manual classification override — see [`BacklogEntry::class_override`].
    pub class_override: Option<ClassOverride>,
    /// The identity a worktree-bearing agent keeps — see [`AgentSpec`].
    pub agent: AgentSpec,
}

impl BacklogEntry {
    /// One line naming this work, for any surface that lists entries.
    ///
    /// `item` is the dispatch *subject*, which for a job is a short name and for an agent is the
    /// entire seed prompt. Rendering it raw is why ten queued setup agents all opened with the same
    /// wrapped paragraph and were impossible to tell apart or recognise. A declared
    /// [`AgentSpec::name`] is the better answer where there is one, because it is the same name the
    /// running agent will appear under in the fleet — the queue and the fleet name one piece of
    /// work identically.
    pub fn title(&self) -> String {
        if let Some(name) = self.agent.name.as_deref().map(str::trim) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        self.item
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string()
    }

    /// Whether this may run inline in a tick, rather than being admitted and spawned.
    ///
    /// The one place the question is answered, so the express path and the executor cannot disagree
    /// about a given entry — which is exactly how a minutes-long build would end up inside the
    /// dispatcher.
    pub fn is_express(&self) -> bool {
        !self.heavy && !self.exec.needs_worktree()
    }
}

impl AddSpec {
    /// The spec that would reproduce `entry`.
    ///
    /// Carries **every** axis across, which is the point: a re-add or a reprioritize that
    /// hand-copies six of nine fields silently drops the other three, and the ones most likely to
    /// be forgotten are exactly the ones that decide where the work may run.
    pub fn from_entry(entry: &BacklogEntry) -> AddSpec {
        AddSpec {
            kind: entry.kind,
            priority: entry.priority,
            recurrence: entry.recurrence,
            touches: entry.touches.clone(),
            substrate: entry.substrate,
            source: entry.source.clone(),
            controller_id: entry.controller_id.clone(),
            task: entry.task.clone(),
            exec: entry.exec.clone(),
            requires: entry.requires.clone(),
            est_secs: entry.est_secs,
            not_after: entry.not_after,
            not_before: entry.not_before,
            recovery: entry.recovery,
            heavy: entry.heavy,
            machine: entry.machine,
            class_override: entry.class_override,
            agent: entry.agent.clone(),
        }
    }
}

impl BacklogEntry {
    /// Build a fresh pending entry for `item` in `repo`, stamping the [`AddSpec`] options and clock.
    ///
    /// Two things are derived rather than demanded of the caller, so every producer gets them right
    /// without remembering to: an [`ExecSpec::Agent`] spec falls back to the kind's
    /// [`default_exec`](BacklogKind::default_exec), and whatever skill the resulting executor
    /// implies is folded into `requires.skills`. That fold is what makes "this entry runs the
    /// `auto-review-pr` skill" and "this entry may only be placed on a node serving `auto-review-pr`"
    /// the same statement instead of two that can disagree.
    pub fn new(repo: &str, item: &str, spec: &AddSpec, now: i64) -> BacklogEntry {
        let exec = match spec.exec {
            ExecSpec::Agent => spec.kind.default_exec(),
            ref explicit => explicit.clone(),
        };
        let mut requires = spec.requires.clone();
        if let Some(skill) = exec.implied_skill() {
            if !requires
                .skills
                .iter()
                .any(|s| s.eq_ignore_ascii_case(skill))
            {
                requires.skills.push(skill.to_string());
            }
        }
        BacklogEntry {
            id: derive_id(item),
            repo: repo.to_string(),
            item: item.to_string(),
            kind: spec.kind,
            priority: spec.priority,
            touches: spec.touches.clone(),
            substrate: spec.substrate,
            added_at: now,
            source: spec.source.clone(),
            controller_id: spec.controller_id.clone(),
            task: spec.task.clone(),
            status: BacklogStatus::Pending,
            updated_at: now,
            exec,
            requires,
            assigned_to: None,
            est_secs: spec.est_secs,
            not_before: spec.not_before,
            not_after: spec.not_after,
            recurrence: spec.recurrence,
            recovery: spec.recovery,
            heavy: spec.heavy,
            machine: spec.machine,
            class_override: spec.class_override,
            agent: spec.agent.clone(),
            launch_failures: 0,
        }
    }

    /// Build a pending entry, **refusing one that names no task** (#1001).
    ///
    /// This is the enforcement point for "work only ever starts from a task". `new` stays
    /// unchecked so the migration and the tests that predate the invariant still compile, but
    /// every production enqueue goes through here — see [`Backlog::untasked`] for how the
    /// grandfathered entries are surfaced rather than silently accepted.
    pub fn checked(
        repo: &str,
        item: &str,
        spec: &AddSpec,
        now: i64,
    ) -> Result<BacklogEntry, UntaskedEnqueue> {
        if spec.task.trim().is_empty() {
            return Err(UntaskedEnqueue {
                repo: repo.to_string(),
                item: item.to_string(),
            });
        }
        Ok(BacklogEntry::new(repo, item, spec, now))
    }
}

/// An attempt to enqueue work that no task governs (#1001).
///
/// A typed error rather than a panic or a silent accept: the caller's right move is usually to
/// mint the task and retry, which `bn orchestrate add` does automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntaskedEnqueue {
    pub repo: String,
    pub item: String,
}

impl std::fmt::Display for UntaskedEnqueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to queue work with no task: {} in {} — mint one with `bn task add` first",
            self.item, self.repo
        )
    }
}

impl std::error::Error for UntaskedEnqueue {}
