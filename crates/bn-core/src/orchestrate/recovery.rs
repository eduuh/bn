//! Lane **recovery policy** — the pure half of `bn orchestrate` that decides when a lane needs
//! rescuing, how to rescue it, and when it is already finished and needs nothing.
//!
//! Split out of [`super`] because it is a self-contained decision layer sitting between the
//! pane-state machine and the persisted queue, and because the never-grow file-size ratchet is
//! satisfied by splitting a file, never by compressing it.
//!
//! Three pieces, in the order a supervision pass consults them:
//!   - [`Liveness`] + [`idle_clean_action`] — is this quiet, clean-looking agent just-launched,
//!     finished, being *read by a human*, or genuinely stalled? (#374)
//!   - [`Backoff`] + [`RelaunchKind`] + [`plan_relaunch`] — cap, backoff window, and soft-vs-hard
//!     realization for a lane that does need recovering (#370 defect 1/4).
//!   - [`ExternalState`] — the work already landed outside the daemon's own tracking, so recover
//!     nothing and reconcile it done (#370 defect 2).

use super::pane::{has_bare_prompt, PaneObservation, PaneState, SupervisorAction};
use serde::{Deserialize, Serialize};

// ===========================================================================
// Idle-clean liveness refinement (#374)
// ===========================================================================

/// Progress / liveness signals for an in-flight agent sitting at an **idle-clean** prompt (#374).
/// Gathered by the CLI from the harness session logs (cumulative AIC, #309), gh (PR state), and the
/// session's last-activity clock. This is the input that tells three look-alike agents apart — a
/// bare idle prompt with a clean tree is identical whether the agent **just launched** (nothing done
/// yet), **finished** (its PR is up, awaiting the merge-queue), or **stalled** (worked, then died at
/// an empty prompt with nothing committed and no PR). Without it, [`classify`] can only call them all
/// `IdleClean` → `Leave`, so a stalled agent hogs its concurrency slot forever and starves the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Liveness {
    /// Cumulative agent output tokens burned so far (AIC, #309). `0` ⇒ the agent produced no turn —
    /// it hasn't started, so an idle-clean prompt means *just-launched*, never *stalled*.
    pub aic: u64,
    /// The item's branch already carries an **open or merged** PR — the work is delivered and landing
    /// is the merge-queue's job. A finished agent is never relaunched (that would re-do landed work).
    pub has_pr: bool,
    /// Seconds since the agent's last sign of life (its newest harness-log event). A large value at a
    /// bare idle-clean prompt means the agent stopped and won't resume on its own — the stall signal.
    ///
    /// Note what this clock does **not** count: it is the *agent's* activity, read from its harness
    /// log. A human scrolling, reading, or typing in the pane moves it not at all — which is why
    /// [`Self::attended`] has to be observed separately.
    pub idle_secs: i64,
    /// A human is at this agent's window, now or within the decay window (see
    /// [`crate::tmux::observe_presence`]). An attended lane is never judged stalled: the
    /// three-way ambiguity this struct resolves has a fourth reading, *being reviewed*, and it looks
    /// exactly like a stall — worked, went quiet, no PR, clean tree — because reading an agent's
    /// output is precisely what produces a long, empty, clean idle.
    pub attended: bool,
}

/// Whether an agent's pane tail positively shows a **bare idle prompt** — an empty input box (`❯`,
/// `┃`, `>`, `│`) with nothing typed. This distinguishes an `IdleClean` that came from a *real* idle
/// prompt from one that came from [`classify`]'s ambiguous fallback (no prompt **and** no spinner
/// captured). The #374 stall recovery gates on it: a fallback-idle pane might be a working agent
/// mid-long-turn whose spinner scrolled off, so only a positively-observed idle prompt is ever
/// eligible to be judged *stalled* — the ambiguous case is always left alone.
pub fn shows_bare_prompt(obs: &PaneObservation) -> bool {
    has_bare_prompt(obs.tail)
}

