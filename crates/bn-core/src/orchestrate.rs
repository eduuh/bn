//! `bn orchestrate` — the native, event-driven fleet supervisor (issues #338 / #340).
//!
//! This module is the **pure core**: the agent-pane state machine (#340) and the persisted queue
//! model (#338). It has zero side effects — no tmux, no git, no launchd — so every classification
//! and scheduling decision is a unit-testable function. The CLI layer (`bn-cli/src/cmd/orchestrate`)
//! captures the live pane / git / PR signals, feeds them in here, and *realizes* the actions this
//! module decides. Keeping the policy pure is the whole point of #340: the pane-state → action
//! mapping lives in code and is unit-tested, instead of being re-authored per wave as an ad-hoc
//! natural-language scheduled prompt.
//!
//! Three parts:
//!   - [`PaneState`] + [`classify`] + [`SupervisorAction`] + [`action_for`] — the state machine.
//!   - [`Liveness`] + [`plan_relaunch`] + [`ExternalState`] — the recovery policy (in [`recovery`]).
//!   - [`OrchestrateState`] + [`QueueItem`] — the persisted, drain-until-empty queue.

use crate::backlog::{ExecutionTarget, Substrate};
use crate::hosts::NodeRecord;
use crate::keep_warm::KeepWarmPolicy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod owner;
mod pane;
mod recovery;
pub use owner::*;
pub use pane::*;
pub use recovery::*;

// ===========================================================================
// The persisted, drain-until-empty queue (#338)
// ===========================================================================

/// The lifecycle status of one queued work item as it flows dispatch → land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemStatus {
    /// Not yet dispatched — waiting for a concurrency slot.
    #[default]
    Queued,
    /// `bn dispatch` fired: a worktree + agent exist and are in flight (counts against the cap).
    Dispatched,
    /// The PR landed via the gated merge-queue — terminal success.
    Merged,
    /// Gave up (relaunch cap hit, or repeated gate failure) — terminal, surfaced for a human.
    Failed,
}

impl ItemStatus {
    pub fn label(self) -> &'static str {
        match self {
            ItemStatus::Queued => "queued",
            ItemStatus::Dispatched => "dispatched",
            ItemStatus::Merged => "merged",
            ItemStatus::Failed => "failed",
        }
    }
    /// Whether this item occupies an in-flight concurrency slot.
    pub fn in_flight(self) -> bool {
        matches!(self, ItemStatus::Dispatched)
    }
    /// Whether this item has reached a terminal state (merged or failed).
    pub fn is_terminal(self) -> bool {
        matches!(self, ItemStatus::Merged | ItemStatus::Failed)
    }
}

/// Durable dispatch transaction phase for one queue item (#712).
///
/// The lifecycle [`ItemStatus`] remains the operator-facing queue/dispatch/terminal state. This
/// finer-grained phase closes the crash window inside `Queued`: a claim is persisted before any
/// external launch, then promoted after the launch is observed. Missing fields in legacy state
/// default to `pending` and [`OrchestrateState::from_json`] backfills already-dispatched items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchPhase {
    /// No launch is reserved; the scheduler may select this item.
    #[default]
    Pending,
    /// Stable dispatch identity is durable, but launch completion is not yet recorded.
    Claimed,
    /// The launch was observed and the item is in its normal dispatched lifecycle.
    Dispatched,
}

