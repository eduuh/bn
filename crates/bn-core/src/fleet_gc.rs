//! Fleet garbage-collection policy (#503) — the pure reconciliation that decides whether a live
//! agent process is a stale straggler to reap, or a healthy agent to leave alone.
//!
//! This is the **pure core**, side-effect-free like the rest of bn-core: the CLI layer
//! (`bn-cli/src/reap.rs`) captures the live signals — tmux windows, worktree dirs, PR state,
//! process trees — and feeds them in here; this module only *classifies*. Keeping the policy
//! pure is the whole point: the "reap this / spare that" decision — the one that must **never**
//! kill an actively-working agent, the orchestrator, or an agent with uncommitted work — lives in
//! code and is unit-tested, instead of being re-derived ad-hoc on every sweep.
//!
//! The shape mirrors [`crate::orchestrate`]'s pane-state machine: an observation struct
//! ([`AgentObservation`]) + a total classifier ([`classify`]) → a [`Disposition`].

use serde::Serialize;
use std::path::Path;

/// The PR backing an agent's branch, as reconciliation sees it. [`PrStatus::Unknown`] is what a
/// failed lookup (offline / not a hosted repo) reads as — it is **never** a reap trigger, so a
/// transient `gh` failure can't strand an agent as "closed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PrStatus {
    /// No PR opened yet for the branch — work still in flight.
    None,
    /// An open PR — work in flight.
    Open,
    /// Merged — the work landed; a still-running agent is a leftover.
    Merged,
    /// Closed without merging — abandoned.
    Closed,
    /// Couldn't determine (offline / not hosted). Never a reap trigger.
    Unknown,
}

/// Why an agent process is a reap candidate — the stale signal, in the priority [`classify`] tests
/// it. Serialized kebab-case for the `bn fleet reap --json` surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReapReason {
    /// Its PR merged — the work landed; the process is a leftover burning CPU.
    PrMerged,
    /// Its PR was closed without merging — the work is abandoned.
    PrClosed,
    /// Its worktree directory is gone — there is nothing left for it to work on.
    WorktreeGone,
    /// No live tmux window — a straggler that reparented away from its (killed) pane, exactly the
    /// no-window/no-PR runaway from a prior dispatch wave that #503 is about.
    WindowGone,
}

impl ReapReason {
    /// The kebab-case label used in dry-run / JSON / event surfaces.
    pub fn label(self) -> &'static str {
        match self {
            ReapReason::PrMerged => "pr-merged",
            ReapReason::PrClosed => "pr-closed",
            ReapReason::WorktreeGone => "worktree-gone",
            ReapReason::WindowGone => "window-gone",
        }
    }
}

/// Why an agent is spared — the keep signal. Serialized kebab-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeepReason {
    /// A legit in-flight agent: live window, present worktree, PR open (or not opened yet).
    Healthy,
    /// Actively working — a live spinner in its pane, or fresh (`active`) fleet health. Never
    /// surprise-killed; `--force` is required to reap it.
    Working,
    /// The orchestrator / `--resume` supervisor session — sacrosanct, never reaped (see
    /// [`classify`]).
    Orchestrator,
    /// Carries uncommitted (dirty) or unpushed work — spared unless `--force`, so a reap never
    /// discards work.
    Uncommitted,
    /// Too recently active to reap yet — a launch/teardown grace window so an agent isn't reaped
    /// mid-transition. Spared unless `--force`.
    Grace,
}

impl KeepReason {
    /// The kebab-case label used in dry-run / JSON / event surfaces.
    pub fn label(self) -> &'static str {
        match self {
            KeepReason::Healthy => "healthy",
            KeepReason::Working => "working",
            KeepReason::Orchestrator => "orchestrator",
            KeepReason::Uncommitted => "uncommitted",
            KeepReason::Grace => "grace",
        }
    }
}

/// The reconciliation verdict for one agent — keep it, or reap it (with the reason either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Disposition {
    /// Leave the agent running.
    Keep { reason: KeepReason },
    /// Terminate the agent's full process tree and prune its (now-dead) window.
    Reap { reason: ReapReason },
}

