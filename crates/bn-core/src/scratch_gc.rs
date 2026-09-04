//! Leaked agent-scratch worktree policy — the pure decision behind `bn wt reap`'s sweep of the
//! `.agent-<branch>-<token>` checkouts `bn pipeline investigate` cuts for its sub-agents.
//!
//! A scratch worktree is created detached and removed by a shell command appended to the
//! sub-agent's own invocation (`…; git worktree remove --force <path>`). That works right up until
//! the agent doesn't exit normally: kill its tmux window, reboot the host, or have the harness die,
//! and the removal never runs. The checkout then stays registered forever, because nothing else
//! collects it — [`crate::fleet_gc`] reaps *processes*, `wt reap` needs a branch (these are
//! detached), and pool prune only knows `_pool/<n>`.
//!
//! This is the **pure core**, side-effect-free like [`crate::fleet_gc`]: the CLI captures the live
//! signals (git status, the process table, tmux panes, directory age) and feeds them in here; this
//! module only *classifies*. The rule it encodes is the one that must never be re-derived ad-hoc:
//! a scratch worktree is disposable only when it holds nothing — no uncommitted files, no commit
//! that exists nowhere else, and nobody working in it.

use serde::Serialize;

/// The prefix [`bn pipeline investigate`] gives a sub-agent's scratch worktree. A real branch
/// worktree never starts with a dot, so this is the marker that separates the two.
pub const SCRATCH_DIR_PREFIX: &str = ".agent-";

/// Whether a worktree directory name is a pipeline scratch checkout.
///
/// Requires something after the prefix, so a bare `.agent-` directory (which no launch produces)
/// is not mistaken for one.
pub fn is_scratch_dir(dir_name: &str) -> bool {
    dir_name
        .strip_prefix(SCRATCH_DIR_PREFIX)
        .is_some_and(|rest| !rest.is_empty())
}

/// Why a scratch worktree is spared. Serialized kebab-case for the dry-run / JSON surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeepReason {
    /// Not a `.agent-*` scratch checkout at all — some other worktree, none of this module's
    /// business.
    NotScratch,
    /// A process is working in it, or a tmux pane sits there. The sub-agent is still running and
    /// will remove its own worktree when it exits.
    Occupied,
    /// Uncommitted or untracked files. The sub-agent wrote something it never committed; removing
    /// the checkout would discard it.
    Dirty,
    /// Carries a commit that exists on no remote — findings that never reached a note or a branch.
    Unpushed,
    /// Younger than the grace window. A backstop for the race where the process table or tmux is
    /// unreadable and a live sub-agent would otherwise look idle.
    Grace,
}

impl KeepReason {
    /// The kebab-case label used in dry-run / JSON / event surfaces.
    pub fn label(self) -> &'static str {
        match self {
            KeepReason::NotScratch => "not-scratch",
            KeepReason::Occupied => "occupied",
            KeepReason::Dirty => "dirty",
            KeepReason::Unpushed => "unpushed",
            KeepReason::Grace => "grace",
        }
    }
}

/// The verdict for one scratch worktree — remove it, or keep it with the reason why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Disposition {
    /// Leave it alone.
    Keep { reason: KeepReason },
    /// A leaked scratch checkout holding nothing — `git worktree remove` it.
    Reap,
}

impl Disposition {
    /// Whether this verdict reaps.
    pub fn is_reap(self) -> bool {
        matches!(self, Disposition::Reap)
    }

    /// The kebab-case label for one-line surfaces.
    pub fn reason_label(self) -> &'static str {
        match self {
            Disposition::Keep { reason } => reason.label(),
            Disposition::Reap => "leaked-scratch",
        }
    }
}

