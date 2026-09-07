//! Walking a task through the pipeline — **one queue entry per stage**, decided here.
//!
//! A single dispatch runs its whole pipeline under one harness and one model (see the comment at the
//! `pipeline_model` call in `bn-cli`'s dispatch). So per-stage models are only *reachable* if each
//! stage is its own queue entry, which makes something have to decide when the next entry is due.
//! This is that decision, and it is pure: the caller reads the backlog and the reviews, this says
//! what to do.
//!
//! # The queue is the state machine
//!
//! A stage's entry **is** its state. Pending or leased means the stage is running; `Done` means it
//! finished; `Failed` means it did not. Nothing else is recorded and nothing else has to be kept in
//! sync — which is the whole reason the state lives there rather than in a second ledger that could
//! disagree with the queue about what has already run.
//!
//! # Advancing is a sweep, never a callback
//!
//! Each stage's completion could have enqueued its successor. It does not, because a callback only
//! fires when everything works: a sweep additionally recovers a machine that was off when a stage
//! landed, an enqueue that failed, and a stage whose entry was cancelled by hand. Re-running it is
//! always safe — the same discipline `bn today sync` uses for the accept→queue hand-off.
//!
//! # The gate is enforced here
//!
//! [`GatePolicy::Hold`](super::profile::GatePolicy::Hold) was resolved and ratcheted by
//! [`super::profile`] but read by nothing, so a configured hold did nothing at all. Rather than
//! inventing a second gate beside the one that already works, the hold is enforced at the only point
//! that decides what runs next: a held stage is not enqueued until its plan review is approved.
//! `plan_review` stays the record and the human's queue item; this just refuses to move past it.

use super::stage::Stage;
use crate::backlog::BacklogStatus;

/// The stages the sweep drives, in flow order.
///
/// Deliberately three of the nine. Investigate → plan → execute is the arc where per-stage models
/// and a human gate change the outcome, and it is the process a task should not be able to skip.
/// After execute, `validate`/`critique`/`ship` already have their own machinery (`bn verify`, the
/// critic lane, promote/land), and driving them from here would mean two things owning one step.
pub const SWEPT: [Stage; 3] = [Stage::Investigate, Stage::Plan, Stage::Execute];

/// One swept stage's entry, as the backlog reports it. `None` means no entry was ever created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageState {
    pub stage: Stage,
    pub status: Option<BacklogStatus>,
}

/// What the sweep should do about one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advance {
    /// A stage is already running; leave it alone.
    InFlight(Stage),
    /// Enqueue this stage next.
    Next(Stage),
    /// The next stage is gated and its plan review has not been approved. Not an error — the point
    /// of the gate — but it must be *reported*, because a hold nobody can see is indistinguishable
    /// from a queue that has quietly stopped.
    HeldForReview(Stage),
    /// A stage failed. A person decides what happens next; re-running it automatically would just
    /// burn the same lane on the same failure.
    Blocked(Stage),
    /// Every swept stage is done — the arc this sweep owns is finished.
    Complete,
}

/// Decide the next move for a task from its stage states.
///
/// `approved` is whether a plan review for this task's worktree has been approved; it only matters
/// for a stage whose profile holds. `held` names the stages configured to wait for a human — passed
/// in rather than read so the whole policy stays testable without a config.
///
/// Order matters and encodes the invariants: something already running always wins (never two
/// agents for one task), a failure stops the walk (never retry into the same wall), and the gate is
/// checked *before* enqueueing rather than after.
pub fn decide(states: &[StageState], held: &[Stage], approved: bool) -> Advance {
    if let Some(running) = states
        .iter()
        .find(|s| s.status.is_some_and(BacklogStatus::is_active))
    {
        return Advance::InFlight(running.stage);
    }
    for state in states {
        match state.status {
            // Finished: keep walking to the first stage that has not run.
            Some(BacklogStatus::Done) => continue,
            Some(BacklogStatus::Failed) => return Advance::Blocked(state.stage),
            // Cancelled reads as "never ran" rather than as a failure: someone took it out of the
            // queue, and the sweep's job is to put back what the pipeline still needs. A stage that
            // should stay out is one whose *task* is closed, which is filtered before we get here.
            None | Some(BacklogStatus::Cancelled) => {
                if held.contains(&state.stage) && !approved {
                    return Advance::HeldForReview(state.stage);
                }
                return Advance::Next(state.stage);
            }
            Some(BacklogStatus::Leased) | Some(BacklogStatus::Pending) => {
                // Unreachable: both are active and caught above. Handled explicitly so adding a
                // status is a compile error here rather than a silent fall-through.
                return Advance::InFlight(state.stage);
            }
        }
    }
    Advance::Complete
}

/// The `source:` a stage's queue entry carries — the sweep's idempotency key.
///
/// Namespaced per task *and* stage, so the sweep can ask the backlog "has this task's plan stage run
/// yet?" without keeping its own ledger. Same discipline as `today:<suggestion-id>`: the queue is
/// the truth, and anything cached elsewhere is only a convenience.
pub fn stage_source(task_id: &str, stage: Stage) -> String {
    format!("{STAGE_SOURCE_PREFIX}{task_id}:{}", stage.as_str())
}

