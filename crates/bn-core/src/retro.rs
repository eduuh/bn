//! The Interviewer (#261) — the Friday 10-minute subjective retro.
//!
//! Every other staff signal is objective (punches, merges, cycle times); the Interviewer captures
//! the subjective channel. Unlike the rest of the staff it is a **pull agent**: the `staff-interviewer`
//! cron never dispatches an unattended agent — it only raises a `warn` attention item ("🎙 weekly
//! retro: 10 minutes") on Friday. You run the session when ready with `bn retro chat` (or
//! `--voice`), or record an explicit `bn retro skip`. Either resolves the item; an ignored item
//! escalates once on Monday, then drops — the Interviewer never nags twice.
//!
//! The session is three fixed questions plus one adaptive follow-up derived from the week's
//! defer/punch/daily history ([`gather_packet`] → [`adaptive_question`]). The raw Q&A lands as a
//! private transcript in the notes repo ([`Config::retro_file`](crate::resolver::Config::retro_file));
//! a structured summary card ([`build_card`]) with energy/drag/avoidance tags is validated by the
//! same contract every staff card passes and written to the feed. This module is the whole member:
//! the session state machine, the packet, tag extraction, the transcript, the card, and the
//! attention lifecycle — pure and testable, with the interactive shell living in the CLI.

use crate::attention::{self, AddOutcome, AttnItem, NewAttn};
use crate::events;
use crate::resolver::Config;
use crate::staff::BriefingCard;
use crate::{daily, track, util};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The card agent name — the transcript author and the card's `agent` field. Not a dispatched
/// roster agent (the Interviewer is pull-only), so it is intentionally absent from `config/agents/*.md`.
pub const AGENT: &str = "interviewer";

/// The attention item's `source` — the label items carry so the lifecycle can find its own item.
pub const ATTN_SOURCE: &str = "staff-interviewer";

/// The attention item title raised each Friday.
pub const ATTN_TITLE: &str = "🎙 weekly retro: 10 minutes";

/// Sinks a validated Interviewer card may route into. The synthesis card carries no items today
/// (it's a feed artifact, not a router), but the allowlist is declared so the contract is explicit.
pub const CARD_SINKS: [&str; 2] = ["note", "attn"];

/// The three fixed questions, in order — the subjective channel's fixed spine.
pub const FIXED_QUESTIONS: [(Axis, &str); 3] = [
    (Axis::Energy, "What energized you this week?"),
    (Axis::Drag, "What dragged?"),
    (Axis::Avoidance, "What did you avoid?"),
];

/// Hard cap on session length — three fixed questions plus at most one adaptive follow-up. No
/// lectures, no advice during the session.
pub const MAX_QUESTIONS: usize = 4;

// --- the session model ---------------------------------------------------------------------------

/// Which subjective axis a question (and its answer) belongs to. The three fixed axes feed the
/// summary card's tag lanes; the adaptive follow-up carries no lane of its own (its answer only
/// contributes referenced repos/branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Energy,
    Drag,
    Avoidance,
    Adaptive,
}

impl Axis {
    /// The human label used in the transcript and the card.
    pub fn label(self) -> &'static str {
        match self {
            Axis::Energy => "energy",
            Axis::Drag => "drag",
            Axis::Avoidance => "avoidance",
            Axis::Adaptive => "adaptive",
        }
    }
}

/// One question in the interview — a fixed spine entry or the derived adaptive follow-up.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Question {
    pub axis: Axis,
    pub prompt: String,
}

/// One answered question — the transcript's unit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Qa {
    pub axis: Axis,
    pub question: String,
    pub answer: String,
}

/// One retro session: the week it covers, when it was recorded, whether answers were spoken, and
/// the answered questions in order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub week: String,
    pub date: String,
    pub voice: bool,
    pub qa: Vec<Qa>,
}

/// The session state machine: either awaiting the answer to the question at `index`, or complete.
/// Pure over `(answered, total)` so the interactive shell and the tests share one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SessionState {
    /// Awaiting the answer to the question at this 0-based index.
    Asking(usize),
    /// Every question has an answer — the session is done.
    Complete,
}

/// The interview state given how many questions are answered out of the total. `answered >= total`
/// (including the capped `total`) is [`SessionState::Complete`]; otherwise the next index is asked.
pub fn session_state(answered: usize, total: usize) -> SessionState {
    if answered >= total {
        SessionState::Complete
    } else {
        SessionState::Asking(answered)
    }
}

impl Session {
    /// A fresh, empty session for `week`/`date`.
    pub fn new(week: impl Into<String>, date: impl Into<String>, voice: bool) -> Self {
        Session {
            week: week.into(),
            date: date.into(),
            voice,
            qa: Vec::new(),
        }
    }

