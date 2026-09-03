//! The week, gathered from every store: what merged, what closed, what was logged, what the retro
//! said, where the quarter stands.
//!
//! A collector. It reports the week's raw material and forms no narrative about it — which is the
//! whole point, because the narrative is the part worth having written by something that can
//! actually write. Read today by the `staff-archivist` agent
//! (`config/agents/staff-archivist.md`).
//!
//! Every source past `week`/`start`/`end` is optional, so a missing one leaves a hole rather than
//! failing the gather. `is_thin` is the one judgment kept here, and only because it is arithmetic:
//! a week with no merges, no goals, no achievements and no wins had no substantive work, and a
//! reader must be able to tell that from a week whose sources merely failed to load.

use serde::Serialize;

// ---- the gathered packet -------------------------------------------------------------------

/// One staff agent's activity over the week — its card count in the feed (the staff's own record of
/// the week). Best-effort: the feed keeps only the latest snapshot per source, so this is a current
/// read, not a true weekly history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentActivity {
    pub agent: String,
    pub cards: usize,
}

/// The objective metrics delta for the window — merges and PR cycle time this week vs the
/// immediately-preceding one, plus daily-goal completion. Mirrors the fields
/// [`crate::insights`] already computes so the numbers agree with `bn insights`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricsDelta {
    pub merged: usize,
    pub prev_merged: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_median_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_cycle_median_hours: Option<f64>,
    pub goals_completed: usize,
    pub goals_carried: usize,
}

impl MetricsDelta {
    /// Whether the delta carries any signal at all — a window with zero merges and zero goals
    /// contributes nothing to the chronicle (feeds the thin-week check).
    pub fn is_quiet(&self) -> bool {
        self.merged == 0
            && self.prev_merged == 0
            && self.goals_completed == 0
            && self.goals_carried == 0
    }
}

/// A logged achievement / decision carried into the packet — the record of what changed, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoggedItem {
    pub date: String,
    pub text: String,
}

/// A candidate "win" the week produced — a completed daily goal (often with a PR link). These are
/// the raw material for brag-proposal extraction: a win not already referenced by a logged
/// achievement becomes a *proposed* brag entry (never auto-logged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Win {
    pub date: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// The Interviewer's retro summary (#264) — the week's subjective close. Optional and absent until
/// that member lands; when present it colours the story's opening sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct InterviewSummary {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub energized: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dragged: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub avoided: Vec<String>,
}

impl InterviewSummary {
    /// Nothing tagged in any lane — treated as absent.
    pub fn is_empty(&self) -> bool {
        self.energized.is_empty() && self.dragged.is_empty() && self.avoided.is_empty()
    }
}

/// Quarter status at week's close (#252) — a snapshot, not a per-week diff (the quarter file
/// carries no history to diff against). `at_risk` feeds the "Thread to pull".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarterStatus {
    pub qid: String,
    pub on_track: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_risk: Option<String>,
}

/// The deterministic packet gathered for a run. Every source past `week`/`start`/`end` is optional
/// (or an empty vec) so a missing source omits its contribution rather than failing the run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Packet {
    /// ISO week label (`YYYY-Www`) — the chronicle's filename stem.
    pub week: String,
    pub start: String,
    pub end: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<AgentActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MetricsDelta>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub achievements: Vec<LoggedItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wins: Vec<Win>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interview: Option<InterviewSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarter: Option<QuarterStatus>,
    /// Whether the week had no substantive work at all — see [`Packet::is_thin`].
    ///
    /// Serialized, and computed here rather than left to the reader, because it is the one thing
    /// in this packet a reader must not guess at: a holiday and a week whose sources failed to
    /// load look identical from the outside, and the difference decides whether the honest answer
    /// is two lines or none.
    #[serde(default)]
    pub thin: bool,
}

impl Packet {
    /// An empty packet for `week`/`start`/`end` — the gather fills in what it finds.
    pub fn new(week: impl Into<String>, start: impl Into<String>, end: impl Into<String>) -> Self {
        Packet {
            week: week.into(),
            start: start.into(),
            end: end.into(),
            activity: Vec::new(),
            metrics: None,
            achievements: Vec::new(),
            wins: Vec::new(),
            interview: None,
            quarter: None,
            thin: true,
        }
    }

    /// Stamp [`is_thin`](Packet::is_thin) onto the packet. Called once the gather has filled every
    /// source, since the answer depends on all of them.
    pub fn seal(mut self) -> Self {
        self.thin = self.is_thin();
        self
    }

    /// A thin week has no *substantive* work — no merges, no goals, no logged achievements, no
    /// wins. Ambient staff cards (a sentinel ticking on cron while you're on vacation) don't lift a
    /// week out of thin; they only steer which honest two-line story it gets. Such a week gets a
    /// two-line chronicle, never filler.
    pub fn is_thin(&self) -> bool {
        let quiet_metrics = self.metrics.map(|m| m.is_quiet()).unwrap_or(true);
        quiet_metrics && self.achievements.is_empty() && self.wins.is_empty()
    }
}