/// Refine an [`PaneState::IdleClean`] pane into its true action (#374) — the liveness half of the
/// state machine that [`action_for`] can't decide from the pane alone. The bare idle prompt with a
/// clean tree is ambiguous, so given the gathered [`Liveness`], resolve which of the three it is:
///   - a human is watching it ⇒ **under review** ⇒ `Leave` (see [`Liveness::attended`]) — the one
///     reading that stops a "recovery" from destroying the transcript being read,
///   - branch has a PR ⇒ **finished** ⇒ `Leave` (the merge-queue lands it; reconcile marks it done),
///   - no tokens burned (`aic == 0`) ⇒ **just-launched** / never-started ⇒ `Leave` (give it room),
///   - worked (`aic > 0`), no PR, idle past `stale_secs` ⇒ **stalled** ⇒ `RelaunchToResume` — recover
///     it (a hard restart re-seeds a fresh session, #383) so its slot stops starving the queue (#374),
///   - otherwise (worked, no PR, but not yet stale) ⇒ `Leave` — a brief between-turns idle or an
///     agent about to open its PR / run `bn agent done` is not cut off before the dwell elapses.
///
/// Pure + unit-tested: the single source of truth for "is this idle-clean agent actually stalled?".
pub fn idle_clean_action(live: Liveness, stale_secs: i64) -> SupervisorAction {
    if live.attended || live.has_pr || live.aic == 0 {
        return SupervisorAction::Leave;
    }
    if live.idle_secs >= stale_secs {
        SupervisorAction::RelaunchToResume
    } else {
        SupervisorAction::Leave
    }
}

// ===========================================================================
// Exponential backoff for relaunch-to-resume
// ===========================================================================

/// Exponential backoff for relaunch-to-resume attempts. A wedged agent (api-error / dns-outage /
/// idle-with-WIP / dead) is relaunched after `delay(attempt)` seconds, doubling each retry up to a
/// cap — so a flapping outage doesn't hot-loop the fleet. Pure and saturating (never overflows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The first retry delay, in seconds.
    pub base_secs: u64,
    /// The multiplier applied per attempt.
    pub factor: u64,
    /// The delay cap, in seconds — the longest the supervisor ever waits between relaunches.
    pub max_secs: u64,
    /// The attempt count after which the supervisor gives up relaunching (marks the item failed).
    pub max_attempts: u32,
    /// Relaunch attempts on a **wedged** pane (api-error / dns-outage) after which a soft resume is
    /// abandoned for a **hard restart** (kill the process + respawn a fresh harness). A hard-wedged
    /// process — e.g. a cached `ENOTFOUND` after a DNS outage — never clears via send-keys, so once
    /// this many soft resumes haven't recovered it, only a fresh process will (#370 defect 1).
    pub hard_restart_after: u32,
}

impl Default for Backoff {
    /// 30s → 1m → 2m → 4m … capped at 30m, giving up after 6 attempts (~1h of retries). A wedged
    /// outage escalates from soft resume to a hard restart after 2 soft attempts.
    fn default() -> Self {
        Backoff {
            base_secs: 30,
            factor: 2,
            max_secs: 1800,
            max_attempts: 6,
            hard_restart_after: 2,
        }
    }
}

impl Backoff {
    /// The delay before the `attempt`-th relaunch (0-based: `delay(0)` is the first retry). Doubles
    /// per attempt from `base_secs`, saturating at `max_secs`.
    pub fn delay(&self, attempt: u32) -> u64 {
        let mut d = self.base_secs;
        for _ in 0..attempt {
            d = d.saturating_mul(self.factor);
            if d >= self.max_secs {
                return self.max_secs;
            }
        }
        d.min(self.max_secs)
    }

    /// Whether a further relaunch is allowed given the attempts made so far. Past the cap, the
    /// supervisor stops flapping and marks the item failed for a human.
    pub fn should_retry(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }
}

/// How a relaunch is realized against the live fleet — the #370-1 distinction the CLI must honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelaunchKind {
    /// Nudge a still-alive pane to resume (send-keys a resume message). Works when the agent is
    /// merely idle/waiting — the process is healthy, it just stopped.
    Soft,
    /// Kill the pane's process and respawn a **fresh** harness (tmux respawn/kill + re-dispatch).
    /// The only thing that clears a hard-wedged process — a cached `ENOTFOUND` after a DNS outage,
    /// a dead window — which a soft send-keys can't touch.
    Hard,
}

