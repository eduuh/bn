//! CLI bridge between the durable core lease registry and tmux process/window liveness.

use bn_core::agent::{
    agent_lease_registry_path, begin_agent_lease, load_agent_leases, reconcile_agent_leases,
    release_agent_lease, AgentLease, AgentLeaseAcquireMode, AgentLeaseBegin, AgentLeaseKey,
    AgentLeaseKind, AgentLeaseRemovalReason, AgentLeaseReservation as CoreAgentLeaseReservation,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) struct AgentLeaseRequest<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
    pub worktree: &'a Path,
    pub session_id: &'a str,
    pub harness: &'a str,
    pub kind: AgentLeaseKind,
    pub tmux_target: &'a str,
    pub replace: bool,
    /// The agent runs in a window that **already exists** — the caller's own pane, not one the
    /// launcher is about to create. `bn agent launch` sets this for an in-place launch; a dispatch
    /// that opens a fresh window leaves it false, so a live window at its target still means a
    /// leftover to clear (see [`remove_dead_launch_target`]).
    pub in_place: bool,
}

pub(crate) struct AgentLeaseReservation {
    inner: CoreAgentLeaseReservation,
}

impl AgentLeaseReservation {
    pub(crate) fn lease(&self) -> &AgentLease {
        self.inner.lease()
    }

    pub(crate) fn commit(self) {
        self.inner.commit();
    }

    pub(crate) fn rollback(self) -> Result<(), String> {
        self.inner
            .rollback()
            .map_err(|error| format!("agent lease rollback: {error}"))
    }
}

fn target_is_live(target: &str) -> bool {
    crate::proc::tmux_raw(&[
        "list-panes",
        "-t",
        &format!("={target}"),
        "-F",
        "#{pane_dead}",
    ])
    .is_some_and(|panes| panes.lines().any(|dead| dead.trim() == "0"))
}

fn window_id_for_target(target: &str) -> Option<String> {
    crate::proc::tmux_raw(&[
        "display-message",
        "-p",
        "-t",
        &format!("={target}"),
        "#{window_id}",
    ])
    .map(|id| id.trim().to_string())
    .filter(|id| !id.is_empty())
}

pub(crate) fn lease_is_live(lease: &AgentLease) -> bool {
    target_is_live(&lease.tmux_target)
}

fn reap(leases: &[AgentLease]) -> Result<(), String> {
    for lease in leases {
        let Some(window_id) = window_id_for_target(&lease.tmux_target) else {
            continue;
        };
        reap_window_ids(std::slice::from_ref(&window_id))?;
        if target_is_live(&lease.tmux_target) {
            return Err(format!(
                "prior agent window {} is still live after reap",
                lease.tmux_target
            ));
        }
    }
    Ok(())
}

fn current_window_id() -> Option<String> {
    std::env::var_os("TMUX").and_then(|_| {
        crate::proc::tmux_raw(&["display-message", "-p", "#{window_id}"])
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    })
}

fn reap_window_ids(window_ids: &[String]) -> Result<(), String> {
    let current = current_window_id();
    for window_id in window_ids {
        if current.as_ref() == Some(window_id) {
            return Err(format!(
                "refusing to reap the current agent window {window_id}; run recovery from another window"
            ));
        }
        crate::reap::kill_window_trees(std::slice::from_ref(window_id));
        if !crate::proc::tmux_ok(&["kill-window", "-t", window_id]) {
            return Err(format!("could not reap prior agent window {window_id}"));
        }
    }
    Ok(())
}

fn remove_dead_launch_target(target: &str) -> Result<(), String> {
    let Some(window_id) = window_id_for_target(target) else {
        return Ok(());
    };
    if target_is_live(target) {
        return Err(format!(
            "target window {target} is still live after lease reconciliation"
        ));
    }
    if !crate::proc::tmux_ok(&["kill-window", "-t", &window_id]) {
        return Err(format!(
            "could not remove dead target window {target} ({window_id})"
        ));
    }
    Ok(())
}