impl Disposition {
    /// Whether this verdict reaps.
    pub fn is_reap(self) -> bool {
        matches!(self, Disposition::Reap { .. })
    }

    /// The kebab-case label of whichever reason this verdict carries — for one-line surfaces.
    pub fn reason_label(self) -> &'static str {
        match self {
            Disposition::Keep { reason } => reason.label(),
            Disposition::Reap { reason } => reason.label(),
        }
    }
}

/// The observed state of one live agent process — everything [`classify`] needs, captured by the
/// CLI from tmux / git / gh / the process table. Deliberately free of any handle (pid, window id)
/// so the policy stays pure; the CLI keeps those alongside and acts on a [`Disposition::Reap`].
#[derive(Debug, Clone)]
pub struct AgentObservation {
    /// A live (non-dead) tmux window exists for this agent's `repo:branch`. `false` ⇒ the window
    /// is gone (or only a `remain-on-exit` dead pane lingers).
    pub has_live_window: bool,
    /// The agent's worktree directory still exists on disk.
    pub worktree_exists: bool,
    /// The PR backing the branch.
    pub pr: PrStatus,
    /// The agent is actively working — a live spinner in its pane, or fresh (`active`) fleet
    /// health. Only ever `true` for an agent that still has a live window (you can't observe a
    /// spinner without a pane), so a windowless straggler is never mistaken for working.
    pub is_working: bool,
    /// The worktree carries uncommitted (dirty tree) or unpushed (ahead-of-remote) work.
    pub has_uncommitted: bool,
    /// This is the orchestrator / `--resume` supervisor session — never reap it.
    pub is_orchestrator: bool,
    /// Seconds since the agent's last recorded activity. `0` when unknown (treated as very fresh,
    /// so the grace window protects it).
    pub idle_secs: i64,
}

impl Default for AgentObservation {
    /// A healthy, freshly-launched agent: live window, present worktree, no PR yet, not idle. The
    /// baseline a test (or the CLI) overrides field-by-field.
    fn default() -> Self {
        AgentObservation {
            has_live_window: true,
            worktree_exists: true,
            pr: PrStatus::None,
            is_working: false,
            has_uncommitted: false,
            is_orchestrator: false,
            idle_secs: 0,
        }
    }
}

/// Default grace: 5 minutes of session-log quiet before a stale agent is eligible to reap. Matches
/// [`crate::agent::DEFAULT_IDLE_MINS`] — an agent quieter than the fleet's own "idle" threshold has
/// clearly stopped, so reaping a stale one past it won't interrupt a live turn.
pub const DEFAULT_MIN_IDLE_SECS: i64 = 5 * 60;

/// Knobs for a reap pass.
#[derive(Debug, Clone, Copy)]
pub struct ReapPolicy {
    /// Override the working / uncommitted / grace safety gates. **Never** overrides the
    /// orchestrator guard — a GC must not nuke its own supervisor.
    pub force: bool,
    /// Minimum idle seconds before a stale agent is eligible — a grace window so a just-launched
    /// or just-finishing agent isn't reaped mid-transition. Bypassed by `force`.
    pub min_idle_secs: i64,
}

impl Default for ReapPolicy {
    fn default() -> Self {
        ReapPolicy {
            force: false,
            min_idle_secs: DEFAULT_MIN_IDLE_SECS,
        }
    }
}

