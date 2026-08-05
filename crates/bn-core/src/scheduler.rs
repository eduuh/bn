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
use crate::hosts::{NodeRecord, NodeStatus};
#[cfg(test)]
use crate::orchestrate::ItemStatus;
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

/// Whether `node` advertises every required capability and none of the excluded capabilities
/// (#728). Labels are matched case-insensitively so config spelling cannot weaken a hard policy.
fn node_satisfies(node: &NodeRecord, required: &[String], excluded: &[String]) -> bool {
    if (!required.is_empty() || !excluded.is_empty())
        && node.target == ExecutionTarget::GitHubRunner
    {
        return false;
    }
    required.iter().all(|cap| {
        node.labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(cap))
    }) && excluded.iter().all(|cap| {
        !node
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(cap))
    })
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

fn node_owns_repo(node: &NodeRecord, repo: &str) -> bool {
    repo_owned(&node.owned_repos, repo)
}

fn node_resources_allow(node: &NodeRecord, caps: &Caps) -> bool {
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
        && node.active_attempts == 0
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

fn missing_required(node: &NodeRecord, required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|cap| {
            !node
                .labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case(cap))
        })
        .cloned()
        .collect()
}

fn matching_excluded(node: &NodeRecord, excluded: &[String]) -> Vec<String> {
    excluded
        .iter()
        .filter(|cap| {
            node.labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case(cap))
        })
        .cloned()
        .collect()
}

fn no_capable_node_reason(required: &[String], excluded: &[String]) -> String {
    match (required.is_empty(), excluded.is_empty()) {
        (false, true) => format!(
            "no READY node satisfies required capability: {}",
            required.join(", ")
        ),
        (true, false) => format!(
            "no READY node avoids excluded capability: {}",
            excluded.join(", ")
        ),
        (false, false) => format!(
            "no READY node satisfies required capability: {} while avoiding: {}",
            required.join(", "),
            excluded.join(", ")
        ),
        (true, true) => "no READY node available".to_string(),
    }
}

/// The outcome of resolving where a local/auto item lands (#729).
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// Dispatch the item on this concrete execution target.
    Place(ExecutionTarget),
    /// No eligible node this pass — the item stays pending (no slot consumed). The string is the
    /// operator-facing reason surfaced on the status line (fail-loud, per #728).
    Unplaced(String),
}

/// Resolve which node a local/auto `item` lands on (#729), applying the hard capability filter
/// (#728) then first-fit selection among eligible READY nodes. Pure over the item + the run's node
/// registry, so the whole placement policy is unit-testable:
///
///   - **Empty registry** (pre-#729 / no `[nodes]` configured) ⇒ `Place(Local)` for an unconstrained
///     repo; a repo with required/excluded capabilities stays `Unplaced` because the implicit box
///     advertises no capabilities (fail-closed).
///   - **Explicit [`ExecutionTarget::SshHost`]** ⇒ eligible only when the registry holds that node
///     [`NodeStatus::Ready`] *and* it satisfies the repo's required capabilities; else `Unplaced`
///     with the reason (a not-Ready or capability-missing host holds the item — never downgraded).
///   - **[`ExecutionTarget::GitHubRunner`]** ⇒ `Place` only for unconstrained repos; a constrained
///     repo stays `Unplaced` because the runner advertises no capability tags.
///   - **[`ExecutionTarget::Local`]/auto with a non-empty registry** ⇒ first-fit: the first READY
///     node (registry order, sorted by `node_id` for determinism) that satisfies the repo's
///     required capabilities; its own `target` is returned. If none matches, a **cap-free** repo
///     still falls back to the implicit local box (`Place(Local)` — the pre-#729 invariant that a
///     `Local` item never waits on the registry), while a repo *with* required capabilities stays
///     `Unplaced` with a reason (e.g. `no READY node satisfies capability: mt5-bridge`), since the
///     unlabelled implicit box can't satisfy them.
pub fn place(item: &QueueItem, nodes: &[NodeRecord], caps: &Caps) -> Placement {
    place_with_load(item, nodes, caps, None, &HashMap::new())
}

