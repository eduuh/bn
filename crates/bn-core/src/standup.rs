//! The Standup agent's deterministic gatherer (#566) — `bn staff gather standup`.
//!
//! The second *dashboard-managed* staff agent (sibling of [`crate::goals_align`]): each morning it
//! turns **yesterday's completed tasks** into a standup summary. This module is the deterministic
//! half — the packet builder. It reuses the **Completed** fold ([`crate::completed`], the `#561`
//! counterpart to the action center): closed branch-note todos, achieved daily goals, resolved
//! attention items, and landed/merged orchestrate work. It narrows that history to *yesterday*
//! (the most recent prior day file — Friday on a Monday, via [`crate::daily::previous_day`]),
//! groups it by repo, and appends **today's focus** (today's open daily goals). That packet is the
//! only world the agent sees; the pinned `staff/standup` skill then writes the standup summary and
//! routes it through the closed staff sinks.
//!
//! Everything the agent needs to be *configurable* — an editable prompt, a selectable model /
//! harness, a daily cron cadence, and on-demand runs via `POST /api/cron/run` — comes for free from
//! the staff roster ([`crate::staff`]); the roster entry ships in `config/agents/standup.md`.
//!
//! Like the Goals-Alignment agent, the packet build is split so the fold is testable in isolation:
//! [`build_packet`] is a pure function over already-read viewmodels, and [`gather_packet_json`]
//! performs the reads (mirroring the `/api/completed` handler) and serializes.

use crate::completed::CompletedItem;
use crate::model::Item;
use crate::util;
use serde::Serialize;

/// The agent name — its roster entry, cron job (`staff-standup`), and card folder.
pub const AGENT: &str = "standup";

/// One completed task, projected down to the fields the standup summary needs — the note-jump /
/// completion-timestamp plumbing of [`CompletedItem`] is dropped.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DoneItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl DoneItem {
    fn from_completed(c: &CompletedItem) -> DoneItem {
        DoneItem {
            id: c.id.clone(),
            kind: c.kind.as_str().to_string(),
            title: c.title.clone(),
            source: c.source.clone(),
            link: c.link.clone(),
            detail: c.detail.clone(),
        }
    }
}

/// Yesterday's completed items for one repo (or the repo-less "other" bucket), most-recent-first
/// within the group — the standup's natural "what got done, per project" grouping.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoGroup {
    /// The repo name, or `"other"` for items with no repo (bare daily goals, unattributed attn).
    pub repo: String,
    pub items: Vec<DoneItem>,
}

/// One open goal for today — the optional "today's focus" beat of the standup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FocusGoal {
    pub text: String,
    /// Linked quarter-goal key (`--q <key>`), when the daily goal is tied to a quarter goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl FocusGoal {
    fn from_item(it: &Item) -> FocusGoal {
        FocusGoal {
            text: it.text.clone(),
            q: it.q.clone().filter(|s| !s.is_empty()),
            link: it.link.clone().filter(|s| !s.is_empty()),
        }
    }
}

/// The deterministic packet the Standup agent is prompted with: yesterday's completed tasks
/// (grouped by repo) and today's focus goals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StandupPacket {
    pub agent: &'static str,
    /// The run's logical date (today).
    pub date: String,
    pub machine: String,
    /// The date the "yesterday" fold covers — the most recent prior day file (Friday on a Monday).
    pub yesterday: String,
    /// Yesterday's completed tasks, grouped by repo (repo name asc; `"other"` last).
    pub completed: Vec<RepoGroup>,
    /// Total completed items across all groups — a quick "how much got done" signal.
    pub completed_count: usize,
    /// Today's open daily goals — the optional "today's focus" beat.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub today_focus: Vec<FocusGoal>,
    /// Deterministic operator notes (degraded inputs), never instructions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Group yesterday's completed items by repo. Repo-less items land in the `"other"` bucket. Groups
