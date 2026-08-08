//! Why the tick did what it did — written to disk on **every** pass, including the ones that start
//! nothing.
//!
//! The machine-scoped sibling of `orchestrate::DispatchDecision`. "Why is nothing running" has to
//! be answerable from disk without re-deriving anything, because by the time someone asks, the
//! conditions that caused it have usually changed. A dispatcher that only records its successes
//! makes its silences unexplainable.
use crate::queue::tick::{TickInputs, TickPlan};
use crate::resolver;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where the latest decision is persisted. Single file, replaced atomically each pass — this is a
/// "current state" record, not a log; the events log already carries the history.
pub fn decision_path() -> PathBuf {
    resolver::state_dir().join("queue-decision.json")
}

/// The persisted explanation of the latest queue tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueDecision {
    pub at: i64,
    /// `started` | `held` | `empty`.
    pub outcome: String,
    /// The bind label — `ram` / `swap` / `load` / `tokens` / `cost` / `local-cap` /
    /// `no-capable-node` / `empty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    /// One human line: `free RAM below the guard`, `no READY node satisfies capability: x`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether this pass was a dry run — so a `--dry-run` experiment can never be mistaken for the
    /// record of a real dispatch.
    #[serde(default)]
    pub dry_run: bool,
    pub pending: usize,
    pub started: usize,
    pub lanes: Vec<LaneRow>,
    pub kinds: Vec<KindRow>,
    pub deferred: Vec<DeferredRow>,
    pub dispatched: Vec<DispatchRow>,
    /// One row per node with what it offers — so `no-capable-node` is *actionable*
    /// ("fedora-box is Ready but serves no skill:auto-review-pr") rather than merely true.
    pub nodes: Vec<NodeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRow {
    pub lane: String,
    pub cap: usize,
    pub in_flight: usize,
    pub pending: usize,
    pub started: usize,
    pub saturated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindRow {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<usize>,
    pub in_flight: usize,
    pub pending: usize,
    pub started: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredRow {
    pub repo: String,
    pub id: String,
    pub kind: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub waiting_since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRow {
    pub repo: String,
    pub id: String,
    pub item: String,
    pub kind: String,
    pub lane: String,
    pub exec: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRow {
    pub node_id: String,
    pub target: String,
    pub status: String,
    pub busy: usize,
    pub skills: Vec<String>,
    pub harnesses: Vec<String>,
}

impl QueueDecision {
    /// Build the record for a completed (or dry-run) pass.
    pub fn build(inputs: &TickInputs, plan: &TickPlan, dry_run: bool) -> QueueDecision {
        let outcome = if !plan.start.is_empty() {
            "started"
        } else if inputs.pending.is_empty() {
            "empty"
        } else {
            "held"
        };
        QueueDecision {
            at: inputs.now,
            outcome: outcome.to_string(),
            bind: plan.bind.as_ref().map(|b| b.label()),
            reason: plan.deferred.first().map(|d| match d.detail.is_empty() {
                true => d.reason.label().to_string(),
                false => d.detail.clone(),
            }),
            dry_run,
            pending: inputs.pending.len(),
            started: plan.start.len(),
            lanes: plan
                .lanes
                .iter()
                .map(|l| LaneRow {
                    lane: l.lane.label().to_string(),
                    cap: l.cap,
                    in_flight: l.in_flight,
                    pending: l.pending,
                    started: l.started,
                    saturated: l.in_flight >= l.cap,
                })
                .collect(),
            kinds: plan
                .kinds
                .iter()
                .filter(|k| k.pending > 0 || k.in_flight > 0 || k.started > 0)
                .map(|k| KindRow {
                    kind: k.kind.label().to_string(),
                    cap: k.cap,
                    in_flight: k.in_flight,
                    pending: k.pending,
                    started: k.started,
                })
                .collect(),
            deferred: plan
                .deferred
                .iter()
                .map(|d| DeferredRow {
                    repo: d.repo.clone(),
                    id: d.id.clone(),
                    kind: d.kind.label().to_string(),
                    reason: d.reason.label().to_string(),
                    detail: d.detail.clone(),
                    waiting_since: d.waiting_since,
                })
                .collect(),
            dispatched: plan
                .start
                .iter()
                .map(|d| DispatchRow {
                    repo: d.repo.clone(),
                    id: d.id.clone(),
                    item: d.item.clone(),
                    kind: d.kind.label().to_string(),
                    lane: d.work_class.label().to_string(),
                    exec: d.exec.label().to_string(),
                    target: d.target.label(),
                    harness: d.harness.clone(),
                    priority: d.priority,
                    rationale: d.rationale.clone(),
                })
                .collect(),
            nodes: inputs
                .nodes
                .iter()
                .map(|n| {
                    let axes = crate::worker::advertise::split_labels(&n.labels);
                    NodeRow {
                        node_id: n.node_id.clone(),
                        target: n.target.label(),
                        status: format!("{:?}", n.status).to_lowercase(),
                        busy: n.active_attempts,
                        skills: axes.skills,
                        harnesses: axes.harnesses,
                    }
                })
                .collect(),
        }
    }

    /// Persist, replacing the previous record.
    pub fn write(&self) -> std::io::Result<()> {
        let path = decision_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::store::save_json_atomic(&path, self)
    }

    /// Read the last decision, or `None` when no tick has run on this machine yet.
    pub fn read() -> Option<QueueDecision> {
        let raw = std::fs::read_to_string(decision_path()).ok()?;
        serde_json::from_str(&raw).ok()
    }
}