    /// How many questions have been answered.
    pub fn answered(&self) -> usize {
        self.qa.len()
    }

    /// The state machine position against a question list.
    pub fn state(&self, questions: &[Question]) -> SessionState {
        session_state(self.qa.len(), questions.len())
    }

    /// Record an answer to `question` (trimmed). A blank answer is still recorded — the session
    /// captures what you said, including "nothing".
    pub fn record(&mut self, question: &Question, answer: impl Into<String>) {
        self.qa.push(Qa {
            axis: question.axis,
            question: question.prompt.clone(),
            answer: answer.into().trim().to_string(),
        });
    }

    /// Whether every question in `questions` has an answer.
    pub fn is_complete(&self, questions: &[Question]) -> bool {
        matches!(self.state(questions), SessionState::Complete)
    }
}

/// The ordered question list for a session: the three fixed questions, plus the adaptive
/// follow-up when the week's data yields one. Never exceeds [`MAX_QUESTIONS`].
pub fn questions(packet: &RetroPacket) -> Vec<Question> {
    let mut qs: Vec<Question> = FIXED_QUESTIONS
        .iter()
        .map(|(axis, prompt)| Question {
            axis: *axis,
            prompt: (*prompt).to_string(),
        })
        .collect();
    if let Some(prompt) = adaptive_question(packet) {
        qs.push(Question {
            axis: Axis::Adaptive,
            prompt,
        });
    }
    qs.truncate(MAX_QUESTIONS);
    qs
}

// --- the week packet (the adaptive question's input) ---------------------------------------------

/// A task that kept slipping — an open daily goal whose (normalized) text recurred across several
/// days of the week. The proxy for "you deferred this N times": a repeatedly-carried goal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeferredTask {
    pub text: String,
    /// Distinct days this week the task was on the list and still open.
    pub days: usize,
}

/// The deterministic packet the adaptive question is derived from: the week's defer/punch/daily
/// history. Pure read — never writes or fetches.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RetroPacket {
    pub week: String,
    pub start: String,
    pub end: String,
    /// Repeatedly-carried open goals, most-slipped first.
    pub deferred: Vec<DeferredTask>,
    /// Goals marked done across the window.
    pub completed: usize,
    /// Distinct goals that slipped (appeared open on ≥2 days).
    pub slipped: usize,
    /// The activity that got the most tracked (punched) time this week, if any.
    pub top_activity: Option<String>,
}

/// The Monday..=`today` bounds of the ISO week containing `today`, plus its `YYYY-Www` label.
/// `None` if `today` doesn't parse.
pub fn week_bounds(today: &str) -> Option<(String, String, String)> {
    let weekday = util::weekday_index(today)?; // 0=Sun..6=Sat
    let since_monday = ((weekday + 6) % 7) as i64;
    let start = util::add_days(today, -since_monday)?;
    let week = util::iso_week(today)?;
    Some((start, today.to_string(), week))
}

fn most_tracked_activity(cfg: &Config, start: &str, end: &str) -> Option<String> {
    let mut activity_secs: BTreeMap<String, i64> = BTreeMap::new();
    let mut day = start.to_string();
    while day.as_str() <= end {
        for interval in track::intervals(&track::read_day(cfg, &day), 0) {
            if !interval.open && interval.secs > 0 {
                *activity_secs.entry(interval.activity).or_insert(0) += interval.secs;
            }
        }
        match util::add_days(&day, 1) {
            Some(next) => day = next,
            None => break,
        }
    }
    activity_secs
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(activity, _)| activity)
}

/// Gather the deterministic weekly packet used to choose the adaptive retro question.
pub fn gather_packet(cfg: &Config, today: &str) -> RetroPacket {
    let (start, end, week) = week_bounds(today)
        .unwrap_or_else(|| (today.to_string(), today.to_string(), today.to_string()));

    let goal_aggregation = daily::aggregate_goals(&daily::read_window(cfg, &start, &end));
    let deferred = goal_aggregation
        .carried
        .into_iter()
        .map(|goal| DeferredTask {
            text: goal.text,
            days: goal.days,
        })
        .collect();

    let top_activity = most_tracked_activity(cfg, &start, &end);

    RetroPacket {
        week,
        start,
        end,
        deferred,
        completed: goal_aggregation.completed,
        slipped: goal_aggregation.slipped,
        top_activity,
    }
}