/// The observed state of one candidate worktree — everything [`classify`] needs, captured by the
/// CLI. Deliberately free of any path handle so the policy stays pure; the caller keeps the path
/// alongside and acts on a [`Disposition::Reap`].
#[derive(Debug, Clone)]
pub struct ScratchObservation<'a> {
    /// The worktree's directory name (not its full path).
    pub dir_name: &'a str,
    /// Count of uncommitted + untracked entries (`git status --porcelain`).
    pub dirty_files: usize,
    /// Commits reachable from HEAD that exist on no remote (`git log HEAD --not --remotes`).
    pub local_only_commits: usize,
    /// A live process has its cwd inside, or a tmux pane is open there.
    pub occupied: bool,
    /// Seconds since the directory was last modified. `0` when unknown, which reads as brand new
    /// and is therefore protected by the grace window.
    pub age_secs: i64,
}

impl Default for ScratchObservation<'_> {
    /// A live, brand-new scratch worktree: clean, occupied, no age. The baseline a test (or the
    /// CLI) overrides field-by-field.
    fn default() -> Self {
        ScratchObservation {
            dir_name: ".agent-branch-f1-1",
            dirty_files: 0,
            local_only_commits: 0,
            occupied: true,
            age_secs: 0,
        }
    }
}

/// Default grace: a full day before a scratch worktree is eligible.
///
/// Deliberately generous. A leak is *permanent* — the two-week-old strays that motivated this sweep
/// were never going to be collected by anything — so waiting a day costs nothing, while the
/// occupancy check it backstops can go blind whenever `lsof`/tmux is unavailable. Cheap insurance
/// against the only failure that matters here: removing a checkout a sub-agent is still using.
pub const DEFAULT_MIN_AGE_SECS: i64 = 24 * 60 * 60;

/// Knobs for a scratch sweep.
#[derive(Debug, Clone, Copy)]
pub struct ScratchPolicy {
    /// Override the grace window. **Never** overrides occupancy, dirtiness, or unpushed commits —
    /// those guard work, and no flag on a garbage collector should discard work.
    pub force: bool,
    /// Minimum directory age before a scratch worktree is eligible.
    pub min_age_secs: i64,
}

impl Default for ScratchPolicy {
    fn default() -> Self {
        ScratchPolicy {
            force: false,
            min_age_secs: DEFAULT_MIN_AGE_SECS,
        }
    }
}

