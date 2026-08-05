//! The Goals-Alignment agent's deterministic gatherer (#563) — `bn staff gather goals-align`.
//!
//! The first *dashboard-managed* staff agent: it keeps the owner aligned with their goals. This
//! module is the deterministic half — the packet builder. It fuses the owner's north-star goals
//! (today's daily goals + the current quarter's goals and its latest impact review) with the
//! **Command Center activities** the owner is actually spending time on (the `/api/actions` fold,
//! [`crate::action_center::build_actions`]) into one compact JSON packet. That packet is the only
//! world the agent sees; the pinned `staff/goals-align` skill then scores each activity's impact
//! against the goals and emits a briefing card ("what has impact / what to reprioritize / where to
//! focus") routed through the closed staff sinks.
//!
//! Everything the agent needs to be *configurable* — an editable prompt, a selectable model /
//! harness, a weekly cron cadence, and on-demand runs via `POST /api/cron/run` — comes for free
//! from the staff roster ([`crate::staff`]); the roster entry ships in `config/agents/goals-align.md`.
//!
//! Like the Scout ([`crate::scout::gather_packet`]), the packet build is split so the fold is
//! testable in isolation: [`build_packet`] is a pure function over already-read viewmodels, and
//! [`gather_packet_json`] performs the reads (mirroring the `/api/actions` handler) and serializes.

use crate::action_center::ActionItem;
use crate::model::Item;
use crate::quarter::Quarter;
use crate::resolver::Config;
use serde::Serialize;

/// The agent name — its roster entry, cron job (`staff-goals-align`), and card folder.
pub const AGENT: &str = "goals-align";

/// A compact projection of one connected note ([`crate::model::GoalNote`]) — a linked branch note
/// or an attached text/upload — as a single human line the agent reads for goal context (#564).
fn note_summary(n: &crate::model::GoalNote) -> String {
    if n.is_link() {
        let repo = n.repo.as_deref().unwrap_or("");
        let branch = n.branch.as_deref().unwrap_or("");
        format!("linked note {repo}/{branch}")
    } else {
        let label = n.title.as_deref().filter(|s| !s.is_empty());
        let text = n.text.as_deref().unwrap_or("").trim();
        match label {
            Some(l) => format!("{l}: {text}"),
            None => text.to_string(),
        }
    }
}

/// One north-star goal (a daily goal) as the agent sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalView {
    pub text: String,
    /// Linked quarter-goal key (`--q <key>`), when the daily goal is tied to a quarter goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Connected-note context lines (#564).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub done: bool,
}

impl GoalView {
    fn from_item(it: &Item) -> GoalView {
        GoalView {
            text: it.text.clone(),
            q: it.q.clone().filter(|s| !s.is_empty()),
            link: it.link.clone().filter(|s| !s.is_empty()),
            notes: it.notes.iter().map(note_summary).collect(),
            done: it.done.unwrap_or(false),
        }
    }
}

/// One quarter goal with its current status from the latest impact review.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuarterGoalView {
    pub key: String,
    pub text: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// `done` / `on-track` / `at-risk` / `off-track` / `unscored` ([`Quarter::status_of`]).
    pub status: String,
}

/// The current quarter's goals plus the recommendation of its most recent impact review.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuarterView {
    pub quarter: String,
    pub goals: Vec<QuarterGoalView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_recommendation: Option<String>,
}

