//! Placement: **which node** a dispatchable item lands on.
//!
//! Split out of [`crate::scheduler`], which keeps admission (*does the machine have room at
//! all?*). The two are genuinely different questions and were only colocated because they were
//! written together: admission is arithmetic over caps and observed load, placement is a matching
//! problem over the node registry. Keeping placement here is what lets the queue tick and the
//! orchestrate run share ONE placement policy instead of two that drift.
//!
//! Behaviour is byte-for-byte what `scheduler::place`/`place_with_load` did before the move;
//! `scheduler` re-exports both so no call site changed.
use crate::backlog::ExecutionTarget;
use crate::capability::{
    matching_excluded, missing_required, no_capable_node_reason, node_satisfies,
};
use crate::hosts::{NodeRecord, NodeStatus};
use crate::orchestrate::QueueItem;
use crate::queue::kind::Requirements;
use crate::scheduler::{
    bind_allows_worker_handoff, node_owns_repo, node_resources_allow, worker_eligible, Bind, Caps,
};
use std::collections::HashMap;

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

/// Resolve which node a queue entry lands on, given what it **requires** rather than what
/// `QueueItem` fields it happens to carry.
///
/// The generalization [`place_with_load`] delegates the hard part to: it takes a [`Requirements`],
/// so the queue tick and the orchestrate run share one placement policy instead of two that drift.
/// Skills, MCPs and raw capability labels are flattened into the one label namespace
/// [`crate::capability`] matches on; harness is checked separately because it is an OR set.
///
/// `nodes` is expected to include a synthesized record for the local box
/// ([`crate::hosts::local_record`]). Without it, the implicit local box advertises nothing and
/// every constrained entry is `Unplaced` — which is the bug this signature exists to make
/// impossible to reintroduce quietly.
pub fn place_req(
    req: &Requirements,
    repo: &str,
    target: &ExecutionTarget,
    nodes: &[NodeRecord],
    caps: &Caps,
    local_bind: Option<Bind>,
    planned: &HashMap<String, usize>,
) -> Placement {
    let probe = QueueItem {
        repo: repo.to_string(),
        target: target.clone(),
        required_capabilities: crate::capability::required_labels(req),
        excluded_capabilities: crate::capability::excluded_labels(req),
        ..Default::default()
    };
    match place_with_load(&probe, nodes, caps, local_bind, planned) {
        // The label filter passed; the harness OR is the one axis it cannot express, so it is
        // checked against the chosen node here rather than being silently ignored.
        Placement::Place(chosen) => match harness_of(&chosen, nodes) {
            Some(labels) if !crate::capability::harness_ok(req, &labels) => {
                Placement::Unplaced(format!(
                    "node offers no acceptable harness: {}",
                    req.harness.join(" | ")
                ))
            }
            _ => Placement::Place(chosen),
        },
        unplaced => unplaced,
    }
}

/// The advertised labels of whichever registry node backs `target`, for the harness check.
fn harness_of(target: &ExecutionTarget, nodes: &[NodeRecord]) -> Option<Vec<String>> {
    nodes
        .iter()
        .find(|n| {
            n.target == *target || matches!(target, ExecutionTarget::SshHost(a) if &n.node_id == a)
        })
        .map(|n| n.labels.clone())
}

/// [`place`], additionally weighing the local admission bind and the workers this same pass has
/// already planned to fill — so one pass cannot double-book a single-slot node.
pub fn place_with_load(
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
                        >= n.slots.max(1) =>
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
                            || planned_workers.get(&n.node_id).copied().unwrap_or_default()
                                < n.slots.max(1))
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
                None => Placement::Unplaced(local_refusal(nodes, item, caps, required, excluded)),
            }
        }
    }
}

/// Why no local node took `item` — the reason that would actually have to change.
///
/// The candidate filter above tests four things, and the old text named only one of them: an
/// item held because the box was under load read as "no READY node satisfies required capability:
/// skill:investigate" against a node whose label list plainly included it. The pressure guard is
/// the right call on a box building BigMono at load 50; blaming the capability sends the reader to
/// fix the wrong thing.
fn local_refusal(
    nodes: &[NodeRecord],
    item: &QueueItem,
    caps: &Caps,
    required: &[String],
    excluded: &[String],
) -> String {
    let capable = nodes.iter().filter(|n| {
        n.target.is_local()
            && node_satisfies(n, required, excluded)
            && node_owns_repo(n, &item.repo)
    });
    for n in capable {
        if n.status != NodeStatus::Ready {
            return format!("local node '{}' is {:?}, not Ready", n.node_id, n.status);
        }
        if !node_resources_allow(n, caps) {
            let load = n
                .capacity
                .as_ref()
                .and_then(|c| {
                    c.loadavg1
                        .map(|l| format!("load {l:.1} on {} cores", c.nproc))
                })
                .unwrap_or_else(|| "resource guard".to_string());
            return format!(
                "local node '{}' is under pressure ({load}) — waiting for it to ease",
                n.node_id
            );
        }
    }
    no_capable_node_reason(required, excluded)
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use crate::worker::capacity::Capacity;

    fn node(load: f64) -> NodeRecord {
        NodeRecord {
            node_id: "box".to_string(),
            target: ExecutionTarget::Local,
            status: NodeStatus::Ready,
            slots: 1,
            labels: vec!["skill:investigate".to_string()],
            capacity: Some(Capacity {
                mem_total_mb: Some(55_000),
                mem_available_mb: Some(44_000),
                swap_total_mb: Some(14_000),
                swap_free_mb: Some(14_000),
                loadavg1: Some(load),
                nproc: 12,
                disk_avail_mb: Some(600_000),
            }),
            ..Default::default()
        }
    }

    fn wants_investigate() -> QueueItem {
        QueueItem {
            repo: "bn".to_string(),
            target: ExecutionTarget::Local,
            required_capabilities: vec!["skill:investigate".to_string()],
            ..Default::default()
        }
    }

    fn caps() -> Caps {
        Caps {
            max_local: 6,
            research: Some(3),
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

    /// The case that sent a reader to the wrong fix: the node had the skill, and was under load.
    #[test]
    fn a_node_under_pressure_is_named_as_such_not_as_lacking_the_skill() {
        let nodes = vec![node(48.7)];
        let got = place_with_load(&wants_investigate(), &nodes, &caps(), None, &HashMap::new());
        let Placement::Unplaced(reason) = got else {
            panic!("load 48.7 on 12 cores must not place, got {got:?}");
        };
        assert!(reason.contains("under pressure"), "{reason}");
        assert!(reason.contains("load 48.7 on 12 cores"), "{reason}");
        assert!(!reason.contains("capability"), "{reason}");
    }

    #[test]
    fn a_calm_node_with_the_skill_takes_the_item() {
        let nodes = vec![node(1.0)];
        let got = place_with_load(&wants_investigate(), &nodes, &caps(), None, &HashMap::new());
        assert!(
            matches!(got, Placement::Place(ExecutionTarget::Local)),
            "{got:?}"
        );
    }

    /// And when the skill really is missing, the old wording stands.
    #[test]
    fn a_missing_skill_is_still_named() {
        let mut n = node(1.0);
        n.labels.clear();
        let got = place_with_load(&wants_investigate(), &[n], &caps(), None, &HashMap::new());
        let Placement::Unplaced(reason) = got else {
            panic!("{got:?}")
        };
        assert!(reason.contains("skill:investigate"), "{reason}");
    }
}