impl DispatchPhase {
    fn is_pending(&self) -> bool {
        *self == DispatchPhase::Pending
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The concurrency lane lives in [`crate::backlog::kind`], next to the [`crate::backlog::BacklogKind`]
/// it projects from, so the kind to lane mapping stays exhaustive. Re-exported so every
/// `orchestrate::WorkClass` path still resolves.
pub use crate::backlog::WorkClass;

/// One item in the orchestration queue: the dispatch subject plus the live lifecycle it accretes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    /// Stable id — the issue number when the item is an issue/PR (`#338` / a URL → `338`), else a
    /// slug of the text. Used to address the item across passes without positional drift.
    pub id: String,
    /// The raw dispatch subject as declared (`338`, an issue/PR URL, or free text).
    pub item: String,
    /// The repo this item dispatches into — the other half of its **`(repo, item)` key** (#453).
    /// Multi-repo orchestration keys the queue by `(repo, id)`, not `id` alone, so two repos'
    /// identically-named items (e.g. both issue `#338`, or two free-text items slugging alike)
    /// coexist in one run with no state/window/branch-slug collision. Defaults to the run's
    /// [`OrchestrateState::repo`]: every constructor sets it, and [`OrchestrateState::from_json`]
    /// backfills it on any pre-#453 state (items persisted without a `repo` key), so a single-repo
    /// run behaves exactly as before — the bare `id` still displays and addresses items; the repo
    /// namespace only disambiguates cross-repo collisions. See [`scoped_key`] / [`QueueItem::key`].
    #[serde(default)]
    pub repo: String,
    pub status: ItemStatus,
    /// Crash-safe dispatch transaction phase (#712). A claimed item still has lifecycle status
    /// `queued`, but is not schedulable and occupies its intended slot until reconciliation.
    #[serde(default, skip_serializing_if = "DispatchPhase::is_pending")]
    pub dispatch_phase: DispatchPhase,
    /// The dispatched branch, once `bn dispatch` created the worktree.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch: Option<String>,
    /// The PR url/number, once opened.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pr: Option<String>,
    /// Relaunch-to-resume attempts so far — the [`Backoff`] index for this item.
    #[serde(default)]
    pub attempts: u32,
    /// Epoch secs when the item was dispatched (0 while queued).
    #[serde(default)]
    pub dispatched_at: i64,
    /// Epoch secs of the last relaunch (0 if never), gating the backoff wait.
    #[serde(default)]
    pub last_relaunch_at: i64,
    /// Epoch secs of the last status/state change.
    #[serde(default)]
    pub updated_at: i64,
    /// The last pane state observed for this item, for `bn orchestrate status`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_state: Option<PaneState>,
    /// Current controller-actionable stuck state, shared by status and back-channel emission.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stuck: Option<crate::agent::StuckLane>,
    /// The harness this item was dispatched under (`copilot` / `claude`), set once dispatched — so
    /// `bn orchestrate status` and the web board can show *what* is running in the window (#372).
    /// Flipped by the #382 usage-limit auto-fallback when the lane is switched to the other harness.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub harness: Option<String>,
    /// Path globs this item is expected to touch — the conflict-aware scheduling hint (#376),
    /// carried over from its durable [`crate::backlog::BacklogEntry::touches`] when the daemon
    /// leases it into the run. Empty = no declared hint, so it's schedulable alongside anything
    /// ([`conflicts`] treats an absent set as overlapping nothing); land-time `bn wt merge --all`
    /// remains the safety net. `["**"]` is the `--solo` degenerate case (overlaps everything).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
    /// Controller-agent that owns this lane, if it entered through a controller's durable backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// While this item is queued, the id of the in-flight item whose touch-area it overlaps —
    /// *why* it's held rather than dispatched (#376). Refreshed each dispatch pass by
    /// `OrchestrateState::refresh_holds` from the live [`conflicts`] policy, so `bn orchestrate
    /// status` and the web board show "held: conflicts with #NNN" without re-deriving, and a hold is
    /// logged once per episode. `None` = not held (or not queued). The authoritative scheduling
    /// decision itself is always live (`next_dispatchable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
    /// Where this item runs (#377): carried from its durable [`crate::backlog::BacklogEntry::substrate`]
    /// when leased. `Local`/`Auto` dispatch into a local worktree + agent (counting against the
    /// resource-aware local concurrency cap). `Remote` is retained only for legacy state and is not
    /// placed by the scheduler. Defaults to `Auto` on a pre-#377 run's state.
    #[serde(default)]
    pub substrate: Substrate,
    /// **Which node this item executes on** (#717) — a *separate axis* from [`substrate`]: substrate
    /// chooses local-worktree vs. GitHub fan-out, `target` chooses *the node* a local/auto item
    /// lands on (the implicit local box or a chosen SSH worker). Carried from the leased backlog
    /// entry; the pure scheduler places a local/auto item on this
    /// target after a hard eligibility filter. Defaults to [`ExecutionTarget::Local`] on a pre-#717
    /// run's state, so the implicit-local-box behavior is preserved unchanged.
    #[serde(default, skip_serializing_if = "ExecutionTarget::is_local")]
    pub target: ExecutionTarget,
    /// Which typed concurrency lane this item is capped in (#385): `Coding` for a `ship` change,
    /// `Research` for a `scout` investigation, derived from the leased
    /// [`crate::backlog::BacklogKind`] via `WorkClass::from_kind`. The resource-aware scheduler
    /// counts in-flight items **per lane** against that lane's cap, so coding and research scale
    /// independently. Defaults to `Coding` on a pre-#385 run's state and for every `--queue` item
    /// (which carries no kind), preserving the single-pool behavior.
    #[serde(default)]
    pub work_class: WorkClass,
    /// The harvested answer for a scout/ask item (#378), set once its findings are routed back —
    /// see `OrchestrateState::record_answer`. A `ship` never carries one (it lands a PR, not an
    /// answer); a `scout`/`ask` produces findings, not a merge, so *this* — not a PR — is its
    /// deliverable. `bn orchestrate answers` / `ask --wait` read it straight off the state file the
    /// daemon already writes (no new IPC). `None` until harvested (or on any pre-#378 run's state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// A claimed dispatch is targeting a GitHub-hosted runner rather than a local/SSH slot. Kept
    /// separate from `substrate` so a failed auto-overflow claim can return to its original policy.
    #[serde(default, skip_serializing_if = "is_false")]
    pub claimed_remote: bool,
    /// **The remote worker placement** for an item dispatched onto an SSH node (#729): the
    /// `node_id` it was placed on, the `request_id`/`attempt_id` the remote `bn worker start` was
    /// admitted under (idempotent atomic admission, #720), and the last-observed worker attempt
    /// state polled over SSH (`QUEUED`/`RUNNING`/`SUCCEEDED`/`FAILED`, or `UNKNOWN` when the node is
    /// unreachable). Persisted on *both* sides — the control-plane state here mirrors the worker's
    /// own registry — so a reconnect/reconcile can line them up. All `None`/empty for a local
    /// dispatch (the implicit box needs no remote handle), keeping pre-#729 state files unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

pub type QueueEntrySpec = (String, Vec<String>, Substrate, WorkClass, Option<String>);

/// The routed-back answer for a scout/ask item (#378) — the "delegate-for-answers" deliverable. A
/// scout runs the investigation-first `investigate` skill, writes findings to its branch note, and
/// may either finish report-only or continue to a bounded PR in the same session; when it reaches
/// `Done` the daemon harvests those findings and records this on the run item (a pure state field),
/// so `bn orchestrate answers` and `ask --wait` surface the answer with zero new IPC — they just
/// read the state file the daemon already persists. `path` points at the findings write-up in the
/// item's (synced, worktree-independent) branch-note dir, so it stays readable after the scout
/// worktree is reaped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// The question that was asked — the scout item's dispatch subject, echoed back beside the
    /// findings so `answers` / `--wait` show *what* was answered without re-deriving it.
    pub question: String,
    /// The harvested findings file in the item's branch-note dir (a sibling write-up when the scout
    /// wrote one, else the note itself) — the link the notification and `answers` point a human at.
    pub path: String,
}

impl QueueItem {
    fn new(repo: &str, item: &str, now: i64) -> QueueItem {
        QueueItem {
            id: derive_id(item),
            item: item.to_string(),
            repo: repo.to_string(),
            status: ItemStatus::Queued,
            dispatch_phase: DispatchPhase::Pending,
            branch: None,
            pr: None,
            attempts: 0,
            dispatched_at: 0,
            last_relaunch_at: 0,
            updated_at: now,
            last_state: None,
            stuck: None,
            harness: None,
            touches: Vec::new(),
            controller_id: None,
            held_by: None,
            substrate: Substrate::Auto,
            work_class: WorkClass::Coding,
            target: ExecutionTarget::Local,
            answer: None,
            claimed_remote: false,
            node: None,
            request_id: None,
            attempt_id: None,
        }
    }