/// The one adaptive follow-up question worth asking, derived from the packet — or `None` when the
/// week yields no signal (the session is then the three fixed questions only). Rules, in priority:
/// a repeatedly-carried task, then a slipped-over-shipped week, then the dominant tracked activity.
pub fn adaptive_question(packet: &RetroPacket) -> Option<String> {
    if let Some(top) = packet.deferred.first() {
        if top.days >= 2 {
            return Some(format!(
                "You kept `{}` open across {} days this week — what's really blocking it?",
                top.text, top.days
            ));
        }
    }
    if packet.slipped > packet.completed && packet.slipped + packet.completed > 0 {
        return Some("More slipped than shipped this week — which one still matters?".to_string());
    }
    if let Some(activity) = &packet.top_activity {
        return Some(format!(
            "Most of your tracked time went to `{activity}` — was that where it needed to go?"
        ));
    }
    None
}

// --- tag extraction --------------------------------------------------------------------------

/// The structured tags mined from a session — energy/drag/avoidance keyword tags plus the
/// referenced repos/branches/issues across every answer. Feeds the summary card.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RetroTags {
    pub energy: Vec<String>,
    pub drag: Vec<String>,
    pub avoidance: Vec<String>,
    /// Referenced repos/branches/issues (`` `owner/repo` ``, `repo/branch`, `#123`), in order of
    /// first mention — the objects the subjective answers point at.
    pub refs: Vec<String>,
}

/// Common words carrying no tag signal — dropped from keyword extraction.
const STOPWORDS: &[&str] = &[
    "this",
    "that",
    "with",
    "from",
    "just",
    "have",
    "been",
    "were",
    "what",
    "when",
    "there",
    "which",
    "about",
    "would",
    "could",
    "really",
    "these",
    "those",
    "them",
    "they",
    "then",
    "than",
    "your",
    "yours",
    "week",
    "weeks",
    "much",
    "some",
    "into",
    "over",
    "very",
    "more",
    "most",
    "like",
    "will",
    "shall",
    "because",
    "while",
    "still",
    "kept",
    "keep",
    "felt",
    "feel",
    "week's",
    "didn't",
    "wasn't",
    "again",
    "something",
    "someone",
    "anything",
    "nothing",
    "really",
];

/// Extract up to `max` keyword tags from free text: alphabetic tokens ≥4 chars that aren't
/// stopwords, lowercased and de-duplicated, in order of appearance.
pub fn keywords(text: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let word = raw.to_lowercase();
        if word.len() < 4 || !word.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        if STOPWORDS.contains(&word.as_str()) || out.contains(&word) {
            continue;
        }
        out.push(word);
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Extract referenced repos/branches/issues from free text: backtick-quoted spans, `a/b` slash
/// tokens (owner/repo or repo/branch), and `#123` issue refs. De-duplicated, first-mention order.
pub fn extract_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |r: String| {
        if !r.is_empty() && !out.contains(&r) {
            out.push(r);
        }
    };

    // Backtick-quoted spans.
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        match after.find('`') {
            Some(close) => {
                push(after[..close].trim().to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }

    // Slash tokens and issue refs from whitespace-split words.
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '#');
        if let Some(num) = token.strip_prefix('#') {
            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                push(token.to_string());
            }
        } else if is_slash_ref(token) {
            push(token.to_string());
        }
    }
    out
}

/// Whether `token` looks like an `a/b` repo/branch reference: exactly the allowed characters, one
/// or more `/`, and non-empty on both sides of every slash.
fn is_slash_ref(token: &str) -> bool {
    if !token.contains('/') {
        return false;
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        return false;
    }
    !token.starts_with('/') && !token.ends_with('/') && !token.contains("//")
}

/// Mine the session's answers into structured tags — keyword tags per fixed axis, plus the
/// referenced repos/branches/issues across all answers.
pub fn extract_tags(session: &Session) -> RetroTags {
    let mut tags = RetroTags::default();
    for qa in &session.qa {
        let lane = match qa.axis {
            Axis::Energy => Some(&mut tags.energy),
            Axis::Drag => Some(&mut tags.drag),
            Axis::Avoidance => Some(&mut tags.avoidance),
            Axis::Adaptive => None,
        };
        if let Some(lane) = lane {
            for kw in keywords(&qa.answer, 3) {
                if !lane.contains(&kw) {
                    lane.push(kw);
                }
            }
        }
        for r in extract_refs(&qa.answer) {
            if !tags.refs.contains(&r) {
                tags.refs.push(r);
            }
        }
    }
    tags
}

// --- transcript ------------------------------------------------------------------------------

