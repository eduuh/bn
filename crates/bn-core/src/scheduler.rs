//! Resource- and budget-aware elastic concurrency scheduler (#377) — the **pure** policy that
//! decides, each dispatch pass, how many queued items go local, which fan out to a remote runner,
//! and which cap (if any) is binding so dispatch *pauses* with a visible reason.
//!
//! Split, like the rest of `orchestrate`, into a pure core (here) and an IO executor (the CLI). The
//! CLI captures the live machine/budget signals ([`Observed`] — load average, free RAM, this-window
//! tokens + cost) and the configured [`Caps`], calls [`pick`], and *realizes* the plan. Nothing
//! here reads the machine, the clock, or disk, so the whole cap arithmetic / eligibility / pause
//! policy is unit-testable against synthetic inputs — the acceptance's "pure and unit-tested"
//! requirement. It composes with (never bypasses) the #376 conflict-aware holding.

use crate::backlog::{ExecutionTarget, Substrate};
use crate::capability::node_satisfies;
use crate::hosts::{NodeRecord, NodeStatus};

use crate::orchestrate::{conflicts, OrchestrateState, QueueItem, WorkClass};
use std::collections::HashMap;

/// The configured ceilings the scheduler enforces this pass (#377). Every guard is optional and
/// **off by default** — an unset guard never binds, so a run with no caps behaves exactly as the
/// pre-#377 fixed-concurrency daemon. Captured from the run policy (`max_local` = the effective
/// concurrency, honoring `--solo`) plus the machine-policy config (`[orchestrate]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    /// Hard ceiling on **coding-lane** (`ship`) agents in flight at once — the effective coding
    /// concurrency (solo ⇒ 1). When no research lane is configured (`research: None`) this is the
    /// single total local pool every lane shares, so a bare `--concurrency N` run behaves exactly
    /// as the pre-#385 single-knob daemon.
    pub max_local: usize,
    /// Hard ceiling on **research-lane** (`scout`) agents in flight at once (#385). `Some(m)` opens
    /// a typed research lane that scales independently of the coding lane (`--coding N --research
    /// M`); `None` = no separate research lane, so research items share the `max_local` pool (the
    /// backward-compatible single-cap behavior). Off by default.
    pub research: Option<usize>,
    /// Pause local dispatch while free RAM (MB) is **below** this — the anti-thrash guard (each
    /// local harness is a ~1–2 GB Node process). `None` = no RAM guard.
    pub min_free_ram_mb: Option<u64>,
    /// Pause local dispatch while the load average is **above** this. `None` = no load guard.
    pub max_loadavg: Option<f64>,
    /// Pause local dispatch while swap usage (percent of total swap in use) is **above** this — the
    /// #668 anti-thrash guard (swap saturation, not agent count, is what actually chokes a
    /// RAM-bound WSL box). `None` = no swap guard. A box with no swap configured never binds it.
    pub max_swap_pct: Option<f64>,
    /// Pause local dispatch once this-window token consumption reaches this rate (tokens/hour) —
    /// token rate limits bind before CPU does. `None` = no token budget.
    pub tokens_per_hour: Option<u64>,
    /// Pause local dispatch once today's spend (the cache-aware API-equiv `$`) reaches this.
    /// `None` = no cost budget. Machine-wide: reaching it pauses **every** lane.
    pub daily_cost_usd: Option<f64>,
    /// Per-work-class daily spend caps (#381 GAP 5) — the cache-aware API-equiv `$` a single lane
    /// may spend today before it (and only it) is paused. `None` = that lane has no separate cost
    /// budget (it's still bound by the machine-wide [`daily_cost_usd`](Caps::daily_cost_usd)). Off
    /// by default. Reaching a lane's cap binds [`Bind::Cost`] on that lane alone, so e.g. a capped
    /// research lane can be shed while the coding lane keeps dispatching. Read via
    /// [`Caps::daily_cost_for`].
    pub daily_cost_usd_coding: Option<f64>,
    pub daily_cost_usd_research: Option<f64>,
    /// Whether this run plan-gates dispatch (#349): when set, an `auto` item is **not**
    /// remote-eligible (a GitHub-hosted agent can't do the plan-review handshake). An explicit
    /// `substrate: remote` item bypasses this — the user opted into un-gated remote execution.
    pub plan_gated: bool,
    /// Whether an `auto` item may **overflow to a remote runner** when local dispatch is full/paused
    /// (#377). Off by default — an auto item then simply waits for a local slot (the pre-#377
    /// behavior), so enabling GitHub-hosted fan-out (which spends CI + AIC) is an explicit opt-in
    /// (`[orchestrate].remote_overflow`). An explicit `substrate: remote` item fans out regardless.
    pub remote_overflow: bool,
    /// Whether the daemon's local box owns this run's repo. Empty `[orchestrate].own` config maps
    /// to `true` for backward compatibility; an explicit allowlist can hand non-owned repos to
    /// eligible workers without dispatching them locally.
    pub local_owned: bool,
}

