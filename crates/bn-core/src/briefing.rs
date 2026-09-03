//! The day's state, folded from every store into one packet.
//!
//! Twice a day something has to answer "where does this day actually stand" — the night shift's
//! draft PRs, the top ranked actions, the shape of the calendar, the quarter, the `q` inbox,
//! yesterday's daily, the standup line, the wrap. Seven sources, all read offline, none of which
//! knows about the others.
//!
//! A collector: it folds, it does not judge. Which of these deserves the person's attention first,
//! and how to say it, belongs to whoever reads the packet — today the `staff-chief-am` and
//! `staff-chief-pm` agents (`config/agents/staff-chief-*.md`).
//!
//! Every source past `date`/`slot` is optional, so a missing one leaves a hole in the packet
//! rather than failing the gather.

use serde::Serialize;

/// Which of the two daily runs a card is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// The 05:30 morning brief.
    Am,
    /// The 16:45 evening shutdown.
    Pm,
}

impl Slot {
    /// The human title of the run.
    pub fn label(self) -> &'static str {
        match self {
            Slot::Am => "morning brief",
            Slot::Pm => "evening shutdown",
        }
    }
}
// ---- the gathered packet -------------------------------------------------------------------

/// A draft PR the night shift left waiting for review — the AM card's "review the night's work".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightPr {
    pub repo: String,
    /// Compact slug (`bn#245`) for display.
    pub slug: String,
    pub link: String,
}

/// One of `bn next`'s top ranked actions, flattened to what the card needs. `source` is the
/// traceable origin label ("review", "goal", "cron", …) so each headline item points back home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopAction {
    pub title: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// Today's calendar shape from the meetings feed cache — best-effort, so every field past the
/// count is optional (a source that can't be parsed degrades, it never errors).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarShape {
    pub meetings: usize,
    /// The day's first meeting time (`HH:MM`), when at least one could be parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_meeting: Option<String>,
    /// The biggest gap between consecutive meeting start times (`HH:MM–HH:MM`), when at least two
    /// timed meetings could be parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_gap: Option<String>,
}

/// Quarter progress at a glance (elapsed vs. on-track, plus the top at-risk goal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarterStatus {
    pub qid: String,
    pub elapsed_pct: u8,
    pub on_track: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_risk: Option<String>,
}

/// The `q` inbox at a glance (count + oldest item's age in days).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxStatus {
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_age_days: Option<i64>,
}

/// A daily rollup — yesterday's for the AM card, today's for the PM card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailySummary {
    pub done: usize,
    pub open: usize,
    pub done_titles: Vec<String>,
    /// Still-open goals, in order — PM uses the head as the drop/defer candidate and, failing a
    /// better signal, as tomorrow's first task.
    pub open_titles: Vec<String>,
}

/// The compact end-of-day wrap folded into the PM shutdown from the retired `wrap-digest` surface
/// (#274). Only the actionable tails the shutdown card didn't already carry: what shipped (merged
/// PRs → `bn close`) and loose commits the note didn't capture (→ `bn sync`). The noisier
/// branches-touched list the standalone digest printed is deliberately dropped — one voice, not a
/// wall — since the merged PRs already say what moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrapSummary {
    /// Count of today's commits the current note didn't capture (→ `bn sync`).
    pub unnoted_commits: usize,
    /// PRs merged today (→ `bn close`).
    pub merged_prs: Vec<WrapPr>,
}

/// A PR merged today, flattened to what the PM card's "what shipped" line needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrapPr {
    /// Compact display label, e.g. `#245 add the widget`.
    pub label: String,
    pub url: String,
}

impl WrapSummary {
    /// True when the day produced nothing worth wrapping — the gather drops it to `None` so the
    /// PM card omits the section entirely.
    pub fn is_empty(&self) -> bool {
        self.unnoted_commits == 0 && self.merged_prs.is_empty()
    }
}

