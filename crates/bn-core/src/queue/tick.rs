//! The dispatcher's decision, as a pure function.
//!
//! One pass of the tick: given a snapshot of the queue, the machine, the nodes and the pauses,
//! decide what to start. Nothing here reads the clock, the disk or the network — the CLI samples
//! the world, calls [`plan_tick`], and *realizes* the intent.
//!
//! That split is the whole point. "Why did nothing start?" becomes a question with a deterministic
//! answer you can print (`bn queue tick --dry-run --json`) and replay, rather than a debugging
//! session against a live fleet. It is also what lets the timeline project the future by running
//! the *same policy* forward, instead of modelling it a second time and drifting.
//!
//! What this deliberately does **not** do is rank. Ranking is [`crate::select::select_wave`] —
//! already the orchestrate run's ranking, already strategic (#458 advise), already conflict-aware
//! (#376). A second ranking here would be a second thing to keep in agreement.
use crate::advise::{AttnSignal, GoalContext};
use crate::backlog::BacklogEntry;
use crate::backlog::ExecutionTarget;
use crate::capability;
use crate::hosts::NodeRecord;
use crate::queue::kind::{BacklogKind, ExecSpec, WorkClass};
use crate::queue::pause::{Pause, PauseScope, PauseState};
use crate::queue::place::{place_req, Placement};
use crate::scheduler::{admit, Bind, Caps, Observed};
use crate::select::{select_wave, SelectCandidate, SelectionInputs, SkipReason};
use std::collections::{BTreeMap, HashMap};

/// Everything one tick needs, captured as a snapshot.
#[derive(Debug, Clone)]
pub struct TickInputs<'a> {
    /// Wall clock for this pass, epoch secs.
    pub now: i64,
    /// The machine this tick runs on — the `assigned_to` value stamped on every lease it takes.
    pub machine: &'a str,
    /// The pauses in force, already folded.
    pub pause: &'a PauseState,
    /// Every PENDING entry, machine-wide, any repo, any kind.
    ///
    /// Deliberately unordered on input: ranking is part of the policy under test, not a
    /// precondition the caller has to satisfy correctly.
    pub pending: &'a [BacklogEntry],
    /// What the machine is already committed to — including agents started by a human typing
    /// `bn dispatch`, so direct dispatch still consumes capacity rather than being invisible to it.
    pub in_flight: &'a [InFlight],
    /// The execution registry, **including a synthesized record for the local box**
    /// ([`crate::hosts::local_record`]). An empty registry here means "this machine can run
    /// nothing", which is a bug in the caller, not a fallback.
    pub nodes: &'a [NodeRecord],
    /// The configured ceilings — the existing #377 struct, reused verbatim.
    pub caps: &'a Caps,
    /// The live machine + budget signals — the existing #377 struct, reused verbatim.
    pub observed: &'a Observed,
    /// Per-kind concurrency ceilings, layered on top of the lane caps.
    pub kind_caps: &'a KindCaps,
    /// Which kinds this tick may claim.
    ///
    /// The migration valve that keeps this tick and the per-repo orchestrate tick from contending
    /// while both exist: each is handed a disjoint set, so they cannot race for an entry even
    /// before the backlog lease would have arbitrated it. `None` = every kind.
    pub kinds: Option<&'a [BacklogKind]>,
    /// Strategic ranking context — the same two inputs `select_wave` already takes, so ranking is
    /// delegated rather than reimplemented.
    pub goals: &'a [GoalContext],
    pub attention: &'a [AttnSignal],
    /// How much **express** (no-worktree) work one pass may run.
    ///
    /// Express work skips the lane caps because it costs nothing like an agent — but it runs
    /// *inline, sequentially, inside the tick*, so "no lane slot" is not the same as "no bound".
    /// Without one, a backlog of 200 queued crons executes all 200 in a single pass: the tick
    /// blocks for minutes, every scheduled pass behind it skips on the lock, and the machine is
    /// hammered by whatever happened to be queued. The lane caps bound concurrency; this bounds a
    /// pass.
    pub max_express: usize,
    /// Times of day when some work is preferred — see [`crate::queue::window`].
    pub windows: &'a [crate::queue::window::PriorityWindow],
}

/// Express items one tick may run. Enough that a normal cron backlog drains in a pass or two,
/// small enough that the tick stays well under its one-minute cadence.
pub const MAX_EXPRESS_PER_TICK: usize = 20;