impl RelaunchKind {
    pub fn label(self) -> &'static str {
        match self {
            RelaunchKind::Soft => "soft-resume",
            RelaunchKind::Hard => "hard-restart",
        }
    }
}

/// Choose how to realize a relaunch (#370 defect 1). A dead pane (no window) can't be nudged — a
/// fresh dispatch *is* the hard restart. A pane a human is **watching** is never hard-restarted: a
/// hard restart kills the window and every line of scrollback in it, so under a reviewer's eyes the
/// worst case of a soft resume (a nudge that doesn't take) is strictly better than the best case of
/// a hard one. A **stalled idle-clean** pane (#374) otherwise hard-restarts straight away:
/// soft-resuming its idle session just restores the same dead prompt (#383). A live but **wedged**
/// pane (api-error / dns-outage) that `hard_restart_after` soft resumes haven't cleared is
/// hard-wedged (cached resolver failure) and needs a fresh process. Everything else (an idle agent
/// with WIP — alive, healthy, just stopped) resumes softly. Pure.
pub fn relaunch_kind(
    state: PaneState,
    attempts: u32,
    has_window: bool,
    attended: bool,
    policy: &Backoff,
) -> RelaunchKind {
    if !has_window {
        return RelaunchKind::Hard;
    }
    // Somebody is reading this pane. Recover it without destroying what they're reading.
    if attended {
        return RelaunchKind::Soft;
    }
    // A stalled idle-clean agent (#374) only reached relaunch because it worked then died at an empty
    // prompt: its harness session is idle/useless, and a soft resume just re-wakes that same dead
    // session (comes back idle at `❯` having done nothing — #383). Only a fresh, re-seeded process
    // resumes the *work*, so skip straight to a hard restart.
    if matches!(state, PaneState::IdleClean) {
        return RelaunchKind::Hard;
    }
    let wedged = matches!(
        state,
        PaneState::ApiError | PaneState::DnsOutage | PaneState::Dead
    );
    if wedged && attempts >= policy.hard_restart_after {
        RelaunchKind::Hard
    } else {
        RelaunchKind::Soft
    }
}

/// The decision a supervision pass reaches for a relaunch-to-resume item — the whole #370 relaunch
/// policy (cap → give up, backoff window → wait, else relaunch soft/hard) in one pure value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchDecision {
    /// The relaunch cap is hit — give up and mark the item failed / needs-attention (#370 defect 4).
    GiveUp,
    /// The backoff window since the last relaunch hasn't elapsed — do nothing this pass.
    Wait,
    /// Relaunch now, realized via this [`RelaunchKind`].
    Relaunch(RelaunchKind),
}

/// Resolve the full relaunch policy for an item, given its wedged pane `state`, `attempts` so far,
/// the epoch secs of its `last_relaunch_at` (0 = never), `now`, whether a live window still exists,
/// whether a human is watching it, and the `policy`. Deterministic and pure — the single unit-tested source of truth so the CLI
/// executor never re-derives "cap? / backoff? / soft or hard?" in prose (the whole point of #340).
///   1. past the attempt cap ⇒ `GiveUp` (surface a failed item instead of looping unbounded).
///   2. inside the backoff window since the last relaunch ⇒ `Wait`.
///   3. otherwise ⇒ `Relaunch` via [`relaunch_kind`] (soft resume, escalating to hard restart).
pub fn plan_relaunch(
    state: PaneState,
    attempts: u32,
    last_relaunch_at: i64,
    now: i64,
    has_window: bool,
    attended: bool,
    policy: &Backoff,
) -> RelaunchDecision {
    if !policy.should_retry(attempts) {
        return RelaunchDecision::GiveUp;
    }
    if last_relaunch_at != 0 && now - last_relaunch_at < policy.delay(attempts) as i64 {
        return RelaunchDecision::Wait;
    }
    RelaunchDecision::Relaunch(relaunch_kind(state, attempts, has_window, attended, policy))
}

