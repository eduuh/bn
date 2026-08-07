//! The **machine-scoped** node registry — who can run work for this box, and were they alive
//! recently.
//!
//! The node registry already existed, but only inside a per-repo orchestrate run's state. That is
//! the wrong scope for a queue that spans every repo: a machine with no run up had no live registry
//! at all, so every declared node sat at `Unknown` with `last_heartbeat: 0` and could never be
//! placed on. Heartbeats only mean something once something persists them somewhere the whole
//! machine can read.
//!
//! One JSON file, replaced atomically. The tick refreshes it; every read surface folds staleness
//! over it at read time, so a registry that stops being updated degrades to "nothing is Ready"
//! rather than to stale confidence.
use crate::hosts::{is_stale, NodeRecord, NodeStatus, DEFAULT_HEARTBEAT_TTL_SECS};
use crate::resolver;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where the machine-scoped registry lives.
pub fn nodes_path() -> PathBuf {
    resolver::state_dir().join("queue-nodes.json")
}

/// The persisted registry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeRegistry {
    /// Epoch secs of the last refresh pass — distinct from any one node's heartbeat, so a registry
    /// that is itself stale (nothing has polled in an hour) is visible as such.
    #[serde(default)]
    pub refreshed_at: i64,
    #[serde(default)]
    pub nodes: Vec<NodeRecord>,
}

impl NodeRegistry {
    /// Read the registry, or an empty one when nothing has refreshed yet.
    pub fn load() -> NodeRegistry {
        std::fs::read_to_string(nodes_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist, replacing the previous file.
    pub fn save(&self) -> std::io::Result<()> {
        let path = nodes_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::store::save_json_atomic(&path, self)
    }

    /// The records with staleness applied: a `Ready` node whose heartbeat lapsed becomes `Offline`.
    ///
    /// Folded on read rather than written into the file, so the demotion is a function of *when you
    /// looked* — which is the only correct reading of a heartbeat. Persisting it instead would mean
    /// a node stayed Offline until something happened to poll it again.
    pub fn live(&self, now: i64, ttl_secs: i64) -> Vec<NodeRecord> {
        self.nodes
            .iter()
            .map(|n| {
                let mut n = n.clone();
                if is_stale(&n, now, ttl_secs) {
                    n.status = NodeStatus::Offline;
                }
                n
            })
            .collect()
    }

    /// [`live`](NodeRegistry::live) at the default TTL.
    pub fn live_default(&self, now: i64) -> Vec<NodeRecord> {
        self.live(now, DEFAULT_HEARTBEAT_TTL_SECS)
    }

    /// Merge a refreshed record in by `node_id`, replacing any previous one.
    pub fn upsert(&mut self, record: NodeRecord) {
        match self
            .nodes
            .iter_mut()
            .find(|n| n.node_id.eq_ignore_ascii_case(&record.node_id))
        {
            Some(slot) => *slot = record,
            None => self.nodes.push(record),
        }
        self.nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    }

    /// Drop records for nodes no longer declared, so removing a `[nodes.<id>]` entry actually
    /// removes it rather than leaving a ghost that still looks placeable.
    pub fn retain_declared(&mut self, declared: &[String], local: &str) {
        self.nodes.retain(|n| {
            n.node_id.eq_ignore_ascii_case(local)
                || declared.iter().any(|d| d.eq_ignore_ascii_case(&n.node_id))
        });
    }
}