    /// Build a queued item carrying its touch-globs (#376), substrate (#377) and typed work-class
    /// lane (#385) from a leased [`crate::backlog::BacklogEntry`], so the resource-aware scheduler
    /// knows where it should run, what it overlaps, and which lane caps it before it's dispatched.
    fn with_entry(
        repo: &str,
        item: &str,
        touches: Vec<String>,
        substrate: Substrate,
        work_class: WorkClass,
        controller_id: Option<String>,
        now: i64,
    ) -> QueueItem {
        QueueItem {
            touches,
            substrate,
            work_class,
            controller_id,
            ..QueueItem::new(repo, item, now)
        }
    }

    /// This item's tmux window name — its sanitized branch (`/` → `-`), matching how `bn dispatch`
    /// names the agent's window. `None` until the item is dispatched (no branch, no window yet).
    pub fn tmux_window(&self) -> Option<String> {
        self.branch.as_deref().map(tmux_window_name)
    }

    /// Whether this is a scout/ask item (#378): a read-only [`WorkClass::Research`] agent that
    /// investigates and reports findings, dispatched with `--scout` and opening **no PR**. The one
    /// predicate the dispatch branch and the answer-harvest key off, so a `ship` is never routed
    /// through the scout path and a scout is never expected to produce a PR.
    pub fn is_scout(&self) -> bool {
        self.work_class == WorkClass::Research
    }
    /// This item's **`(repo, id)` key** (#453) — the tuple the orchestrate queue is addressed by,
    /// so two repos' identically-named items never alias. `id` alone was ambiguous across repos.
    pub fn key(&self) -> (&str, &str) {
        (self.repo.as_str(), self.id.as_str())
    }

    fn reset_stuck_tracking(&mut self) {
        self.stuck = None;
    }

    /// Whether the scheduler may begin a fresh dispatch transaction for this item.
    pub fn is_dispatchable(&self) -> bool {
        self.status == ItemStatus::Queued && self.dispatch_phase == DispatchPhase::Pending
    }

    /// Whether this item reserves a dispatch slot. A durable claim counts immediately so a daemon
    /// restart cannot fill the same capacity with replacement work before reconciling the claim.
    pub fn occupies_dispatch_slot(&self) -> bool {
        self.status.in_flight() || self.dispatch_phase == DispatchPhase::Claimed
    }

    /// The canonical single-string form of this item's [`key`](Self::key) — `<repo>/<id>` (#453),
    /// for any map that needs one string key (a window→prompt / dispatch-tracking map). The
    /// repo-namespaced form of [`derive_id`]'s output; see [`scoped_key`].
    pub fn scoped_key(&self) -> String {
        scoped_key(&self.repo, &self.id)
    }
}

/// The tmux window name `bn dispatch` opens for a branch: `/` → `-` (the worktree-layout rule), so
/// a `dispatch/352` branch lives in window `dispatch-352`. Pure — the navigation half of #372.
pub fn tmux_window_name(branch: &str) -> String {
    branch.replace('/', "-")
}

/// A recovery / lifecycle event recorded on the state for `bn orchestrate status` — the audit trail
/// that replaces the hand-typed "report one terse line per agent". Also mirrored to the event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEvent {
    pub ts: i64,
    /// The item id this event concerns.
    pub id: String,
    pub state: PaneState,
    pub action: SupervisorAction,
    /// Human-readable detail (e.g. "relaunch #2 after dns-outage").
    pub note: String,
}

/// Capacity for one dispatch lane at the end of the latest orchestrator tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCapacity {
    pub lane: String,
    pub cap: usize,
    pub in_flight: usize,
    pub queued: usize,
    pub available: usize,
    pub saturated: bool,
}

