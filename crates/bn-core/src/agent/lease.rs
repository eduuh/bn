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
            branch: normalized_branch(branch),
            worktree: worktree.to_string_lossy().into_owned(),
        }
    }
}

/// The form a lease key stores a branch in: trimmed, `/` flattened to `-`, lowercased — so
/// `feature/X` and `feature-x` name the one checkout, exactly as the worktree dirs already do.
///
/// Exposed because anything comparing an agent lease's branch against a branch recorded *elsewhere*
/// — the warm pool's `leases.json`, a tmux window stamp — has to compare the same normalization.
/// Comparing a raw name against a normalized one reads as two different tenants, which is one of
/// the ways a slot with a live agent in it can look free to whoever asks next.
pub fn normalized_branch(branch: &str) -> String {
    branch.trim().replace('/', "-").to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentLeaseKind {
    Dispatch,
    PipelineInvestigate,
    PipelinePlan,
    /// The `bn control` coordination agent — reserved to the repo's controller pool slot.
    Control,
    /// Today's agent — the day's hosted session, rooted in the configured personal-notes checkout
    /// and leased against that repo/branch like any other agent. It opens a fixed `today` window in
    /// that repo's own session, and carries no placement rule beyond the universal "not the main
    /// worktree".
    Today,
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
            AgentLeaseKind::Today => "today",
        }
    }
}

/// A repo's primary branch. Never dispatched onto, never reaped, and never the base of new work —
/// the guard dispatch, reap, and the sentinel share. Compared case-insensitively so a normalized
/// lease key ("main") and a raw branch name ("Main") agree.
pub fn is_reserved_branch(branch: &str) -> bool {
    let b = branch.trim().to_ascii_lowercase();
    b == "main" || b == "master"
}

/// The repo's **main slot** — `…/_pool/1`, reserved and never handed out to a worker (they lease
/// `2..=size`). It is where main-branch work happens: the checkout you land in when you pick "main"
/// in the picker, and the `bn control` coordination agent's home. Both want the same thing, a
/// detached checkout sitting at the repo's base that ships nothing of its own, which is why one
/// reserved slot serves both.
///
/// It exists so [`is_main_worktree_path`]'s checkout never has to: the stow-source `main` worktree
/// stays clean and unvisited, and the work that used to dirty it happens here instead.
pub const MAIN_POOL_SLOT: u32 = 1;

/// The stow-source main worktree — `…/worktree/<repo>/main` — which must host no agent at all and
/// stay clean on `main`. Matches the shape the repo's `pre-commit` hook already refuses commits in
/// (`case "$top" in */worktree/*/main)`), so the agent rule and the commit rule read the same
/// checkout the same way.
///
/// It is deliberately not a place you go: the pickers route "main" to [`MAIN_POOL_SLOT`] instead,
/// and the `main-sync` job hard-resets this checkout to `origin/<default>` every 15 minutes. Its
/// job is to *exist* — a clean, current checkout for stow, hooks and tooling to read.
pub fn is_main_worktree_path(worktree: &Path) -> bool {
    let mut parts = worktree.components().rev().filter_map(|c| match c {
        std::path::Component::Normal(part) => part.to_str(),
        _ => None,
    });
    let Some(leaf) = parts.next() else {
        return false;
    };
    if !is_reserved_branch(leaf) {
        return false;
    }
    // …/worktree/<repo>/<leaf>: skip the repo segment, then require the `worktree` root.
    parts
        .next()
        .is_some_and(|_repo| parts.next() == Some("worktree"))
}

/// The reserved main slot — `…/_pool/1`, [`MAIN_POOL_SLOT`]. The pool lays slots out as
/// `<root>/worktree/<repo>/_pool/<n>`, so the check is the slot number under a `_pool` parent.
pub fn is_controller_slot_path(worktree: &Path) -> bool {
    let mut parts = worktree.components().rev().filter_map(|c| match c {
        std::path::Component::Normal(part) => part.to_str(),
        _ => None,
    });
    let Some(slot) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
        return false;
    };
    slot == MAIN_POOL_SLOT && parts.next() == Some("_pool")
}