/// The deterministic packet gathered for a run. Every source past `date`/`slot` is optional (or an
/// empty vec) so a missing source omits its card section rather than failing the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Packet {
    pub date: String,
    pub slot: Slot,
    /// Draft PRs from the night shift waiting on review (AM).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub night: Vec<NightPr>,
    /// The top `bn next` actions (already ranked; the card shows the first three).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TopAction>,
    /// Momentum context — the open-punch repo/branch, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarShape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarter: Option<QuarterStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox: Option<InboxStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily: Option<DailySummary>,
    /// The paste-ready standup line, folded into the AM brief from the retired `standup-push`
    /// surface (#274). AM only; `None` unless `[standup].enabled` and the day has signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standup: Option<String>,
    /// The end-of-day wrap, folded into the PM shutdown from the retired `wrap-digest` surface
    /// (#274). PM only; `None` unless `[wrap].enabled` and the day produced work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<WrapSummary>,
}

impl Packet {
    /// An empty packet for `date`/`slot` — the gather fills in what it finds.
    pub fn new(date: impl Into<String>, slot: Slot) -> Self {
        Packet {
            date: date.into(),
            slot,
            night: Vec::new(),
            actions: Vec::new(),
            momentum: None,
            calendar: None,
            quarter: None,
            inbox: None,
            daily: None,
            standup: None,
            wrap: None,
        }
    }
}

// ---- calendar parsing (pure, best-effort) --------------------------------------------------
/// Build a [`CalendarShape`] from the meetings feed items' texts. `None` when there are no
/// meetings at all; a shape with `next_meeting`/`free_gap` left `None` when times can't be parsed
/// (best-effort — the source degrades, it never errors).
pub fn calendar_shape(meeting_texts: &[String]) -> Option<CalendarShape> {
    if meeting_texts.is_empty() {
        return None;
    }
    let mut times: Vec<u32> = meeting_texts
        .iter()
        .filter_map(|t| parse_first_time(t))
        .collect();
    times.sort_unstable();
    times.dedup();
    let next_meeting = times.first().map(|m| hhmm(*m));
    let free_gap = biggest_gap(&times).map(|(a, b)| format!("{}–{}", hhmm(a), hhmm(b)));
    Some(CalendarShape {
        meetings: meeting_texts.len(),
        next_meeting,
        free_gap,
    })
}

/// The first `HH:MM` (24h) time in a prose line, as minutes since midnight. Best-effort: scans for
/// a `:` flanked by 1–2 digits before and 2 digits after, validating the ranges.
pub fn parse_first_time(text: &str) -> Option<u32> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != ':' {
            continue;
        }
        // Up to two digits before the colon.
        let mut hh = String::new();
        let mut j = i;
        while j > 0 && chars[j - 1].is_ascii_digit() && hh.len() < 2 {
            j -= 1;
            hh.insert(0, chars[j]);
        }
        // Exactly two digits after the colon (and not a third, to avoid 09:000).
        if hh.is_empty() || i + 2 >= chars.len() {
            continue;
        }
        if !chars[i + 1].is_ascii_digit() || !chars[i + 2].is_ascii_digit() {
            continue;
        }
        if chars.get(i + 3).is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let mm: u32 = format!("{}{}", chars[i + 1], chars[i + 2]).parse().ok()?;
        let hh: u32 = hh.parse().ok()?;
        if hh < 24 && mm < 60 {
            return Some(hh * 60 + mm);
        }
    }
    None
}

/// The biggest gap between consecutive (sorted) meeting start times, as `(start, end)` minutes.
/// `None` with fewer than two distinct times.
fn biggest_gap(sorted_times: &[u32]) -> Option<(u32, u32)> {
    if sorted_times.len() < 2 {
        return None;
    }
    let mut best = (sorted_times[0], sorted_times[1]);
    let mut best_span = best.1 - best.0;
    for w in sorted_times.windows(2) {
        let span = w[1] - w[0];
        if span > best_span {
            best_span = span;
            best = (w[0], w[1]);
        }
    }
    (best_span > 0).then_some(best)
}

fn hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}
