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
    /// A human is at this agent's window — scrolling its scrollback, looking at it in an attached
    /// client, or having done either within [`crate::tmux::ATTENDED_DECAY_SECS`]. Spared unless
    /// `--force`: reviewing an agent's work is *work*, and a GC that pulls the pane out from under a
    /// reviewer is worse than one that leaves a finished lane running for another minute.
    Attended,
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
            KeepReason::Attended => "attended",
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
    /// A human is at this agent's window, now or within the decay window (see
    /// [`crate::tmux::observe_presence`]). Only observable for an agent that still has a live
    /// window; a windowless straggler is never attended.
    pub attended: bool,
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
            attended: false,
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
/// 3. **Stale, but guarded** — a stale agent is still spared (unless `--force`) when a human is
///    watching its window, it is actively working, it carries uncommitted/unpushed work, or it is
///    still inside the idle grace window.
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
        // Never take a pane away from the human reading it. This gate comes first because it is the
        // only one that outranks *every* stale signal: a merged PR, a deleted worktree and a closed
        // window all still describe an agent whose transcript someone is in the middle of reviewing,
        // and the other guards (working / uncommitted) are all blind to a read-only agent — which
        // never dirties a tree and never opens a PR, and so was permanently reap-eligible.
        if obs.attended {
            return Disposition::Keep {
                reason: KeepReason::Attended,
            };
        }
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

/// The directory name the warm pool keeps its slots under — `worktree/<repo>/_pool/<n>`. A branch
/// never lives at that path, so it is the marker that separates a pooled agent from one in a
/// conventional per-branch worktree.
pub const POOL_DIR_NAME: &str = "_pool";

/// Given the managed `worktree_dir` and a process's working directory, extract the `(repo, branch)`
/// the cwd belongs to — `Some` iff `cwd` is `worktree_dir/<repo>/<branch>` or a descendant of it.
/// Pure so the "is this process rooted in a managed worktree, and which one?" mapping is testable
/// without a live process table. The sanitized branch dir name is returned verbatim (dispatch
/// stores `feat/x` as `feat-x`); the caller reconciles it against tmux/PR state.
///
/// A pool slot (`worktree_dir/<repo>/_pool/<n>`) is deliberately **not** a match: `_pool` is a
/// directory, not a branch, and answering `("bn", "_pool")` is worse than answering nothing —
/// every pooled agent then shares one meaningless identity, its PR is never looked up and its
/// fleet health never matches, so a reap pass silently skips exactly the agents that now hold the
/// whole fleet. Ask [`pool_slot`] instead and resolve the tenant through the pool's own ledger.
pub fn worktree_repo_branch(worktree_dir: &Path, cwd: &Path) -> Option<(String, String)> {
    let rel = cwd.strip_prefix(worktree_dir).ok()?;
    let mut comps = rel.components();
    let repo = comps.next()?.as_os_str().to_str()?.to_string();
    let branch = comps.next()?.as_os_str().to_str()?.to_string();
    if repo.is_empty() || branch.is_empty() || branch == POOL_DIR_NAME {
        return None;
    }
    Some((repo, branch))
}

/// `(repo, slot)` when `cwd` is inside a warm-pool slot (`worktree_dir/<repo>/_pool/<n>`), else
/// `None` — the pooled counterpart of [`worktree_repo_branch`]. The branch a slot currently holds
/// isn't in the path, so the caller resolves it from the pool ledger (or the checkout's HEAD); this
/// only answers "which slot is this, if any".
pub fn pool_slot(worktree_dir: &Path, cwd: &Path) -> Option<(String, u32)> {
    let rel = cwd.strip_prefix(worktree_dir).ok()?;
    let mut comps = rel.components();
    let repo = comps.next()?.as_os_str().to_str()?.to_string();
    if repo.is_empty() || comps.next()?.as_os_str().to_str()? != POOL_DIR_NAME {
        return None;
    }
    let slot = comps.next()?.as_os_str().to_str()?.parse().ok()?;
    Some((repo, slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long-idle agent whose PR merged, with nothing uncommitted — the shape a finished lane has
    /// once its work lands, and the exact shape of an agent whose transcript you are reading.
    fn finished_and_quiet() -> AgentObservation {
        AgentObservation {
            pr: PrStatus::Merged,
            idle_secs: DEFAULT_MIN_IDLE_SECS * 10,
            ..AgentObservation::default()
        }
    }

    #[test]
    fn a_watched_agent_is_never_reaped() {
        let obs = AgentObservation {
            attended: true,
            ..finished_and_quiet()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Attended
            }
        );
    }

    #[test]
    fn the_same_agent_is_reaped_once_nobody_is_watching() {
        assert_eq!(
            classify(&finished_and_quiet(), &ReapPolicy::default()),
            Disposition::Reap {
                reason: ReapReason::PrMerged
            }
        );
    }

    /// The read-only agent this gate exists for: it investigates, opens no PR and dirties no tree,
    /// so `is_working` and `has_uncommitted` are both false and every other guard waves it through.
    /// Reading its findings is the only thing keeping it alive.
    #[test]
    fn a_read_only_agent_under_review_survives_a_dead_window() {
        let obs = AgentObservation {
            has_live_window: false,
            attended: true,
            idle_secs: DEFAULT_MIN_IDLE_SECS * 10,
            ..AgentObservation::default()
        };
        assert_eq!(
            classify(&obs, &ReapPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Attended
            }
        );
    }

    #[test]
    fn force_still_reaps_a_watched_agent() {
        let obs = AgentObservation {
            attended: true,
            ..finished_and_quiet()
        };
        let policy = ReapPolicy {
            force: true,
            ..ReapPolicy::default()
        };
        assert!(classify(&obs, &policy).is_reap());
    }

    #[test]
    fn the_orchestrator_outranks_everything_including_force() {
        let obs = AgentObservation {
            is_orchestrator: true,
            ..finished_and_quiet()
        };
        let policy = ReapPolicy {
            force: true,
            ..ReapPolicy::default()
        };
        assert_eq!(
            classify(&obs, &policy),
            Disposition::Keep {
                reason: KeepReason::Orchestrator
            }
        );
    }
}