/// The whole orchestration run: a repo-scoped queue plus its concurrency policy and recovery log.
/// Persisted as one JSON file in the state dir so the daemon (and `bn orchestrate status`) share it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrateState {
    /// The single repo this run targets — every queued item dispatches into it (issues live here).
    pub repo: String,
    pub queue: Vec<QueueItem>,
    /// Max agents dispatched at once. `solo` overrides this to 1.
    pub concurrency: usize,
    /// The **research-lane** local cap (#385): when set, `scout`/research agents ([`WorkClass::Research`])
    /// scale on this budget independently of the coding lane (`concurrency`), so `--coding 5 --research 2`
    /// runs 5 ship + 2 scout agents concurrently. `None` = no separate research lane: research items
    /// share the single `concurrency` pool (the pre-#385 single-knob behavior). Adjustable live via
    /// `bn orchestrate scale` ([`OrchestrateState::scale`]) — the daemon picks it up on its next tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<usize>,
    /// Refactor mode (e.g. a main.rs-touching change like #333): one agent at a time, no overlap.
    #[serde(default)]
    pub solo: bool,
    /// The recovery / lifecycle audit trail (newest last).
    #[serde(default)]
    pub recovery: Vec<RecoveryEvent>,
    pub started_at: i64,
    pub updated_at: i64,
    /// Set by `bn orchestrate stop` — the daemon's next tick sees it and exits cleanly.
    #[serde(default)]
    pub stopped: bool,
    /// Set by `bn orchestrate supervise` — this run is a **standing, session-independent supervisor**
    /// (#426). Each tick it additionally *adopts* live standalone `bn dispatch` lanes for the repo
    /// (see `OrchestrateState::adopt_dispatched_lanes`) so their green PRs land and dead/stalled
    /// lanes recover with nobody at the keyboard, it lands ready PRs every pass (not only when it has
    /// its own in-flight items), and it does **not** exit when the queue drains — it keeps standing to
    /// catch future lanes and PRs. A plain `--queue` run leaves this `false` and behaves as before.
    #[serde(default)]
    pub supervise: bool,
    /// Opt-in policy for automatically replenishing this resident supervisor's durable backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<KeepWarmPolicy>,
    /// Last time this repo launched keep-warm ideation scouts. Persisted for restart-safe rate limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_warm_last_brainstorm_at: Option<i64>,
    /// Set by `bn orchestrate add --start` (any backlog-seeded run) — this run is a **resident,
    /// self-feeding** loop (#368): on drain it does **not** exit but stays alive pulling the next
    /// batch from the durable backlog (#375) as slots free, so a multi-wave backlog runs to
    /// completion with **zero scheduled prompts** even when waves are appended with gaps between
    /// them. Unlike `supervise` (#426) it does not adopt external standalone `bn dispatch` lanes —
    /// it only feeds itself from the backlog. Only `stop` ends it. A bounded `--queue` run leaves
    /// this `false` and exits on drain as before, preserving the one-shot-wave behavior.
    #[serde(default)]
    pub self_feed: bool,
    /// The harness the daemon dispatches new agents under (`bn orchestrate --agent`, #382), or the
    /// resolved config default. `None` on a pre-#382 run's state, which `dispatch_harness` reads as
    /// [`DEFAULT_HARNESS`] (`copilot`) — the prior hardcoded behavior. A per-item [`QueueItem::harness`]
    /// still wins for a lane the usage-limit fallback has already switched.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub harness: Option<String>,
    /// The system boot marker (`boot-id` / boot epoch) observed at the last daemon pass (#380).
    /// When the daemon restarts after a **reboot** this changes — every in-flight tmux window died
    /// with the machine, so the fleet would classify `Dead` → hard-restart and each re-dispatch
    /// would burn a relaunch `attempt` toward the [`Backoff::max_attempts`] cap for a *machine*
    /// event, not the item's fault. Detecting the fresh boot lets the daemon reset the backoff for
    /// in-flight items whose worktree survived (see `OrchestrateState::boot_reset_candidates`).
    /// `None` on a pre-#380 run or before the first pass records one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub boot_id: Option<String>,
    /// Why local dispatch was capped/paused on the last pass (#377) — the label of the binding cap
    /// (`ram` / `load` / `tokens` / `cost` / `local-cap`), or
    /// `None` when nothing bound (slots free, or the queue is empty). Set by the CLI executor after
    /// the resource-aware scheduler, so `bn orchestrate status` shows *why* pending
    /// items aren't dispatching without re-observing the machine. Purely informational — the
    /// authoritative decision is always re-made live each pass.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dispatch_bind: Option<String>,
    /// Why the latest dispatch pass launched no work, or `None` when it launched at least one item.
    /// Unlike [`dispatch_bind`](Self::dispatch_bind), this covers the whole decision: empty backlog,
    /// all work already in flight, conflict holds, placement failures, and admission guards. The
    /// daemon persists the concise operator-facing line so every status surface reports the exact
    /// last-tick decision instead of re-deriving it from a later snapshot.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dispatch_reason: Option<String>,
    /// The worker-node registry this run may place work on (#717, M1 foundation) — the identity +
    /// capacity stubs the pure scheduler filters when resolving a
    /// [`QueueItem::target`]/[`ExecutionTarget::SshHost`] to a concrete, placement-eligible node.
    /// Empty on a pre-#717 run's state (and any run that never registered a node), which reads as
    /// "only the implicit local box is available" — so every item stays on `Local` exactly as
    /// before. No live capacity is fetched yet; this holds the stub records only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeRecord>,
}