impl Caps {
    /// The local concurrency ceiling for `class` (#385): the coding lane uses `max_local`; the
    /// research lane uses its own `research` cap when a typed lane is configured, else it falls back
    /// to `max_local` — the shared single pool of the pre-#385 single-knob behavior. Paired with the
    /// per-lane in-flight count [`pick`] tracks, this is what makes admission work-class aware.
    pub fn cap_for(&self, class: WorkClass) -> usize {
        match (class, self.research) {
            (WorkClass::Research, Some(m)) => m,
            _ => self.max_local,
        }
    }

    /// Whether a distinct research lane is configured (#385) — coding and research then count their
    /// in-flight agents against separate caps; otherwise every lane shares the `max_local` pool.
    pub fn typed(&self) -> bool {
        self.research.is_some()
    }

    /// The per-work-class daily spend cap for `class` (#381 GAP 5), or `None` when that lane has no
    /// separate cost budget. The scheduler dispatches exactly the two lanes
    /// ([`WorkClass::Coding`] / [`WorkClass::Research`]); each has its own optional cap. Paired with
    /// [`Observed::cost_today_for`], this is what makes the cost pause work-class aware — a capped
    /// lane sheds while the other keeps going.
    pub fn daily_cost_for(&self, class: WorkClass) -> Option<f64> {
        match class {
            WorkClass::Coding => self.daily_cost_usd_coding,
            WorkClass::Research => self.daily_cost_usd_research,
        }
    }
}

/// The live machine + budget signals the CLI captures and passes in (#377). RAM / load are
/// `Option` because the reader is best-effort — an unreadable signal reads as "no pressure" so the
/// guard fails **open** (never blocking dispatch on an unknown). Token / cost default to `0` when
/// nothing is attributable this window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Observed {
    /// The current load average (typically the 1-minute figure), or `None` if unreadable.
    pub loadavg: Option<f64>,
    /// Free/available RAM in MB, or `None` if unreadable.
    pub free_ram_mb: Option<u64>,
    /// Swap in use as a percent of total swap (0.0–100.0), or `None` if unreadable / no swap
    /// configured. `None` fails **open** (the swap guard then never binds).
    pub swap_pct: Option<f64>,
    /// Tokens consumed in the trailing hour (fresh input + output; cache reads excluded).
    pub tokens_this_hour: u64,
    /// Today's spend in the cache-aware API-equiv `$` benchmark.
    pub cost_today_usd: f64,
    /// Today's spend attributed to the **coding** lane, in the same `$` benchmark (#381 GAP 5).
    /// `0.0` when the coding cost cap is unconfigured (it's only observed when its cap is set).
    pub cost_today_coding_usd: f64,
    /// Today's spend attributed to the **research** lane (#381 GAP 5). `0.0` when unconfigured.
    pub cost_today_research_usd: f64,
}

impl Observed {
    /// Today's spend attributed to `class` (#381 GAP 5) — the figure the per-lane cost cap
    /// ([`Caps::daily_cost_for`]) is checked against, for whichever of the two dispatchable lanes.
    pub fn cost_today_for(&self, class: WorkClass) -> f64 {
        match class {
            WorkClass::Coding => self.cost_today_coding_usd,
            WorkClass::Research => self.cost_today_research_usd,
        }
    }
}

