//! What this person is weak at, gathered from every signal the system already has.
//!
//! A collector. It reports the evidence — the recurring themes a post-merge review kept naming,
//! the dojo area never drilled, what the code is actually written in, which learn notes already
//! exist, and what the last few weeks chose — and picks nothing. Choosing the week's one topic is
//! judgment, and belongs to the `staff-curriculum` agent (`config/agents/staff-curriculum.md`).
//!
//! Two derivations stay here because they are arithmetic over the evidence rather than opinion
//! about it: [`classify_continuity`] (how many consecutive weeks a topic has been carried, and
//! whether its goal was ever completed) and [`signal_of`] (whether there is enough evidence to
//! choose from at all). A reader must be able to tell "three weeks on the same topic, never
//! finished" from "a fresh week", and must know when it is guessing.

use crate::dojo::WeakArea;
use serde::{Deserialize, Serialize};

/// The stable prefix of a weekly practice goal, so the topic a prior week chose can be read back
/// out of its card. It is a wire format between one week's output and the next week's evidence,
/// which is why it lives here rather than in the prompt that writes it.
pub const GOAL_PREFIX: &str = "curriculum: ";
/// The stable suffix of a weekly practice goal (a fixed 30-minute daily box).
pub const GOAL_SUFFIX: &str = " — 30m";
/// A theme flagged at least this many times in the week counts as a real (rich) signal.
pub const THEME_SIGNAL_MIN: u32 = 2;
/// Once a topic has ridden this many consecutive weekly cards unfinished it is a zombie: proposed
/// once, carried twice. A month must never pass on one unfinished topic.
pub const CARRY_DROP_AT: u32 = 3;

// --- the gathered packet ------------------------------------------------------------------------

/// How a theme's count moved against the prior week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    /// Not seen in the prior week.
    New,
    /// Up on the prior week.
    Rising,
    /// Unchanged.
    Steady,
    /// Down on the prior week.
    Falling,
}

/// The trend of `count` (this week) against `prior` (last week).
pub fn trend_of(count: u32, prior: u32) -> Trend {
    match (count, prior) {
        (_, 0) => Trend::New,
        (c, p) if c > p => Trend::Rising,
        (c, p) if c < p => Trend::Falling,
        _ => Trend::Steady,
    }
}

/// One recurring-theme tag from the Mentor's week (degraded to learnings themes until the Mentor
/// ships), with this-week and prior-week counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeTag {
    /// The tag / theme label (e.g. `error-paths`).
    pub tag: String,
    /// Times flagged this week.
    pub count: u32,
    /// Times flagged the prior week.
    pub prior: u32,
}

impl ThemeTag {
    /// The trend of this tag against the prior week.
    pub fn trend(&self) -> Trend {
        trend_of(self.count, self.prior)
    }
}

/// One line of the deterministic tech inventory — a language and the repos it dominates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TechFact {
    /// Language label (e.g. `Rust`).
    pub language: String,
    /// Repos where this language leads, most-representative first.
    pub repos: Vec<String>,
    /// Source files counted for this language across the scan (the weight).
    pub files: u32,
}

/// A prior Curriculum card, flattened to what continuity needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorCard {
    /// The card's date (`YYYY-MM-DD`).
    pub date: String,
    /// The topic it proposed (parsed from its Monday goal).
    pub topic: String,
    /// Whether that topic's Monday goal was later marked done — the completion signal.
    pub completed: bool,
}

/// Last week's honest verdict on the prior topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The prior topic's Monday goal was marked done.
    Completed,
    /// Unfinished and still within the carry budget — carried into this week.
    Carried,
    /// Unfinished and over the carry budget — the two-carry rule drops it (a zombie).
    Dropped,
}

impl Verdict {
    /// The lowercase label used in the card body.
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Completed => "completed",
            Verdict::Carried => "carried",
            Verdict::Dropped => "dropped",
        }
    }
}

/// The continuity read over the last few Curriculum cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuity {
    /// The most recent prior topic, if any.
    pub prior_topic: Option<String>,
    /// Last week's verdict on it, if any.
    pub verdict: Option<Verdict>,
    /// How many consecutive most-recent cards share the prior topic (`0` when there are none).
    pub carry_count: u32,
    /// This week must finish-with-smaller-scope-or-drop the prior topic (unfinished, carried at
    /// least once).
    pub must_resolve: bool,
}

/// Whether the week's signals are strong enough to target, or thin enough to fall back on habit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    /// A real theme or dojo weak-spot to target.
    Rich,
    /// Nothing strong — fall back on the tech inventory to keep the habit.
    Thin,
}