/// Rolled-up counts for `bn orchestrate status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Counts {
    pub queued: usize,
    pub in_flight: usize,
    pub merged: usize,
    pub failed: usize,
    pub total: usize,
    /// Of `in_flight`, how many are historical **remote** runner records (#377), occupying no local
    /// slot. `in_flight - remote` are the local agents.
    #[serde(default)]
    pub remote: usize,
}

/// Derive a stable id from a dispatch subject: the issue/PR number from `#338`, `338`, or a GitHub
/// issue/PR URL; otherwise a lowercase slug of the text (first few words). Pure — no network.
pub fn derive_id(item: &str) -> String {
    let t = item.trim();
    // A bare number or #number.
    let bare = t.strip_prefix('#').unwrap_or(t);
    if !bare.is_empty() && bare.chars().all(|c| c.is_ascii_digit()) {
        return bare.to_string();
    }
    // A GitHub issue/PR URL: .../issues/NNN or .../pull/NNN (tolerate a trailing slug/fragment).
    for marker in ["/issues/", "/pull/"] {
        if let Some(rest) = t.split(marker).nth(1) {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num.is_empty() {
                return num;
            }
        }
    }
    // Free text → slug of the first ~6 words.
    let slug: String = t
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "item".to_string()
    } else {
        slug
    }
}

/// The repo-namespaced key for an item — `<repo>/<id>` (#453). [`derive_id`] yields the per-repo
/// `id` (an issue number or a text slug); this composes it with the repo so the orchestrate queue,
/// the window→prompt map, and any dispatch-tracking map are keyed by the full `(repo, item)` pair,
/// and two repos' identically-named items (both `#338`) never collide. Pure — the keying half of
/// the multi-repo foundation. A single-repo run still shows/addresses the bare `id`; only the
/// *key* is namespaced, so its behavior is unchanged.
pub fn scoped_key(repo: &str, id: &str) -> String {
    format!("{}/{}", repo.trim(), id)
}

// ===========================================================================
// Conflict-aware scheduling (#376) — the coarse, pure predicted-overlap policy
// ===========================================================================
//
// Parallel lanes that edit the same file/registration point (e.g. `crates/bn-cli/src/cli.rs`
// enums, `main.rs` match arms, `config/crons.toml`) collide and pile up sequential rebases — today
// only caught at land time by `bn wt merge --all`, after the losing agent has already burned a
// full run. These functions let the scheduler *predict* that overlap from each item's declared
// `touches` globs and hold a conflicting candidate rather than co-schedule it — a bn-native
// per-touch-area lock. Prediction is a pure optimization and can stay coarse: any missed overlap
// is still serialized safely at land time, and an over-prediction only costs a little throughput.

/// Whether two declared touch-glob **sets** are predicted to overlap — the conflict policy at the
/// heart of #376. True iff some glob in `a` and some glob in `b` can match a common path. An empty
/// set means "no declared hint", which conflicts with nothing (the item stays schedulable with
/// anything; land-time serialization remains the safety net). Pure, total, and symmetric.
pub fn touches_conflict(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| glob_overlap(x, y)))
}

/// Whether a candidate's declared `touches` are predicted to conflict with **any** in-flight item's
/// — the issue's `conflicts(candidate, inflight)` overlap policy. `inflight` is each dispatched
/// item's touch-set. Pure over its inputs, so the whole scheduling decision is unit-testable
/// without the daemon's side effects.
pub fn conflicts(candidate: &[String], inflight: &[Vec<String>]) -> bool {
    inflight
        .iter()
        .any(|other| touches_conflict(candidate, other))
}

/// Whether two path globs can match a common path — a coarse, dependency-free glob *intersection*.
/// Understands the touch-hint vocabulary: literal path segments, `*` (any run within one segment),
/// `?` (exactly one char), and `**` (any run of whole segments, so `"**"` — the `--solo`
/// degenerate case — overlaps everything). Symmetric and pure; `/`-separated, order-insensitive.
pub fn glob_overlap(a: &str, b: &str) -> bool {
    fn segs(g: &str) -> Vec<&str> {
        g.split('/').filter(|s| !s.is_empty()).collect()
    }
    seg_overlap(&segs(a), &segs(b))
}

/// Segment-level glob intersection: can these two `/`-split glob segment lists match a common path?
/// `**` matches zero-or-more whole segments (recursing both ways); otherwise the two head segments
/// must intersect ([`seg_match`]) and the tails must too.
fn seg_overlap(a: &[&str], b: &[&str]) -> bool {
    match (a.first(), b.first()) {
        (None, None) => true,
        // One side exhausted: overlaps only if every remaining segment is `**` (matches zero).
        (None, Some(_)) => b.iter().all(|s| *s == "**"),
        (Some(_), None) => a.iter().all(|s| *s == "**"),
        (Some(&x), Some(&y)) => {
            if x == "**" {
                // `**` matches zero segments (advance past it) or ≥1 (consume one of b, keep `**`).
                seg_overlap(&a[1..], b) || seg_overlap(a, &b[1..])
            } else if y == "**" {
                seg_overlap(a, &b[1..]) || seg_overlap(&a[1..], b)
            } else {
                seg_match(x, y) && seg_overlap(&a[1..], &b[1..])
            }
        }
    }
}

