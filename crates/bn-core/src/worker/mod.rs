//! `bn worker` (#719, epic #707 M1) — a durable, worker-local record of dispatch attempts,
//! queryable over SSH. This module owns the two pieces of the registry slice:
//!
//! - [`registry`] — the SQLite (WAL) attempt registry: the 4-state machine
//!   `QUEUED → RUNNING → SUCCEEDED | FAILED` with `LOST` derived at reconcile, idempotent on
//!   `attempt_id`.
//! - [`capacity`] — the `/proc`-based capacity snapshot (RAM/swap/load/disk) + the READY/PRESSURE
//!   admission verdict a scheduler (#720) consumes.
//!
//! [`status`] assembles the versioned `bn worker status --json` payload from both. The registry is
//! deliberately worker-LOCAL and non-synced (git stays audit-only), so its DB lives under the
//! machine-local `~/.bn` tooling root, not the synced notes repo — see [`registry_path`].

pub mod advertise;
pub mod capacity;
pub mod registry;
pub mod spawn;
pub mod transport;

use crate::resolver;
use std::path::PathBuf;

/// Path to this worker's registry DB. `$BN_WORKER_DB` wins (tests, explicit override); otherwise
/// `<bn_home>/worker/registry.db` — under the machine-local `~/.bn` tooling root (NOT the synced
/// notes repo), because the registry is worker-local liveness state that must never sync across
/// hosts.
pub fn registry_path() -> PathBuf {
    if let Ok(p) = std::env::var("BN_WORKER_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    resolver::bn_home().join("worker").join("registry.db")
}

/// This worker's node identity: a stable id (hostname) plus its capability labels. Labels are the
/// intrinsic OS/arch capabilities plus any operator-declared extras from `$BN_WORKER_LABELS`
/// (comma-separated), so a scheduler (#720) can placement-match on them.
pub fn node_id() -> String {
    // The same pinned identity the state dir uses, not the live hostname: a node that renames
    // itself mid-run reappears to the scheduler as a second, idle machine.
    crate::resolver::Config::load().machine().to_string()
}

/// The node's capability labels: `os:<x>`, `arch:<y>`, then any `$BN_WORKER_LABELS` extras
/// (comma-separated, trimmed, empties dropped). Sorted-stable order: intrinsics first, then extras
/// in declared order.
pub fn node_labels() -> Vec<String> {
    let mut labels = vec![
        format!("os:{}", std::env::consts::OS),
        format!("arch:{}", std::env::consts::ARCH),
    ];
    if let Ok(extra) = std::env::var("BN_WORKER_LABELS") {
        for l in extra.split(',') {
            let l = l.trim();
            if !l.is_empty() {
                labels.push(l.to_string());
            }
        }
    }
    labels
}

/// The versioned `bn worker status --json` payload (epic #707 v4): schema version, node identity +
/// derived admission status, a capacity snapshot, and the durable attempt registry.
pub mod status {
    use super::{capacity, node_id, node_labels, registry};
    use serde::Serialize;
    use std::path::Path;

    /// Node identity + labels + the locally-derived admission status.
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct NodeInfo {
        pub node_id: String,
        pub labels: Vec<String>,
        pub status: capacity::NodeStatus,
    }

    /// The full status payload. `schema_version` mirrors the registry schema so a remote poller can
    /// negotiate compatibility.
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WorkerStatus {
        pub schema_version: u32,
        pub node: NodeInfo,
        pub capacity: capacity::Capacity,
        pub attempts: Vec<registry::Attempt>,
    }

    /// Build the status payload from a `reg` and a `worktree_root` (the volume the disk figure is
    /// measured on). Reads the registry and samples capacity; pure aside from those reads.
    ///
    /// `cfg` is what lets the node advertise its **skills, harnesses and MCP servers** rather than
    /// just `os:`/`arch:`. Without it a remote node reports nothing a skill requirement could match,
    /// so skill-aware placement silently only ever works on the box the daemon runs on. `None`
    /// falls back to the intrinsic labels, for the paths that genuinely have no config.
    pub fn build(
        reg: &registry::Registry,
        worktree_root: &Path,
        cfg: Option<&crate::resolver::Config>,
    ) -> registry::Result<WorkerStatus> {
        let cap = capacity::sample(worktree_root);
        Ok(WorkerStatus {
            schema_version: reg.schema_version()?,
            node: NodeInfo {
                node_id: node_id(),
                labels: match cfg {
                    Some(cfg) => super::advertise::advertised_labels(cfg, worktree_root),
                    None => node_labels(),
                },
                status: cap.status(),
            },
            capacity: cap,
            attempts: reg.list()?,
        })
    }
}

/// Re-export the assembled status type at the module root for convenience.
pub use status::{NodeInfo, WorkerStatus};