/// Which cap bound local dispatch this pass — the "why we paused" surfaced in `bn orchestrate
/// status`. [`Bind::LocalCap`] is ordinary fullness (`max_local` reached); the rest are
/// resource/budget **pauses** that hold items `pending` until pressure clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// The local concurrency cap (`max_local`) is reached — plain fullness, not machine pressure.
    LocalCap,
    /// Free RAM is below the guard.
    Ram,
    /// Swap usage is above the guard.
    Swap,
    /// Load average is above the guard.
    Load,
    /// The hourly token budget is reached.
    Tokens,
    /// The daily cost budget is reached.
    Cost,
}

impl Bind {
    /// The kebab-case label persisted on the run state + shown in status (`ram` / `load` /
    /// `tokens` / `cost` / `local-cap`).
    pub fn label(self) -> &'static str {
        match self {
            Bind::LocalCap => "local-cap",
            Bind::Ram => "ram",
            Bind::Swap => "swap",
            Bind::Load => "load",
            Bind::Tokens => "tokens",
            Bind::Cost => "cost",
        }
    }

    /// Parse a [`label`](Bind::label) back into a [`Bind`] — the inverse used by `bn orchestrate
    /// status` to render the reason from the label persisted on the run state.
    pub fn from_label(s: &str) -> Option<Bind> {
        match s {
            "local-cap" => Some(Bind::LocalCap),
            "ram" => Some(Bind::Ram),
            "swap" => Some(Bind::Swap),
            "load" => Some(Bind::Load),
            "tokens" => Some(Bind::Tokens),
            "cost" => Some(Bind::Cost),
            _ => None,
        }
    }

    /// Whether this bind is a resource/budget **pause** (items stay pending until it clears), as
    /// opposed to ordinary local-cap fullness. Only pauses warrant the `orchestrate_paused` event.
    pub fn is_pause(self) -> bool {
        !matches!(self, Bind::LocalCap)
    }

    /// A one-line human reason for the status surface.
    pub fn reason(self) -> &'static str {
        match self {
            Bind::LocalCap => "at local concurrency cap",
            Bind::Ram => "paused — free RAM below the guard",
            Bind::Swap => "paused — swap usage above the guard",
            Bind::Load => "paused — load average above the guard",
            Bind::Tokens => "paused — hourly token budget reached",
            Bind::Cost => "paused — daily cost budget reached",
        }
    }
}

/// The gate deciding whether **another** local agent of `class` may be dispatched given the lane's
/// current `in_flight_for_class`, and if not, which cap bound (#377/#385). Budget guards are checked
/// first (token rate / spend bind before the machine does), then the resource guards — all
/// machine-wide, so they pause **every** lane; finally the per-lane local cap
/// ([`Caps::cap_for`]) binds when the class's own lane is full. A guard whose observed signal is
/// missing (RAM / load) is skipped — it can't bind on an unknown. Pure.
pub fn admit(
    caps: &Caps,
    obs: &Observed,
    class: WorkClass,
    in_flight_for_class: usize,
) -> Option<Bind> {
    if let Some(tph) = caps.tokens_per_hour {
        if obs.tokens_this_hour >= tph {
            return Some(Bind::Tokens);
        }
    }
    if let Some(max) = caps.daily_cost_usd {
        if obs.cost_today_usd >= max {
            return Some(Bind::Cost);
        }
    }
    // Per-lane cost budget (#381 GAP 5): a lane whose own daily spend is capped is paused
    // independently — checked against this class's attributed spend, so shedding a capped
    // research lane never holds back the coding lane (and vice-versa).
    if let Some(max) = caps.daily_cost_for(class) {
        if obs.cost_today_for(class) >= max {
            return Some(Bind::Cost);
        }
    }
    if let (Some(min), Some(free)) = (caps.min_free_ram_mb, obs.free_ram_mb) {
        if free < min {
            return Some(Bind::Ram);
        }
    }
    if let (Some(max), Some(swap)) = (caps.max_swap_pct, obs.swap_pct) {
        if swap > max {
            return Some(Bind::Swap);
        }
    }
    if let (Some(max), Some(load)) = (caps.max_loadavg, obs.loadavg) {
        if load > max {
            return Some(Bind::Load);
        }
    }
    if in_flight_for_class >= caps.cap_for(class) {
        return Some(Bind::LocalCap);
    }
    None
}