/// Whether two single path *segments* (no `/`, may carry `*`/`?`) can match a common string. Fast
/// path for the common all-literal case; otherwise a small wildcard-intersection search.
fn seg_match(x: &str, y: &str) -> bool {
    let wild = |s: &str| s.bytes().any(|c| c == b'*' || c == b'?');
    if !wild(x) && !wild(y) {
        return x == y;
    }
    wild_overlap(x.as_bytes(), y.as_bytes())
}

/// Non-empty-intersection test for two single-segment glob patterns over `*` (any run of chars) and
/// `?` (one char): does some concrete string match *both*? A reachability search over the product of
/// the two patterns' glob automata — `*` self-loops (and epsilon-skips to match empty), `?`/literals
/// consume one char. Bounded by `(x+1)·(y+1)` states, so it always terminates.
fn wild_overlap(x: &[u8], y: &[u8]) -> bool {
    #[derive(Clone, Copy)]
    enum Class {
        Any,
        Lit(u8),
    }
    // The class the pattern at `k` accepts for one consumed char, and where it lands afterwards
    // (`*` stays put — it may match more; `?`/literal advance).
    fn accept(pat: &[u8], k: usize) -> (Class, usize) {
        match pat[k] {
            b'*' => (Class::Any, k),
            b'?' => (Class::Any, k + 1),
            c => (Class::Lit(c), k + 1),
        }
    }
    let (nx, ny) = (x.len(), y.len());
    let mut seen = vec![vec![false; ny + 1]; nx + 1];
    let mut stack = vec![(0usize, 0usize)];
    while let Some((i, j)) = stack.pop() {
        if seen[i][j] {
            continue;
        }
        seen[i][j] = true;
        // Accept: both remainders can match the empty string (all `*`).
        if x[i..].iter().all(|&c| c == b'*') && y[j..].iter().all(|&c| c == b'*') {
            return true;
        }
        // Epsilon: a leading `*` may match zero chars — advance past it.
        if i < nx && x[i] == b'*' {
            stack.push((i + 1, j));
        }
        if j < ny && y[j] == b'*' {
            stack.push((i, j + 1));
        }
        // Consume one common char on both sides.
        if i < nx && j < ny {
            let (cx, ni) = accept(x, i);
            let (cy, nj) = accept(y, j);
            let compatible = match (cx, cy) {
                (Class::Any, _) | (_, Class::Any) => true,
                (Class::Lit(p), Class::Lit(q)) => p == q,
            };
            if compatible {
                stack.push((ni, nj));
            }
        }
    }
    false
}

impl OrchestrateState {
    /// Seed a fresh run from a declared queue of dispatch subjects.
    pub fn new(
        repo: &str,
        items: &[String],
        concurrency: usize,
        solo: bool,
        now: i64,
    ) -> OrchestrateState {
        OrchestrateState {
            repo: repo.to_string(),
            queue: items.iter().map(|i| QueueItem::new(repo, i, now)).collect(),
            concurrency: concurrency.max(1),
            research: None,
            solo,
            recovery: Vec::new(),
            started_at: now,
            updated_at: now,
            stopped: false,
            supervise: false,
            keep_warm: None,
            keep_warm_last_brainstorm_at: None,
            self_feed: false,
            harness: None,
            boot_id: None,
            dispatch_bind: None,
            dispatch_reason: None,
            nodes: Vec::new(),
        }
    }

    /// Append declared items to the queue, skipping ids already present — the "same-repo declare
    /// extends the run" behavior (#395). Returns how many items were actually added. Never touches
    /// existing items, so an in-flight wave keeps its dispatched/merged lifecycle untouched.
    pub fn extend(&mut self, items: &[String], now: i64) -> usize {
        let with: Vec<(String, Vec<String>)> =
            items.iter().map(|i| (i.clone(), Vec::new())).collect();
        self.extend_with_touches(&with, now)
    }

    /// Like [`extend`], but each `(item, touches)` carries the declared touch-globs (#376) from the
    /// durable backlog (#375), so the leased run item keeps the conflict-aware scheduling hint. Same
    /// skip-existing-ids dedup and return count as [`extend`].
    pub fn extend_with_touches(&mut self, items: &[(String, Vec<String>)], now: i64) -> usize {
        let with: Vec<QueueEntrySpec> = items
            .iter()
            .map(|(i, t)| {
                (
                    i.clone(),
                    t.clone(),
                    Substrate::Auto,
                    WorkClass::Coding,
                    None,
                )
            })
            .collect();
        self.extend_with_entries(&with, now)
    }

    /// Like [`extend_with_touches`], but each entry also carries its [`Substrate`] (#377) and typed
    /// [`WorkClass`] lane (#385) from the leased [`crate::backlog::BacklogEntry`], so the
    /// resource-aware scheduler knows whether an item is local/remote/auto and which lane caps it
    /// before dispatch. Same skip-existing-ids dedup and return count as [`extend`].
    pub fn extend_with_entries(&mut self, items: &[QueueEntrySpec], now: i64) -> usize {
        let mut added = 0usize;
        let repo = self.repo.clone();
        for (item, touches, substrate, work_class, controller_id) in items {
            let id = derive_id(item);
            // #453: dedup on the `(repo, id)` key, not `id` alone — a declare extends *this* run's
            // repo, so an id already present for this repo is skipped, while the same id owned by
            // another repo (a future multi-repo run) stays a distinct item.
            if self.queue.iter().any(|q| q.repo == repo && q.id == id) {
                continue;
            }
            self.queue.push(QueueItem::with_entry(
                &repo,
                item,
                touches.clone(),
                *substrate,
                *work_class,
                controller_id.clone(),
                now,
            ));
            added += 1;
        }
        if added > 0 {
            self.updated_at = now;
        }
        added
    }