/// Stamp a leased window with everything needed to find it again: the session it hosts, the kind of
/// agent, and the repo/branch/worktree it belongs to.
///
/// The repo/branch/worktree options are what [`observed_agent_windows`] filters on and what the
/// supervisor's window join and `bn agent send`'s target resolution read — a window carrying only
/// `@bn_session_id` is invisible to all three. `wt::open_worktree_window_cmd_named` stamps the same
/// three when *it* creates a window; this covers the in-place case, where the window already existed
/// and the agent moved in.
pub(crate) fn mark_window(lease: &AgentLease) {
    let target = format!("={}", lease.tmux_target);
    for (option, value) in [
        ("@bn_session_id", lease.session_id.as_str()),
        ("@bn_agent_kind", lease.kind.label()),
        ("@bn_repo", lease.key.repo.as_str()),
        ("@bn_branch", lease.key.branch.as_str()),
        ("@bn_worktree", lease.key.worktree.as_str()),
    ] {
        let _ = crate::proc::tmux_run(&["set-window-option", "-t", &target, option, value]);
    }
}

/// Drop the lease held by `session_id` (idempotent). Called when a lease-holding agent is
/// deliberately stopped, so the worktree is free again immediately rather than waiting for the
/// next reconcile to notice the window is gone.
pub(crate) fn release(session_id: &str) -> Result<bool, String> {
    release_agent_lease(&agent_lease_registry_path(), session_id)
        .map_err(|error| format!("agent lease release: {error}"))
}

/// Who holds `worktree` right now, from the perspective of a launch about to happen in it.
pub(crate) enum WorktreeHolder {
    /// Nobody — the launch should take a lease of its own.
    Free,
    /// A lease taken for *this* window, moments ago, by the launcher that seeded this agent
    /// (`bn dispatch`, `bn control start`). Not a conflict, and not something to lease over.
    Ours,
    /// A different live agent. The launch must refuse rather than put two agents in one checkout.
    Other(Box<AgentLease>),
}

/// Read the lease state of `worktree` for the one-agent-per-worktree guard in `bn agent launch`.
///
/// The window comparison is what separates [`WorktreeHolder::Ours`] from a real conflict: a seeded
/// launch runs *inside* the window its launcher just leased, so without this every dispatched agent
/// would refuse itself. Outside tmux there is no window to compare and [`lease_is_live`] cannot see
/// the holder either, so the read degrades to `Free` — which is why the shell wrapper, not this, is
/// the enforcing layer for a hand-typed launch on a bare terminal.
pub(crate) fn worktree_holder(worktree: &Path) -> WorktreeHolder {
    let Ok(registry) = bn_core::agent::load_agent_leases(&agent_lease_registry_path()) else {
        return WorktreeHolder::Free;
    };
    let key = AgentLeaseKey::new("", "", worktree);
    let here = current_window_id();
    let mut holder = WorktreeHolder::Free;
    for lease in registry.leases {
        if lease.key.worktree != key.worktree || !lease_is_live(&lease) {
            continue;
        }
        if here.is_some() && window_id_for_target(&lease.tmux_target) == here {
            return WorktreeHolder::Ours;
        }
        holder = WorktreeHolder::Other(Box::new(lease));
    }
    holder
}

pub(crate) fn reserve(request: AgentLeaseRequest<'_>) -> Result<AgentLeaseReservation, String> {
    let lease = AgentLease {
        key: AgentLeaseKey::new(request.repo, request.branch, request.worktree),
        session_id: request.session_id.to_string(),
        harness: request.harness.to_string(),
        kind: request.kind,
        tmux_target: request.tmux_target.to_string(),
        acquired_at: bn_core::util::now_epoch_secs(),
    };
    let mode = if request.replace {
        AgentLeaseAcquireMode::Replace
    } else {
        AgentLeaseAcquireMode::Refuse
    };
    match begin_agent_lease(
        &agent_lease_registry_path(),
        lease,
        mode,
        bn_core::util::now_epoch_secs(),
        lease_is_live,
    )
    .map_err(|error| format!("agent lease: {error}"))?
    {
        AgentLeaseBegin::Held(owner) => Err(format!(
            "worktree already has a live {} agent (session {}, window {})",
            owner.kind.label(),
            owner.session_id,
            owner.tmux_target
        )),
        AgentLeaseBegin::Reserved(mut reservation) => {
            if let Err(error) = reap(reservation.replaced()) {
                let rollback = reservation.rollback();
                if let Err(rollback_error) = rollback {
                    return Err(format!("{error}; {rollback_error}"));
                }
                return Err(error);
            }
            if !request.in_place {
                if let Err(error) = remove_dead_launch_target(&reservation.lease().tmux_target) {
                    let rollback = reservation.rollback();
                    if let Err(rollback_error) = rollback {
                        return Err(format!("{error}; {rollback_error}"));
                    }
                    return Err(error);
                }
            }
            reservation.discard_replaced();
            Ok(AgentLeaseReservation { inner: reservation })
        }
    }
}