fn place_with_load(
    item: &QueueItem,
    nodes: &[NodeRecord],
    caps: &Caps,
    local_bind: Option<Bind>,
    planned_workers: &HashMap<String, usize>,
) -> Placement {
    let required = &item.required_capabilities;
    let excluded = &item.excluded_capabilities;
    match &item.target {
        ExecutionTarget::GitHubRunner if required.is_empty() && excluded.is_empty() => {
            Placement::Place(ExecutionTarget::GitHubRunner)
        }
        ExecutionTarget::GitHubRunner => Placement::Unplaced(
            "github-runner advertises no capabilities for this repo policy".to_string(),
        ),
        ExecutionTarget::SshHost(alias) => {
            if !bind_allows_worker_handoff(local_bind) {
                return Placement::Unplaced(
                    local_bind
                        .map(|bind| bind.reason().to_string())
                        .unwrap_or_else(|| "worker handoff is blocked".to_string()),
                );
            }
            // Resolve the named ssh alias to a node by either its stable id (the #717 convention,
            // where node_id == alias) or its declared target (#729, where a node like `fedora-box`
            // is reached via `ssh:fedora`), so both naming styles address the same worker.
            let want = ExecutionTarget::SshHost(alias.clone());
            match nodes
                .iter()
                .find(|n| &n.node_id == alias || n.target == want)
            {
                Some(n) if n.status != NodeStatus::Ready => {
                    Placement::Unplaced(format!("node '{alias}' is {:?}, not Ready", n.status))
                }
                Some(n) if !node_owns_repo(n, &item.repo) => {
                    Placement::Unplaced(format!("node '{alias}' does not own repo '{}'", item.repo))
                }
                Some(n) if !missing_required(n, required).is_empty() => {
                    Placement::Unplaced(format!(
                        "node '{alias}' lacks required capability: {}",
                        missing_required(n, required).join(", ")
                    ))
                }
                Some(n) if !matching_excluded(n, excluded).is_empty() => {
                    Placement::Unplaced(format!(
                        "node '{alias}' has excluded capability: {}",
                        matching_excluded(n, excluded).join(", ")
                    ))
                }
                Some(n) if !node_resources_allow(n, caps) => {
                    Placement::Unplaced(format!("node '{alias}' is blocked by resource guards"))
                }
                Some(n)
                    if n.active_attempts
                        + planned_workers.get(&n.node_id).copied().unwrap_or_default()
                        > 0 =>
                {
                    Placement::Unplaced(format!("node '{alias}' has no idle worker slot"))
                }
                Some(n) => Placement::Place(if n.target.is_local() {
                    want
                } else {
                    n.target.clone()
                }),
                None => Placement::Unplaced(format!("no node '{alias}' in registry")),
            }
        }
        ExecutionTarget::Local => {
            if nodes.is_empty() {
                return if caps.local_owned
                    && local_bind.is_none()
                    && required.is_empty()
                    && excluded.is_empty()
                {
                    Placement::Place(ExecutionTarget::Local)
                } else {
                    Placement::Unplaced(no_capable_node_reason(required, excluded))
                };
            }
            let mut candidates: Vec<&NodeRecord> = nodes
                .iter()
                .filter(|n| {
                    (n.target.is_local()
                        && n.status == NodeStatus::Ready
                        && node_satisfies(n, required, excluded)
                        && node_owns_repo(n, &item.repo)
                        && node_resources_allow(n, caps)
                        || bind_allows_worker_handoff(local_bind) && worker_eligible(n, item, caps))
                        && (n.target.is_local()
                            || planned_workers.get(&n.node_id).copied().unwrap_or_default() == 0)
                        && match (&n.target, local_bind) {
                            (ExecutionTarget::Local, None) => caps.local_owned,
                            (ExecutionTarget::Local, Some(_)) => false,
                            _ => true,
                        }
                })
                .collect();
            candidates.sort_by_key(|n| {
                (
                    usize::from(!n.target.is_local()),
                    n.active_attempts
                        + planned_workers.get(&n.node_id).copied().unwrap_or_default(),
                    n.node_id.as_str(),
                )
            });
            match candidates.first() {
                Some(n) => Placement::Place(n.target.clone()),
                // No registered node matched. For a cap-free repo the daemon's own box is always an
                // implicit, always-available fallback (the pre-#729 invariant a `Local` item never
                // waits for the registry). For a repo with required capabilities the implicit box
                // carries no labels, so it can't satisfy them — the item stays pending with a
                // reason rather than dispatching to a doomed node (fail-loud, #728).
                None if caps.local_owned
                    && local_bind.is_none()
                    && required.is_empty()
                    && excluded.is_empty() =>
                {
                    Placement::Place(ExecutionTarget::Local)
                }
                None => Placement::Unplaced(no_capable_node_reason(required, excluded)),
            }
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::OrchestrateState;

    /// Caps with a `max_local` coding cap, no research lane, and every guard off — the baseline
    /// (behaves as the pre-#377 fixed-concurrency / pre-#385 single-pool daemon).
    fn caps(max_local: usize) -> Caps {
        Caps {
            max_local,
            research: None,
            min_free_ram_mb: None,
            max_loadavg: None,
            max_swap_pct: None,
            tokens_per_hour: None,
            daily_cost_usd: None,
            daily_cost_usd_coding: None,
            daily_cost_usd_research: None,
            plan_gated: false,
            remote_overflow: false,
            local_owned: true,
        }
    }

    fn tv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Build a run state from `(id, status, touches, substrate)` rows, with `max_local` cap. All
    /// items default to the coding lane ([`WorkClass::Coding`]) — the untyped baseline.
    fn state(
        rows: &[(&str, ItemStatus, &[&str], Substrate)],
        max_local: usize,
    ) -> OrchestrateState {
        let ids: Vec<String> = rows.iter().map(|(id, ..)| id.to_string()).collect();
        let mut s = OrchestrateState::new("bn", &ids, max_local, false, 0);
        for (slot, (_, status, touches, substrate)) in s.queue.iter_mut().zip(rows) {
            slot.status = *status;
            slot.touches = tv(touches);
            slot.substrate = *substrate;
        }
        s
    }

    /// Build a run from `(id, status, substrate, work_class)` rows — the #385 typed-lane helper, so
    /// a test can mix coding and research items. `coding`/`research` set the two lane caps.
    fn lane_state(
        rows: &[(&str, ItemStatus, Substrate, WorkClass)],
        coding: usize,
        research: Option<usize>,
    ) -> OrchestrateState {
        let ids: Vec<String> = rows.iter().map(|(id, ..)| id.to_string()).collect();
        let mut s = OrchestrateState::new("bn", &ids, coding, false, 0);
        s.research = research;
        for (slot, (_, status, substrate, work_class)) in s.queue.iter_mut().zip(rows) {
            slot.status = *status;
            slot.substrate = *substrate;
            slot.work_class = *work_class;
        }
        s
    }

    /// Typed [`Caps`] for the #385 lane tests: coding cap + research cap, guards off.
    fn lane_caps(coding: usize, research: usize) -> Caps {
        Caps {
            research: Some(research),
            ..caps(coding)
        }
    }

    // ---- admit: cap arithmetic + guard priority ----

    #[test]
    fn admit_open_when_below_cap_and_no_guards() {
        assert_eq!(
            admit(&caps(4), &Observed::default(), WorkClass::Coding, 0),
            None
        );
        assert_eq!(
            admit(&caps(4), &Observed::default(), WorkClass::Coding, 3),
            None
        );
    }

    #[test]
    fn admit_binds_local_cap_at_ceiling() {
        assert_eq!(
            admit(&caps(2), &Observed::default(), WorkClass::Coding, 2),
            Some(Bind::LocalCap)
        );
        assert_eq!(
            admit(&caps(2), &Observed::default(), WorkClass::Coding, 5),
            Some(Bind::LocalCap)
        );
    }

    #[test]
    fn admit_ram_guard_pauses_below_min() {
        let c = Caps {
            min_free_ram_mb: Some(2000),
            ..caps(10)
        };
        let low = Observed {
            free_ram_mb: Some(1500),
            ..Default::default()
        };
        assert_eq!(admit(&c, &low, WorkClass::Coding, 0), Some(Bind::Ram));
        // At/above the floor it's fine.
        let ok = Observed {
            free_ram_mb: Some(2000),
            ..Default::default()
        };
        assert_eq!(admit(&c, &ok, WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_ram_guard_fails_open_when_unreadable() {
        let c = Caps {
            min_free_ram_mb: Some(2000),
            ..caps(10)
        };
        // free_ram_mb == None (couldn't read) must not block dispatch.
        assert_eq!(admit(&c, &Observed::default(), WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_swap_guard_pauses_above_max() {
        let c = Caps {
            max_swap_pct: Some(50.0),
            ..caps(10)
        };
        let hot = Observed {
            swap_pct: Some(62.0),
            ..Default::default()
        };
        assert_eq!(admit(&c, &hot, WorkClass::Coding, 0), Some(Bind::Swap));
        // At/below the ceiling it's fine.
        let calm = Observed {
            swap_pct: Some(50.0),
            ..Default::default()
        };
        assert_eq!(admit(&c, &calm, WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_swap_guard_fails_open_when_unreadable() {
        let c = Caps {
            max_swap_pct: Some(50.0),
            ..caps(10)
        };
        // swap_pct == None (no swap / unreadable) must not block dispatch.
        assert_eq!(admit(&c, &Observed::default(), WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_load_guard_pauses_above_max() {
        let c = Caps {
            max_loadavg: Some(8.0),
            ..caps(10)
        };
        let hot = Observed {
            loadavg: Some(9.5),
            ..Default::default()
        };
        assert_eq!(admit(&c, &hot, WorkClass::Coding, 0), Some(Bind::Load));
        let calm = Observed {
            loadavg: Some(8.0),
            ..Default::default()
        };
        assert_eq!(admit(&c, &calm, WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_token_budget_binds_before_resources() {
        // Token AND ram both tripped → tokens wins (rate limits bind before CPU).
        let c = Caps {
            tokens_per_hour: Some(1_000),
            min_free_ram_mb: Some(2000),
            ..caps(10)
        };
        let obs = Observed {
            tokens_this_hour: 1_000,
            free_ram_mb: Some(100),
            ..Default::default()
        };
        assert_eq!(admit(&c, &obs, WorkClass::Coding, 0), Some(Bind::Tokens));
    }

    #[test]
    fn admit_cost_budget_pauses_at_cap() {
        let c = Caps {
            daily_cost_usd: Some(25.0),
            ..caps(10)
        };
        let spent = Observed {
            cost_today_usd: 25.0,
            ..Default::default()
        };
        assert_eq!(admit(&c, &spent, WorkClass::Coding, 0), Some(Bind::Cost));
        let under = Observed {
            cost_today_usd: 24.99,
            ..Default::default()
        };
        assert_eq!(admit(&c, &under, WorkClass::Coding, 0), None);
    }

    #[test]
    fn admit_per_class_cost_budget_pauses_only_its_lane() {
        // A typed run with a research-lane cost cap; the coding lane is uncapped. The research
        // lane has overspent while coding hasn't — research pauses, coding keeps dispatching.
        let c = Caps {
            research: Some(2),
            daily_cost_usd_research: Some(5.0),
            ..caps(5)
        };
        let obs = Observed {
            cost_today_research_usd: 6.0,
            cost_today_coding_usd: 100.0, // coding has no cap → its spend is irrelevant
            ..Default::default()
        };
        assert_eq!(
            admit(&c, &obs, WorkClass::Research, 0),
            Some(Bind::Cost),
            "research lane over its own cost cap → paused"
        );
        assert_eq!(
            admit(&c, &obs, WorkClass::Coding, 0),
            None,
            "coding lane has no cost cap → the research cap does not pause it"
        );
    }

    #[test]
    fn admit_per_class_cost_budget_under_cap_admits() {
        // Coding lane capped and under budget → admitted; the machine-wide cap stays off.
        let c = Caps {
            daily_cost_usd_coding: Some(30.0),
            ..caps(5)
        };
        let under = Observed {
            cost_today_coding_usd: 29.99,
            ..Default::default()
        };
        assert_eq!(admit(&c, &under, WorkClass::Coding, 0), None);
        let at = Observed {
            cost_today_coding_usd: 30.0,
            ..Default::default()
        };
        assert_eq!(admit(&c, &at, WorkClass::Coding, 0), Some(Bind::Cost));
    }

    #[test]
    fn pick_fills_up_to_max_local() {
        let s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
                ("3", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            2,
        );
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(p.local, vec![0, 1]);
        assert!(p.remote.is_empty());
        // The third couldn't be placed → local-cap bound.
        assert_eq!(p.bind, Some(Bind::LocalCap));
        assert!(!p.paused(), "local-cap fullness is not a pause");
    }

    #[test]
    fn pick_counts_existing_local_inflight_against_cap() {
        let s = state(
            &[
                ("1", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            2,
        );
        // One local slot already used → exactly one more fits.
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(p.local, vec![1]);
        assert_eq!(p.bind, None);
    }

    #[test]
    fn pick_pauses_on_resource_guard_no_local_dispatch() {
        let s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            10,
        );
        let c = Caps {
            min_free_ram_mb: Some(4000),
            ..caps(10)
        };
        let low = Observed {
            free_ram_mb: Some(1000),
            ..Default::default()
        };
        let p = pick(&s, &c, &low);
        assert!(p.local.is_empty(), "RAM pause dispatches nothing locally");
        assert_eq!(p.bind, Some(Bind::Ram));
        assert!(p.paused());
    }

    #[test]
    fn pick_resumes_when_pressure_clears() {
        let s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            10,
        );
        let c = Caps {
            min_free_ram_mb: Some(4000),
            ..caps(10)
        };
        // RAM recovered above the floor → both dispatch, nothing bound.
        let ok = Observed {
            free_ram_mb: Some(8000),
            ..Default::default()
        };
        let p = pick(&s, &c, &ok);
        assert_eq!(p.local, vec![0, 1]);
        assert_eq!(p.bind, None);
    }

    // ---- pick: composition with #376 conflict holding ----

    #[test]
    fn pick_holds_conflicting_candidate_but_fills_disjoint_one() {
        let s = state(
            &[
                (
                    "100",
                    ItemStatus::Dispatched,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Auto,
                ),
                (
                    "200",
                    ItemStatus::Queued,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Auto,
                ),
                (
                    "300",
                    ItemStatus::Queued,
                    &["crates/bn-core/src/staff.rs"],
                    Substrate::Auto,
                ),
            ],
            10,
        );
        let p = pick(&s, &caps(10), &Observed::default());
        // 200 conflicts with the in-flight 100 → held; 300 is disjoint → dispatched.
        assert_eq!(p.local, vec![2]);
        assert!(p.remote.is_empty(), "a conflict hold never fans out");
        // Held-by-conflict is not a cap bind.
        assert_eq!(p.bind, None);
    }

    #[test]
    fn pick_serializes_two_just_picked_overlapping_items() {
        let s = state(
            &[
                (
                    "1",
                    ItemStatus::Queued,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Auto,
                ),
                (
                    "2",
                    ItemStatus::Queued,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Auto,
                ),
            ],
            10,
        );
        // Both want cli.rs: only the first is picked this pass; the second holds behind it.
        let p = pick(&s, &caps(10), &Observed::default());
        assert_eq!(p.local, vec![0]);
        assert!(p.remote.is_empty());
    }

    // ---- pick: remote substrate + fan-out ----

    #[test]
    fn pick_always_fans_out_explicit_remote_no_local_slot() {
        let s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Remote),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            2,
        );
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(p.remote, vec![0], "explicit remote item fans out");
        assert_eq!(
            p.local,
            vec![1],
            "and the auto item still takes a local slot"
        );
    }

    #[test]
    fn pick_overflows_eligible_auto_to_remote_when_local_full() {
        let s = state(
            &[
                ("100", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("338", ItemStatus::Queued, &[], Substrate::Auto),
                ("342", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        let c = Caps {
            remote_overflow: true,
            ..caps(1)
        };
        // Local cap is 1 and already full → both queued issue-linked auto items overflow to remote.
        let p = pick(&s, &c, &Observed::default());
        assert!(p.local.is_empty());
        assert_eq!(p.remote, vec![1, 2]);
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_does_not_overflow_without_opt_in() {
        let s = state(
            &[
                ("100", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("338", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        // remote_overflow off (default) → the eligible auto item waits locally, never fans out.
        let p = pick(&s, &caps(1), &Observed::default());
        assert!(p.local.is_empty());
        assert!(p.remote.is_empty());
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_does_not_overflow_non_issue_linked_auto() {
        let s = state(
            &[
                ("100", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("add-a-feature", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        let c = Caps {
            remote_overflow: true,
            ..caps(1)
        };
        // A free-text (slug id) auto item is not remote-eligible → it waits locally, never fans out.
        let p = pick(&s, &c, &Observed::default());
        assert!(p.local.is_empty());
        assert!(p.remote.is_empty());
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_does_not_overflow_when_plan_gated() {
        let s = state(
            &[
                ("100", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("338", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        let c = Caps {
            plan_gated: true,
            remote_overflow: true,
            ..caps(1)
        };
        // Plan-gated auto items can't do the remote handshake → held local, no fan-out.
        let p = pick(&s, &c, &Observed::default());
        assert!(p.remote.is_empty());
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_explicit_remote_bypasses_plan_gate() {
        let s = state(
            &[("free-text", ItemStatus::Queued, &[], Substrate::Remote)],
            2,
        );
        let c = Caps {
            plan_gated: true,
            ..caps(2)
        };
        // Explicit remote is honored even for a non-issue-linked, plan-gated run — user opted in.
        let p = pick(&s, &c, &Observed::default());
        assert_eq!(p.remote, vec![0]);
    }

    #[test]
    fn pick_overflows_to_remote_when_budget_paused() {
        let s = state(
            &[
                ("338", ItemStatus::Queued, &[], Substrate::Auto),
                ("342", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            10,
        );
        let c = Caps {
            tokens_per_hour: Some(1_000),
            remote_overflow: true,
            ..caps(10)
        };
        let spent = Observed {
            tokens_this_hour: 5_000,
            ..Default::default()
        };
        // Local is token-paused, but the eligible auto items still make progress via remote.
        let p = pick(&s, &c, &spent);
        assert!(p.local.is_empty());
        assert_eq!(p.remote, vec![0, 1]);
        assert_eq!(p.bind, Some(Bind::Tokens));
        assert!(p.paused());
    }

    #[test]
    fn admit_remote_unifies_explicit_and_auto_remote_paths() {
        let blocked = RemoteGateState {
            bind: Some(Bind::LocalCap),
            held: false,
            overflow_enabled: true,
            plan_gated: false,
            issue_linked: true,
        };
        assert_eq!(
            admit_remote(Substrate::Remote, blocked),
            RemoteAdmission::Remote(None),
            "explicit remote always fans out"
        );
        assert_eq!(
            admit_remote(Substrate::Auto, blocked),
            RemoteAdmission::Remote(Some(Bind::LocalCap)),
            "eligible auto overflows through the same admission path"
        );
        assert_eq!(
            admit_remote(Substrate::Local, blocked),
            RemoteAdmission::Local,
            "explicit local never fans out"
        );
    }

    #[test]
    fn admit_remote_keeps_ineligible_auto_on_local_path() {
        let open = RemoteGateState {
            bind: None,
            held: false,
            overflow_enabled: true,
            plan_gated: false,
            issue_linked: true,
        };
        assert_eq!(
            admit_remote(Substrate::Auto, open),
            RemoteAdmission::Local,
            "auto uses an available local slot"
        );

        let blocked = RemoteGateState {
            bind: Some(Bind::LocalCap),
            ..open
        };
        for blocked in [
            RemoteGateState {
                overflow_enabled: false,
                ..blocked
            },
            RemoteGateState {
                held: true,
                ..blocked
            },
            RemoteGateState {
                plan_gated: true,
                ..blocked
            },
            RemoteGateState {
                issue_linked: false,
                ..blocked
            },
        ] {
            assert_eq!(
                admit_remote(Substrate::Auto, blocked),
                RemoteAdmission::Local
            );
        }
    }

    #[test]
    fn pick_holds_local_candidate_conflicting_with_a_planned_remote() {
        // A remote item and a local candidate both touch cli.rs: the remote fans out, and the
        // conflicting local candidate is held (serialized) rather than co-scheduled into a rebase.
        let s = state(
            &[
                (
                    "501",
                    ItemStatus::Queued,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Remote,
                ),
                (
                    "502",
                    ItemStatus::Queued,
                    &["crates/bn-cli/src/cli.rs"],
                    Substrate::Auto,
                ),
                (
                    "503",
                    ItemStatus::Queued,
                    &["crates/bn-core/src/staff.rs"],
                    Substrate::Auto,
                ),
            ],
            10,
        );
        let p = pick(&s, &caps(10), &Observed::default());
        assert_eq!(p.remote, vec![0]);
        // 502 conflicts with the planned-remote 501 → held; the disjoint 503 dispatches locally.
        assert_eq!(p.local, vec![2]);
    }

    #[test]
    fn bind_label_round_trips() {
        for b in [
            Bind::LocalCap,
            Bind::Ram,
            Bind::Load,
            Bind::Tokens,
            Bind::Cost,
        ] {
            assert_eq!(Bind::from_label(b.label()), Some(b));
        }
        assert_eq!(Bind::from_label("nonsense"), None);
        // Only the local cap is plain fullness; every other bind is a pause.
        assert!(!Bind::LocalCap.is_pause());
        assert!(Bind::Ram.is_pause() && Bind::Tokens.is_pause());
    }

    #[test]
    fn no_caps_behaves_as_fixed_concurrency() {
        // The regression guard: with every guard off, pick is just "fill to max_local".
        let s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
                ("3", ItemStatus::Queued, &[], Substrate::Auto),
                ("4", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            3,
        );
        let p = pick(&s, &caps(3), &Observed::default());
        assert_eq!(p.local, vec![0, 1, 2]);
        assert!(
            p.remote.is_empty(),
            "no fan-out without a substrate/overflow reason beyond cap"
        );
    }

    // ---- #385: typed concurrency lanes (coding vs research) ----

    #[test]
    fn cap_for_maps_each_lane_to_its_cap() {
        // Typed: each lane resolves to its own cap.
        let c = lane_caps(5, 2);
        assert!(c.typed());
        assert_eq!(c.cap_for(WorkClass::Coding), 5);
        assert_eq!(c.cap_for(WorkClass::Research), 2);
        // Untyped: research falls back to the shared coding pool.
        let u = caps(4);
        assert!(!u.typed());
        assert_eq!(u.cap_for(WorkClass::Coding), 4);
        assert_eq!(u.cap_for(WorkClass::Research), 4);
    }

    #[test]
    fn admit_is_per_lane_when_typed() {
        let c = lane_caps(5, 2);
        // The coding lane is full at 5, but the research lane still has room at 1/2.
        assert_eq!(
            admit(&c, &Observed::default(), WorkClass::Coding, 5),
            Some(Bind::LocalCap)
        );
        assert_eq!(
            admit(&c, &Observed::default(), WorkClass::Research, 1),
            None
        );
        // And the research lane binds at its own (smaller) cap independent of coding.
        assert_eq!(
            admit(&c, &Observed::default(), WorkClass::Research, 2),
            Some(Bind::LocalCap)
        );
    }

    #[test]
    fn admit_global_guards_pause_every_lane() {
        // A machine/budget guard binds regardless of which lane the candidate is in (#377 semantics
        // preserved): RAM pressure pauses coding *and* research.
        let c = Caps {
            min_free_ram_mb: Some(4000),
            ..lane_caps(5, 2)
        };
        let low = Observed {
            free_ram_mb: Some(1000),
            ..Default::default()
        };
        assert_eq!(admit(&c, &low, WorkClass::Coding, 0), Some(Bind::Ram));
        assert_eq!(admit(&c, &low, WorkClass::Research, 0), Some(Bind::Ram));
    }

    #[test]
    fn pick_fills_coding_and_research_lanes_independently() {
        // 3 coding + 3 research queued, caps 2 coding / 1 research → 2 coding + 1 research placed,
        // the rest held. The two lanes cap independently ("5 coding + 2 research"-style).
        let s = lane_state(
            &[
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                ("c2", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                ("c3", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                (
                    "r2",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                (
                    "r3",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            2,
            Some(1),
        );
        let p = pick(&s, &lane_caps(2, 1), &Observed::default());
        // c1,c2 (coding cap 2) and r1 (research cap 1).
        assert_eq!(p.local, vec![0, 1, 3]);
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_full_coding_lane_does_not_block_research() {
        // The coding lane is already full (2 dispatched vs cap 2), yet a queued research item still
        // dispatches on its own lane — a full ship lane never starves scouts.
        let s = lane_state(
            &[
                (
                    "c1",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Coding,
                ),
                (
                    "c2",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Coding,
                ),
                ("c3", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            2,
            Some(2),
        );
        let p = pick(&s, &lane_caps(2, 2), &Observed::default());
        // c3 is held (coding full), r1 dispatches (research lane has room).
        assert_eq!(p.local, vec![3]);
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_counts_existing_per_lane_inflight_against_that_lane() {
        // One research item already in flight (research cap 2 → one slot left); the coding lane is
        // untouched by it and fills to its own cap of 2.
        let s = lane_state(
            &[
                (
                    "r0",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                ("c2", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                (
                    "r2",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            2,
            Some(2),
        );
        let p = pick(&s, &lane_caps(2, 2), &Observed::default());
        // c1,c2 fill coding (0→2); r1 fills the one remaining research slot (1→2); r2 is held.
        assert_eq!(p.local, vec![1, 2, 3]);
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn untyped_run_shares_one_pool_across_lanes() {
        // Backward compat: no research lane (research = None) → coding and research items share the
        // single `max_local` pool. 2 coding + 2 research, cap 3 → exactly 3 placed total.
        let s = lane_state(
            &[
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                ("c2", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r2",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            3,
            None,
        );
        let p = pick(&s, &caps(3), &Observed::default());
        assert_eq!(
            p.local,
            vec![0, 1, 2],
            "one shared pool of 3, in queue order"
        );
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_reserves_the_last_slot_for_a_queued_scout_over_a_ship() {
        // #378: a ship holds all-but-one slot; a queued ship *earlier* in the queue and a queued
        // scout/ask both want the last slot. The reservation gives it to the ask (reserve, never
        // preempt), so the ship is held even though it comes first.
        let s = lane_state(
            &[
                (
                    "c0",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Coding,
                ),
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "ask",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            2,
            None,
        );
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(
            p.local,
            vec![2],
            "the ask wins the reserved slot; the ship waits"
        );
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_ask_never_preempts_a_running_ship_then_dispatches_when_it_frees() {
        // At cap 1 a ship holds the only slot: the ask waits (reserve, never preempt) — nothing is
        // dispatched this pass.
        let running = lane_state(
            &[
                (
                    "c0",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Coding,
                ),
                (
                    "ask",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            1,
            None,
        );
        let p = pick(&running, &caps(1), &Observed::default());
        assert!(
            p.local.is_empty(),
            "the ask does not preempt the running ship"
        );
        assert_eq!(p.bind, Some(Bind::LocalCap));

        // The ship lands → the slot frees → the ask dispatches on this next pass.
        let freed = lane_state(
            &[
                ("c0", ItemStatus::Merged, Substrate::Auto, WorkClass::Coding),
                (
                    "ask",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            1,
            None,
        );
        let p2 = pick(&freed, &caps(1), &Observed::default());
        assert_eq!(p2.local, vec![1], "the ask takes the freed slot");
        assert_eq!(p2.bind, None);
    }

    #[test]
    fn pick_reserves_exactly_one_slot_ships_keep_the_rest() {
        // #378: one ship in flight, cap 3 (two free), queued ship + two asks. The reservation
        // secures ONE slot for a scout — the first ask gets in, a ship gets in too (throughput
        // continues), and the second ask waits. Reserve exactly one, never starve ships.
        let s = lane_state(
            &[
                (
                    "c0",
                    ItemStatus::Dispatched,
                    Substrate::Auto,
                    WorkClass::Coding,
                ),
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "ask1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
                (
                    "ask2",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            3,
            None,
        );
        let p = pick(&s, &caps(3), &Observed::default());
        assert_eq!(
            p.local,
            vec![1, 2],
            "the queued ship and the first ask both dispatch; the second ask waits"
        );
        assert_eq!(p.bind, Some(Bind::LocalCap));
    }

    #[test]
    fn pick_reserves_nothing_when_no_scout_is_queued() {
        // No research item queued → nothing reserved, ships fill every slot (pre-#378 behavior).
        let s = lane_state(
            &[
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                ("c2", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
            ],
            2,
            None,
        );
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(
            p.local,
            vec![0, 1],
            "both ships dispatch — no slot held back"
        );
        assert_eq!(p.bind, None);
    }

    #[test]
    fn pick_typed_run_does_not_reserve_a_shared_slot() {
        // #378: the reservation is untyped-only. With a research lane configured, the coding lane
        // fills its own cap regardless of a queued scout (research has its own dedicated slots).
        let s = lane_state(
            &[
                ("c1", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                ("c2", ItemStatus::Queued, Substrate::Auto, WorkClass::Coding),
                (
                    "r1",
                    ItemStatus::Queued,
                    Substrate::Auto,
                    WorkClass::Research,
                ),
            ],
            2,
            Some(1),
        );
        let p = pick(&s, &lane_caps(2, 1), &Observed::default());
        assert_eq!(
            p.local,
            vec![0, 1, 2],
            "coding fills to its cap of 2 (not held back), research fills its lane of 1"
        );
    }

    #[test]
    fn work_class_maps_from_backlog_kind() {
        use crate::backlog::BacklogKind;
        assert_eq!(WorkClass::from_kind(BacklogKind::Ship), WorkClass::Coding);
        assert_eq!(
            WorkClass::from_kind(BacklogKind::Scout),
            WorkClass::Research
        );
        // Default is the coding lane (the `ship` default / pre-#385 behavior).
        assert_eq!(WorkClass::default(), WorkClass::Coding);
    }

    // --- #717 placement axis: ExecutionTarget + generalized pick() ---------------------------

    use crate::hosts::{NodeRecord, NodeStatus};

    /// A registry node stub with the given id + status (capacity fields zeroed — no live probe).
    fn node(id: &str, status: NodeStatus) -> NodeRecord {
        NodeRecord {
            node_id: id.to_string(),
            status,
            ..NodeRecord::default()
        }
    }

    #[test]
    fn target_eligible_gates_on_ready_node() {
        let nodes = vec![
            node("fedora", NodeStatus::Ready),
            node("draining-box", NodeStatus::Draining),
        ];
        // Local + GitHub runner never need a registry node.
        assert!(target_eligible(&ExecutionTarget::Local, &[]));
        assert!(target_eligible(&ExecutionTarget::GitHubRunner, &[]));
        // A chosen SSH host is eligible only when a matching node is Ready.
        assert!(target_eligible(
            &ExecutionTarget::SshHost("fedora".into()),
            &nodes
        ));
        assert!(!target_eligible(
            &ExecutionTarget::SshHost("draining-box".into()),
            &nodes
        ));
        assert!(!target_eligible(
            &ExecutionTarget::SshHost("unknown".into()),
            &nodes
        ));
    }

    #[test]
    fn pick_places_local_item_on_eligible_chosen_host() {
        // One queued local item targeting the SSH host `fedora`, which is Ready in the registry.
        let mut s = state(&[("338", ItemStatus::Queued, &[], Substrate::Auto)], 2);
        s.queue[0].target = ExecutionTarget::SshHost("fedora".into());
        s.nodes = vec![node("fedora", NodeStatus::Ready)];

        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(p.local, vec![0], "the item takes a local-lane slot");
        assert_eq!(
            p.placements,
            vec![(0, ExecutionTarget::SshHost("fedora".into()))],
            "and is placed on the chosen, eligible host"
        );
        assert!(p.bind.is_none());
    }

    #[test]
    fn pick_holds_local_item_when_chosen_host_ineligible() {
        // Same item, but `fedora` is Draining (not Ready) — the hard filter rejects it, so the
        // item is NOT dispatched (it waits for its host, never silently downgraded to local).
        let mut s = state(&[("338", ItemStatus::Queued, &[], Substrate::Auto)], 2);
        s.queue[0].target = ExecutionTarget::SshHost("fedora".into());
        s.nodes = vec![node("fedora", NodeStatus::Draining)];

        let p = pick(&s, &caps(2), &Observed::default());
        assert!(p.local.is_empty(), "an ineligible host holds the item");
        assert!(p.placements.is_empty());

        // With no matching node at all it's likewise held.
        s.nodes.clear();
        let p = pick(&s, &caps(2), &Observed::default());
        assert!(p.local.is_empty());
    }

    #[test]
    fn pick_default_target_is_local_and_needs_no_registry() {
        // A default (Local) item dispatches with an empty node registry — the pre-#717 behavior —
        // and its placement records Local.
        let s = state(&[("338", ItemStatus::Queued, &[], Substrate::Auto)], 2);
        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(p.local, vec![0]);
        assert_eq!(p.placements, vec![(0, ExecutionTarget::Local)]);
    }

    #[test]
    fn pick_holds_chosen_host_but_still_places_a_ready_sibling() {
        // Two queued auto items: #1 explicitly targets a not-yet-Ready host (held), #2 first-fits
        // onto the Ready local node. The held host item must not consume a slot or block the
        // sibling; the held one is recorded unplaced with a reason (fail-loud, no silent downgrade).
        let mut s = state(
            &[
                ("1", ItemStatus::Queued, &[], Substrate::Auto),
                ("2", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            2,
        );
        s.queue[0].target = ExecutionTarget::SshHost("fedora".into());
        s.nodes = vec![
            node("fedora", NodeStatus::Unknown),
            local_node("wsl-box", &[]),
        ];

        let p = pick(&s, &caps(2), &Observed::default());
        assert_eq!(
            p.local,
            vec![1],
            "only the eligible local sibling dispatches"
        );
        assert_eq!(p.placements, vec![(1, ExecutionTarget::Local)]);
        assert_eq!(
            p.unplaced.len(),
            1,
            "the held host item is recorded unplaced"
        );
        assert_eq!(p.unplaced[0].0, 0);
    }

    /// A READY registry node with `target = Local` (the box the daemon runs on, e.g. `wsl-box`) and
    /// the given capability labels — the local half of a multi-node registry.
    fn local_node(id: &str, labels: &[&str]) -> NodeRecord {
        NodeRecord {
            node_id: id.to_string(),
            target: ExecutionTarget::Local,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            status: NodeStatus::Ready,
            ..NodeRecord::default()
        }
    }

    /// A READY registry node reached over SSH (`target = ssh:<alias>`) with the given labels — the
    /// remote worker half of a multi-node registry (e.g. `fedora-box` → `ssh:fedora`).
    fn ssh_node(id: &str, alias: &str, labels: &[&str]) -> NodeRecord {
        NodeRecord {
            node_id: id.to_string(),
            target: ExecutionTarget::SshHost(alias.to_string()),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            status: NodeStatus::Ready,
            ..NodeRecord::default()
        }
    }

    #[test]
    fn place_prefers_owned_local_node_while_local_capacity_is_available() {
        // A linux-safe lane with a two-node registry stays on its owned local box while the local
        // lane is admissible. The remote worker is the overflow/handoff path, not first choice.
        let item = QueueItem {
            repo: "bn".into(),
            target: ExecutionTarget::Local,
            ..QueueItem::default()
        };
        let nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &["os:windows-wsl", "mt5-bridge", "has-node"]),
        ];
        assert_eq!(
            place(&item, &nodes, &caps(2)),
            Placement::Place(ExecutionTarget::Local),
            "owned local capacity wins before remote overflow"
        );
    }

    #[test]
    fn place_never_puts_mt5_on_a_linux_only_node() {
        // mt5-data-api requires mt5-bridge; only the Windows/WSL box advertises it. With ONLY a
        // Linux node available it stays pending with a clear reason — never the doomed clone.
        let item = QueueItem {
            repo: "mt5-data-api".into(),
            target: ExecutionTarget::Local,
            required_capabilities: vec!["mt5-bridge".into()],
            ..QueueItem::default()
        };

        let linux_only = vec![ssh_node("fedora-box", "fedora", &["os:linux"])];
        match place(&item, &linux_only, &caps(2)) {
            Placement::Unplaced(reason) => {
                assert!(
                    reason.contains("mt5-bridge"),
                    "reason names the missing cap: {reason}"
                );
            }
            other => panic!("mt5 must not be placed on a linux-only registry: {other:?}"),
        }

        // With the mt5-bridge box present it places there (on the local WSL node).
        let full = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &["os:windows-wsl", "mt5-bridge", "has-node"]),
        ];
        assert_eq!(
            place(&item, &full, &caps(2)),
            Placement::Place(ExecutionTarget::Local),
            "mt5 lands on the only node advertising mt5-bridge"
        );
    }

    #[test]
    fn place_explicit_ssh_host_must_satisfy_caps_and_be_ready() {
        // Explicitly targeting a linux node for an mt5 item is refused (capability filter), even
        // though the node is Ready — the affinity is a hard gate, not just a default.
        let item = QueueItem {
            repo: "mt5-data-api".into(),
            target: ExecutionTarget::SshHost("fedora".into()),
            required_capabilities: vec!["mt5-bridge".into()],
            ..QueueItem::default()
        };
        let nodes = vec![ssh_node("fedora-box", "fedora", &["os:linux"])];
        match place(&item, &nodes, &caps(2)) {
            Placement::Unplaced(reason) => assert!(reason.contains("mt5-bridge")),
            other => panic!("expected capability refusal, got {other:?}"),
        }
    }

    #[test]
    fn place_empty_registry_is_local_only_for_unconstrained_repos() {
        let unconstrained = QueueItem {
            repo: "bn".into(),
            ..QueueItem::default()
        };
        assert_eq!(
            place(&unconstrained, &[], &caps(2)),
            Placement::Place(ExecutionTarget::Local)
        );

        // A constrained repo fails closed because the implicit box advertises no capabilities.
        let constrained = QueueItem {
            repo: "mt5-data-api".into(),
            required_capabilities: vec!["mt5-bridge".into()],
            ..QueueItem::default()
        };
        match place(&constrained, &[], &caps(2)) {
            Placement::Unplaced(reason) => assert!(reason.contains("mt5-bridge")),
            other => panic!("expected constrained repo to stay pending, got {other:?}"),
        }
    }

    #[test]
    fn place_rejects_nodes_with_excluded_capabilities() {
        let item = QueueItem {
            repo: "arena".into(),
            excluded_capabilities: vec!["gpu".into()],
            ..QueueItem::default()
        };
        let nodes = vec![ssh_node("gpu-builder", "builder", &["os:linux", "GPU"])];
        match place(&item, &nodes, &caps(2)) {
            Placement::Unplaced(reason) => assert!(reason.contains("gpu")),
            other => panic!("expected excluded capability refusal, got {other:?}"),
        }
    }

    #[test]
    fn place_rejects_capability_tagged_github_runner_for_constrained_repo() {
        let item = QueueItem {
            repo: "mt5-data-api".into(),
            required_capabilities: vec!["mt5-bridge".into()],
            ..QueueItem::default()
        };
        let nodes = vec![NodeRecord {
            node_id: "cloud".into(),
            target: ExecutionTarget::GitHubRunner,
            labels: vec!["mt5-bridge".into()],
            status: NodeStatus::Ready,
            ..NodeRecord::default()
        }];

        match place(&item, &nodes, &caps(2)) {
            Placement::Unplaced(reason) => assert!(reason.contains("mt5-bridge")),
            other => panic!("expected capability-unknown runner refusal, got {other:?}"),
        }
    }

    #[test]
    fn pick_hands_auto_item_to_idle_remote_when_local_lane_is_full() {
        // End-to-end through pick(): one local item fills the local cap, so the queued sibling is
        // handed to the idle owned fedora worker instead of waiting.
        let mut s = state(
            &[
                ("running", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("729", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        s.nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &["mt5-bridge"]),
        ];
        let p = pick(&s, &caps(1), &Observed::default());
        assert_eq!(p.local, vec![1]);
        assert_eq!(
            p.placements,
            vec![(1, ExecutionTarget::SshHost("fedora".into()))]
        );
        assert!(p.unplaced.is_empty());
        assert!(p.bind.is_none(), "successful handoff is not a paused tick");
    }

    #[test]
    fn pick_respects_remote_ownership_and_resource_guards() {
        let mut s = state(
            &[
                ("running", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("756", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        let mut worker = ssh_node("fedora-box", "fedora", &["os:linux"]);
        worker.owned_repos = vec!["other-repo".into()];
        worker.capacity = Some(crate::worker::capacity::Capacity {
            mem_total_mb: Some(16_000),
            mem_available_mb: Some(8_000),
            swap_total_mb: Some(4_000),
            swap_free_mb: Some(4_000),
            loadavg1: Some(1.0),
            nproc: 8,
            disk_avail_mb: Some(100_000),
        });
        s.nodes = vec![worker.clone(), local_node("wsl-box", &[])];

        let mut guarded = caps(1);
        guarded.min_free_ram_mb = Some(4_000);
        guarded.max_loadavg = Some(4.0);
        let ownership_blocked = pick(&s, &guarded, &Observed::default());
        assert!(ownership_blocked.local.is_empty());
        assert_eq!(ownership_blocked.unplaced.len(), 1);

        worker.owned_repos = vec!["bn".into()];
        worker.capacity.as_mut().unwrap().mem_available_mb = Some(2_000);
        s.nodes[0] = worker;
        let resource_blocked = pick(&s, &guarded, &Observed::default());
        assert!(resource_blocked.local.is_empty());
        assert_eq!(resource_blocked.unplaced.len(), 1);
    }

    #[test]
    fn pick_uses_remote_when_local_box_does_not_own_repo() {
        let mut s = state(&[("756", ItemStatus::Queued, &[], Substrate::Auto)], 1);
        let mut worker = ssh_node("fedora-box", "fedora", &["os:linux"]);
        worker.owned_repos = vec!["bn".into()];
        s.nodes = vec![worker, local_node("wsl-box", &[])];
        let mut c = caps(1);
        c.local_owned = false;

        let p = pick(&s, &c, &Observed::default());
        assert_eq!(
            p.placements,
            vec![(0, ExecutionTarget::SshHost("fedora".into()))]
        );
    }

    #[test]
    fn pick_never_assigns_two_items_to_one_idle_worker_in_a_pass() {
        let mut s = state(
            &[
                ("running", ItemStatus::Dispatched, &[], Substrate::Auto),
                ("756", ItemStatus::Queued, &[], Substrate::Auto),
                ("757", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        s.nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &[]),
        ];

        let p = pick(&s, &caps(1), &Observed::default());
        assert_eq!(p.local, vec![1]);
        assert_eq!(p.unplaced.len(), 1);
    }

    #[test]
    fn pick_does_not_reuse_worker_reserved_by_existing_claim() {
        let mut s = state(
            &[
                (
                    "remote-running",
                    ItemStatus::Dispatched,
                    &[],
                    Substrate::Auto,
                ),
                (
                    "local-running",
                    ItemStatus::Dispatched,
                    &[],
                    Substrate::Auto,
                ),
                ("756", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        s.queue[0].node = Some("fedora".into());
        s.nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &[]),
        ];

        let p = pick(&s, &caps(1), &Observed::default());
        assert!(p.local.is_empty());
        assert_eq!(p.unplaced.len(), 1);
    }

    #[test]
    fn worker_attempts_do_not_consume_local_lane_capacity() {
        let mut s = state(
            &[
                (
                    "remote-running",
                    ItemStatus::Dispatched,
                    &[],
                    Substrate::Auto,
                ),
                ("756", ItemStatus::Queued, &[], Substrate::Auto),
            ],
            1,
        );
        s.queue[0].node = Some("fedora".into());
        s.nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &[]),
        ];

        let p = pick(&s, &caps(1), &Observed::default());
        assert_eq!(p.placements, vec![(1, ExecutionTarget::Local)]);
    }

    #[test]
    fn token_and_cost_budgets_block_worker_handoff() {
        let mut s = state(&[("756", ItemStatus::Queued, &[], Substrate::Auto)], 1);
        s.nodes = vec![
            ssh_node("fedora-box", "fedora", &["os:linux"]),
            local_node("wsl-box", &[]),
        ];

        let mut token_caps = caps(1);
        token_caps.tokens_per_hour = Some(100);
        let token_plan = pick(
            &s,
            &token_caps,
            &Observed {
                tokens_this_hour: 100,
                ..Observed::default()
            },
        );
        assert!(token_plan.local.is_empty());
        assert_eq!(token_plan.bind, Some(Bind::Tokens));

        let mut cost_caps = caps(1);
        cost_caps.daily_cost_usd = Some(10.0);
        let cost_plan = pick(
            &s,
            &cost_caps,
            &Observed {
                cost_today_usd: 10.0,
                ..Observed::default()
            },
        );
        assert!(cost_plan.local.is_empty());
        assert_eq!(cost_plan.bind, Some(Bind::Cost));
    }

    #[test]
    fn pick_records_unplaced_mt5_with_no_capable_node() {
        // An mt5 lane with only a Linux node available: no slot consumed, recorded unplaced.
        let mut s = state(&[("mt5", ItemStatus::Queued, &[], Substrate::Auto)], 2);
        s.queue[0].repo = "mt5-data-api".into();
        s.queue[0].required_capabilities = vec!["mt5-bridge".into()];
        s.nodes = vec![ssh_node("fedora-box", "fedora", &["os:linux"])];
        let p = pick(&s, &caps(2), &Observed::default());
        assert!(p.local.is_empty(), "no doomed dispatch");
        assert_eq!(p.unplaced.len(), 1);
        assert!(p.unplaced[0].1.contains("mt5-bridge"));
    }

    #[test]
    fn pick_does_not_overflow_constrained_repo_to_github_runner() {
        let mut s = state(&[("728", ItemStatus::Queued, &[], Substrate::Auto)], 1);
        s.queue[0].repo = "mt5-data-api".into();
        s.queue[0].required_capabilities = vec!["mt5-bridge".into()];
        let mut c = caps(0);
        c.remote_overflow = true;

        let p = pick(&s, &c, &Observed::default());
        assert!(p.remote.is_empty());
        assert_eq!(p.unplaced.len(), 1);
        assert!(p.unplaced[0].1.contains("github-runner"));
    }
}