    /// The effective concurrency cap: 1 in solo mode, else the configured cap (min 1).
    pub fn effective_concurrency(&self) -> usize {
        if self.solo {
            1
        } else {
            self.concurrency.max(1)
        }
    }

    /// Live-adjust the typed concurrency lanes (#385) — the pure state mutation behind
    /// `bn orchestrate scale`. Applies each provided cap (a `None` argument leaves that lane
    /// untouched): `coding` sets the coding lane (`concurrency`, min 1); `research` sets/creates the
    /// research lane. Returns the resulting `(coding_cap, research_cap)` so the CLI can echo the new
    /// budgets. A running daemon picks the change up on its next tick — no restart, no re-declare.
    pub fn scale(
        &mut self,
        coding: Option<usize>,
        research: Option<usize>,
    ) -> (usize, Option<usize>) {
        if let Some(c) = coding {
            self.concurrency = c.max(1);
        }
        if let Some(r) = research {
            self.research = Some(r);
        }
        (self.concurrency.max(1), self.research)
    }

    /// How many items currently occupy an in-flight slot (local **and** remote). Used for
    /// reconcile / status / "is there work in flight" — not the local concurrency cap, which counts
    /// only `local_in_flight`.
    pub fn in_flight(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| i.occupies_dispatch_slot())
            .count()
    }

    /// Whether the run is complete: no queued and no in-flight items remain (all merged/failed).
    pub fn is_drained(&self) -> bool {
        self.queue.iter().all(|i| i.status.is_terminal())
    }

    /// Whether this run stays *resident* once its ledger drains, rather than exiting: a standing
    /// supervisor (#426) or a self-feeding backlog drainer (#368). A resident run keeps polling —
    /// the supervisor for future standalone lanes + green PRs, the self-feeder for the next batch
    /// appended to the durable backlog (#375) — so a multi-wave backlog completes with no scheduled
    /// prompt. A bounded `--queue` run is not resident and finishes when drained.
    pub fn is_resident(&self) -> bool {
        self.supervise || self.self_feed
    }

    /// Rolled-up counts for status.
    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            total: self.queue.len(),
            ..Default::default()
        };
        for i in &self.queue {
            match i.status {
                ItemStatus::Queued => c.queued += 1,
                ItemStatus::Dispatched => {
                    c.in_flight += 1;
                    if i.substrate == Substrate::Remote {
                        c.remote += 1;
                    }
                }
                ItemStatus::Merged => c.merged += 1,
                ItemStatus::Failed => c.failed += 1,
            }
        }
        c
    }

    /// Mark the item at `idx` dispatched onto `branch`.
    pub fn mark_dispatched(&mut self, idx: usize, branch: Option<String>, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Dispatched;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            it.branch = branch;
            it.dispatched_at = now;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// The tmux `session:window` target to jump to an item's agent (`tmux switch-client -t …`), or
    /// `None` until the item is dispatched. The session is the **item's** repo — how `bn dispatch`
    /// opens the window (`<repo>:<sanitized-branch>`) — so status points straight at the running
    /// agent even in a multi-repo run (#453); a single-repo item's `repo` equals `self.repo`, so the
    /// target is unchanged. Falls back to `self.repo` for a legacy item whose `repo` is unset.
    pub fn tmux_target(&self, item: &QueueItem) -> Option<String> {
        let repo = if item.repo.is_empty() {
            self.repo.as_str()
        } else {
            item.repo.as_str()
        };
        item.tmux_window().map(|w| format!("{}:{}", repo, w))
    }

    /// Deserialize from JSON text (for load / tests).
    pub fn from_json(s: &str) -> Option<OrchestrateState> {
        let mut st: OrchestrateState = serde_json::from_str(s).ok()?;
        // #453: backfill the per-item `repo` on any pre-#453 state (items persisted without a
        // `repo` key), so every item carries its half of the `(repo, id)` key and a legacy
        // single-repo run keeps behaving exactly as before — the run's `repo` is the queue's repo.
        let repo = st.repo.clone();
        for it in &mut st.queue {
            if it.repo.is_empty() {
                it.repo = repo.clone();
            }
            if it.status != ItemStatus::Queued && it.dispatch_phase == DispatchPhase::Pending {
                it.dispatch_phase = DispatchPhase::Dispatched;
            }
        }
        Some(st)
    }

    /// Serialize to pretty JSON text.
    pub fn to_json(&self) -> std::io::Result<String> {
        crate::store::to_json_pretty(self)
    }

    /// Load the run state from `path`, or `None` if absent/unparseable.
    pub fn load(path: &Path) -> Option<OrchestrateState> {
        let s = std::fs::read_to_string(path).ok()?;
        OrchestrateState::from_json(&s)
    }

    /// Atomically persist to `path` (temp sibling + rename), creating the parent dir on demand — so
    /// a concurrent `bn orchestrate status` never reads a torn write.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::save_json_atomic(path, self)
    }

    // -----------------------------------------------------------------------
    // Web write-layer mutations (#392)
    // -----------------------------------------------------------------------

    /// Remove a queued item by id, returning whether one was removed. Only [`ItemStatus::Queued`]
    /// items can be dropped — an in-flight (dispatched) item owns a running agent and can't be
    /// silently forgotten (kill it instead), so this is a no-op for anything past `Queued`.
    pub fn remove_queued(&mut self, id: &str, now: i64) -> bool {
        let before = self.queue.len();
        self.queue.retain(|i| !(i.id == id && i.is_dispatchable()));
        let removed = self.queue.len() != before;
        if removed {
            self.updated_at = now;
        }
        removed
    }

    /// Reorder the **queued** subset of the queue to follow `ids` (ids not present are ignored;
    /// queued items missing from `ids` keep their relative order after the listed ones). Only the
    /// `Queued` slots are permuted — dispatched/merged/failed items keep their positions, so a
    /// reorder never disturbs an in-flight wave's lifecycle. Returns whether the order changed.
    pub fn reorder_queued(&mut self, ids: &[String], now: i64) -> bool {
        // The positions (indices into `self.queue`) currently holding a queued item.
        let slots: Vec<usize> = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, i)| i.is_dispatchable())
            .map(|(idx, _)| idx)
            .collect();
        // Desired order of queued items: those named in `ids` first (in that order), then any
        // queued item not named, in its existing order — so a partial list is a safe "bring these
        // to the front" rather than a drop.
        let mut desired: Vec<usize> = Vec::with_capacity(slots.len());
        for id in ids {
            if let Some(&idx) = slots
                .iter()
                .find(|&&idx| &self.queue[idx].id == id && !desired.contains(&idx))
            {
                desired.push(idx);
            }
        }
        for &idx in &slots {
            if !desired.contains(&idx) {
                desired.push(idx);
            }
        }
        if desired == slots {
            return false; // already in this order — no write
        }
        // Rebuild: pull the queued items out in the desired order and drop them back into the same
        // physical slots, leaving every non-queued item exactly where it was.
        let reordered: Vec<QueueItem> =
            desired.iter().map(|&idx| self.queue[idx].clone()).collect();
        for (slot, item) in slots.iter().zip(reordered) {
            self.queue[*slot] = item;
        }
        self.updated_at = now;
        true
    }
}

