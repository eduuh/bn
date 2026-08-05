//! Durable, atomic ownership of an agent-bearing worktree.
//!
//! Tmux window names are presentation, not identity: dispatch and pipeline stages can choose
//! different names while pointing at the same checkout. This registry keys ownership by normalized
//! repo/branch plus the canonical worktree path, and uses a required file lock for every mutation.

use crate::store::FileLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A newly reserved lease remains authoritative while its launcher creates the tmux window.
pub const AGENT_LEASE_STARTING_GRACE_SECS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentLeaseKey {
    pub repo: String,
    pub branch: String,
    pub worktree: String,
}

impl AgentLeaseKey {
    pub fn new(repo: &str, branch: &str, worktree: &Path) -> Self {
        let worktree = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
        Self {
            repo: repo.trim().to_lowercase(),
            branch: branch.trim().replace('/', "-").to_lowercase(),
            worktree: worktree.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentLeaseKind {
    Dispatch,
    PipelineInvestigate,
    PipelinePlan,
    /// The `bn control` coordination agent — reserved to a repo's main/master worktree.
    Control,
}

impl AgentLeaseKind {
    /// The stable kebab-case label used in messages and tmux window options. This is the single
    /// source of truth every caller (dispatch, doctor, the CLI lease bridge) shares.
    pub fn label(self) -> &'static str {
        match self {
            AgentLeaseKind::Dispatch => "dispatch",
            AgentLeaseKind::PipelineInvestigate => "pipeline-investigate",
            AgentLeaseKind::PipelinePlan => "pipeline-plan",
            AgentLeaseKind::Control => "control",
        }
    }
}

/// A repo's primary branch, reserved exclusively for the `bn control` coordination agent. No
/// dispatched / pipeline / worker agent may ever occupy it, and — conversely — a control agent may
/// occupy *only* this branch (the bidirectional invariant enforced in [`begin_agent_lease`]).
/// Compared case-insensitively so a normalized lease key ("main") and a raw branch name ("Main")
/// agree with the dispatch / reap / cleanup guards.
pub fn is_reserved_branch(branch: &str) -> bool {
    let b = branch.trim().to_ascii_lowercase();
    b == "main" || b == "master"
}

/// The main↔control invariant checked before every acquisition: the reserved (main/master)
/// worktree is control-only, and a control agent is reserved-branch-only. Returns a human-readable
/// refusal when the pairing is illegal, so the message surfaces verbatim to the operator.
fn validate_kind_branch(kind: AgentLeaseKind, branch: &str) -> Result<(), String> {
    match (kind, is_reserved_branch(branch)) {
        (AgentLeaseKind::Control, false) => Err(format!(
            "the bn control agent may only run in a repo's main/master worktree, not '{branch}'"
        )),
        (kind, true) if kind != AgentLeaseKind::Control => Err(format!(
            "'{branch}' is reserved for the bn control agent — a {} agent cannot occupy the \
             main/master worktree",
            kind.label()
        )),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLease {
    pub key: AgentLeaseKey,
    pub session_id: String,
    pub harness: String,
    pub kind: AgentLeaseKind,
    /// Exact tmux target without the leading `=`, for example `bn:Plan bug · a1b2c3`.
    pub tmux_target: String,
    pub acquired_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLeaseRegistry {
    #[serde(default)]
    pub leases: Vec<AgentLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLeaseAcquireMode {
    Refuse,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLeaseAcquire {
    Acquired { replaced: Vec<AgentLease> },
    Held(AgentLease),
}

pub enum AgentLeaseBegin {
    Reserved(AgentLeaseReservation),
    Held(AgentLease),
}

/// An atomic lease transition held across the caller's reap + launch sequence.
///
/// The new owner is persisted before process changes begin, while the registry lock remains held.
/// Committing keeps it; rolling back (also the `Drop` fallback) restores the complete prior
/// registry, so a failed replacement never leaves the surviving old agent unleased.
pub struct AgentLeaseReservation {
    path: PathBuf,
    original: AgentLeaseRegistry,
    lease: AgentLease,
    replaced: Vec<AgentLease>,
    _lock: FileLock,
    committed: bool,
}

impl AgentLeaseReservation {
    pub fn lease(&self) -> &AgentLease {
        &self.lease
    }

    pub fn replaced(&self) -> &[AgentLease] {
        &self.replaced
    }

    /// The prior owners have been successfully reaped; a later launch failure must leave the key
    /// unowned rather than restore leases for processes that no longer exist.
    pub fn discard_replaced(&mut self) {
        self.original
            .leases
            .retain(|current| current.key != self.lease.key);
    }

    pub fn commit(mut self) {
        self.committed = true;
    }

    pub fn rollback(mut self) -> std::io::Result<()> {
        save_agent_leases(&self.path, &self.original)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for AgentLeaseReservation {
    fn drop(&mut self) {
        if !self.committed {
            let _ = save_agent_leases(&self.path, &self.original);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLeaseRemovalReason {
    Stale,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLeaseRemoval {
    pub lease: AgentLease,
    pub reason: AgentLeaseRemovalReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentLeaseReconcile {
    pub kept: Vec<AgentLease>,
    pub removed: Vec<AgentLeaseRemoval>,
}

/// Agent-lease invariant violations for `bn doctor`: every lease whose kind/branch pairing breaks
/// the main↔control rule, rendered `repo/branch (session): reason`. Empty in a healthy fleet — the
/// acquire-time guard in [`begin_agent_lease`] prevents new ones — so a non-empty result means a
/// lease predating the guard or hand-edited state that an operator should reconcile.
pub fn lease_invariant_violations(registry: &AgentLeaseRegistry) -> Vec<String> {
    registry
        .leases
        .iter()
        .filter_map(|lease| {
            validate_kind_branch(lease.kind, &lease.key.branch)
                .err()
                .map(|reason| {
                    format!(
                        "{}/{} ({}): {reason}",
                        lease.key.repo, lease.key.branch, lease.session_id
                    )
                })
        })
        .collect()
}

pub fn agent_lease_registry_path() -> PathBuf {
    crate::resolver::state_dir().join("agent-leases.json")
}

pub fn load_agent_leases(path: &Path) -> std::io::Result<AgentLeaseRegistry> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgentLeaseRegistry::default());
        }
        Err(error) => return Err(error),
    };
    serde_json::from_str(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn save_agent_leases(path: &Path, registry: &AgentLeaseRegistry) -> std::io::Result<()> {
    crate::store::save_json_atomic(path, registry)
}

fn is_authoritative(lease: &AgentLease, now: i64, is_live: &impl Fn(&AgentLease) -> bool) -> bool {
    is_live(lease) || now.saturating_sub(lease.acquired_at) < AGENT_LEASE_STARTING_GRACE_SECS
}

/// Atomically reserve one worktree for an agent.
///
/// Stale matching leases are reclaimed. `Replace` additionally returns every live prior owner so
/// the caller can reap it before launching the replacement; `Refuse` leaves the oldest live owner
/// untouched and reports it.
pub fn begin_agent_lease(
    path: &Path,
    lease: AgentLease,
    mode: AgentLeaseAcquireMode,
    now: i64,
    is_live: impl Fn(&AgentLease) -> bool,
) -> std::io::Result<AgentLeaseBegin> {
    // Enforce the main↔control invariant before touching the registry: a rule violation is a
    // policy refusal, not a transient conflict, so fail fast without taking the lock.
    if let Err(reason) = validate_kind_branch(lease.kind, &lease.key.branch) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let lock = FileLock::acquire_required(path)?;
    let original = load_agent_leases(path)?;
    let mut registry = original.clone();
    let mut matching = registry
        .leases
        .iter()
        .filter(|current| current.key == lease.key)
        .filter(|current| is_authoritative(current, now, &is_live))
        .cloned()
        .collect::<Vec<_>>();
    matching.sort_by_key(|current| (current.acquired_at, current.session_id.clone()));

    if mode == AgentLeaseAcquireMode::Refuse {
        if let Some(owner) = matching.first() {
            return Ok(AgentLeaseBegin::Held(owner.clone()));
        }
    }

    registry.leases.retain(|current| current.key != lease.key);
    registry.leases.push(lease.clone());
    registry
        .leases
        .sort_by_key(|current| (current.key.clone(), current.acquired_at));
    save_agent_leases(path, &registry)?;
    Ok(AgentLeaseBegin::Reserved(AgentLeaseReservation {
        path: path.to_path_buf(),
        original,
        lease,
        replaced: matching,
        _lock: lock,
        committed: false,
    }))
}

pub fn acquire_agent_lease(
    path: &Path,
    lease: AgentLease,
    mode: AgentLeaseAcquireMode,
    now: i64,
    is_live: impl Fn(&AgentLease) -> bool,
) -> std::io::Result<AgentLeaseAcquire> {
    match begin_agent_lease(path, lease, mode, now, is_live)? {
        AgentLeaseBegin::Held(owner) => Ok(AgentLeaseAcquire::Held(owner)),
        AgentLeaseBegin::Reserved(reservation) => {
            let replaced = reservation.replaced().to_vec();
            reservation.commit();
            Ok(AgentLeaseAcquire::Acquired { replaced })
        }
    }
}

/// Release only the reservation owned by `session_id`; another replacement owner is untouched.
pub fn release_agent_lease(path: &Path, session_id: &str) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut registry = load_agent_leases(path)?;
    let before = registry.leases.len();
    registry
        .leases
        .retain(|lease| lease.session_id != session_id);
    let removed = registry.leases.len() != before;
    if removed {
        save_agent_leases(path, &registry)?;
    }
    Ok(removed)
}

/// Remove stale leases and collapse duplicate live owners to the oldest reservation per key.
///
/// The caller owns process/window cleanup for returned removals; keeping policy here pure makes the
/// same deterministic result usable by doctor, dispatch recovery, and tests.
pub fn reconcile_agent_leases(
    path: &Path,
    now: i64,
    is_live: impl Fn(&AgentLease) -> bool,
) -> std::io::Result<AgentLeaseReconcile> {
    let _lock = FileLock::acquire_required(path)?;
    let mut registry = load_agent_leases(path)?;
    let mut grouped = BTreeMap::<AgentLeaseKey, Vec<AgentLease>>::new();
    for lease in registry.leases.drain(..) {
        grouped.entry(lease.key.clone()).or_default().push(lease);
    }

    let mut report = AgentLeaseReconcile::default();
    for (_, mut leases) in grouped {
        leases.sort_by_key(|lease| (lease.acquired_at, lease.session_id.clone()));
        let mut live = leases
            .iter()
            .filter(|lease| is_authoritative(lease, now, &is_live))
            .cloned();
        let keeper = live.next();
        if let Some(keeper) = &keeper {
            report.kept.push(keeper.clone());
        }
        for lease in leases {
            if keeper.as_ref() == Some(&lease) {
                continue;
            }
            report.removed.push(AgentLeaseRemoval {
                reason: if is_authoritative(&lease, now, &is_live) {
                    AgentLeaseRemovalReason::Duplicate
                } else {
                    AgentLeaseRemovalReason::Stale
                },
                lease,
            });
        }
    }

    registry.leases = report.kept.clone();
    save_agent_leases(path, &registry)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(root: &Path, session: &str, acquired_at: i64) -> AgentLease {
        lease_kind(root, session, acquired_at, AgentLeaseKind::Dispatch)
    }

    fn lease_kind(
        root: &Path,
        session: &str,
        acquired_at: i64,
        kind: AgentLeaseKind,
    ) -> AgentLease {
        AgentLease {
            key: AgentLeaseKey::new("BN", "dispatch/Lock", root),
            session_id: session.into(),
            harness: "copilot".into(),
            kind,
            tmux_target: format!("bn:{session}"),
            acquired_at,
        }
    }

    #[test]
    fn key_normalizes_branch_and_canonicalizes_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let key = AgentLeaseKey::new(" BN ", "Dispatch/LOCK", dir.path());
        assert_eq!(key.repo, "bn");
        assert_eq!(key.branch, "dispatch-lock");
        assert_eq!(
            key.worktree,
            std::fs::canonicalize(dir.path()).unwrap().to_string_lossy()
        );
    }

    #[test]
    fn acquire_refuses_a_live_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        assert!(matches!(
            acquire_agent_lease(
                &path,
                lease(dir.path(), "first", 100),
                AgentLeaseAcquireMode::Refuse,
                100,
                |_| false,
            )
            .unwrap(),
            AgentLeaseAcquire::Acquired { .. }
        ));
        assert_eq!(
            acquire_agent_lease(
                &path,
                lease(dir.path(), "second", 101),
                AgentLeaseAcquireMode::Refuse,
                101,
                |_| false,
            )
            .unwrap(),
            AgentLeaseAcquire::Held(lease(dir.path(), "first", 100))
        );
    }

    #[test]
    fn dispatch_then_pipeline_launch_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let dispatch = lease_kind(dir.path(), "dispatch", 100, AgentLeaseKind::Dispatch);
        let pipeline = lease_kind(dir.path(), "pipeline", 101, AgentLeaseKind::PipelinePlan);
        assert!(matches!(
            acquire_agent_lease(
                &path,
                dispatch.clone(),
                AgentLeaseAcquireMode::Refuse,
                100,
                |_| false,
            )
            .unwrap(),
            AgentLeaseAcquire::Acquired { .. }
        ));
        assert_eq!(
            acquire_agent_lease(&path, pipeline, AgentLeaseAcquireMode::Refuse, 101, |_| {
                false
            },)
            .unwrap(),
            AgentLeaseAcquire::Held(dispatch)
        );
    }

    #[test]
    fn pipeline_then_dispatch_launch_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let pipeline = lease_kind(dir.path(), "pipeline", 100, AgentLeaseKind::PipelinePlan);
        let dispatch = lease_kind(dir.path(), "dispatch", 101, AgentLeaseKind::Dispatch);
        acquire_agent_lease(
            &path,
            pipeline.clone(),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        )
        .unwrap();
        assert_eq!(
            acquire_agent_lease(&path, dispatch, AgentLeaseAcquireMode::Refuse, 101, |_| {
                false
            },)
            .unwrap(),
            AgentLeaseAcquire::Held(pipeline)
        );
    }

    #[test]
    fn simultaneous_launches_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let root = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["dispatch", "pipeline"].map(|session| {
            let path = path.clone();
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                acquire_agent_lease(
                    &path,
                    lease(&root, session, 100),
                    AgentLeaseAcquireMode::Refuse,
                    100,
                    |_| false,
                )
                .unwrap()
            })
        });
        let outcomes = handles.map(|handle| handle.join().unwrap());
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, AgentLeaseAcquire::Acquired { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, AgentLeaseAcquire::Held(_)))
                .count(),
            1
        );
        assert_eq!(load_agent_leases(&path).unwrap().leases.len(), 1);
    }

    #[test]
    fn stale_owner_is_reclaimed_and_fresh_replaces_live_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        acquire_agent_lease(
            &path,
            lease(dir.path(), "old", 100),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        )
        .unwrap();
        assert_eq!(
            acquire_agent_lease(
                &path,
                lease(dir.path(), "new", 200),
                AgentLeaseAcquireMode::Refuse,
                200,
                |_| false,
            )
            .unwrap(),
            AgentLeaseAcquire::Acquired { replaced: vec![] }
        );
        assert_eq!(
            acquire_agent_lease(
                &path,
                lease(dir.path(), "replacement", 201),
                AgentLeaseAcquireMode::Replace,
                201,
                |_| true,
            )
            .unwrap(),
            AgentLeaseAcquire::Acquired {
                replaced: vec![lease(dir.path(), "new", 200)]
            }
        );
    }

    #[test]
    fn failed_reap_restores_prior_owner_but_failed_launch_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let old = lease(dir.path(), "old", 100);
        acquire_agent_lease(
            &path,
            old.clone(),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        )
        .unwrap();

        let AgentLeaseBegin::Reserved(failed_reap) = begin_agent_lease(
            &path,
            lease(dir.path(), "first-replacement", 101),
            AgentLeaseAcquireMode::Replace,
            101,
            |_| true,
        )
        .unwrap() else {
            panic!("replacement should reserve");
        };
        failed_reap.rollback().unwrap();
        assert_eq!(load_agent_leases(&path).unwrap().leases, vec![old]);

        let AgentLeaseBegin::Reserved(mut failed_launch) = begin_agent_lease(
            &path,
            lease(dir.path(), "second-replacement", 102),
            AgentLeaseAcquireMode::Replace,
            102,
            |_| true,
        )
        .unwrap() else {
            panic!("replacement should reserve");
        };
        failed_launch.discard_replaced();
        failed_launch.rollback().unwrap();
        assert!(load_agent_leases(&path).unwrap().leases.is_empty());
    }