impl QuarterView {
    fn from_quarter(q: &Quarter) -> QuarterView {
        let goals = q
            .front
            .goals
            .iter()
            .map(|g| QuarterGoalView {
                key: g.key.clone(),
                text: g.text.clone(),
                done: g.done,
                metric: g.metric.clone(),
                status: q.status_of(&g.key).to_string(),
            })
            .collect();
        QuarterView {
            quarter: q.front.quarter.clone(),
            goals,
            latest_recommendation: q
                .latest_review()
                .map(|r| r.recommendation.clone())
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

/// One Command Center activity, projected down to the fields the alignment judgement needs — the
/// dispatch payload / suggested-prompt noise of [`ActionItem`] is dropped.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivityView {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub source: String,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl ActivityView {
    fn from_action(a: &ActionItem) -> ActivityView {
        ActivityView {
            id: a.id.clone(),
            kind: a.kind.as_str().to_string(),
            title: a.title.clone(),
            source: a.source.clone(),
            severity: a.severity.clone(),
            repo: a.repo.clone(),
            link: a.link.clone(),
        }
    }
}

/// The deterministic packet the Goals-Alignment agent is prompted with: the north-star goals
/// (daily + quarter) and the Command Center activities to weigh against them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalsAlignPacket {
    pub agent: &'static str,
    pub date: String,
    pub machine: String,
    /// Today's daily goals (open and completed).
    pub daily_goals: Vec<GoalView>,
    /// The current quarter's goals + latest review recommendation, when a quarter file exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarter: Option<QuarterView>,
    /// The Command Center activity feed — what the owner is actually spending time on.
    pub activities: Vec<ActivityView>,
    /// Deterministic operator notes (degraded inputs), never instructions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Build the packet from already-read viewmodels — a pure fold, so the shape is unit-testable
/// without touching the store or the clock.
pub fn build_packet(
    machine: &str,
    today: &str,
    daily_goals: &[Item],
    quarter: Option<&Quarter>,
    activities: &[ActionItem],
) -> GoalsAlignPacket {
    let mut notes = Vec::new();
    if daily_goals.is_empty() {
        notes.push("no daily goals set today — alignment is quarter-only".to_string());
    }
    if quarter.is_none() {
        notes.push("no quarter file for the current quarter — no quarter north star".to_string());
    }
    if activities.is_empty() {
        notes.push("Command Center is empty — no activities to weigh".to_string());
    }

    GoalsAlignPacket {
        agent: AGENT,
        date: today.to_string(),
        machine: machine.to_string(),
        daily_goals: daily_goals.iter().map(GoalView::from_item).collect(),
        quarter: quarter.map(QuarterView::from_quarter),
        activities: activities.iter().map(ActivityView::from_action).collect(),
        notes,
    }
}

/// `bn staff gather goals-align` entry point: read today's daily goals, the current quarter, and
/// the Command Center activity fold (the same reads the `/api/actions` handler does), build the
/// packet, and serialize it. A read-only gather — it never creates a daily/quarter file.
pub fn gather_packet_json(cfg: &Config, today: &str) -> String {
    let daily_goals = crate::daily::read(&cfg.daily_file(today))
        .map(|d| d.front.goals)
        .unwrap_or_default();

    let quarter = crate::quarter::quarter_id(today)
        .and_then(|qid| crate::quarter::read_quarter(cfg, &qid).ok().flatten());

    // Command Center activity fold — mirrors the `/api/actions` source assembly.
    let attn = crate::attention::open_items(today);
    let reviews = crate::plan_review::list();
    let crons = crate::crons::status_views(cfg, None);
    let note_summaries = crate::rollups::summary(cfg);
    let queue: Vec<crate::orchestrate::QueueItem> = crate::orchestrate::run_paths()
        .into_iter()
        .filter_map(|p| crate::orchestrate::OrchestrateState::load(&p))
        .flat_map(|st| st.queue.into_iter())
        .collect();
    let sources = crate::action_center::ActionSources {
        attn: &attn,
        reviews: &reviews,
        crons: &crons,
        notes: &note_summaries,
        goals: &daily_goals,
        queue: &queue,
        suggestions: &[],
        now: crate::util::now_epoch_secs(),
    };
    let activities = crate::action_center::build_actions(&sources);

    let packet = build_packet(
        cfg.machine(),
        today,
        &daily_goals,
        quarter.as_ref(),
        &activities,
    );
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_center::{ActionKind, Urgency};
    use crate::model::GoalNote;
    use crate::quarter::{GoalAssessment, ImpactReview, QuarterFront, QuarterGoal};
    use serde_json::Value;

    fn goal(text: &str, q: Option<&str>, done: bool) -> Item {
        Item {
            id: "g1".into(),
            text: text.into(),
            created: "2026-07-15".into(),
            done: Some(done),
            done_at: None,
            link: None,
            pct: None,
            est: None,
            actual: None,
            q: q.map(str::to_string),
            notes: Vec::new(),
        }
    }

    fn action(id: &str, kind: ActionKind, title: &str) -> ActionItem {
        ActionItem {
            id: id.into(),
            kind,
            title: title.into(),
            source: "test".into(),
            severity: "warn".into(),
            repo: Some("bn".into()),
            branch: None,
            link: None,
            detail: None,
            payload: Value::Null,
            suggested_prompt: String::new(),
            default_urgency: Urgency::Queued,
            created: 0,
            stale: false,
        }
    }

    fn quarter_with(goals: Vec<QuarterGoal>, reviews: Vec<ImpactReview>) -> Quarter {
        Quarter {
            front: QuarterFront {
                quarter: "2026-Q3".into(),
                created: "2026-07-01".into(),
                goals,
                reviews,
            },
        }
    }

    #[test]
    fn packet_fuses_goals_quarter_and_activities() {
        let daily = vec![goal("ship the alignment agent", Some("ship-v1"), false)];
        let q = quarter_with(
            vec![QuarterGoal {
                key: "ship-v1".into(),
                text: "ship v1".into(),
                created: "2026-07-01".into(),
                done: false,
                done_at: None,
                metric: Some("pr_cycle_time<-30%".into()),
            }],
            vec![ImpactReview {
                date: "2026-07-10".into(),
                window_start: "2026-07-01".into(),
                window_end: "2026-07-10".into(),
                assessments: vec![GoalAssessment {
                    key: "ship-v1".into(),
                    status: "on-track".into(),
                    evidence: "3 PRs merged".into(),
                }],
                ranking: vec![],
                recommendation: "keep shipping ship-v1".into(),
            }],
        );
        let acts = vec![
            action("todo:bn/x/t1", ActionKind::Todo, "unrelated refactor"),
            action("goal:g1", ActionKind::Goal, "ship the alignment agent"),
        ];

        let p = build_packet("laptop", "2026-07-15", &daily, Some(&q), &acts);

        assert_eq!(p.agent, "goals-align");
        assert_eq!(p.date, "2026-07-15");
        assert_eq!(p.machine, "laptop");
        assert_eq!(p.daily_goals.len(), 1);
        assert_eq!(p.daily_goals[0].q.as_deref(), Some("ship-v1"));

        let qv = p.quarter.expect("quarter present");
        assert_eq!(qv.goals.len(), 1);
        assert_eq!(qv.goals[0].status, "on-track");
        assert_eq!(
            qv.latest_recommendation.as_deref(),
            Some("keep shipping ship-v1")
        );

        assert_eq!(p.activities.len(), 2);
        assert_eq!(p.activities[0].kind, "todo");
        assert_eq!(p.activities[1].kind, "goal");
        assert!(p.notes.is_empty());
    }

    #[test]
    fn empty_inputs_degrade_with_notes_not_errors() {
        let p = build_packet("laptop", "2026-07-15", &[], None, &[]);
        assert!(p.quarter.is_none());
        assert!(p.daily_goals.is_empty());
        assert!(p.activities.is_empty());
        assert_eq!(p.notes.len(), 3);
        // Serializes to valid JSON with the agent name — the harness contract.
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"agent\":\"goals-align\""));
    }