/// Classify one agent → keep or reap. **Pure and total** — the single source of truth for the
/// #503 reap decision.
///
/// The order is what makes it safe:
/// 1. **Orchestrator is sacrosanct** — never reaped, even with `--force`.
/// 2. **Not stale** — a live window, a present worktree, and a PR that's open or not-yet-opened
///    (or whose state is unknown) ⇒ a healthy in-flight agent, kept.
/// 3. **Stale, but guarded** — a stale agent is still spared (unless `--force`) when it is actively
///    working, carries uncommitted/unpushed work, or is still inside the idle grace window.
/// 4. **Reap** — otherwise, tagged with the stale reason.
pub fn classify(obs: &AgentObservation, policy: &ReapPolicy) -> Disposition {
    // (1) The orchestrator / `--resume` supervisor is never reaped — force does not apply. Killing
    // it would take down the whole fleet's driver, the opposite of a GC's job.
    if obs.is_orchestrator {
        return Disposition::Keep {
            reason: KeepReason::Orchestrator,
        };
    }

    // (2) The stale signal, in priority order. `Open` / `None` / `Unknown` PR never triggers a
    // reap on PR grounds, so a transient `gh` failure (Unknown) can't strand a live agent.
    let reason = if obs.pr == PrStatus::Merged {
        ReapReason::PrMerged
    } else if obs.pr == PrStatus::Closed {
        ReapReason::PrClosed
    } else if !obs.worktree_exists {
        ReapReason::WorktreeGone
    } else if !obs.has_live_window {
        ReapReason::WindowGone
    } else {
        return Disposition::Keep {
            reason: KeepReason::Healthy,
        };
    };

    // (3) Safety gates. `--force` overrides all of these (but not the orchestrator guard above).
    if !policy.force {
        // Never kill an agent mid-turn.
        if obs.is_working {
            return Disposition::Keep {
                reason: KeepReason::Working,
            };
        }
        // Never discard uncommitted/unpushed work.
        if obs.has_uncommitted {
            return Disposition::Keep {
                reason: KeepReason::Uncommitted,
            };
        }
        // Grace: don't reap something that only just went quiet (launch/teardown race).
        if obs.idle_secs < policy.min_idle_secs {
            return Disposition::Keep {
                reason: KeepReason::Grace,
            };
        }
    }

    // (4) Reap the stale straggler.
    Disposition::Reap { reason }
}