/// Render a session as its private transcript markdown — the raw Q&A that never leaves the notes
/// repo. Deterministic; the file written to [`Config::retro_file`](crate::resolver::Config::retro_file).
pub fn render_transcript(session: &Session) -> String {
    let mode = if session.voice { " · voice" } else { "" };
    let mut out = format!(
        "# Weekly retro — {}\n\n_{} · {}{}_\n",
        session.week, AGENT, session.date, mode
    );
    for qa in &session.qa {
        let answer = if qa.answer.trim().is_empty() {
            "_(no answer)_"
        } else {
            qa.answer.trim()
        };
        out.push_str(&format!("\n## {}\n\n{}\n", qa.question, answer));
    }
    out
}

/// Write the session transcript to the notes repo, returning its path. Creates the dir on demand.
pub fn write_transcript(cfg: &Config, session: &Session) -> std::io::Result<PathBuf> {
    let path = cfg.retro_file(&session.week);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, render_transcript(session))?;
    Ok(path)
}

// --- summary card ----------------------------------------------------------------------------

/// Build the structured summary card from a session's tags — the shareable synthesis (energy/drag/
/// avoidance tags with the referenced repos/branches). No routed items: it's a feed artifact.
pub fn build_card(session: &Session, tags: &RetroTags, today: &str) -> BriefingCard {
    let headline = format!("Weekly retro — {}: energy · drag · avoidance", session.week);
    let mut body = String::new();
    let section = |body: &mut String, title: &str, entries: &[String]| {
        if !entries.is_empty() {
            body.push_str(&format!("## {title}\n\n{}\n\n", entries.join(", ")));
        }
    };
    section(&mut body, "Energy", &tags.energy);
    section(&mut body, "Drag", &tags.drag);
    section(&mut body, "Avoidance", &tags.avoidance);
    section(&mut body, "Referenced", &tags.refs);
    if body.is_empty() {
        body.push_str("Session recorded; no tags extracted.\n");
    }
    BriefingCard {
        agent: AGENT.to_string(),
        date: today.to_string(),
        headline,
        body_md: body.trim_end().to_string(),
        items: Vec::new(),
    }
}

/// Build the summary card, validate it through the same contract every staff card passes, and
/// write it to the feed store. Returns the written path or the validation reason on failure.
pub fn validate_and_write_card(
    cfg: &Config,
    session: &Session,
    tags: &RetroTags,
    today: &str,
) -> Result<PathBuf, String> {
    let card = build_card(session, tags, today);
    let json = serde_json::to_string(&card).map_err(|e| e.to_string())?;
    let sinks: Vec<String> = CARD_SINKS.iter().map(|s| s.to_string()).collect();
    let validated = crate::staff::validate_card(&json, AGENT, &sinks, &[], today)?;
    crate::staff::write_card(cfg, &validated).map_err(|e| e.to_string())
}

// --- attention lifecycle ---------------------------------------------------------------------

/// The dedup key for the week's attention item — one per ISO week, so a re-firing cron piles up
/// nothing.
pub fn attn_key(week: &str) -> String {
    format!("retro:{week}")
}

/// The open retro attention items (raised by this member), folded from the event log.
pub fn open_items(today: &str) -> Vec<AttnItem> {
    attention::open_items(today)
        .into_iter()
        .filter(|i| i.source == ATTN_SOURCE)
        .collect()
}

/// Raise the weekly retro attention item (`warn`, idempotent per ISO week). Does nothing new if an
/// open item already carries this week's key.
pub fn raise(today: &str) -> std::io::Result<AddOutcome> {
    let week = util::iso_week(today).unwrap_or_else(|| today.to_string());
    let key = attn_key(&week);
    let detail = "Ten minutes, three questions, then one follow-up:\n\
         - What energized you this week?\n\
         - What dragged?\n\
         - What did you avoid?\n\n\
         Run `bn retro chat` (or `bn retro chat --voice`), or `bn retro skip` to skip the week.";
    attention::add_new(
        &NewAttn {
            source: ATTN_SOURCE,
            title: ATTN_TITLE,
            severity: "warn",
            key: Some(&key),
            detail: Some(detail),
            ..Default::default()
        },
        today,
    )
}

/// Resolve (ack) every open retro item — called when a session or skip is recorded. Returns how
/// many were resolved.
pub fn resolve(today: &str) -> std::io::Result<usize> {
    let mut n = 0;
    for it in open_items(today) {
        if attention::ack(&it.id)? {
            n += 1;
        }
    }
    Ok(n)
}

/// One weekly-tick action on the attention item — the escalation state machine's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tick {
    /// Raise this week's item (Friday, when unhandled and none open).
    Raise,
    /// Escalate the open `warn` item to `urgent` (Monday, once).
    Escalate,
    /// Drop the open item — the escalated nag has run its course.
    Drop,
    /// Nothing to do.
    Noop,
}