/// One unit of work the machine is already committed to: a leased entry, a dispatched lane, a live
/// agent session. Counted against the lane and kind caps, and its `touches` join the conflict map.
#[derive(Debug, Clone, PartialEq)]
pub struct InFlight {
    pub repo: String,
    pub id: String,
    pub kind: BacklogKind,
    pub work_class: WorkClass,
    /// The node it landed on. `None` = the implicit local box.
    pub node: Option<String>,
    pub touches: Vec<String>,
    /// Epoch secs it started — the timeline's span origin.
    pub started_at: i64,
}

/// Per-kind concurrency ceilings, applied **after** the lane caps.
///
/// A kind with no entry is unbounded (limited only by its lane), so this is purely additive: an
/// unconfigured machine behaves exactly as the lane caps alone dictate. Configured under
/// `[queue.kind_caps]`, e.g. `review-pr = 1`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindCaps(pub BTreeMap<BacklogKind, usize>);

impl KindCaps {
    pub fn cap_for(&self, kind: BacklogKind) -> Option<usize> {
        self.0.get(&kind).copied()
    }
}

/// What the tick decided — **intent only**. Nothing here has happened yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickPlan {
    /// The work to start this pass, in dispatch order.
    pub start: Vec<Dispatch>,
    /// Entries whose deadline passed — to be resolved, not retried. Separate from `deferred`
    /// because a deferral says "later" and this says "never": carrying an expired entry forward
    /// would mean a missed 09:00 digest waiting all day to run at the wrong time.
    pub expired: Vec<Deferred>,
    /// Every candidate considered and not started, each with its reason.
    pub deferred: Vec<Deferred>,
    /// The single headline reason the tick started nothing (or nothing more). `None` when the
    /// queue drained cleanly.
    pub bind: Option<TickBind>,
    /// Lane occupancy after this plan is realized.
    pub lanes: Vec<LaneOccupancy>,
    /// Per-kind occupancy after this plan is realized.
    pub kinds: Vec<KindOccupancy>,
}

/// One intended launch: which entry, on which node, through which executor, under which harness.
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatch {
    pub repo: String,
    pub id: String,
    pub item: String,
    pub kind: BacklogKind,
    pub work_class: WorkClass,
    pub exec: ExecSpec,
    /// The node it was placed on.
    pub target: ExecutionTarget,
    /// The harness the chosen node offers, from the requirement's preference order. `None` = let
    /// the executor pick its configured default.
    pub harness: Option<String>,
    pub priority: i64,
    /// Whether this costs like an agent — see [`crate::backlog::BacklogEntry::heavy`]. The executor
    /// reads it to decide between running inline and spawning detached.
    pub heavy: bool,
    /// The advise rationale that justified starting it now — carried onto the event and the
    /// timeline mark, so a start is always explainable after the fact.
    pub rationale: String,
}

/// One entry that stayed pending this pass, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Deferred {
    pub repo: String,
    pub id: String,
    pub kind: BacklogKind,
    pub reason: DeferReason,
    /// Operator-facing detail, e.g. `node 'fedora-box' lacks required skill: auto-review-pr`.
    pub detail: String,
    /// Epoch secs it has been pending since — the "waiting 3h" column and the starvation alarm.
    pub waiting_since: i64,
}

/// Why one entry stayed pending. The middle three mirror [`SkipReason`] so the two policies report
/// the same vocabulary; the rest are the placement, kind-cap and pause reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferReason {
    /// A pause covers this entry — carries the scope that blocked it.
    Paused(PauseScope),
    /// A resource / budget / lane guard bound.
    Capacity(Bind),
    /// The per-kind ceiling is full while the lane still has room.
    KindCap,
    /// Predicted touch-set overlap with in-flight or already-picked work (#376).
    Conflict,
    /// No path to a clean merge.
    CantMerge,
    /// No READY node satisfies the entry's requirements — the detail names which one.
    NoCapableNode,
    /// This tick may not claim this kind (the migration partition).
    NotMyKind,
    /// This pass has already run its budget of inline express work; the next pass takes it.
    ExpressBudget,
    /// Its moment has passed — see [`crate::backlog::BacklogEntry::not_after`]. Not a deferral so
    /// much as an abandonment: the tick resolves it rather than carrying it forward.
    Expired,
}

impl DeferReason {
    /// The kebab-case label for the decision file and the status surface.
    pub fn label(&self) -> &'static str {
        match self {
            DeferReason::Paused(_) => "paused",
            DeferReason::Capacity(_) => "capacity",
            DeferReason::KindCap => "kind-cap",
            DeferReason::Conflict => "conflict",
            DeferReason::CantMerge => "cant-merge",
            DeferReason::NoCapableNode => "no-capable-node",
            DeferReason::NotMyKind => "not-my-kind",
            DeferReason::ExpressBudget => "express-budget",
            DeferReason::Expired => "expired",
        }
    }
}