    #[test]
    fn replacement_lock_is_held_until_launch_transaction_commits() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let first = begin_agent_lease(
            &path,
            lease(dir.path(), "first", 100),
            AgentLeaseAcquireMode::Replace,
            100,
            |_| false,
        )
        .unwrap();
        let AgentLeaseBegin::Reserved(first) = first else {
            panic!("first launch should reserve");
        };

        let (tx, rx) = mpsc::channel();
        let second_path = path;
        let second_root = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let outcome = begin_agent_lease(
                &second_path,
                lease(&second_root, "second", 101),
                AgentLeaseAcquireMode::Replace,
                101,
                |_| false,
            )
            .unwrap();
            tx.send(outcome).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        first.commit();
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let AgentLeaseBegin::Reserved(second) = second else {
            panic!("fresh replacement should reserve after the first commits");
        };
        assert_eq!(second.replaced().len(), 1);
        second.commit();
        handle.join().unwrap();
    }

    #[test]
    fn reconcile_keeps_oldest_live_owner_and_classifies_removals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        save_agent_leases(
            &path,
            &AgentLeaseRegistry {
                leases: vec![
                    lease(dir.path(), "new-live", 120),
                    lease(dir.path(), "old-live", 100),
                    lease(dir.path(), "stale", 90),
                ],
            },
        )
        .unwrap();

        let report =
            reconcile_agent_leases(&path, 200, |lease| lease.session_id != "stale").unwrap();
        assert_eq!(report.kept, vec![lease(dir.path(), "old-live", 100)]);
        assert_eq!(
            report.removed,
            vec![
                AgentLeaseRemoval {
                    lease: lease(dir.path(), "stale", 90),
                    reason: AgentLeaseRemovalReason::Stale,
                },
                AgentLeaseRemoval {
                    lease: lease(dir.path(), "new-live", 120),
                    reason: AgentLeaseRemovalReason::Duplicate,
                },
            ]
        );
    }

    #[test]
    fn release_cannot_remove_a_replacement_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        acquire_agent_lease(
            &path,
            lease(dir.path(), "new", 100),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        )
        .unwrap();
        assert!(!release_agent_lease(&path, "old").unwrap());
        assert!(release_agent_lease(&path, "new").unwrap());
        assert!(load_agent_leases(&path).unwrap().leases.is_empty());
    }

    fn control_lease(root: &Path, branch: &str, session: &str) -> AgentLease {
        AgentLease {
            key: AgentLeaseKey::new("bn", branch, root),
            session_id: session.into(),
            harness: "copilot".into(),
            kind: AgentLeaseKind::Control,
            tmux_target: format!("orch:{session}"),
            acquired_at: 100,
        }
    }

    fn agent_lease_on(root: &Path, branch: &str, session: &str) -> AgentLease {
        AgentLease {
            key: AgentLeaseKey::new("bn", branch, root),
            session_id: session.into(),
            harness: "copilot".into(),
            kind: AgentLeaseKind::Dispatch,
            tmux_target: format!("bn:{session}"),
            acquired_at: 100,
        }
    }

    #[test]
    fn is_reserved_branch_matches_main_master_case_insensitively() {
        assert!(is_reserved_branch("main"));
        assert!(is_reserved_branch("master"));
        assert!(is_reserved_branch(" Main "));
        assert!(is_reserved_branch("MASTER"));
        assert!(!is_reserved_branch("dispatch/fix"));
        assert!(!is_reserved_branch("main-menu"));
        assert!(!is_reserved_branch(""));
    }

    #[test]
    fn control_agent_is_refused_off_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let err = match begin_agent_lease(
            &path,
            control_lease(dir.path(), "dispatch/feature", "ctrl"),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        ) {
            Ok(_) => panic!("control off main must be refused"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("only run in a repo's main"));
        assert!(load_agent_leases(&path).unwrap().leases.is_empty());
    }

    #[test]
    fn control_agent_is_accepted_on_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        assert!(matches!(
            acquire_agent_lease(
                &path,
                control_lease(dir.path(), "main", "ctrl"),
                AgentLeaseAcquireMode::Refuse,
                100,
                |_| false,
            )
            .unwrap(),
            AgentLeaseAcquire::Acquired { .. }
        ));
    }

    #[test]
    fn non_control_agent_is_refused_on_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.json");
        let err = match begin_agent_lease(
            &path,
            agent_lease_on(dir.path(), "master", "ship"),
            AgentLeaseAcquireMode::Refuse,
            100,
            |_| false,
        ) {
            Ok(_) => panic!("non-control agent on main must be refused"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err
            .to_string()
            .contains("reserved for the bn control agent"));
        assert!(load_agent_leases(&path).unwrap().leases.is_empty());
    }

    #[test]
    fn lease_invariant_violations_flags_only_illegal_pairings() {
        let dir = tempfile::tempdir().unwrap();
        let clean = AgentLeaseRegistry {
            leases: vec![
                control_lease(dir.path(), "main", "ctrl"),
                agent_lease_on(dir.path(), "dispatch/feature", "ship"),
            ],
        };
        assert!(lease_invariant_violations(&clean).is_empty());

        let dirty = AgentLeaseRegistry {
            leases: vec![
                control_lease(dir.path(), "dispatch/off-main", "stray-ctrl"),
                agent_lease_on(dir.path(), "main", "stray-ship"),
            ],
        };
        let violations = lease_invariant_violations(&dirty);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().any(|v| v.contains("stray-ctrl")));
        assert!(violations.iter().any(|v| v.contains("stray-ship")));
    }
}