#[derive(Debug, Clone)]
struct ObservedAgentWindow {
    lease: AgentLease,
    window_id: String,
    created_at: i64,
}

fn observed_agent_windows() -> Vec<ObservedAgentWindow> {
    let format = "#{window_id}\t#{session_name}:#{window_name}\t#{@bn_repo}\t#{@bn_branch}\t#{@bn_worktree}\t#{pane_current_path}\t#{@bn_agent}\t#{@bn_session_id}\t#{@bn_agent_kind}\t#{window_created}\t#{pane_dead}";
    let Some(output) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", format]) else {
        return Vec::new();
    };
    parse_observed_agent_windows(&output)
}

fn parse_observed_agent_windows(output: &str) -> Vec<ObservedAgentWindow> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() < 11 {
                return None;
            }
            let window_id = fields[0];
            let target = fields[1];
            let repo = fields[2];
            let branch = fields[3];
            let worktree = if fields[4].is_empty() {
                fields[5]
            } else {
                fields[4]
            };
            let state = fields[6];
            let session_id = fields[7];
            let kind = fields[8];
            if repo.is_empty()
                || branch.is_empty()
                || worktree.is_empty()
                || (state.is_empty() && session_id.is_empty())
                || fields[10] != "0"
            {
                return None;
            }
            Some(ObservedAgentWindow {
                lease: AgentLease {
                    key: AgentLeaseKey::new(repo, branch, Path::new(worktree)),
                    session_id: if session_id.is_empty() {
                        format!("tmux:{target}")
                    } else {
                        session_id.to_string()
                    },
                    harness: "unknown".into(),
                    kind: match kind {
                        "control" => AgentLeaseKind::Control,
                        "today" => AgentLeaseKind::Today,
                        "pipeline-plan" => AgentLeaseKind::PipelinePlan,
                        "pipeline-investigate" => AgentLeaseKind::PipelineInvestigate,
                        _ if target.contains("-plan-") => AgentLeaseKind::PipelinePlan,
                        _ => AgentLeaseKind::Dispatch,
                    },
                    tmux_target: target.to_string(),
                    acquired_at: fields[9].parse().unwrap_or(0),
                },
                window_id: window_id.to_string(),
                created_at: fields[9].parse().unwrap_or(0),
            })
        })
        .collect()
}