/// Why the tick started nothing more this pass.
///
/// Ordered most-global first, and it is the **first** applicable one — so "paused" is never hidden
/// behind "at local concurrency cap", which is the failure mode that makes an operator go looking
/// for a resource problem that is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickBind {
    Paused(PauseScope),
    Capacity(Bind),
    NoCapableNode,
    /// Nothing was pending in the allowed kind set.
    Empty,
}

impl TickBind {
    pub fn label(&self) -> String {
        match self {
            TickBind::Paused(scope) => format!("paused:{}", scope.key()),
            TickBind::Capacity(bind) => bind.label().to_string(),
            TickBind::NoCapableNode => "no-capable-node".to_string(),
            TickBind::Empty => "empty".to_string(),
        }
    }
}

/// Lane occupancy after the plan — the machine-scoped sibling of `orchestrate::LaneCapacity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneOccupancy {
    pub lane: WorkClass,
    pub cap: usize,
    pub in_flight: usize,
    pub pending: usize,
    pub started: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindOccupancy {
    pub kind: BacklogKind,
    pub cap: Option<usize>,
    pub in_flight: usize,
    pub pending: usize,
    pub started: usize,
}

/// Decide this tick's plan — the whole admission + placement policy, pure.
///
/// Order is deliberate, and each step is the reason surface for the entries it drops:
///
///   1. **Kind partition**, then **pause** — a covered entry never reaches ranking, so a pause
///      reads as `paused` rather than as whatever cap it would have hit later.
///   2. **Rank** — delegated to [`select_wave`] with unbounded capacity, so the strategic order,
///      the conflict hold and the can't-merge filter are the *same* implementation the orchestrate
///      tick already uses. Capacity is applied here instead, because this tick's notion of
///      capacity is per-lane and per-kind rather than one number.
///   3. **Express** — work that needs no worktree (a headless job, a shell cron) skips the lane
///      caps and placement entirely. It costs nothing like an agent does, and queuing a two-second
///      housekeeping job behind the research cap would make the machine slower, not safer.
///   4. **Admit** — [`admit`] per lane, verbatim, then the per-kind ceiling.
///   5. **Place** — [`place_req`], which consults the entry's requirements against what each node
///      advertises.
pub fn plan_tick(inputs: &TickInputs) -> TickPlan {
    let mut plan = TickPlan::default();

    // 1. Partition by kind, then by deadline, then by pause. Each produces a record rather than a
    //    silent drop.
    let mut claimable: Vec<&BacklogEntry> = Vec::new();
    for entry in inputs.pending {
        let lane = WorkClass::from_kind(entry.kind);
        // Time-sensitive work that has missed its moment is abandoned before anything else looks
        // at it. Running it late is the failure being prevented, so ranking it would be beside the
        // point — and a pause must not be reported for something that was never going to run.
        if entry.not_after.is_some_and(|t| inputs.now > t) {
            plan.expired.push(defer(
                entry,
                DeferReason::Expired,
                "its scheduled window has passed".to_string(),
            ));
            continue;
        }
        if let Some(kinds) = inputs.kinds {
            if !kinds.contains(&entry.kind) {
                continue; // Another dispatcher owns it; not this tick's business to report on.
            }
        }
        // Recovery work is never paused. A pause is a statement about *new work*; the passes that
        // recover stuck agents and free dead leases are what make the paused state recoverable.
        // Deferring them here would mean `bn queue pause`, typed because the machine was unhappy,
        // silently switched off the machinery that fixes it.
        let gate = (!entry.recovery)
            .then(|| {
                inputs
                    .pause
                    .blocks(&entry.repo, entry.kind, lane, entry.assigned_to.as_deref())
            })
            .flatten();
        match gate {
            Some(pause) => plan.deferred.push(defer(
                entry,
                DeferReason::Paused(pause.scope.clone()),
                pause_detail(pause),
            )),
            None => claimable.push(entry),
        }
    }

    // 2. Rank, with capacity left unbounded so ranking and admission stay separable.
    let candidates: Vec<SelectCandidate> = claimable
        .iter()
        .map(|e| SelectCandidate {
            id: e.id.clone(),
            repo: e.repo.clone(),
            title: e.item.clone(),
            kind: e.kind,
            // Effective priority: the entry's own, plus whatever an open window adds. Applied
            // here, at the point ranking consumes it, so a window influences *order* without
            // touching the durable entry — close the window and the queue returns to its base
            // ordering with nothing to undo.
            priority: e.priority.saturating_add(crate::queue::window::boost_for(
                e,
                inputs.windows,
                inputs.now,
            )),
            goal: None,
            touches: e.touches.clone(),
            // A pending entry is by definition not already in a run — the lease is what moves it
            // out of pending — so nothing here can be un-mergeable for that reason.
            mergeable: true,
        })
        .collect();
    let inflight_touches: Vec<Vec<String>> =
        inputs.in_flight.iter().map(|f| f.touches.clone()).collect();
    let selection = select_wave(&SelectionInputs {
        goals: inputs.goals,
        candidates: &candidates,
        attention: inputs.attention,
        capacity: candidates.len(),
        inflight_touches: &inflight_touches,
    });
    for skipped in &selection.skipped {
        if let Some(entry) = find(&claimable, &skipped.repo, &skipped.id) {
            let reason = match skipped.reason {
                SkipReason::CantMerge => DeferReason::CantMerge,
                SkipReason::Conflict => DeferReason::Conflict,
                SkipReason::Capacity => DeferReason::Capacity(Bind::LocalCap),
            };
            let detail = match skipped.reason {
                SkipReason::Conflict => "touches overlap work already in flight".to_string(),
                _ => String::new(),
            };
            plan.deferred.push(defer(entry, reason, detail));
        }
    }

    // 3–5. Walk in rank order, applying the caps and placement each entry actually faces.
    let mut lane_used: BTreeMap<WorkClass, usize> = BTreeMap::new();
    let mut kind_used: BTreeMap<BacklogKind, usize> = BTreeMap::new();
    for f in inputs.in_flight {
        *lane_used.entry(f.work_class).or_default() += 1;
        *kind_used.entry(f.kind).or_default() += 1;
    }
    let mut planned_nodes: HashMap<String, usize> = HashMap::new();
    let mut express_used = 0usize;

    for chosen in &selection.wave {
        let Some(entry) = find(&claimable, &chosen.repo, &chosen.id) else {
            continue;
        };
        let lane = WorkClass::from_kind(entry.kind);

        // Per-kind ceiling first: it is the narrower statement, so reporting it is more useful
        // than reporting the lane the entry would also have fitted in.
        if let Some(cap) = inputs.kind_caps.cap_for(entry.kind) {
            if kind_used.get(&entry.kind).copied().unwrap_or_default() >= cap {
                plan.deferred.push(defer(
                    entry,
                    DeferReason::KindCap,
                    format!("{} at its cap of {cap}", entry.kind.label()),
                ));
                continue;
            }
        }

        // Express: no worktree, no lane slot, no placement — but bounded per pass, because it runs
        // inline inside this tick rather than concurrently beside it.
        if entry.is_express() {
            if express_used >= inputs.max_express {
                plan.deferred.push(defer(
                    entry,
                    DeferReason::ExpressBudget,
                    format!("{} express items already run this pass", inputs.max_express),
                ));
                continue;
            }
            express_used += 1;
            *kind_used.entry(entry.kind).or_default() += 1;
            plan.start.push(dispatch(
                entry,
                lane,
                ExecutionTarget::Local,
                None,
                &chosen.rationale,
            ));
            continue;
        }

        let used = lane_used.get(&lane).copied().unwrap_or_default();
        let local_bind = admit(inputs.caps, inputs.observed, lane, used);
        match place_req(
            &entry.requires,
            &entry.repo,
            &ExecutionTarget::Local,
            inputs.nodes,
            inputs.caps,
            local_bind,
            &planned_nodes,
        ) {
            Placement::Place(target) => {
                let harness = harness_for(entry, &target, inputs.nodes);
                if let ExecutionTarget::SshHost(alias) = &target {
                    *planned_nodes.entry(alias.clone()).or_default() += 1;
                }
                *lane_used.entry(lane).or_default() += 1;
                *kind_used.entry(entry.kind).or_default() += 1;
                plan.start
                    .push(dispatch(entry, lane, target, harness, &chosen.rationale));
            }
            Placement::Unplaced(reason) => {
                // A lane or resource guard excludes the local box from placement, so placement
                // then reports "no capable node" — true, but not the cause. Name the guard
                // instead, and carry ITS wording: a reason of `capacity` under a detail reading
                // "no node has skill:X" sends the reader off installing a skill that was never
                // the problem.
                let (deferral, detail) = match local_bind {
                    Some(bind) => (DeferReason::Capacity(bind), bind.reason().to_string()),
                    None => (DeferReason::NoCapableNode, reason),
                };
                plan.deferred.push(defer(entry, deferral, detail));
            }
        }
    }

    plan.lanes = occupancy_lanes(inputs, &claimable, &plan, &lane_used);
    plan.kinds = occupancy_kinds(inputs, &claimable, &plan, &kind_used);
    plan.bind = headline_bind(inputs, &claimable, &plan);
    plan
}

