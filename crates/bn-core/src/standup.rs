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
//! the Markdown agent catalog; the manifest ships in `config/agents/standup.md`.
//!
//! Like the Goals-Alignment agent, the packet build is split so the fold is testable in isolation:
//! [`build_packet`] is a pure function over already-read viewmodels, and [`gather_packet_json`]
//! performs the reads (mirroring the `/api/completed` handler) and serializes.

use crate::completed::CompletedItem;
use crate::model::Item;
use crate::util;
use serde::Serialize;

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

/// The standing weaknesses the brief should coach against — the "where am I losing" half of the
/// morning, computed from the scorecard so the agent reasons over measurements rather than
/// impressions. Every field is a plain number the prompt may quote; none of it is advice.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gaps {
    /// Trailing window these figures cover.
    pub window_days: i64,
    /// Merged PRs in the window, and how many ever received a review from someone else.
    pub merged: usize,
    pub reviewed: usize,
    /// reviewed ÷ merged — review coverage. `None` when nothing merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_coverage: Option<f64>,
    /// Open branches in flight, and how many have gone stale.
    pub in_flight: usize,
    pub stale: usize,
    /// Age of the oldest open branch, in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_branch_days: Option<i64>,
    /// The branch the WIP guard says to finish next, and when the backlog clears at current cadence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_by: Option<String>,
    /// Share of classified commits that were `feat:` — the building-vs-maintaining axis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_ratio: Option<f64>,
    /// Rework rate, and whether it was measured at all. Churn is read from the *current directory's*
    /// git log, so a run from an excluded repo reports `churn_counted: false` — say nothing about
    /// churn in that case rather than quoting a number that describes tooling work.
    pub churn_counted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub churn_rate: Option<f64>,
    /// Agent churn-rate ÷ hand churn-rate. Above 1.5 means agent-written code is reworked more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_vs_hand_ratio: Option<f64>,
}

impl Gaps {
    /// Read the gap figures off a scorecard. Pure projection — no judgement, so the numbers the
    /// brief quotes are exactly the numbers `bn metrics` shows.
    pub fn from_scorecard(s: &crate::metrics::Scorecard) -> Gaps {
        Gaps {
            window_days: s.window_days,
            merged: s.pr.merged,
            reviewed: s.review_latency.reviewed,
            review_coverage: (s.pr.merged > 0)
                .then(|| s.review_latency.reviewed as f64 / s.pr.merged as f64),
            in_flight: s.wip_guard.in_flight,
            stale: s.wip_guard.stale,
            oldest_branch_days: s.branch_age_max_days,
            finish_next: s.wip_guard.recommend.clone(),
            clear_by: s.wip_guard.clear_by.clone(),
            feature_ratio: s.impact.feature_ratio,
            churn_counted: s.churn.counted,
            churn_rate: s.churn.counted.then_some(s.churn.churn_rate).flatten(),
            agent_vs_hand_ratio: s
                .churn
                .counted
                .then_some(s.churn.agent_vs_hand_ratio)
                .flatten(),
        }
    }
}

/// The deterministic packet the Standup agent is prompted with: yesterday's completed tasks
/// (grouped by repo) and today's focus goals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StandupPacket {
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
    /// Standing weaknesses from the scorecard — the "what to improve" beat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Gaps>,
    /// This cycle's learning pick — the "what to read" beat. `None` when the curriculum has not
    /// chosen a topic yet, in which case the brief simply omits the beat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading: Option<Reading>,
    /// Deterministic operator notes (degraded inputs), never instructions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The current curriculum pick, as the brief should present it: one topic, one resource, bounded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The agent whose latest card carries this cycle's learning pick.
///
/// A card is filed under the roster name that produced it, and that name changed when the agent
/// became a Markdown manifest — leaving this filter matching nothing and the Reading beat frozen
/// on the last card the old native job ever wrote, still presented as current.
const CURRICULUM_AGENT: &str = "staff-curriculum";

/// This cycle's learning pick, read off the most recent stored Curriculum card.
///
/// The curriculum job already chose one topic and one resource and wrote them to the feed; re-deriving
/// them here would risk disagreeing with the card the user can open. So this reads that card rather
/// than re-running the choice: the goal item carries `curriculum: <topic> — 30m`, and the research
/// item carries the resource title and its link.
pub fn reading_from_feed(cfg: &crate::resolver::Config) -> Option<Reading> {
    let mut refs: Vec<crate::staff::CardRef> = crate::staff::list_cards(cfg)
        .into_iter()
        .filter(|c| c.agent == CURRICULUM_AGENT)
        .collect();
    refs.sort_by(|a, b| b.date.cmp(&a.date));
    let card = crate::staff::read_card(&refs.first()?.path)?;

    let topic = card.items.iter().find_map(|it| {
        (it.kind == "goal")
            .then(|| it.title.strip_prefix(crate::weakness::GOAL_PREFIX))
            .flatten()
            .map(|rest| {
                rest.strip_suffix(crate::weakness::GOAL_SUFFIX)
                    .unwrap_or(rest)
                    .to_string()
            })
    })?;

    let resource = card.items.iter().find(|it| it.kind == "research");
    Some(Reading {
        topic,
        why: None,
        title: resource
            .map(|r| {
                r.title
                    .strip_prefix("resource: ")
                    .unwrap_or(&r.title)
                    .to_string()
            })
            .filter(|s| !s.is_empty()),
        url: resource
            .map(|r| r.link.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                card.items
                    .iter()
                    .find(|it| it.kind == "goal" && !it.link.is_empty())
                    .map(|it| it.link.clone())
            }),
    })
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
    gaps: Option<Gaps>,
    reading: Option<Reading>,
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
    if gaps.as_ref().is_some_and(|g| !g.churn_counted) {
        notes.push(
            "churn was not measured on this run (the gather ran from a scorecard-excluded repo) — \
             say nothing about rework rate"
                .to_string(),
        );
    }

    StandupPacket {
        date: today.to_string(),
        machine: machine.to_string(),
        yesterday: yesterday.to_string(),
        completed: groups,
        completed_count,
        today_focus,
        gaps,
        reading,
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
    // PRs merged on GitHub yesterday, from the whole-person metrics log (events live in one file
    // per machine, so a single-machine read sees a fraction of them). This is where nearly all
    // finished work actually shows up — the orchestrate queue below only knows about work it
    // dispatched itself.
    let events = crate::events::read_events_across(&cfg.metrics_logs_all_machines());
    let merged_prs = crate::merged_prs::discover_merged(&events, &yesterday);

    let sources = crate::completed::CompletedSources {
        todos: &todos,
        goals: &yesterday_goals,
        attn: &attn,
        merged: &merged,
        merged_prs: &merged_prs,
    };
    let linker = crate::rollups::FsNoteLinker::new(cfg, &crate::runner::RealRunner);
    let completed = crate::completed::build_completed(&sources, &linker);

    let today_goals = crate::daily::view(cfg, today).front.goals;

    // The improve beat: the same scorecard `bn metrics` prints, projected to the few figures the
    // brief is allowed to coach from. Offline — it reads the event log plus the cwd git log.
    let gaps = Some(Gaps::from_scorecard(&crate::metrics::scorecard(
        cfg,
        &crate::runner::RealRunner,
        crate::util::now_epoch_secs(),
        crate::metrics::DEFAULT_WINDOW_DAYS,
        today,
    )));
    let reading = reading_from_feed(cfg);

    let packet = build_packet(
        cfg.machine(),
        today,
        &yesterday,
        &completed,
        &today_goals,
        gaps,
        reading,
    );
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
}