/// are ordered by repo name ascending with `"other"` last; items keep the caller's recency order.
fn group_by_repo(items: &[&CompletedItem]) -> Vec<RepoGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<DoneItem>> =
        std::collections::HashMap::new();
    for c in items {
        let repo = c.repo.clone().unwrap_or_else(|| "other".to_string());
        if !groups.contains_key(&repo) {
            order.push(repo.clone());
        }
        groups
            .entry(repo)
            .or_default()
            .push(DoneItem::from_completed(c));
    }
    order.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("other", "other") => std::cmp::Ordering::Equal,
        ("other", _) => std::cmp::Ordering::Greater,
        (_, "other") => std::cmp::Ordering::Less,
        (x, y) => x.cmp(y),
    });
    order
        .into_iter()
        .map(|repo| RepoGroup {
            items: groups.remove(&repo).unwrap_or_default(),
            repo,
        })
        .collect()
}

/// Build the packet from already-read viewmodels — a pure fold, so the shape is unit-testable
/// without touching the store or the clock. `completed` is the full Completed history; only items
/// whose completion falls on `yesterday` (local date) are folded in. `today_goals` are today's
/// daily goals; the open ones become "today's focus".
pub fn build_packet(
    machine: &str,
    today: &str,
    yesterday: &str,
    completed: &[CompletedItem],
    today_goals: &[Item],
) -> StandupPacket {
    let done_yesterday: Vec<&CompletedItem> = completed
        .iter()
        .filter(|c| util::local_date_of(c.completed) == yesterday)
        .collect();
    let completed_count = done_yesterday.len();
    let groups = group_by_repo(&done_yesterday);

    let today_focus: Vec<FocusGoal> = today_goals
        .iter()
        .filter(|g| g.done != Some(true))
        .map(FocusGoal::from_item)
        .collect();

    let mut notes = Vec::new();
    if completed_count == 0 {
        notes.push(format!(
            "no completed tasks recorded for {yesterday} — a quiet day, or the day's work landed elsewhere"
        ));
    }
    if today_focus.is_empty() {
        notes.push("no open daily goals for today — no focus to project".to_string());
    }

    StandupPacket {
        agent: AGENT,
        date: today.to_string(),
        machine: machine.to_string(),
        yesterday: yesterday.to_string(),
        completed: groups,
        completed_count,
        today_focus,
        notes,
    }
}

/// `bn staff gather standup` entry point: resolve yesterday (the most recent prior day file), read
/// the Completed fold sources (the same reads the `/api/completed` handler does, but with
/// yesterday's daily goals), read today's goals for the focus beat, build the packet, and serialize
/// it. A read-only gather — it never creates a daily file.
pub fn gather_packet_json(cfg: &crate::resolver::Config, today: &str) -> String {
    // "Yesterday" is the most recent day file strictly before today (Friday on a Monday). Fall
    // back to the literal calendar day before when no prior file exists.
    let prev = crate::daily::previous_day(cfg, today);
    let yesterday = prev
        .as_ref()
        .map(|d| d.front.date.clone())
        .or_else(|| util::add_days(today, -1))
        .unwrap_or_else(|| today.to_string());
    let yesterday_goals = prev.map(|d| d.front.goals).unwrap_or_default();

    // Completed fold sources — mirrors the `/api/completed` handler's assembly, but the goals are
    // yesterday's (achieved-yesterday goals are the ones that belong in the standup).
    let todos = crate::rollups::completed_todos(cfg);
    let attn = crate::attention::completed_items(today);
    let merged: Vec<crate::orchestrate::QueueItem> = crate::orchestrate::run_paths()
        .into_iter()
        .filter_map(|p| crate::orchestrate::OrchestrateState::load(&p))
        .flat_map(|st| st.queue.into_iter())
        .collect();
    let sources = crate::completed::CompletedSources {
        todos: &todos,
        goals: &yesterday_goals,
        attn: &attn,
        merged: &merged,
    };
    let linker = crate::rollups::FsNoteLinker::new(cfg, &crate::runner::RealRunner);
    let completed = crate::completed::build_completed(&sources, &linker);

    let today_goals = crate::daily::view(cfg, today).front.goals;

    let packet = build_packet(cfg.machine(), today, &yesterday, &completed, &today_goals);
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
}