/// The single headline reason, first-applicable in most-global order.
fn headline_bind(
    inputs: &TickInputs,
    claimable: &[&BacklogEntry],
    plan: &TickPlan,
) -> Option<TickBind> {
    if !plan.deferred.is_empty() {
        if let Some(DeferReason::Paused(scope)) = plan
            .deferred
            .iter()
            .map(|d| &d.reason)
            .find(|r| matches!(r, DeferReason::Paused(_)))
        {
            return Some(TickBind::Paused(scope.clone()));
        }
    }
    if claimable.is_empty() && plan.deferred.is_empty() {
        return Some(TickBind::Empty);
    }
    if let Some(DeferReason::Capacity(bind)) = plan
        .deferred
        .iter()
        .map(|d| &d.reason)
        .find(|r| matches!(r, DeferReason::Capacity(_)))
    {
        return Some(TickBind::Capacity(*bind));
    }
    if plan
        .deferred
        .iter()
        .any(|d| d.reason == DeferReason::NoCapableNode)
    {
        return Some(TickBind::NoCapableNode);
    }
    let _ = inputs;
    None
}

fn occupancy_lanes(
    inputs: &TickInputs,
    claimable: &[&BacklogEntry],
    plan: &TickPlan,
    lane_used: &BTreeMap<WorkClass, usize>,
) -> Vec<LaneOccupancy> {
    WorkClass::ALL
        .iter()
        .map(|lane| LaneOccupancy {
            lane: *lane,
            cap: inputs.caps.cap_for(*lane),
            in_flight: lane_used.get(lane).copied().unwrap_or_default(),
            pending: claimable
                .iter()
                .filter(|e| WorkClass::from_kind(e.kind) == *lane)
                .count(),
            started: plan.start.iter().filter(|d| d.work_class == *lane).count(),
        })
        .collect()
}

