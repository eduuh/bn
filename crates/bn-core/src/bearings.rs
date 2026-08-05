//! A bounded, read-only current-state snapshot shared by CLI and web status surfaces.

use crate::agent::{AgentHealth, AgentRole, Fleet, LiveAgent};
use crate::attention::AttnItem;
use crate::orchestrate::{Counts, ItemStatus, OrchestrateState, QueueItem};
use crate::resolver::Config;
use serde::Serialize;

pub const MAX_AGENTS: usize = 8;
pub const MAX_RUNS: usize = 6;
pub const MAX_LANES_PER_RUN: usize = 5;
pub const MAX_ATTENTION: usize = 8;
const MAX_TEXT_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsSnapshot {
    pub generated_at: i64,
    pub fleet: BearingsFleet,
    pub orchestration: BearingsOrchestration,
    pub attention: BearingsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsFleet {
    pub starting: usize,
    pub active: usize,
    pub idle: usize,
    pub stuck: usize,
    pub total_tokens: i64,
    pub agents: Vec<BearingsAgent>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsAgent {
    pub target: String,
    pub harness: String,
    pub health: AgentHealth,
    pub role: AgentRole,
    pub last_activity_ts: i64,
    pub idle_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsOrchestration {
    pub runs: Vec<BearingsRun>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsRun {
    pub repo: String,
    pub counts: Counts,
    pub resident: bool,
    pub stopped: bool,
    pub updated_at: i64,
    pub lanes: Vec<BearingsLane>,
    pub omitted_lanes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsLane {
    pub repo: String,
    pub id: String,
    pub item: String,
    pub branch: Option<String>,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsAttention {
    pub items: Vec<BearingsAttentionItem>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BearingsAttentionItem {
    pub id: String,
    pub severity: String,
    pub title: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub link: Option<String>,
    pub created: i64,
}

impl BearingsSnapshot {
    /// Gather the machine's current fleet, orchestration, and actionable-attention state.
    pub fn gather(cfg: &Config, now: i64, today: &str) -> Self {
        let fleet = crate::agent::supervise(
            cfg,
            crate::agent::DEFAULT_IDLE_MINS,
            crate::agent::DEFAULT_STUCK_MINS,
        );
        let runs = crate::orchestrate::run_paths()
            .iter()
            .filter_map(|path| OrchestrateState::load(path))
            .collect::<Vec<_>>();
        let attention = crate::attention::open_items(today);
        Self::from_parts(now, &fleet, &runs, &attention)
    }

    /// Build the stable bounded view from already-read source values.
    pub fn from_parts(
        generated_at: i64,
        fleet: &Fleet,
        runs: &[OrchestrateState],
        attention: &[AttnItem],
    ) -> Self {
        let mut runs = runs.iter().collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at));

        Self {
            generated_at,
            fleet: BearingsFleet::from_fleet(fleet),
            orchestration: BearingsOrchestration::from_runs(&runs),
            attention: BearingsAttention::from_items(attention),
        }
    }

    /// Render a compact Markdown artifact suitable for a terminal, handoff, or status card.
    pub fn render_markdown(&self) -> String {
        let mut out = vec![
            "# Bearings".to_string(),
            String::new(),
            format!("Generated at `{}`.", self.generated_at),
            String::new(),
            format!(
                "## Fleet — {} active · {} starting · {} idle · {} stuck",
                self.fleet.active, self.fleet.starting, self.fleet.idle, self.fleet.stuck
            ),
        ];
        if self.fleet.agents.is_empty() {
            out.push("- No live agents.".to_string());
        } else {
            out.extend(self.fleet.agents.iter().map(|agent| {
                format!(
                    "- **{}** [{}] · {} · {} · idle {}",
                    inline(&agent.target),
                    agent.health.label(),
                    inline(&agent.harness),
                    agent.role.label(),
                    duration(agent.idle_secs)
                )
            }));
            push_omitted(&mut out, self.fleet.omitted, "agents");
        }

        out.push(String::new());
        out.push("## Orchestration".to_string());
        if self.orchestration.runs.is_empty() {
            out.push("- No orchestration runs.".to_string());
        } else {
            for run in &self.orchestration.runs {
                let mode = if run.stopped {
                    "stopped"
                } else if run.resident {
                    "resident"
                } else {
                    "bounded"
                };
                out.push(format!(
                    "- **{}** [{}] · {} queued · {} in flight · {} merged · {} failed",
                    inline(&run.repo),
                    mode,
                    run.counts.queued,
                    run.counts.in_flight,
                    run.counts.merged,
                    run.counts.failed
                ));
                out.extend(run.lanes.iter().map(|lane| {
                    let branch = lane
                        .branch
                        .as_deref()
                        .map(|value| format!(" · {}", inline(value)))
                        .unwrap_or_default();
                    format!(
                        "  - `{}/{}` {} [{}]{}",
                        inline(&lane.repo),
                        inline(&lane.id),
                        inline(&lane.item),
                        lane.status.label(),
                        branch
                    )
                }));
                push_omitted(&mut out, run.omitted_lanes, "lanes");
            }
            push_omitted(&mut out, self.orchestration.omitted, "runs");
        }

        out.push(String::new());
        out.push("## Attention".to_string());
        if self.attention.items.is_empty() {
            out.push("- Nothing actionable.".to_string());
        } else {
            out.extend(self.attention.items.iter().map(|item| {
                let target = match (&item.repo, &item.branch) {
                    (Some(repo), Some(branch)) => {
                        format!(" · {}/{}", inline(repo), inline(branch))
                    }
                    (Some(repo), None) => format!(" · {}", inline(repo)),
                    _ => String::new(),
                };
                format!(
                    "- **{}** [{}] {}{}",
                    inline(&item.id),
                    inline(&item.severity),
                    inline(&item.title),
                    target
                )
            }));
            push_omitted(&mut out, self.attention.omitted, "items");
        }
        out.push(String::new());
        out.join("\n")
    }
}

impl BearingsFleet {
    fn from_fleet(fleet: &Fleet) -> Self {
        Self {
            starting: fleet.starting,
            active: fleet.active,
            idle: fleet.idle,
            stuck: fleet.stuck,
            total_tokens: fleet.total_tokens,
            agents: fleet
                .agents
                .iter()
                .take(MAX_AGENTS)
                .map(Into::into)
                .collect(),
            omitted: fleet.agents.len().saturating_sub(MAX_AGENTS),
        }
    }
}

impl From<&LiveAgent> for BearingsAgent {
    fn from(agent: &LiveAgent) -> Self {
        Self {
            target: bounded_text(&agent.target),
            harness: bounded_text(&agent.harness),
            health: agent.health,
            role: agent.role,
            last_activity_ts: agent.last_activity_ts,
            idle_secs: agent.idle_secs,
        }
    }
}

impl BearingsOrchestration {
    fn from_runs(runs: &[&OrchestrateState]) -> Self {
        Self {
            runs: runs
                .iter()
                .take(MAX_RUNS)
                .map(|run| (*run).into())
                .collect(),
            omitted: runs.len().saturating_sub(MAX_RUNS),
        }
    }
}

impl From<&OrchestrateState> for BearingsRun {
    fn from(run: &OrchestrateState) -> Self {
        let visible = run
            .queue
            .iter()
            .filter(|item| matches!(item.status, ItemStatus::Queued | ItemStatus::Dispatched))
            .collect::<Vec<_>>();
        Self {
            repo: bounded_text(&run.repo),
            counts: run.counts(),
            resident: run.is_resident(),
            stopped: run.stopped,
            updated_at: run.updated_at,
            lanes: visible
                .iter()
                .take(MAX_LANES_PER_RUN)
                .map(|item| BearingsLane::from_item(item, &run.repo))
                .collect(),
            omitted_lanes: visible.len().saturating_sub(MAX_LANES_PER_RUN),
        }
    }
}

impl BearingsLane {
    fn from_item(item: &QueueItem, run_repo: &str) -> Self {
        Self {
            repo: bounded_text(if item.repo.is_empty() {
                run_repo
            } else {
                &item.repo
            }),
            id: bounded_text(&item.id),
            item: bounded_text(&item.item),
            branch: item.branch.as_deref().map(bounded_text),
            status: item.status,
        }
    }
}

impl BearingsAttention {
    fn from_items(items: &[AttnItem]) -> Self {
        Self {
            items: items.iter().take(MAX_ATTENTION).map(Into::into).collect(),
            omitted: items.len().saturating_sub(MAX_ATTENTION),
        }
    }
}

impl From<&AttnItem> for BearingsAttentionItem {
    fn from(item: &AttnItem) -> Self {
        Self {
            id: bounded_text(&item.id),
            severity: bounded_text(&item.severity),
            title: bounded_text(&item.title),
            repo: item.repo.as_deref().map(bounded_text),
            branch: item.branch.as_deref().map(bounded_text),
            link: item.link.as_deref().map(bounded_text),
            created: item.created,
        }
    }
}

fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3_600)
    }
}

fn inline(value: &str) -> String {
    bounded_text(value)
}

fn bounded_text(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let truncated = chars.by_ref().take(MAX_TEXT_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn push_omitted(out: &mut Vec<String>, omitted: usize, noun: &str) {
    if omitted > 0 {
        out.push(format!("- … {omitted} more {noun}."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentRole, LiveAgent};
    use crate::attention::AttnItem;

    fn agent(index: usize) -> LiveAgent {
        LiveAgent {
            session_id: format!("session-{index}"),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: format!("feature-{index}"),
            target: format!("bn:feature-{index}"),
            last_activity_ts: 100,
            idle_secs: index as i64 * 60,
            health: AgentHealth::Active,
            role: AgentRole::Worker,
            turns: 1,
            tool_calls: 2,
            input_tokens: 0,
            output_tokens: 3,
            cache_read_tokens: 0,
        }
    }

    fn attention(index: usize) -> AttnItem {
        AttnItem {
            id: format!("a{index}"),
            source: "manual".into(),
            title: format!("attention {index}"),
            severity: "warn".into(),
            repo: Some("bn".into()),
            branch: Some(format!("feature-{index}")),
            link: None,
            key: None,
            detail: None,
            mode: None,
            prompt: None,
            task_type: None,
            worktree_path: None,
            uncommitted_files: Vec::new(),
            created: index as i64,
        }
    }

    #[test]
    fn snapshot_caps_every_repeated_section_and_reports_omissions() {
        let agents = (0..MAX_AGENTS + 3).map(agent).collect::<Vec<_>>();
        let fleet = Fleet {
            active: agents.len(),
            agents,
            ..Default::default()
        };
        let runs = (0..MAX_RUNS + 2)
            .map(|index| {
                let items = (0..MAX_LANES_PER_RUN + 2)
                    .map(|lane| format!("task-{index}-{lane}"))
                    .collect::<Vec<_>>();
                OrchestrateState::new("bn", &items, 2, false, index as i64)
            })
            .collect::<Vec<_>>();
        let attention = (0..MAX_ATTENTION + 4).map(attention).collect::<Vec<_>>();

        let snapshot = BearingsSnapshot::from_parts(200, &fleet, &runs, &attention);

        assert_eq!(snapshot.fleet.agents.len(), MAX_AGENTS);
        assert_eq!(snapshot.fleet.omitted, 3);
        assert_eq!(snapshot.orchestration.runs.len(), MAX_RUNS);
        assert_eq!(snapshot.orchestration.omitted, 2);
        assert!(snapshot
            .orchestration
            .runs
            .iter()
            .all(|run| run.lanes.len() == MAX_LANES_PER_RUN));
        assert_eq!(snapshot.attention.items.len(), MAX_ATTENTION);
        assert_eq!(snapshot.attention.omitted, 4);
    }

    #[test]
    fn markdown_is_line_bounded_and_collapses_multiline_text() {
        let fleet = Fleet {
            agents: vec![agent(0)],
            active: 1,
            ..Default::default()
        };
        let mut item = attention(0);
        item.title = "first line\nsecond line".into();
        let snapshot = BearingsSnapshot::from_parts(200, &fleet, &[], &[item]);

        let rendered = snapshot.render_markdown();

        assert!(rendered.contains("first line second line"));
        assert_eq!(rendered.lines().count(), 12);
        assert!(!rendered.contains("first line\nsecond line"));
    }

    #[test]
    fn machine_snapshot_strings_are_normalized_and_bounded() {
        let mut item = attention(0);
        item.title = format!("first\n{}", "x".repeat(MAX_TEXT_CHARS + 20));

        let snapshot = BearingsSnapshot::from_parts(200, &Fleet::default(), &[], &[item]);
        let title = &snapshot.attention.items[0].title;

        assert!(!title.contains('\n'));
        assert_eq!(title.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn lane_identity_keeps_the_item_repository() {
        let mut run = OrchestrateState::new("control", &["851".into()], 1, false, 100);
        run.queue[0].repo = "bn".into();

        let snapshot = BearingsSnapshot::from_parts(200, &Fleet::default(), &[run], &[]);

        assert_eq!(snapshot.orchestration.runs[0].lanes[0].repo, "bn");
    }
}
