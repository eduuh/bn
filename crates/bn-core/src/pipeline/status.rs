//! Pure task-pipeline **phase** inference for the `bn pipeline` read model (#657).
//!
//! The pipeline view deliberately owns no state. Callers gather the existing branch-note,
//! plan-review, attention, verify, PR, and live-agent signals into [`TaskSignals`], then this
//! module derives the current phase and the next action.
//!
//! # Phase is not [`crate::pipeline::stage::Stage`]
//!
//! Two vocabularies describe a task's position, and they are not interchangeable:
//!
//! - [`Phase`] (here) — the **4-value inference** shown by the `bn pipeline` tracker. Derived
//!   read-only from signals on every call; nothing persists it.
//! - [`crate::pipeline::stage::Stage`] — the **9-value durable state machine** (EPIC #746),
//!   persisted in note frontmatter and the `pipeline_stage` event log.
//!
//! They were both named `Stage` and both serialize lowercase, so `"execute"` and `"plan"` are
//! members of *both* vocabularies — a JSON consumer reading a field named `stage` could not tell
//! which it had. This module's type is `Phase` and its JSON field is `phase` precisely so the
//! 9-value vocabulary owns `stage` unambiguously on the wire.

use serde::Serialize;

/// The default coding pipeline from #649 — the inferred, non-persisted view.
///
/// See the module docs: this is **not** [`crate::pipeline::stage::Stage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Plan,
    Execute,
    Critique,
    Done,
}

impl Phase {
    pub const ALL: [Phase; 4] = [Phase::Plan, Phase::Execute, Phase::Critique, Phase::Done];

    pub fn label(self) -> &'static str {
        match self {
            Phase::Plan => "Plan",
            Phase::Execute => "Execute",
            Phase::Critique => "Critique",
            Phase::Done => "Done",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Phase::Plan => 0,
            Phase::Execute => 1,
            Phase::Critique => 2,
            Phase::Done => 3,
        }
    }
}

/// The latest plan-review state for the task, when one exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReview {
    Pending { id: String },
    Approved { id: String },
    ChangesRequested { id: String },
    Rejected { id: String },
}

/// Existing persisted/live facts used to infer one task's pipeline position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSignals {
    pub repo: String,
    pub branch: String,
    pub goal: Option<String>,
    pub note_closed: bool,
    pub has_done_when: bool,
    pub has_todos: bool,
    pub open_todos: usize,
    pub next_todo: Option<String>,
    pub blocker: Option<String>,
    pub pr_url: Option<String>,
    pub verify_passed: bool,
    pub plan_review: Option<PlanReview>,
    pub critic_active: bool,
    pub critic_failed: bool,
    pub critic_passed: bool,
    pub agent_state: Option<String>,
}