/// Given the managed `worktree_dir` and a process's working directory, extract the `(repo, branch)`
/// the cwd belongs to — `Some` iff `cwd` is `worktree_dir/<repo>/<branch>` or a descendant of it.
/// Pure so the "is this process rooted in a managed worktree, and which one?" mapping is testable
/// without a live process table. The sanitized branch dir name is returned verbatim (dispatch
/// stores `feat/x` as `feat-x`); the caller reconciles it against tmux/PR state.
pub fn worktree_repo_branch(worktree_dir: &Path, cwd: &Path) -> Option<(String, String)> {
    let rel = cwd.strip_prefix(worktree_dir).ok()?;
    let mut comps = rel.components();
    let repo = comps.next()?.as_os_str().to_str()?.to_string();
    let branch = comps.next()?.as_os_str().to_str()?.to_string();
    if repo.is_empty() || branch.is_empty() {
        return None;
    }
    Some((repo, branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A stale agent (window gone), quiet past the grace window, nothing to protect → reaped.
    fn stale_idle() -> AgentObservation {
        AgentObservation {
            has_live_window: false,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        }
    }

    #[test]
    fn healthy_agent_is_kept() {
        let obs = AgentObservation::default();
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Healthy
            }
        );
    }

    #[test]
    fn window_gone_idle_is_reaped() {
        // The #503 straggler: process alive, no window, past the grace window, nothing to save.
        assert_eq!(
            classify(&stale_idle(), &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::WindowGone
            }
        );
    }

    #[test]
    fn worktree_gone_is_reaped() {
        let obs = AgentObservation {
            worktree_exists: false,
            has_live_window: true, // window alone doesn't save it — the worktree is gone
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::WorktreeGone
            }
        );
    }

    #[test]
    fn merged_pr_is_reaped_even_with_live_window() {
        let obs = AgentObservation {
            pr: PrStatus::Merged,
            has_live_window: true,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::PrMerged
            }
        );
    }

    #[test]
    fn closed_pr_is_reaped() {
        let obs = AgentObservation {
            pr: PrStatus::Closed,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::PrClosed
            }
        );
    }

    #[test]
    fn pr_priority_beats_window_and_worktree() {
        // A merged PR is reported over the also-true window/worktree-gone signals.
        let obs = AgentObservation {
            pr: PrStatus::Merged,
            has_live_window: false,
            worktree_exists: false,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::PrMerged
            }
        );
    }

    #[test]
    fn open_pr_keeps_a_windowed_agent() {
        let obs = AgentObservation {
            pr: PrStatus::Open,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Healthy
            }
        );
    }

    #[test]
    fn unknown_pr_never_triggers_reap() {
        // A `gh` failure reads as Unknown; a healthy windowed agent must not be reaped on it.
        let obs = AgentObservation {
            pr: PrStatus::Unknown,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Healthy
            }
        );
    }

    #[test]
    fn working_agent_is_never_surprise_killed() {
        // Even with a merged PR, an agent mid-turn is spared (until --force).
        let obs = AgentObservation {
            pr: PrStatus::Merged,
            is_working: true,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Working
            }
        );
    }

    #[test]
    fn uncommitted_work_is_spared() {
        let obs = AgentObservation {
            pr: PrStatus::Closed,
            has_uncommitted: true,
            idle_secs: DEFAULT_MIN_IDLE_SECS + 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Uncommitted
            }
        );
    }

    #[test]
    fn grace_window_spares_a_freshly_quiet_agent() {
        // Stale (window gone) but only just went quiet — spared by the grace window.
        let obs = AgentObservation {
            has_live_window: false,
            idle_secs: DEFAULT_MIN_IDLE_SECS - 1,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Grace
            }
        );
    }

    #[test]
    fn orchestrator_is_never_reaped_even_with_force() {
        // Sacrosanct: even a fully-stale, forced pass keeps the orchestrator/--resume session.
        let obs = AgentObservation {
            is_orchestrator: true,
            has_live_window: false,
            worktree_exists: false,
            pr: PrStatus::Closed,
            idle_secs: DEFAULT_MIN_IDLE_SECS * 100,
            ..AgentObservation::default()
        };
        let forced = ReapPolicy {
            force: true,
            ..ReapPolicy::default()
        };
        assert_eq!(
            classify(&obs, &forced),
            Disposition::Keep {
                reason: KeepReason::Orchestrator
            }
        );
    }

    #[test]
    fn force_overrides_working_uncommitted_and_grace() {
        let forced = ReapPolicy {
            force: true,
            ..ReapPolicy::default()
        };
        // Working + uncommitted + inside grace, but stale (closed PR) → reaped under --force.
        let obs = AgentObservation {
            pr: PrStatus::Closed,
            is_working: true,
            has_uncommitted: true,
            idle_secs: 0,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &forced),
            Disposition::Reap {
                reason: ReapReason::PrClosed
            }
        );
    }

    #[test]
    fn force_does_not_reap_a_healthy_agent() {
        // --force only overrides the *safety gates*; it never reaps a non-stale agent.
        let forced = ReapPolicy {
            force: true,
            ..ReapPolicy::default()
        };
        assert_eq!(
            classify(&AgentObservation::default(), &forced),
            Disposition::Keep {
                reason: KeepReason::Healthy
            }
        );
    }

    #[test]
    fn worktree_repo_branch_extracts_managed_pair() {
        // Uses the same worktree_dir a test Config carries, so the mapping matches production.
        let cfg = crate::test_support::config_with_roots("/n", "/n", "/wt", "testhost");
        let wt = &cfg.worktree_dir;
        assert_eq!(
            worktree_repo_branch(wt, &wt.join("bn").join("dispatch-fleet-gc-reap-503")),
            Some(("bn".to_string(), "dispatch-fleet-gc-reap-503".to_string()))
        );
        // A descendant (the agent cd'd into a subdir) still maps to the worktree.
        assert_eq!(
            worktree_repo_branch(wt, &wt.join("bn").join("feat-x").join("crates/bn-core")),
            Some(("bn".to_string(), "feat-x".to_string()))
        );
    }

    #[test]
    fn worktree_repo_branch_rejects_outside_paths() {
        let wt = PathBuf::from("/wt");
        assert_eq!(
            worktree_repo_branch(&wt, &PathBuf::from("/Users/edd")),
            None
        );
        assert_eq!(worktree_repo_branch(&wt, &PathBuf::from("/wt")), None);
        // A bare repo dir (only one segment under worktree_dir) is not a worktree.
        assert_eq!(worktree_repo_branch(&wt, &PathBuf::from("/wt/bn")), None);
    }
}