/// The placement invariant checked before every acquisition, keyed on the worktree the lease
/// actually points at rather than its branch name:
///
/// 1. the main worktree hosts no agent, of any kind — work happens in a leased pool slot;
/// 2. a control agent runs only in the reserved main slot, and that slot takes no other kind of
///    agent (it is yours and the controller's — a dispatch belongs in a worker slot).
///
/// Returns a human-readable refusal when the placement is illegal, so the message surfaces verbatim
/// to the operator.
fn validate_kind_placement(kind: AgentLeaseKind, key: &AgentLeaseKey) -> Result<(), String> {
    let worktree = Path::new(&key.worktree);
    if is_main_worktree_path(worktree) {
        return Err(format!(
            "{} is the main worktree — it hosts no agents and stays clean on '{}'. Work on main \
             happens in the main slot (_pool/{MAIN_POOL_SLOT}); dispatch work leases a worker \
             slot: bn dispatch \"<what you're doing>\"",
            key.worktree, key.branch
        ));
    }
    match (kind, is_controller_slot_path(worktree)) {
        (AgentLeaseKind::Control, false) => Err(format!(
            "the bn control agent may only run in the reserved main slot (_pool/{MAIN_POOL_SLOT}), not {}",
            key.worktree
        )),
        (kind, true) if kind != AgentLeaseKind::Control => Err(format!(
            "_pool/{MAIN_POOL_SLOT} is the reserved main slot (yours and bn control's) — a {} \
             agent cannot occupy it",
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

    /// The prior owners have been successfully reaped; a later launch failure must leave the
    /// worktree unowned rather than restore leases for processes that no longer exist. Scoped to
    /// the worktree for the same reason ownership is: a rollback must not resurrect a *different*
    /// branch's lease on a checkout whose agent we just killed.
    pub fn discard_replaced(&mut self) {
        self.original
            .leases
            .retain(|current| current.key.worktree != self.lease.key.worktree);
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

/// Agent-lease invariant violations for `bn doctor`: every lease whose placement breaks the
/// main-is-agent-free / controller-slot rule, rendered `repo/branch (session): reason`. Empty in a
/// healthy fleet — the acquire-time guard in [`begin_agent_lease`] prevents new ones — so a
/// non-empty result means a lease predating the guard or hand-edited state to reconcile. This is
/// also what surfaces leases written under the old main↔control rule after an upgrade.
pub fn lease_invariant_violations(registry: &AgentLeaseRegistry) -> Vec<String> {
    registry
        .leases
        .iter()
        .filter_map(|lease| {
            validate_kind_placement(lease.kind, &lease.key)
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
/// Stale leases on that worktree are reclaimed. `Replace` additionally returns every live prior
/// owner so the caller can reap it before launching the replacement; `Refuse` leaves the oldest
/// live owner untouched and reports it.
///
/// **Ownership is scoped to the worktree, not to `(repo, branch, worktree)`.** The invariant this
/// registry exists to hold is *one agent per checkout*, and keying the conflict on the whole key
/// let two branches co-own one directory without the registry noticing — which is how a pool slot
/// came to host two live agents editing each other's files. A different branch in the same
/// checkout is the conflict, not an unrelated tenancy. Our own `session_id` never conflicts with
/// itself, so re-reserving a lease we already hold stays idempotent.
pub fn begin_agent_lease(
    path: &Path,
    lease: AgentLease,
    mode: AgentLeaseAcquireMode,
    now: i64,
    is_live: impl Fn(&AgentLease) -> bool,
) -> std::io::Result<AgentLeaseBegin> {
    // Enforce the placement invariant before touching the registry: a rule violation is a policy
    // refusal, not a transient conflict, so fail fast without taking the lock.
    if let Err(reason) = validate_kind_placement(lease.kind, &lease.key) {
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
        .filter(|current| current.key.worktree == lease.key.worktree)
        .filter(|current| current.session_id != lease.session_id)
        .filter(|current| is_authoritative(current, now, &is_live))
        .cloned()
        .collect::<Vec<_>>();
    matching.sort_by_key(|current| (current.acquired_at, current.session_id.clone()));

    if mode == AgentLeaseAcquireMode::Refuse {
        if let Some(owner) = matching.first() {
            return Ok(AgentLeaseBegin::Held(owner.clone()));
        }
    }

    // One checkout, one lease: drop every entry for this worktree — the live owners we just
    // collected as `replaced`, our own prior entry, and any dead leftovers (including the
    // different-branch pairs older registries were free to accumulate) — then write ours.
    registry
        .leases
        .retain(|current| current.key.worktree != lease.key.worktree);
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