/// The escalation state machine — pure over the weekday, whether the week is already handled, and
/// the open item's severity (`None` == no open item). The whole "raise Friday / escalate once
/// Monday / then drop / never nag twice" contract lives here so it is exhaustively testable.
///
/// `weekday` is `0=Sun..6=Sat` ([`util::weekday_index`]). Escalation is guarded by severity: a
/// `warn` item escalates to `urgent` exactly once (a re-fire finds it already `urgent` and drops),
/// so the Interviewer can never escalate the same item twice.
pub fn decide_tick(weekday: u32, handled: bool, current_severity: Option<&str>) -> Tick {
    if handled {
        return Tick::Noop;
    }
    match weekday {
        5 => match current_severity {
            // Friday: raise when nothing is open for the week.
            None => Tick::Raise,
            Some(_) => Tick::Noop,
        },
        1 => match current_severity {
            // Monday: escalate the warn once; a re-fire (already urgent) drops instead.
            Some("warn") => Tick::Escalate,
            Some(_) => Tick::Drop,
            None => Tick::Noop,
        },
        2..=4 => match current_severity {
            // Tue–Thu: the escalated urgent nag has run its course — drop it (never nag twice).
            Some("urgent") => Tick::Drop,
            _ => Tick::Noop,
        },
        // Saturday / Sunday: quiet.
        _ => Tick::Noop,
    }
}

/// The outcome of one [`run_weekly_tick`] — the action taken and any stale prior-week items swept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TickReport {
    pub week: String,
    pub action: Tick,
    /// Open retro items from earlier weeks that were dropped as housekeeping.
    pub dropped_stale: usize,
}

/// Run the weekly attention tick for `today`: sweep any stale prior-week items, then apply the
/// [`decide_tick`] action for this week's item. The `staff-interviewer` cron's whole body.
pub fn run_weekly_tick(today: &str) -> std::io::Result<TickReport> {
    let week = util::iso_week(today).unwrap_or_else(|| today.to_string());
    let key = attn_key(&week);
    let open = open_items(today);

    // Housekeeping: a lingering item from an earlier week is dropped so items never accumulate.
    let mut dropped_stale = 0;
    for it in &open {
        if it.key.as_deref() != Some(key.as_str()) && attention::remove(&it.id)? {
            dropped_stale += 1;
        }
    }

    let handled = week_handled(&week);
    let current = open
        .iter()
        .find(|it| it.key.as_deref() == Some(key.as_str()));
    let weekday = util::weekday_index(today).unwrap_or(0);
    let action = decide_tick(weekday, handled, current.map(|i| i.severity.as_str()));

    match action {
        Tick::Raise => {
            raise(today)?;
        }
        Tick::Escalate => {
            if let Some(it) = current {
                attention::update(
                    &it.id,
                    None,
                    Some("urgent"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )?;
            }
        }
        Tick::Drop => {
            if let Some(it) = current {
                attention::remove(&it.id)?;
            }
        }
        Tick::Noop => {}
    }

    Ok(TickReport {
        week,
        action,
        dropped_stale,
    })
}

// --- session / skip record + status ----------------------------------------------------------

/// How a given week's retro was handled, folded from the event log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStatus {
    /// A `bn retro chat` session was recorded.
    Session,
    /// The week was explicitly skipped.
    Skipped,
    /// Neither — no session, no skip.
    Open,
}

/// Record that a session was completed for its week — the durable marker the tick reads to know
/// the week is handled (so it neither raises nor escalates). Appends `retro_session`.
pub fn record_session(session: &Session) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("week".into(), json!(session.week));
    fields.insert("date".into(), json!(session.date));
    fields.insert("questions".into(), json!(session.qa.len()));
    fields.insert("voice".into(), json!(session.voice));
    events::append("retro_session", fields)
}

/// Record that a week was explicitly skipped (not silently missed). Appends `retro_skip`.
pub fn record_skip(week: &str, date: &str) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("week".into(), json!(week));
    fields.insert("date".into(), json!(date));
    events::append("retro_skip", fields)
}

/// Whether `week` already had a session or an explicit skip recorded.
pub fn week_handled(week: &str) -> bool {
    !matches!(week_status(week), WeekStatus::Open)
}