/// The stable machine/human read model emitted by `bn pipeline`.
///
/// The `phase`/`phases` fields carry the 4-value inference vocabulary. A field named `stage` on
/// this type would collide with the durable 9-value one emitted by `bn pipeline status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineView {
    pub repo: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub phase: Phase,
    pub action: String,
    pub phases: Vec<PhaseView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhaseView {
    pub id: Phase,
    pub status: PhaseStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStatus {
    Done,
    Active,
    Pending,
}

/// Infer a task's current phase and next action without reading or mutating any store.
pub fn infer(signals: &TaskSignals) -> PipelineView {
    let phase = if signals.note_closed {
        Phase::Done
    } else if signals.critic_failed {
        Phase::Execute
    } else if signals.critic_passed {
        Phase::Done
    } else if signals.critic_active
        || (signals.pr_url.is_some()
            && signals.verify_passed
            && signals.open_todos == 0
            && signals.blocker.is_none())
    {
        Phase::Critique
    } else if needs_planning(signals) {
        Phase::Plan
    } else {
        Phase::Execute
    };

    let action = action_for(phase, signals);
    let current = phase.index();
    let phases = Phase::ALL
        .into_iter()
        .map(|id| PhaseView {
            status: match id.index().cmp(&current) {
                std::cmp::Ordering::Less => PhaseStatus::Done,
                std::cmp::Ordering::Equal => PhaseStatus::Active,
                std::cmp::Ordering::Greater => PhaseStatus::Pending,
            },
            id,
        })
        .collect();

    PipelineView {
        repo: signals.repo.clone(),
        branch: signals.branch.clone(),
        goal: signals.goal.clone(),
        phase,
        action,
        phases,
    }
}

fn needs_planning(signals: &TaskSignals) -> bool {
    match signals.plan_review {
        Some(PlanReview::Approved { .. }) => false,
        Some(
            PlanReview::Pending { .. }
            | PlanReview::ChangesRequested { .. }
            | PlanReview::Rejected { .. },
        ) => true,
        None => !signals.has_done_when || !signals.has_todos,
    }
}

fn action_for(phase: Phase, signals: &TaskSignals) -> String {
    match phase {
        Phase::Plan => match &signals.plan_review {
            Some(PlanReview::Pending { id }) => format!("Review plan {id}"),
            Some(PlanReview::ChangesRequested { id }) => format!("Revise plan {id}"),
            Some(PlanReview::Rejected { id }) => format!("Plan {id} was rejected; restart or stop"),
            Some(PlanReview::Approved { .. }) | None
                if !signals.has_done_when || !signals.has_todos =>
            {
                "Write done-when criteria and ordered todos".to_string()
            }
            Some(PlanReview::Approved { .. }) | None => "Submit the plan for review".to_string(),
        },
        Phase::Execute if signals.critic_failed => "Fix the critic findings".to_string(),
        Phase::Execute => {
            if let Some(state) = signals.agent_state.as_deref() {
                match state {
                    "working" => return "Agent is working".to_string(),
                    "waiting" | "needs-decision" => {
                        return "Agent is waiting for input".to_string()
                    }
                    "blocked" | "failed" => return format!("Agent is {state}"),
                    _ => {}
                }
            }
            if let Some(blocker) = &signals.blocker {
                return format!("Resolve blocker: {blocker}");
            }
            if let Some(todo) = &signals.next_todo {
                return format!("Next: {todo}");
            }
            if !signals.verify_passed {
                "Run bn verify".to_string()
            } else if signals.pr_url.is_none() {
                "Open the PR".to_string()
            } else {
                "Start a fresh-context critique".to_string()
            }
        }
        Phase::Critique if signals.critic_active => {
            "Fresh-context critic is reviewing the change".to_string()
        }
        Phase::Critique => signals
            .pr_url
            .as_ref()
            .map(|url| format!("Run fresh-context critique for {url}"))
            .unwrap_or_else(|| "Run fresh-context critique".to_string()),
        Phase::Done => "Task complete".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals() -> TaskSignals {
        TaskSignals {
            repo: "bn".into(),
            branch: "dispatch-657".into(),
            goal: Some("show pipeline".into()),
            has_done_when: true,
            has_todos: true,
            open_todos: 1,
            next_todo: Some("t1 implement it".into()),
            ..Default::default()
        }
    }

    #[test]
    fn missing_contract_is_planning() {
        let mut s = signals();
        s.has_done_when = false;
        let view = infer(&s);
        assert_eq!(view.phase, Phase::Plan);
        assert_eq!(view.action, "Write done-when criteria and ordered todos");
    }

    #[test]
    fn pending_and_changes_requested_reviews_stay_in_plan() {
        let mut s = signals();
        s.plan_review = Some(PlanReview::Pending { id: "r14".into() });
        assert_eq!(infer(&s).action, "Review plan r14");
        s.plan_review = Some(PlanReview::ChangesRequested { id: "r14".into() });
        assert_eq!(infer(&s).action, "Revise plan r14");
    }

    #[test]
    fn approved_plan_executes_the_next_todo() {
        let mut s = signals();
        s.plan_review = Some(PlanReview::Approved { id: "r14".into() });
        let view = infer(&s);
        assert_eq!(view.phase, Phase::Execute);
        assert_eq!(view.action, "Next: t1 implement it");
    }

    #[test]
    fn live_agent_action_wins_during_execution() {
        let mut s = signals();
        s.agent_state = Some("working".into());
        assert_eq!(infer(&s).action, "Agent is working");
    }

    #[test]
    fn verified_pr_moves_to_critique() {
        let mut s = signals();
        s.open_todos = 0;
        s.next_todo = None;
        s.verify_passed = true;
        s.pr_url = Some("https://github.com/eduuh/bn/pull/1".into());
        let view = infer(&s);
        assert_eq!(view.phase, Phase::Critique);
        assert!(view.action.contains("pull/1"));
    }

    #[test]
    fn verified_pr_stays_in_execution_while_work_remains() {
        let mut s = signals();
        s.verify_passed = true;
        s.pr_url = Some("https://github.com/eduuh/bn/pull/1".into());
        assert_eq!(infer(&s).phase, Phase::Execute);

        s.open_todos = 0;
        s.next_todo = None;
        s.blocker = Some("finish the migration".into());
        assert_eq!(infer(&s).phase, Phase::Execute);
    }

    #[test]
    fn critic_failure_loops_back_and_pass_finishes() {
        let mut s = signals();
        s.critic_failed = true;
        assert_eq!(infer(&s).phase, Phase::Execute);
        assert_eq!(infer(&s).action, "Fix the critic findings");
        s.critic_failed = false;
        s.critic_passed = true;
        assert_eq!(infer(&s).phase, Phase::Done);

        s.critic_failed = true;
        assert_eq!(infer(&s).phase, Phase::Execute);
    }

    #[test]
    fn phase_statuses_mark_past_active_and_pending() {
        let view = infer(&signals());
        assert_eq!(
            view.phases,
            vec![
                PhaseView {
                    id: Phase::Plan,
                    status: PhaseStatus::Done,
                },
                PhaseView {
                    id: Phase::Execute,
                    status: PhaseStatus::Active,
                },
                PhaseView {
                    id: Phase::Critique,
                    status: PhaseStatus::Pending,
                },
                PhaseView {
                    id: Phase::Done,
                    status: PhaseStatus::Pending,
                },
            ]
        );
    }

    #[test]
    fn closed_note_is_done_even_without_critic_signal() {
        let mut s = signals();
        s.note_closed = true;
        assert_eq!(infer(&s).phase, Phase::Done);
    }

    /// The whole point of the rename (#994): the inferred vocabulary must never serialize into a
    /// field named `stage`, because `bn pipeline status` emits the durable 9-value vocabulary under
    /// that name and `"execute"`/`"plan"` are members of both.
    #[test]
    fn view_serializes_phase_never_stage() {
        let json = serde_json::to_value(infer(&signals())).unwrap();
        assert_eq!(json["phase"], "execute");
        assert!(
            json.get("stage").is_none(),
            "PipelineView must not carry a `stage` field: {json}"
        );
        assert!(
            json.get("stages").is_none(),
            "PipelineView must not carry a `stages` field: {json}"
        );
        assert_eq!(json["phases"][1]["id"], "execute");
        assert_eq!(json["phases"][1]["status"], "active");
    }
}