/// The namespace every [`stage_source`] lives under.
pub const STAGE_SOURCE_PREFIX: &str = "pipeline:";

/// Whether a queue entry's `source` is a pipeline stage's — i.e. one of the rows the sweep reads
/// as state. Backlog compaction keeps such rows even once they are terminal, because a `Done` or
/// `Failed` stage that compaction had dropped reads back as "never ran", and the sweep would
/// enqueue it again — the loop this predicate exists to prevent.
pub fn is_stage_source(source: &str) -> bool {
    source.starts_with(STAGE_SOURCE_PREFIX)
}

/// The workflow skill a stage runs when no config pins one.
///
/// These are the shipped skills whose job *is* the stage, so a machine with no `[pipeline.stages.*]`
/// config still walks the arc correctly instead of running a bare agent three times.
pub fn default_skill(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Investigate => Some("investigate"),
        Stage::Plan => Some("plan"),
        Stage::Execute => Some("plan-work"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(statuses: [Option<BacklogStatus>; 3]) -> Vec<StageState> {
        SWEPT
            .iter()
            .zip(statuses)
            .map(|(&stage, status)| StageState { stage, status })
            .collect()
    }

    #[test]
    fn a_task_with_nothing_queued_starts_at_investigate() {
        assert_eq!(
            decide(&states([None, None, None]), &[], false),
            Advance::Next(Stage::Investigate)
        );
    }

    #[test]
    fn a_finished_stage_hands_over_to_the_next() {
        assert_eq!(
            decide(&states([Some(BacklogStatus::Done), None, None]), &[], false),
            Advance::Next(Stage::Plan)
        );
    }

    /// The invariant that stops one task growing two agents. It has to beat every other rule: a
    /// running stage means the sweep has nothing to add, whatever the later stages look like.
    #[test]
    fn work_already_running_wins_over_everything() {
        for running in [BacklogStatus::Pending, BacklogStatus::Leased] {
            assert_eq!(
                decide(&states([Some(running), None, None]), &[], true),
                Advance::InFlight(Stage::Investigate)
            );
        }
        // Even when an earlier stage is done and a later one is live.
        assert_eq!(
            decide(
                &states([Some(BacklogStatus::Done), Some(BacklogStatus::Leased), None]),
                &[],
                true
            ),
            Advance::InFlight(Stage::Plan)
        );
    }

    #[test]
    fn a_failed_stage_stops_the_walk() {
        assert_eq!(
            decide(
                &states([Some(BacklogStatus::Done), Some(BacklogStatus::Failed), None]),
                &[],
                true
            ),
            Advance::Blocked(Stage::Plan)
        );
    }

    /// The gate, finally load-bearing: `execute` is held, so an approved plan is what releases it.
    #[test]
    fn a_held_stage_waits_for_an_approved_plan() {
        let after_plan = states([Some(BacklogStatus::Done), Some(BacklogStatus::Done), None]);
        assert_eq!(
            decide(&after_plan, &[Stage::Execute], false),
            Advance::HeldForReview(Stage::Execute)
        );
        assert_eq!(
            decide(&after_plan, &[Stage::Execute], true),
            Advance::Next(Stage::Execute)
        );
    }

    /// A hold must not leak backwards onto a stage that runs before the review exists, or the
    /// pipeline could never reach the review that would release it.
    #[test]
    fn holding_a_later_stage_does_not_block_an_earlier_one() {
        assert_eq!(
            decide(&states([None, None, None]), &[Stage::Execute], false),
            Advance::Next(Stage::Investigate)
        );
    }

    /// Default (no config) is `auto` everywhere, so nothing waits until someone asks it to.
    #[test]
    fn with_no_stage_held_the_arc_runs_unattended() {
        assert_eq!(
            decide(
                &states([Some(BacklogStatus::Done), Some(BacklogStatus::Done), None]),
                &[],
                false
            ),
            Advance::Next(Stage::Execute)
        );
    }

    #[test]
    fn a_cancelled_stage_is_queued_again_rather_than_treated_as_failed() {
        assert_eq!(
            decide(
                &states([Some(BacklogStatus::Cancelled), None, None]),
                &[],
                false
            ),
            Advance::Next(Stage::Investigate)
        );
    }

    #[test]
    fn all_three_done_is_complete() {
        assert_eq!(
            decide(
                &states([
                    Some(BacklogStatus::Done),
                    Some(BacklogStatus::Done),
                    Some(BacklogStatus::Done)
                ]),
                &[Stage::Execute],
                true
            ),
            Advance::Complete
        );
    }

    #[test]
    fn the_source_key_is_namespaced_per_task_and_stage() {
        assert_eq!(stage_source("t42", Stage::Plan), "pipeline:t42:plan");
        assert_ne!(
            stage_source("t42", Stage::Plan),
            stage_source("t42", Stage::Execute)
        );
        assert_ne!(
            stage_source("t42", Stage::Plan),
            stage_source("t43", Stage::Plan)
        );
    }

    #[test]
    fn every_swept_stage_knows_which_skill_runs_it() {
        for stage in SWEPT {
            assert!(
                default_skill(stage).is_some(),
                "{stage:?} has no default workflow, so a config-less machine would run a bare agent"
            );
        }
    }
}