/// The recorded status of `week`, folded from the event log (latest wins: a session or skip marks
/// it handled).
pub fn week_status(week: &str) -> WeekStatus {
    let mut status = WeekStatus::Open;
    for ev in events::read_all() {
        let is_ours = ev.fields.get("week").and_then(Value::as_str) == Some(week);
        if !is_ours {
            continue;
        }
        match ev.kind.as_str() {
            "retro_session" => status = WeekStatus::Session,
            "retro_skip" => status = WeekStatus::Skipped,
            _ => {}
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports;
    use crate::test_support::{config, with_state_and_notes_dir};
    use tempfile::tempdir;

    fn packet_with_deferred() -> RetroPacket {
        RetroPacket {
            week: "2026-W28".into(),
            start: "2026-07-06".into(),
            end: "2026-07-10".into(),
            deferred: vec![
                DeferredTask {
                    text: "refactor store".into(),
                    days: 3,
                },
                DeferredTask {
                    text: "write docs".into(),
                    days: 2,
                },
            ],
            completed: 4,
            slipped: 2,
            top_activity: Some("bn".into()),
        }
    }

    // --- session state machine -----------------------------------------------------------------

    #[test]
    fn session_state_walks_then_completes() {
        assert_eq!(session_state(0, 4), SessionState::Asking(0));
        assert_eq!(session_state(3, 4), SessionState::Asking(3));
        assert_eq!(session_state(4, 4), SessionState::Complete);
        // Over-answering can never re-open a complete session.
        assert_eq!(session_state(5, 4), SessionState::Complete);
    }

    #[test]
    fn questions_are_three_fixed_plus_adaptive_capped_at_four() {
        let qs = questions(&packet_with_deferred());
        assert_eq!(qs.len(), 4);
        assert_eq!(qs[0].axis, Axis::Energy);
        assert_eq!(qs[1].axis, Axis::Drag);
        assert_eq!(qs[2].axis, Axis::Avoidance);
        assert_eq!(qs[3].axis, Axis::Adaptive);
        assert!(qs.len() <= MAX_QUESTIONS);
    }

    #[test]
    fn questions_are_three_when_no_adaptive_signal() {
        let empty = RetroPacket::default();
        let qs = questions(&empty);
        assert_eq!(qs.len(), 3);
        assert!(qs.iter().all(|q| q.axis != Axis::Adaptive));
    }

    #[test]
    fn session_records_and_completes() {
        let packet = packet_with_deferred();
        let qs = questions(&packet);
        let mut s = Session::new("2026-W28", "2026-07-10", false);
        assert!(!s.is_complete(&qs));
        for q in &qs {
            assert!(matches!(s.state(&qs), SessionState::Asking(_)));
            s.record(q, "  an answer  ");
        }
        assert!(s.is_complete(&qs));
        assert_eq!(s.answered(), 4);
        assert_eq!(s.qa[0].answer, "an answer"); // trimmed
    }

    // --- adaptive question ---------------------------------------------------------------------

    #[test]
    fn adaptive_question_prefers_repeatedly_carried_task() {
        let q = adaptive_question(&packet_with_deferred()).unwrap();
        assert!(q.contains("refactor store"));
        assert!(q.contains("3 days"));
        assert!(q.contains("blocking"));
    }

    #[test]
    fn adaptive_question_falls_back_to_slipped_over_shipped() {
        let packet = RetroPacket {
            completed: 1,
            slipped: 3,
            ..Default::default()
        };
        let q = adaptive_question(&packet).unwrap();
        assert!(q.contains("slipped than shipped"));
    }

    #[test]
    fn adaptive_question_falls_back_to_top_activity() {
        let packet = RetroPacket {
            top_activity: Some("meetings".into()),
            ..Default::default()
        };
        let q = adaptive_question(&packet).unwrap();
        assert!(q.contains("meetings"));
    }

    #[test]
    fn adaptive_question_none_on_empty_week() {
        assert!(adaptive_question(&RetroPacket::default()).is_none());
    }

    // --- packet gather -------------------------------------------------------------------------

    #[test]
    fn gather_packet_counts_carried_goals_across_days() {
        let root = tempdir().unwrap();
        with_state_and_notes_dir(root.path(), "testhost", || {
            let cfg = config(root.path());
            // Wed of the week; Monday is 2026-07-06.
            let today = "2026-07-08";
            // Same open goal on Mon/Tue/Wed → carried 3 days. One done goal.
            daily::add(&cfg, "2026-07-06", "refactor store", None, None).unwrap();
            daily::add(&cfg, "2026-07-06", "ship feature", None, None).unwrap();
            daily::done(&cfg, "2026-07-06", "ship feature", None).unwrap();
            daily::add(&cfg, "2026-07-07", "refactor store", None, None).unwrap();
            daily::add(&cfg, "2026-07-08", "refactor store", None, None).unwrap();

            let packet = gather_packet(&cfg, today);
            assert_eq!(packet.week, "2026-W28");
            assert_eq!(packet.start, "2026-07-06");
            assert_eq!(packet.deferred.len(), 1);
            assert_eq!(packet.deferred[0].text, "refactor store");
            assert_eq!(packet.deferred[0].days, 3);
            assert_eq!(packet.slipped, 1);
            assert_eq!(packet.completed, 1);
        });
    }

    #[test]
    fn weekly_and_retro_share_goal_aggregation_counts() {
        let root = tempdir().unwrap();
        with_state_and_notes_dir(root.path(), "testhost", || {
            let cfg = config(root.path());
            let today = "2026-07-08";

            daily::add(&cfg, "2026-07-06", "Refactor   store", None, None).unwrap();
            daily::add(&cfg, "2026-07-06", "refactor store", None, None).unwrap();
            daily::add(&cfg, "2026-07-07", "REFACTOR STORE", None, None).unwrap();
            daily::add(&cfg, today, "ship feature", None, None).unwrap();
            daily::done(&cfg, today, "ship feature", None).unwrap();

            let weekly = reports::weekly(&cfg, today, Some(2), false);
            let retro = gather_packet(&cfg, today);

            assert_eq!(weekly.completed_total, 1);
            assert_eq!(weekly.completed_total, retro.completed);
            assert_eq!(retro.slipped, 1);
            assert_eq!(
                retro.deferred,
                vec![DeferredTask {
                    text: "Refactor   store".into(),
                    days: 3,
                }]
            );
        });
    }

    // --- tag extraction ------------------------------------------------------------------------

    #[test]
    fn extract_refs_finds_backticks_slashes_and_issues() {
        let refs = extract_refs("shipped `bn/main` and eduuh/bn, closed #261 finally");
        assert!(refs.contains(&"bn/main".to_string()));
        assert!(refs.contains(&"eduuh/bn".to_string()));
        assert!(refs.contains(&"#261".to_string()));
    }

    #[test]
    fn extract_refs_dedups_and_ignores_prose_slashes() {
        let refs = extract_refs("either/or is prose; `bn/main` and bn/main again");
        assert_eq!(
            refs.iter().filter(|r| r.as_str() == "bn/main").count(),
            1,
            "deduped"
        );
        // "either/or" is a valid slash token shape, so it's captured — but only once.
        assert_eq!(refs.iter().filter(|r| r.as_str() == "either/or").count(), 1);
    }

    #[test]
    fn keywords_drops_short_and_stopwords() {
        let kw = keywords("This week the deploy pipeline dragged badly", 3);
        assert!(!kw.contains(&"this".to_string()));
        assert!(!kw.contains(&"week".to_string()));
        assert!(kw.contains(&"deploy".to_string()));
        assert!(kw.len() <= 3);
    }

    #[test]
    fn extract_tags_routes_by_axis() {
        let mut s = Session::new("2026-W28", "2026-07-10", false);
        s.qa.push(Qa {
            axis: Axis::Energy,
            question: "What energized you this week?".into(),
            answer: "shipping the dispatch feature felt great".into(),
        });
        s.qa.push(Qa {
            axis: Axis::Drag,
            question: "What dragged?".into(),
            answer: "the flaky review pipeline on `eduuh/bn`".into(),
        });
        s.qa.push(Qa {
            axis: Axis::Avoidance,
            question: "What did you avoid?".into(),
            answer: "documentation again".into(),
        });
        let tags = extract_tags(&s);
        assert!(tags.energy.contains(&"shipping".to_string()));
        assert!(
            tags.drag.contains(&"flaky".to_string()) || tags.drag.contains(&"review".to_string())
        );
        assert!(tags.avoidance.contains(&"documentation".to_string()));
        assert!(tags.refs.contains(&"eduuh/bn".to_string()));
    }

    #[test]
    fn transcript_renders_questions_and_answers() {
        let mut s = Session::new("2026-W28", "2026-07-10", true);
        s.qa.push(Qa {
            axis: Axis::Energy,
            question: "What energized you this week?".into(),
            answer: "shipping".into(),
        });
        let md = render_transcript(&s);
        assert!(md.contains("# Weekly retro — 2026-W28"));
        assert!(md.contains("· voice"));
        assert!(md.contains("## What energized you this week?"));
        assert!(md.contains("shipping"));
    }

    // --- summary card --------------------------------------------------------------------------

    #[test]
    fn card_validates_and_writes_like_any_staff_card() {
        let root = tempdir().unwrap();
        with_state_and_notes_dir(root.path(), "testhost", || {
            let cfg = config(root.path());
            let mut s = Session::new("2026-W28", "2026-07-10", false);
            s.qa.push(Qa {
                axis: Axis::Energy,
                question: "What energized you this week?".into(),
                answer: "shipping the interviewer on `eduuh/bn`".into(),
            });
            let tags = extract_tags(&s);
            let path = validate_and_write_card(&cfg, &s, &tags, "2026-07-10").unwrap();
            assert!(path.exists());
            let card = crate::staff::read_card(&path).unwrap();
            assert_eq!(card.agent, AGENT);
            assert_eq!(card.date, "2026-07-10");
            assert!(card.headline.contains("Weekly retro"));
        });
    }

    // --- attention lifecycle -------------------------------------------------------------------

    #[test]
    fn decide_tick_friday_raises_when_unhandled_and_none_open() {
        assert_eq!(decide_tick(5, false, None), Tick::Raise);
        assert_eq!(decide_tick(5, false, Some("warn")), Tick::Noop);
        assert_eq!(decide_tick(5, true, None), Tick::Noop);
    }

    #[test]
    fn decide_tick_monday_escalates_once_then_drops() {
        // First Monday touch: warn → escalate.
        assert_eq!(decide_tick(1, false, Some("warn")), Tick::Escalate);
        // A second touch finds it already urgent → drop, never a second escalation.
        assert_eq!(decide_tick(1, false, Some("urgent")), Tick::Drop);
        // Handled over the weekend → nothing.
        assert_eq!(decide_tick(1, true, Some("warn")), Tick::Noop);
        assert_eq!(decide_tick(1, false, None), Tick::Noop);
    }

    #[test]
    fn decide_tick_midweek_drops_escalated_and_is_quiet_otherwise() {
        for wd in [2, 3, 4] {
            assert_eq!(decide_tick(wd, false, Some("urgent")), Tick::Drop);
            assert_eq!(decide_tick(wd, false, Some("warn")), Tick::Noop);
            assert_eq!(decide_tick(wd, false, None), Tick::Noop);
        }
        // Weekend is always quiet.
        assert_eq!(decide_tick(6, false, Some("warn")), Tick::Noop);
        assert_eq!(decide_tick(0, false, Some("urgent")), Tick::Noop);
    }

    #[test]
    fn tick_lifecycle_raise_resolve() {
        with_state_and_notes_dir(tempdir().unwrap().path(), "testhost", || {
            let friday = "2026-07-10"; // Friday, W28
            let report = run_weekly_tick(friday).unwrap();
            assert_eq!(report.action, Tick::Raise);
            let items = open_items(friday);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].severity, "warn");
            assert_eq!(items[0].key.as_deref(), Some("retro:2026-W28"));

            // A re-fire the same Friday is idempotent — still one open item.
            run_weekly_tick(friday).unwrap();
            assert_eq!(open_items(friday).len(), 1);

            // Recording a session resolves the item.
            let s = Session::new("2026-W28", friday, false);
            record_session(&s).unwrap();
            assert_eq!(resolve(friday).unwrap(), 1);
            assert!(open_items(friday).is_empty());
            assert_eq!(week_status("2026-W28"), WeekStatus::Session);
        });
    }

    #[test]
    fn tick_lifecycle_escalate_then_drop_never_twice() {
        with_state_and_notes_dir(tempdir().unwrap().path(), "testhost", || {
            let friday = "2026-07-10"; // W28 Friday
            run_weekly_tick(friday).unwrap();

            // Monday of W29 (2026-07-13): the still-open W28 item is a stale prior week → dropped
            // as housekeeping, and nothing new is raised (Monday doesn't raise).
            let monday = "2026-07-13";
            let report = run_weekly_tick(monday).unwrap();
            assert_eq!(report.dropped_stale, 1);
            assert!(open_items(monday).is_empty());
        });
    }

    #[test]
    fn tick_escalates_same_week_monday_once() {
        with_state_and_notes_dir(tempdir().unwrap().path(), "testhost", || {
            // Raise on a Monday directly by seeding a warn item for THIS week, then tick Monday.
            let monday = "2026-07-13"; // W29 Monday
            raise(monday).unwrap();
            let report = run_weekly_tick(monday).unwrap();
            assert_eq!(report.action, Tick::Escalate);
            let items = open_items(monday);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].severity, "urgent");

            // Tuesday: the escalated nag is dropped — and it never escalated twice.
            let tuesday = "2026-07-14";
            let report = run_weekly_tick(tuesday).unwrap();
            assert_eq!(report.action, Tick::Drop);
            assert!(open_items(tuesday).is_empty());
        });
    }

    #[test]
    fn skip_records_and_resolves() {
        with_state_and_notes_dir(tempdir().unwrap().path(), "testhost", || {
            let friday = "2026-07-10";
            run_weekly_tick(friday).unwrap();
            record_skip("2026-W28", friday).unwrap();
            resolve(friday).unwrap();
            assert!(open_items(friday).is_empty());
            assert_eq!(week_status("2026-W28"), WeekStatus::Skipped);
            assert!(week_handled("2026-W28"));
        });
    }
}