/// The local gate state that determines whether an item may use remote execution (#697).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteGateState {
    /// The local admission bind, if a resource, budget, or concurrency cap blocked the item.
    pub bind: Option<Bind>,
    /// Whether conflict-aware scheduling held the item behind overlapping work.
    pub held: bool,
    /// Whether this run opted into remote overflow for `auto` items.
    pub overflow_enabled: bool,
    /// Whether the run requires the local-only plan-review handshake.
    pub plan_gated: bool,
    /// Whether the item id identifies an issue/PR that the remote runner can be seeded from.
    pub issue_linked: bool,
}

/// Which execution path remote admission selects for an item (#697).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAdmission {
    Local,
    /// Fan out remotely, surfacing the local bind for overflowed `auto` work when present.
    Remote(Option<Bind>),
}

/// Decide whether an item uses the local placement path or a remote runner (#697).
///
/// Explicit `remote` work always fans out because the user selected that substrate. `auto` work
/// only overflows when local admission is bound, no conflict hold applies, overflow is enabled,
/// and the item can run without the local plan-review handshake. `local` work never fans out.
/// Pure over the substrate and captured gate state so every remote path is unit-testable here.
pub fn admit_remote(substrate: Substrate, gate: RemoteGateState) -> RemoteAdmission {
    match substrate {
        Substrate::Remote => RemoteAdmission::Remote(None),
        Substrate::Auto
            if gate.overflow_enabled
                && gate.bind.is_some()
                && !gate.held
                && !gate.plan_gated
                && gate.issue_linked =>
        {
            RemoteAdmission::Remote(gate.bind)
        }
        Substrate::Local | Substrate::Auto => RemoteAdmission::Local,
    }
}