fn current_target() -> Option<String> {
    std::env::var_os("TMUX").and_then(|_| {
        crate::proc::tmux_raw(&["display-message", "-p", "#{session_name}:#{window_name}"])
            .map(|target| target.trim().to_string())
            .filter(|target| !target.is_empty())
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AgentLeaseReconcileReport {
    pub conflicts: usize,
    pub duplicate_windows_reaped: usize,
    pub stale_leases_removed: usize,
    pub errors: Vec<String>,
}

pub(crate) fn reconcile() -> AgentLeaseReconcileReport {
    let path = agent_lease_registry_path();
    let now = bn_core::util::now_epoch_secs();
    let mut report = AgentLeaseReconcileReport::default();
    match reconcile_agent_leases(&path, now, lease_is_live) {
        Ok(core) => {
            report.stale_leases_removed = core
                .removed
                .iter()
                .filter(|removal| removal.reason == AgentLeaseRemovalReason::Stale)
                .count();
            // Actual duplicate windows are reconciled below after choosing a keeper that never
            // kills the caller's current window. Registry-only duplicates need no process action.
        }
        Err(error) => {
            report
                .errors
                .push(format!("could not reconcile agent lease registry: {error}"));
            return report;
        }
    }

    let registry = load_agent_leases(&path).unwrap_or_default();
    let leased_targets = registry
        .leases
        .iter()
        .map(|lease| (lease.key.clone(), lease.tmux_target.clone()))
        .collect::<BTreeMap<_, _>>();
    let current = current_target();
    let current_id = current_window_id();
    let mut grouped = BTreeMap::<AgentLeaseKey, Vec<ObservedAgentWindow>>::new();
    for window in observed_agent_windows() {
        grouped
            .entry(window.lease.key.clone())
            .or_default()
            .push(window);
    }

    for (key, mut windows) in grouped {
        windows.sort_by_key(|window| (window.created_at, window.lease.tmux_target.clone()));
        let keeper_index = current_id
            .as_ref()
            .and_then(|window_id| {
                windows
                    .iter()
                    .position(|window| &window.window_id == window_id)
            })
            .or_else(|| {
                current.as_ref().and_then(|target| {
                    windows
                        .iter()
                        .position(|window| &window.lease.tmux_target == target)
                })
            })
            .or_else(|| {
                leased_targets.get(&key).and_then(|target| {
                    windows
                        .iter()
                        .position(|window| &window.lease.tmux_target == target)
                })
            })
            .unwrap_or(0);
        let preferred = windows[keeper_index].lease.clone();
        let mut conflict_recorded = windows.len() > 1;
        if conflict_recorded {
            report.conflicts += 1;
        }
        let mut candidate = leased_targets
            .get(&key)
            .and_then(|target| {
                windows
                    .iter()
                    .find(|window| &window.lease.tmux_target == target)
                    .map(|window| window.lease.clone())
            })
            .unwrap_or(preferred);
        let mut reconciled = false;
        let errors_before = report.errors.len();
        for _ in 0..3 {
            let transaction = begin_agent_lease(
                &path,
                candidate.clone(),
                AgentLeaseAcquireMode::Replace,
                now,
                lease_is_live,
            );
            let AgentLeaseBegin::Reserved(mut reservation) = (match transaction {
                Ok(transaction) => transaction,
                Err(error) => {
                    report.errors.push(format!(
                        "could not reserve owner for {}: {error}",
                        candidate.tmux_target
                    ));
                    break;
                }
            }) else {
                unreachable!("replace mode always reserves")
            };
            if let Some(owner) = reservation
                .replaced()
                .iter()
                .find(|owner| owner.tmux_target != candidate.tmux_target && lease_is_live(owner))
                .cloned()
            {
                let _ = reservation.rollback();
                if !conflict_recorded {
                    report.conflicts += 1;
                    conflict_recorded = true;
                }
                candidate = owner;
                continue;
            }

            let keep_id = windows
                .iter()
                .filter(|window| window.lease.tmux_target == candidate.tmux_target)
                .min_by_key(|window| {
                    (
                        current_id.as_ref() != Some(&window.window_id),
                        window.created_at,
                        window.window_id.clone(),
                    )
                })
                .map(|window| window.window_id.clone());
            let duplicates = windows
                .iter()
                .filter(|window| Some(&window.window_id) != keep_id.as_ref())
                .map(|window| window.window_id.clone())
                .collect::<Vec<_>>();
            match reap_window_ids(&duplicates) {
                Ok(()) => {
                    reservation.discard_replaced();
                    reservation.commit();
                    report.duplicate_windows_reaped += duplicates.len();
                    reconciled = true;
                }
                Err(error) => {
                    let _ = reservation.rollback();
                    report.errors.push(error);
                }
            }
            break;
        }
        if !reconciled && report.errors.len() == errors_before {
            report.errors.push(format!(
                "agent ownership changed repeatedly while reconciling {}/{}",
                key.repo, key.branch
            ));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_windows_ignore_plain_shells_and_keep_agent_markers() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().to_string_lossy();
        let output = format!(
            "@1\tbn:shell\tbn\tdispatch-x\t{wt}\t{wt}\t\t\t\t10\t0\n\
             @2\tbn:dispatch\tbn\tdispatch-x\t{wt}\t{wt}\tworking\t\tdispatch\t11\t0\n\
             @3\tbn:plan\tbn\tdispatch-x\t{wt}\t{wt}\t\tsession-plan\tpipeline-plan\t12\t0\n\
             @4\tbn:dead\tbn\tdispatch-x\t{wt}\t{wt}\tworking\tdead\tdispatch\t9\t1\n"
        );

        let windows = parse_observed_agent_windows(&output);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].lease.tmux_target, "bn:dispatch");
        assert_eq!(windows[0].window_id, "@2");
        assert_eq!(windows[0].lease.session_id, "tmux:bn:dispatch");
        assert_eq!(windows[0].lease.kind, AgentLeaseKind::Dispatch);
        assert_eq!(windows[1].lease.tmux_target, "bn:plan");
        assert_eq!(windows[1].lease.session_id, "session-plan");
        assert_eq!(windows[1].lease.kind, AgentLeaseKind::PipelinePlan);
    }
}