/// Locked read-modify-write of a run's state file — the atomic primitive #392 shares between the
/// web write layer and the daemon's tick so a concurrent edit is never clobbered. Acquires the
/// required [`crate::store::FileLock`] on `path`, loads the current state, applies `f`, stamps
/// `updated_at`, and atomically saves. Returns the closure's value, or `None` when no run exists at
/// `path` (nothing to mutate). Lock and save failures are returned rather than permitting an
/// unlocked mutation. The lock is held only for the (fast) in-memory mutation — never across a
/// subprocess — so contention windows stay tiny.
pub fn mutate<R>(
    path: &Path,
    f: impl FnOnce(&mut OrchestrateState) -> R,
) -> std::io::Result<Option<R>> {
    let _lock = crate::store::FileLock::acquire_required(path)?;
    let Some(mut st) = OrchestrateState::load(path) else {
        return Ok(None);
    };
    let out = f(&mut st);
    st.updated_at = crate::util::now_epoch_secs();
    st.save(path)?;
    Ok(Some(out))
}
//
// One state file **per repo** — `orchestrate-<repo>.json` — so two concurrent orchestrations
// (different repos) never share a file and a second `--queue` can never overwrite a different live
// run. The pre-#395 layout was a single machine-wide `orchestrate.json`; [`migrate_legacy_state`]
// moves it to its per-repo path on first contact so an in-flight wave survives the upgrade.

/// Sanitize a repo name into a filename/launchd-label-safe run key: `[A-Za-z0-9._-]` pass through,
/// everything else becomes `-`. Empty input degrades to `"run"` so a path/label is always valid.
pub fn sanitize_run_key(repo: &str) -> String {
    let key: String = repo
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let key = key.trim_matches('-').to_string();
    if key.is_empty() {
        "run".to_string()
    } else {
        key
    }
}

/// The state file name for a repo's run: `orchestrate-<key>.json`.
pub fn state_file_name(repo: &str) -> String {
    format!("orchestrate-{}.json", sanitize_run_key(repo))
}

/// The canonical on-disk path for a repo's orchestration run state (#395: keyed per repo, so
/// concurrent runs in different repos never collide).
pub fn state_path_for(repo: &str) -> PathBuf {
    crate::resolver::state_dir().join(state_file_name(repo))
}

/// Every per-run state file in `dir` (`orchestrate-*.json`), sorted for deterministic listing.
/// Excludes the legacy `orchestrate.json`, controller-owner leases, and atomic-save `*.json.tmp`
/// siblings.
pub fn run_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("orchestrate-") && n.ends_with(".json") && !n.ends_with(".owner.json")
            })
        })
        .collect();
    files.sort();
    files
}

/// Every per-run state file in the state dir.
pub fn run_paths() -> Vec<PathBuf> {
    run_files_in(&crate::resolver::state_dir())
}

#[cfg(test)]
mod run_file_tests {
    use super::*;

    #[test]
    fn run_files_exclude_controller_owner_leases() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in [
            "orchestrate-zeta.json",
            "orchestrate-alpha.json",
            "orchestrate-alpha.owner.json",
            "orchestrate-alpha.json.tmp",
            "orchestrate.json",
        ] {
            std::fs::write(dir.path().join(name), "{}").expect("write fixture");
        }

        let names = run_files_in(dir.path())
            .into_iter()
            .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec!["orchestrate-alpha.json", "orchestrate-zeta.json"]
        );
    }
}