/// Classify one candidate worktree → keep or reap. **Pure and total.**
///
/// The order is what makes it safe:
/// 1. **Not a scratch checkout** — kept, and not this sweep's business.
/// 2. **Someone is in it** — a live process or an open pane means the sub-agent is still running
///    and will clean up after itself. Not overridable.
/// 3. **It holds work** — uncommitted files, or a commit on no remote. Not overridable: `--force`
///    on a GC exists to skip a *grace window*, never to throw away someone's output.
/// 4. **Grace** — too young to judge, unless `force`.
/// 5. **Reap** — a leaked checkout holding nothing.
pub fn classify(obs: &ScratchObservation, policy: &ScratchPolicy) -> Disposition {
    // (1) Only `.agent-*` checkouts are in scope. Everything else belongs to `wt reap`'s
    // branch-based path or to pool prune.
    if !is_scratch_dir(obs.dir_name) {
        return Disposition::Keep {
            reason: KeepReason::NotScratch,
        };
    }

    // (2) A running sub-agent removes its own worktree on exit; pulling it out from under one is
    // the exact failure this sweep must never cause.
    if obs.occupied {
        return Disposition::Keep {
            reason: KeepReason::Occupied,
        };
    }

    // (3) The work guards. No `force` path past these — a scratch worktree holding a commit that
    // exists nowhere else is a sub-agent's findings, and losing them is worse than the leak.
    if obs.dirty_files > 0 {
        return Disposition::Keep {
            reason: KeepReason::Dirty,
        };
    }
    if obs.local_only_commits > 0 {
        return Disposition::Keep {
            reason: KeepReason::Unpushed,
        };
    }

    // (4) Grace: don't judge a checkout that may still be mid-launch.
    if !policy.force && obs.age_secs < policy.min_age_secs {
        return Disposition::Keep {
            reason: KeepReason::Grace,
        };
    }

    // (5) A leak: detached, empty, nobody in it, old enough to be certain.
    Disposition::Reap
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaked<'a>(dir_name: &'a str) -> ScratchObservation<'a> {
        ScratchObservation {
            dir_name,
            occupied: false,
            age_secs: DEFAULT_MIN_AGE_SECS + 1,
            ..Default::default()
        }
    }

    #[test]
    fn recognizes_scratch_dirs() {
        assert!(is_scratch_dir(".agent-dispatch-fix-a-thing-f1786605100-1"));
        assert!(!is_scratch_dir(".agent-"));
        assert!(!is_scratch_dir("_pool"));
        assert!(!is_scratch_dir("main"));
        assert!(!is_scratch_dir("feat-add-tailscale"));
        // A dotted sibling that isn't a pipeline scratch dir stays out of scope.
        assert!(!is_scratch_dir(".git"));
    }

    #[test]
    fn reaps_an_old_empty_unoccupied_scratch_worktree() {
        let obs = leaked(".agent-dispatch-fix-failing-cron-f1786605100-1");
        assert_eq!(classify(&obs, &ScratchPolicy::default()), Disposition::Reap);
    }

    #[test]
    fn never_touches_a_non_scratch_worktree() {
        let obs = ScratchObservation {
            dir_name: "feat-add-tailscale",
            occupied: false,
            dirty_files: 0,
            local_only_commits: 0,
            age_secs: i64::MAX,
        };
        assert_eq!(
            classify(&obs, &ScratchPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::NotScratch
            }
        );
        // Even forced: this sweep simply does not own other worktrees.
        let forced = ScratchPolicy {
            force: true,
            ..Default::default()
        };
        assert!(!classify(&obs, &forced).is_reap());
    }

    #[test]
    fn spares_an_occupied_scratch_worktree() {
        let obs = ScratchObservation {
            occupied: true,
            ..leaked(".agent-x-f1-1")
        };
        assert_eq!(
            classify(&obs, &ScratchPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Occupied
            }
        );
    }

    #[test]
    fn force_never_discards_work_or_evicts_a_live_sub_agent() {
        let forced = ScratchPolicy {
            force: true,
            min_age_secs: DEFAULT_MIN_AGE_SECS,
        };
        for (obs, expected) in [
            (
                ScratchObservation {
                    occupied: true,
                    ..leaked(".agent-x-f1-1")
                },
                KeepReason::Occupied,
            ),
            (
                ScratchObservation {
                    dirty_files: 1,
                    ..leaked(".agent-x-f1-1")
                },
                KeepReason::Dirty,
            ),
            (
                ScratchObservation {
                    local_only_commits: 1,
                    ..leaked(".agent-x-f1-1")
                },
                KeepReason::Unpushed,
            ),
        ] {
            assert_eq!(
                classify(&obs, &forced),
                Disposition::Keep { reason: expected },
                "force must not override the {} guard",
                expected.label()
            );
        }
    }

    #[test]
    fn grace_protects_a_young_scratch_worktree_and_force_skips_it() {
        let obs = ScratchObservation {
            age_secs: 60,
            ..leaked(".agent-x-f1-1")
        };
        assert_eq!(
            classify(&obs, &ScratchPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Grace
            }
        );
        let forced = ScratchPolicy {
            force: true,
            ..Default::default()
        };
        assert!(classify(&obs, &forced).is_reap());
    }

    #[test]
    fn unknown_age_reads_as_brand_new() {
        // age_secs 0 is "couldn't stat it", which must protect rather than expose.
        let obs = ScratchObservation {
            age_secs: 0,
            ..leaked(".agent-x-f1-1")
        };
        assert_eq!(
            classify(&obs, &ScratchPolicy::default()),
            Disposition::Keep {
                reason: KeepReason::Grace
            }
        );
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(Disposition::Reap.reason_label(), "leaked-scratch");
        assert_eq!(
            Disposition::Keep {
                reason: KeepReason::Unpushed
            }
            .reason_label(),
            "unpushed"
        );
    }
}