fn occupancy_kinds(
    inputs: &TickInputs,
    claimable: &[&BacklogEntry],
    plan: &TickPlan,
    kind_used: &BTreeMap<BacklogKind, usize>,
) -> Vec<KindOccupancy> {
    BacklogKind::ALL
        .iter()
        .map(|kind| KindOccupancy {
            kind: *kind,
            cap: inputs.kind_caps.cap_for(*kind),
            in_flight: kind_used.get(kind).copied().unwrap_or_default(),
            pending: claimable.iter().filter(|e| e.kind == *kind).count(),
            started: plan.start.iter().filter(|d| d.kind == *kind).count(),
        })
        .collect()
}

/// The harness to launch under: the first acceptable one the chosen node advertises.
fn harness_for(
    entry: &BacklogEntry,
    target: &ExecutionTarget,
    nodes: &[NodeRecord],
) -> Option<String> {
    let labels = nodes
        .iter()
        .find(|n| n.target == *target)
        .map(|n| n.labels.as_slice())
        .unwrap_or_default();
    capability::pick_harness(&entry.requires, labels)
}

fn dispatch(
    entry: &BacklogEntry,
    lane: WorkClass,
    target: ExecutionTarget,
    harness: Option<String>,
    rationale: &str,
) -> Dispatch {
    Dispatch {
        repo: entry.repo.clone(),
        id: entry.id.clone(),
        item: entry.item.clone(),
        kind: entry.kind,
        work_class: lane,
        exec: entry.exec.clone(),
        target,
        harness,
        priority: entry.priority,
        heavy: entry.heavy,
        rationale: rationale.to_string(),
    }
}

fn defer(entry: &BacklogEntry, reason: DeferReason, detail: String) -> Deferred {
    Deferred {
        repo: entry.repo.clone(),
        id: entry.id.clone(),
        kind: entry.kind,
        reason,
        detail,
        waiting_since: entry.added_at,
    }
}

fn pause_detail(pause: &Pause) -> String {
    match pause.reason.is_empty() {
        true => format!("paused: {}", pause.scope.key()),
        false => format!("paused: {} — {}", pause.scope.key(), pause.reason),
    }
}

fn find<'a>(entries: &[&'a BacklogEntry], repo: &str, id: &str) -> Option<&'a BacklogEntry> {
    entries
        .iter()
        .find(|e| e.repo == repo && e.id == id)
        .copied()
}
