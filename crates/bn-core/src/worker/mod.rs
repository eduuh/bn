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
    gethostname::gethostname().to_string_lossy().into_owned()
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
    pub fn build(reg: &registry::Registry, worktree_root: &Path) -> registry::Result<WorkerStatus> {
        let cap = capacity::sample(worktree_root);
        Ok(WorkerStatus {
            schema_version: reg.schema_version()?,
            node: NodeInfo {
                node_id: node_id(),
                labels: node_labels(),
                status: cap.status(),
            },
            capacity: cap,
            attempts: reg.list()?,
        })
    }
}

/// Re-export the assembled status type at the module root for convenience.
pub use status::{NodeInfo, WorkerStatus};

/// Parse the `verdict` from a `bn worker start --json` envelope (#729) — `accepted`,
/// `already_exists`, or `rejected_capacity`. The control-plane treats the first two as a
/// successful (idempotent) admission and the last as a capacity rejection. `None` when the output
/// isn't a well-formed envelope (unreachable node, non-JSON error). Pure so the remote-dispatch
/// verdict handling is unit-testable without a live worker.
pub fn parse_start_verdict(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.get("data")?.get("verdict")?.as_str().map(str::to_string)
}

/// Whether a start verdict counts as a successful (idempotent) admission (#720/#729): `accepted`
/// (fresh) or `already_exists` (a retry of the same `request_id`) — both mean the attempt is now
/// live on the worker. `rejected_capacity` (or anything else) is not.
pub fn start_admitted(verdict: &str) -> bool {
    verdict == "accepted" || verdict == "already_exists"
}

/// Extract the state of attempt `attempt_id` from a `bn worker status --json` envelope (#729) —
/// the value the control-plane mirrors into its per-item `worker_state`. `None` when the envelope
/// is malformed or carries no such attempt (the caller then treats the attempt as `UNKNOWN`,
/// blocking its redispatch until reconciled). Pure so the liveness fold is unit-testable.
pub fn attempt_state(stdout: &str, attempt_id: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let attempts = v.get("data")?.get("attempts")?.as_array()?;
    attempts
        .iter()
        .find(|a| a.get("attempt_id").and_then(|x| x.as_str()) == Some(attempt_id))
        .and_then(|a| a.get("state").and_then(|x| x.as_str()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_start_verdict_reads_envelope() {
        let out = r#"{"ok":true,"command":"worker","data":{"verdict":"accepted","request_id":"r","attempt_id":"a"},"error":null}"#;
        assert_eq!(parse_start_verdict(out).as_deref(), Some("accepted"));
        assert!(start_admitted("accepted"));
        assert!(start_admitted("already_exists"));
        assert!(!start_admitted("rejected_capacity"));
        // Non-JSON / unreachable output ⇒ None (treated as not-admitted).
        assert_eq!(parse_start_verdict("ssh: connect: timed out"), None);
    }

    #[test]
    fn attempt_state_finds_matching_attempt() {
        let out = r#"{"ok":true,"command":"worker","data":{"schema_version":4,"node":{},"capacity":{},
            "attempts":[
              {"attempt_id":"a1","request_id":"r1","state":"RUNNING"},
              {"attempt_id":"a2","request_id":"r2","state":"SUCCEEDED"}
            ]}}"#;
        assert_eq!(attempt_state(out, "a1").as_deref(), Some("RUNNING"));
        assert_eq!(attempt_state(out, "a2").as_deref(), Some("SUCCEEDED"));
        // Unknown attempt / malformed ⇒ None (caller marks it UNKNOWN).
        assert_eq!(attempt_state(out, "missing"), None);
        assert_eq!(attempt_state("garbage", "a1"), None);
    }

    #[test]
    fn registry_path_honors_env_override() {
        let _g = crate::test_support::env_lock();
        let _e = crate::test_support::EnvGuard::set("BN_WORKER_DB", "/tmp/bn-test-worker.db");
        assert_eq!(registry_path(), PathBuf::from("/tmp/bn-test-worker.db"));
    }

    #[test]
    fn labels_include_os_and_arch() {
        let labels = node_labels();
        assert!(labels.iter().any(|l| l.starts_with("os:")));
        assert!(labels.iter().any(|l| l.starts_with("arch:")));
    }

    #[test]
    fn empty_registry_status_is_versioned_and_valid() {
        let reg = registry::Registry::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let st = status::build(&reg, dir.path()).unwrap();
        assert_eq!(st.schema_version, registry::SCHEMA_VERSION);
        assert!(st.attempts.is_empty());
        assert!(!st.node.node_id.is_empty());
        // Serializes to a JSON object carrying every top-level key.
        let v: serde_json::Value = serde_json::to_value(&st).unwrap();
        for key in ["schema_version", "node", "capacity", "attempts"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}