/// External completion signals for an in-flight item, gathered by the CLI for reconciliation
/// (#370 defect 2). The daemon must mark an item **done** when its work has landed *outside* its
/// own PR tracking — a merged PR on the branch, or a closed linked issue — not only when the daemon
/// opened the PR itself. Pure so "is this item actually already complete?" is one unit-tested
/// predicate, keyed on the branch/issue's real state rather than a daemon-held PR handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExternalState {
    /// The item's branch has a **merged** pull request (opened by anyone).
    pub branch_pr_merged: bool,
    /// The item's linked issue is **closed**.
    pub issue_closed: bool,
    /// The daemon's own worktree for the branch has vanished (the merge-queue reaped it) — the
    /// pre-existing "it merged" signal, folded in so all completion logic lives in one predicate.
    pub worktree_gone: bool,
}

impl ExternalState {
    /// Whether the item is already complete and should be reconciled to a terminal `merged`, even if
    /// the daemon never opened its PR. Any one signal suffices — the fix for the infinite-relaunch
    /// loop on an externally-finished item (#370 defect 2).
    pub fn is_resolved(self) -> bool {
        self.branch_pr_merged || self.issue_closed || self.worktree_gone
    }

    /// The signal that resolved the item (for the recovery note / event log), or `None` if unresolved.
    /// Prefers the most specific evidence: a merged PR, then a closed issue, then a reaped worktree.
    pub fn via(self) -> Option<&'static str> {
        if self.branch_pr_merged {
            Some("branch-merged-pr")
        } else if self.issue_closed {
            Some("issue-closed")
        } else if self.worktree_gone {
            Some("worktree-reaped")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;

    /// An agent that worked, went quiet at a clean prompt, and has no PR — the #374 stall shape,
    /// which is also exactly what an agent looks like while you read what it produced.
    fn worked_then_quiet() -> Liveness {
        Liveness {
            aic: 5_000,
            has_pr: false,
            idle_secs: 3_600,
            attended: false,
        }
    }

    #[test]
    fn an_unwatched_stalled_agent_is_still_recovered() {
        assert_eq!(
            idle_clean_action(worked_then_quiet(), 600),
            SupervisorAction::RelaunchToResume
        );
    }

    #[test]
    fn a_watched_agent_is_never_judged_stalled() {
        let live = Liveness {
            attended: true,
            ..worked_then_quiet()
        };
        assert_eq!(idle_clean_action(live, 600), SupervisorAction::Leave);
    }

    #[test]
    fn presence_outlasts_any_amount_of_agent_quiet() {
        // The idle clock only tracks the *agent*. Reading for an hour must not become a stall.
        let live = Liveness {
            attended: true,
            idle_secs: 86_400,
            ..worked_then_quiet()
        };
        assert_eq!(idle_clean_action(live, 600), SupervisorAction::Leave);
    }

    #[test]
    fn a_watched_wedged_pane_resumes_softly_instead_of_being_killed() {
        let policy = Backoff::default();
        // Past `hard_restart_after`, an unwatched wedged pane escalates to a hard restart...
        assert_eq!(
            relaunch_kind(PaneState::ApiError, 3, true, false, &policy),
            RelaunchKind::Hard
        );
        // ...but not while somebody is reading it: a nudge can't destroy scrollback.
        assert_eq!(
            relaunch_kind(PaneState::ApiError, 3, true, true, &policy),
            RelaunchKind::Soft
        );
    }

    #[test]
    fn a_windowless_lane_hard_restarts_regardless() {
        // There is no pane to watch and nothing to preserve — a fresh dispatch is the only recovery.
        assert_eq!(
            relaunch_kind(PaneState::Dead, 0, false, true, &Backoff::default()),
            RelaunchKind::Hard
        );
    }

    #[test]
    fn presence_does_not_short_circuit_the_backoff_or_the_cap() {
        let policy = Backoff::default();
        assert_eq!(
            plan_relaunch(
                PaneState::ApiError,
                policy.max_attempts,
                0,
                1_000,
                true,
                true,
                &policy
            ),
            RelaunchDecision::GiveUp
        );
        assert_eq!(
            plan_relaunch(PaneState::ApiError, 0, 1_000, 1_001, true, true, &policy),
            RelaunchDecision::Wait
        );
        assert_eq!(
            plan_relaunch(PaneState::ApiError, 0, 0, 1_000, true, true, &policy),
            RelaunchDecision::Relaunch(RelaunchKind::Soft)
        );
    }
}