/// An id is issue-linked when it's a bare number — [`crate::orchestrate::derive_id`] returns the
/// issue/PR number for `#N` / `N` / a GitHub issue-or-PR URL, while free text becomes a word slug.
fn is_issue_linked(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// The **hard eligibility filter** for placing a local/auto item on its chosen [`ExecutionTarget`]
/// (#717) — the first gate the generalized [`pick`] applies before a candidate can be dispatched.
/// Pure over the item's target + the run's node registry, so placement policy is unit-testable:
///   - [`ExecutionTarget::Local`] — always eligible (the implicit box the daemon runs on);
///   - [`ExecutionTarget::GitHubRunner`] — always eligible here (the cloud target carries its own
///     capacity, not a registry node);
///   - [`ExecutionTarget::SshHost`] — eligible **only** when the registry holds a node whose
///     `node_id` matches the alias *and* whose status is [`NodeStatus::Ready`]. A missing,
///     draining, offline, or not-yet-heartbeating node fails the filter, so the item is not placed
///     (it waits for its chosen host rather than silently falling back to local).
///
/// No live capacity is weighed yet (#719) — this is the identity/status gate only.
pub fn target_eligible(target: &ExecutionTarget, nodes: &[NodeRecord]) -> bool {
    match target {
        ExecutionTarget::Local | ExecutionTarget::GitHubRunner => true,
        ExecutionTarget::SshHost(alias) => nodes
            .iter()
            .any(|n| &n.node_id == alias && n.status == NodeStatus::Ready),
    }
}

fn repo_matches(configured: &str, repo: &str) -> bool {
    configured.eq_ignore_ascii_case(repo)
        || configured.rsplit(['/', '\\']).next().is_some_and(|name| {
            repo.rsplit(['/', '\\'])
                .next()
                .is_some_and(|repo_name| name.eq_ignore_ascii_case(repo_name))
        })
}

/// Whether an ownership allowlist permits `repo`. Empty is unrestricted for backward
/// compatibility; entries match either the full slug or trailing repo name.
pub fn repo_owned(owned_repos: &[String], repo: &str) -> bool {
    owned_repos.is_empty() || owned_repos.iter().any(|owned| repo_matches(owned, repo))
}

pub(crate) fn node_owns_repo(node: &NodeRecord, repo: &str) -> bool {
    repo_owned(&node.owned_repos, repo)
}

pub(crate) fn node_resources_allow(node: &NodeRecord, caps: &Caps) -> bool {
    let Some(capacity) = node.capacity else {
        return true;
    };
    if let (Some(min), Some(free)) = (caps.min_free_ram_mb, capacity.mem_available_mb) {
        if free < min {
            return false;
        }
    }
    if let (Some(max), Some(load)) = (caps.max_loadavg, capacity.loadavg1) {
        if load > max {
            return false;
        }
    }
    capacity.status() == crate::worker::capacity::NodeStatus::Ready
}

/// Whether an SSH worker can accept `item` now. This is the shared pure predicate used by both
/// placement and backlog refill sizing, so ownership/capability/resource rules cannot drift.
pub fn worker_eligible(node: &NodeRecord, item: &QueueItem, caps: &Caps) -> bool {
    matches!(node.target, ExecutionTarget::SshHost(_))
        && node.status == NodeStatus::Ready
        && node.active_attempts < node.slots.max(1)
        && node_satisfies(
            node,
            &item.required_capabilities,
            &item.excluded_capabilities,
        )
        && node_owns_repo(node, &item.repo)
        && node_resources_allow(node, caps)
}

/// Whether a local admission bind may be relieved by an SSH worker. Local machine pressure and
/// plain local fullness may hand off; global token/cost budgets remain binding across boxes.
pub fn bind_allows_worker_handoff(bind: Option<Bind>) -> bool {
    !matches!(bind, Some(Bind::Tokens | Bind::Cost))
}

/// Placement moved to [`crate::queue::place`] — re-exported so every existing
/// `scheduler::{Placement, place, place_with_load}` path still resolves.
pub use crate::queue::place::{place, place_with_load, Placement};

/// The dispatch plan for one pass (#377): the queue indices to dispatch **locally**, those to fan
/// out to a **remote** runner (occupying no local slot), and the binding cap that stopped local
/// dispatch (for the status line + pause event). Indices point into `st.queue`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// Queue indices to dispatch into a local worktree + agent, in order.
    pub local: Vec<usize>,
    /// Queue indices to fan out to a remote runner — these occupy no local slot.
    pub remote: Vec<usize>,
    /// **Where each locally-dispatched item lands** (#717) — one `(queue index, target)` pair per
    /// entry in [`local`](Plan::local), in the same order, naming the node the scheduler placed it
    /// on: [`ExecutionTarget::Local`] (the implicit box) or a chosen [`ExecutionTarget::SshHost`].
    /// This is the new placement axis — orthogonal to [`remote`](Plan::remote), which is GitHub
    /// fan-out. On a run with no node registry every local pick is placed `Local`, so a pre-#717
    /// executor that ignores this field behaves exactly as before.
    pub placements: Vec<(usize, ExecutionTarget)>,
    /// **Items that could not be placed this pass** (#729) — one `(queue index, reason)` pair per
    /// local/auto candidate whose chosen/eligible node set was empty after the hard capability
    /// filter (#728). The item stays pending (no slot consumed, never silently downgraded); the
    /// reason is operator-facing (e.g. `no READY node satisfies capability: mt5-bridge`) so a
    /// doomed dispatch is refused loudly rather than hanging on an ineligible node. Empty on a
    /// single-node run (placement always resolves `Local`).
    pub unplaced: Vec<(usize, String)>,
    /// The cap that stopped further local dispatch, or `None` if nothing bound (queue drained, or
    /// only conflict-held items remain — a hold is surfaced separately by #376, not as a cap).
    pub bind: Option<Bind>,
}

impl Plan {
    /// Whether the binding cap is a resource/budget **pause** (as opposed to plain local-cap
    /// fullness or nothing bound). Drives the `orchestrate_paused` event.
    pub fn paused(&self) -> bool {
        self.bind.is_some_and(Bind::is_pause)
    }
}