    #[test]
    fn connected_notes_are_summarized() {
        let mut g = goal("write docs", None, false);
        g.notes = vec![
            GoalNote {
                id: "n1".into(),
                repo: Some("bn".into()),
                branch: Some("feat-docs".into()),
                text: None,
                title: None,
                created: "2026-07-15".into(),
            },
            GoalNote {
                id: "n2".into(),
                repo: None,
                branch: None,
                text: Some("outline is in the wiki".into()),
                title: Some("outline".into()),
                created: "2026-07-15".into(),
            },
        ];
        let p = build_packet("laptop", "2026-07-15", std::slice::from_ref(&g), None, &[]);
        assert_eq!(p.daily_goals[0].notes.len(), 2);
        assert_eq!(p.daily_goals[0].notes[0], "linked note bn/feat-docs");
        assert_eq!(p.daily_goals[0].notes[1], "outline: outline is in the wiki");
    }

    #[test]
    fn unscored_quarter_goal_without_review() {
        let q = quarter_with(
            vec![QuarterGoal {
                key: "explore".into(),
                text: "explore ideas".into(),
                created: "2026-07-01".into(),
                done: false,
                done_at: None,
                metric: None,
            }],
            vec![],
        );
        let p = build_packet("laptop", "2026-07-15", &[], Some(&q), &[]);
        let qv = p.quarter.unwrap();
        assert_eq!(qv.goals[0].status, "unscored");
        assert!(qv.latest_recommendation.is_none());
    }
}