/// The deterministic packet gathered for a run. Every source past `date`/`machine` is optional (an
/// empty vec) so a missing source degrades the plan rather than failing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    /// Run date (`YYYY-MM-DD`, the Sunday).
    pub date: String,
    /// The machine the plan is for.
    pub machine: String,
    /// Mentor recurring-theme tags with counts + prior-week counts (degraded to learnings themes).
    pub themes: Vec<ThemeTag>,
    /// The weak / never-drilled area selected by dojo's canonical mastery policy.
    pub weak_area: Option<WeakArea>,
    /// The tech inventory, most-used language first.
    pub tech: Vec<TechFact>,
    /// Topics already covered by existing learn notes — so the plan builds on, not duplicates them.
    pub existing_notes: Vec<String>,
    /// The last few Curriculum cards, newest first.
    pub prior_cards: Vec<PriorCard>,
    /// The continuity read (derived from `prior_cards`).
    pub continuity: Continuity,
    /// Whether the signal is rich or thin (derived).
    pub signal: Signal,
    /// The coming Monday (`YYYY-MM-DD`) — the day a week's practice goal is scheduled for. Carried
    /// in the packet so a reader schedules against the calendar rather than guessing at "next
    /// Monday" from a date it was handed.
    #[serde(default)]
    pub monday: String,
}

impl Packet {
    /// Assemble a packet from its gathered sources, deriving [`continuity`](Packet::continuity) and
    /// [`signal`](Packet::signal). Themes are sorted by descending count; dojo supplies its
    /// precomputed weak-area selection.
    pub fn assemble(
        date: impl Into<String>,
        machine: impl Into<String>,
        mut themes: Vec<ThemeTag>,
        weak_area: Option<WeakArea>,
        tech: Vec<TechFact>,
        existing_notes: Vec<String>,
        prior_cards: Vec<PriorCard>,
    ) -> Packet {
        themes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        let continuity = classify_continuity(&prior_cards);
        let signal = signal_of(&themes, weak_area.as_ref());
        Packet {
            date: date.into(),
            machine: machine.into(),
            themes,
            weak_area,
            tech,
            existing_notes,
            prior_cards,
            continuity,
            signal,
            monday: String::new(),
        }
    }

    /// The packet as pretty JSON — the agent-facing gather output (`bn curriculum gather`).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// --- continuity + signal (pure) -----------------------------------------------------------------

/// The continuity read over `prior_cards` (newest first). The `carry_count` is how many
/// consecutive most-recent cards share the newest card's topic; the verdict on that topic is
/// [`Completed`](Verdict::Completed) when its goal was done, [`Dropped`](Verdict::Dropped) once it
/// has ridden [`CARRY_DROP_AT`] cards unfinished (the two-carry rule), else
/// [`Carried`](Verdict::Carried). `must_resolve` flags an unfinished topic carried at least once —
/// this week must finish it with a smaller scope or drop it.
pub fn classify_continuity(prior_cards: &[PriorCard]) -> Continuity {
    let Some(last) = prior_cards.first() else {
        return Continuity {
            prior_topic: None,
            verdict: None,
            carry_count: 0,
            must_resolve: false,
        };
    };
    let carry_count = prior_cards
        .iter()
        .take_while(|c| c.topic == last.topic)
        .count() as u32;
    let verdict = if last.completed {
        Verdict::Completed
    } else if carry_count >= CARRY_DROP_AT {
        Verdict::Dropped
    } else {
        Verdict::Carried
    };
    let must_resolve = !last.completed && carry_count >= 2;
    Continuity {
        prior_topic: Some(last.topic.clone()),
        verdict: Some(verdict),
        carry_count,
        must_resolve,
    }
}

/// Whether the week's signals are rich (a real theme or a concrete dojo weak-spot) or thin.
pub fn signal_of(themes: &[ThemeTag], weak_area: Option<&WeakArea>) -> Signal {
    let rich_theme = themes.iter().any(|t| t.count >= THEME_SIGNAL_MIN);
    if rich_theme || weak_area.is_some() {
        Signal::Rich
    } else {
        Signal::Thin
    }
}

/// The deterministic fallback topic from the tech inventory — the dominant language mapped to a
/// concrete, slightly-stretching subtopic. `None` when the inventory is empty.
pub fn fallback_topic(tech: &[TechFact]) -> Option<(String, String)> {
    let lead = tech.first()?;
    let sub = fallback_subtopic(&lead.language);
    let repo = lead.repos.first().cloned().unwrap_or_default();
    Some((format!("{}: {sub}", lead.language), repo))
}

/// A canned, slightly-advanced subtopic for a language — deterministic so the fallback is stable.
fn fallback_subtopic(language: &str) -> &'static str {
    match language.to_ascii_lowercase().as_str() {
        "rust" => "lifetimes in async contexts",
        "typescript" | "javascript" => "structural typing & narrowing edge cases",
        "python" => "async iterators & context managers",
        "go" => "goroutine lifecycle & context cancellation",
        "c" | "c++" | "cpp" => "ownership discipline without a borrow checker",
        "java" | "kotlin" => "the memory model & happens-before",
        "ruby" => "metaprogramming without foot-guns",
        "shell" | "bash" => "safe quoting & failure modes (set -euo pipefail)",
        _ => "one deep fundamentals chapter",
    }
}