/// Decide this pass's dispatch plan (#377) — the pure elastic-concurrency + remote fan-out policy.
///
/// Walks the queued items in their leased priority order and, composing with the #376
/// conflict-aware holding:
///   - [`admit_remote`] selects the local or remote path for every substrate from the same captured
///     gate state;
///   - an item on the local path fills a slot when [`admit`] allows **and** it doesn't conflict with
///     an already-in-flight-or-just-picked touch-set; otherwise it stays pending.
///
/// A conflict-held item is never fanned out: it serializes locally behind its holder (#376),
/// matching the land-time safety net. Pure over `st` / `caps` / `obs` — no IO, no clock.
///
/// #385: admission is **per typed lane**. Each `local`/`auto` item is admitted against its own
/// work-class cap ([`WorkClass`]/[`Caps::cap_for`]) counting only that lane's in-flight agents, so a
/// full coding lane never blocks a research candidate (and vice-versa) — `--coding 5 --research 2`
/// runs both lanes concurrently. An untyped run (`caps.research == None`) counts every lane against
/// the one shared `max_local` pool, exactly as before.
///
/// #378: an untyped run additionally **reserves one shared-pool slot** for a queued scout/ask
/// (research-class) item — the ad-hoc priority lane. While such an item waits, the coding lane is
/// held back by one slot so the scout can dispatch on the next tick without preempting a mid-flight
/// ship (reserve, never preempt); the reservation is exactly one slot and lifts the moment the
/// scout is picked, so ships never starve. A typed run reserves nothing — research has its own lane.
pub fn pick(st: &OrchestrateState, caps: &Caps, obs: &Observed) -> Plan {
    let mut plan = Plan::default();
    // Seed the in-flight touch-sets from live in-flight items (local + remote), then grow it as we
    // pick locals this pass, so two just-picked overlapping items don't co-schedule (#376).
    let mut inflight_touches: Vec<Vec<String>> = st
        .queue
        .iter()
        .filter(|i| i.occupies_dispatch_slot())
        .map(|i| i.touches.clone())
        .collect();
    // #385: track per-lane local in-flight. In an untyped run the two lanes share one pool, so the
    // count read for admission is their sum against `max_local`; in a typed run each candidate is
    // admitted against only its own lane's occupancy.
    let mut coding = st.local_node_in_flight_class(WorkClass::Coding);
    let mut research = st.local_node_in_flight_class(WorkClass::Research);
    let typed = caps.typed();
    // #378: the ad-hoc priority lane. In an *untyped* run (research shares the coding pool) a queued
    // scout/ask (research-class) item RESERVES one shared-pool slot, so it can dispatch on the next
    // tick without preempting a mid-flight ship — reserve, never preempt. `reserve_active` means we
    // still owe that one slot to a *waiting* scout: it starts set whenever a research item is queued
    // and clears the moment one research item is placed this pass, so the reservation is EXACTLY one
    // slot (a second queued scout competes for the pool normally) and lifts entirely once the ask
    // has its slot — ships then use every slot. When no scout is queued nothing is reserved. A typed
    // run reserves nothing here: research already has its own cap (`--research M`).
    let mut reserve_active = !typed
        && st
            .queue
            .iter()
            .any(|i| i.is_dispatchable() && i.work_class == WorkClass::Research);
    let mut planned_workers: HashMap<String, usize> = st
        .queue
        .iter()
        .filter(|item| item.occupies_dispatch_slot())
        .filter_map(|item| item.node.as_ref())
        .fold(HashMap::new(), |mut counts, node_id| {
            let stable_id = st
                .nodes
                .iter()
                .find(|node| {
                    node.node_id == *node_id
                        || node.target == ExecutionTarget::SshHost(node_id.clone())
                })
                .map(|node| node.node_id.clone())
                .unwrap_or_else(|| node_id.clone());
            *counts.entry(stable_id).or_default() += 1;
            counts
        });

    for (idx, item) in st.queue.iter().enumerate() {
        if !item.is_dispatchable() {
            continue;
        }
        // Is a slot in this item's lane admissible right now?
        let class = item.work_class;
        let in_flight_for_class = if typed {
            match class {
                WorkClass::Coding => coding,
                WorkClass::Research => research,
            }
        } else {
            coding + research
        };
        // #378: while a scout/ask is still waiting for its reserved slot, a coding candidate counts
        // that slot as if the (shared) pool were one fuller — so a ship can't consume the slot the
        // ask needs (reserve, never preempt). Research candidates ignore it (they *take* the reserved
        // slot), and a typed run reserves nothing.
        let reserve = usize::from(reserve_active && !typed && class == WorkClass::Coding);
        let bind = admit(caps, obs, class, in_flight_for_class + reserve);
        let held = !item.touches.is_empty() && conflicts(&item.touches, &inflight_touches);
        let remote = admit_remote(
            item.substrate,
            RemoteGateState {
                bind,
                held,
                overflow_enabled: caps.remote_overflow,
                plan_gated: caps.plan_gated,
                issue_linked: is_issue_linked(&item.id),
            },
        );
        if item.substrate == Substrate::Remote {
            if !item.required_capabilities.is_empty() || !item.excluded_capabilities.is_empty() {
                plan.unplaced.push((
                    idx,
                    "github-runner advertises no capabilities for this repo policy".to_string(),
                ));
                continue;
            }
            plan.remote.push(idx);
            // A remote PR still touches files, so feed its touches into the overlap prediction:
            // a later local candidate that would collide with it at land time is held (#376),
            // rather than co-scheduled into a wasted rebase.
            inflight_touches.push(item.touches.clone());
            continue;
        }
        if !held {
            // A slot in its lane is free and it doesn't conflict. Now the placement axis (#717 +
            // #729): resolve *which node* it lands on, applying the hard capability filter (#728)
            // and first-fit selection among eligible READY nodes. If no node is eligible the item
            // stays pending with a recorded reason (never silently downgraded to local, no slot
            // consumed) — the fail-loud contract.
            let target = match place_with_load(item, &st.nodes, caps, bind, &planned_workers) {
                Placement::Place(t) => t,
                Placement::Unplaced(reason) => {
                    if let RemoteAdmission::Remote(remote_bind) = remote {
                        if !item.required_capabilities.is_empty()
                            || !item.excluded_capabilities.is_empty()
                        {
                            plan.unplaced.push((
                                idx,
                                "github-runner advertises no capabilities for this repo policy"
                                    .to_string(),
                            ));
                            continue;
                        }
                        if let Some(bind) = remote_bind {
                            plan.bind.get_or_insert(bind);
                        }
                        plan.remote.push(idx);
                        inflight_touches.push(item.touches.clone());
                        continue;
                    }
                    plan.unplaced.push((idx, reason));
                    if let Some(b) = bind {
                        plan.bind.get_or_insert(b);
                    }
                    continue;
                }
            };
            plan.local.push(idx);
            plan.placements.push((idx, target.clone()));
            inflight_touches.push(item.touches.clone());
            match target {
                ExecutionTarget::SshHost(alias) => {
                    if let Some(node) = st.nodes.iter().find(|node| {
                        node.node_id == alias
                            || node.target == ExecutionTarget::SshHost(alias.clone())
                    }) {
                        *planned_workers.entry(node.node_id.clone()).or_default() += 1;
                    }
                    if class == WorkClass::Research {
                        reserve_active = false;
                    }
                }
                _ => match class {
                    WorkClass::Coding => coding += 1,
                    WorkClass::Research => {
                        research += 1;
                        // A scout took a slot — the one reserved priority slot is now satisfied, so the
                        // coding lane gets the full pool back for the rest of this pass.
                        reserve_active = false;
                    }
                },
            }
            continue;
        }
        // Local can't take it now. Record the first binding cap (the pause reason for status).
        if let Some(b) = bind {
            plan.bind.get_or_insert(b);
        }
        // Stays pending (conflict-held, or not remote-eligible) — the binding cap is shown.
    }
    plan
}
