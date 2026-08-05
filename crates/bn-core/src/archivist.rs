//! The Archivist (#263) — the Friday private weekly chronicle: the *story* of the week.
//!
//! The brag file records *that* things happened; nobody records what the week was actually about,
//! what changed in how you work, what future-you will wish was written down. The Archivist writes
//! that narrative once a week, strictly privately — it explicitly replaces the rejected "Publicist"
//! idea: nothing here is ever published or externally visible. It exists so privacy doesn't cost
//! you the compounding value of a written record — for review season, for `bn recall` (#249), for
//! future-you. A year of chronicles is a searchable memory of your working life.
//!
//! Split of concerns (mirrors [`crate::chief`]): this module is the pure `Packet → Chronicle →
//! markdown` core — no IO, fully unit-testable. The CLI (`bn archivist`, the `staff-archivist`
//! native job) gathers the packet from the live offline stores, validates the rendered chronicle
//! *before* it lands, writes it to the private notes repo, and mirrors any proposed brag entries to
//! the feed. Deterministic and offline like the Chief: a cron fires Friday 17:30 (after the
//! Interviewer's retro and the PM shutdown, so the week's subjective and objective closes are both
//! in) and folds what the system already knows into the one chronicle.
//!
//! Privacy posture: the output path is inside the private notes repo and the framework's closed
//! sink set applies — there is no publish/post sink and none can be added. Proposed brag entries
//! are surfaced as *card items only* and never auto-logged: `bn achieve` stays a human act, the
//! Archivist drafts, never commits achievements on your behalf.
//!
//! Enrichment gaps (built against what exists today; each degrades cleanly, none blocks): the
//! Interviewer transcript summary (#264) is optional and absent until that member lands; there is
//! no dedicated decisions store, so *What changed* is sourced from logged achievements; the feed
//! keeps only the latest snapshot per source (no per-week history), so staff activity is a
//! best-effort current read; the quarter file carries no per-week diff, so the quarter signal is a
//! status snapshot. The "in your voice" LLM narrative is a future enrichment — this deterministic
//! assembly is the honest floor that always ships and is fully testable. Section headers are clean
//! `## ` units so `bn recall` (#249) can chunk a chronicle section-by-section once it lands.

use serde::Serialize;

/// The five sections every full (non-thin) chronicle carries, in render order. `validate` gates on
/// all five being present before a chronicle lands, so the record's shape is enforced by code, not
/// convention.
pub const SECTIONS: [&str; 5] = [
    "The story",
    "What changed",
    "The numbers that matter",
    "For the record",
    "Thread to pull",
];

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
        }
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

// ---- the chronicle -------------------------------------------------------------------------

/// A proposed brag entry the Archivist noticed unlogged — surfaced as a card item only, never
/// auto-logged (`bn achieve` stays a human act).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BragProposal {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// The assembled chronicle — the structured, machine-readable counterpart to the rendered
/// markdown. `thin` selects the two-line degrade path. `proposals` never appear in the rendered
/// body: they are the feed card the CLI mirrors, kept out of the private file so nothing reads as
/// logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chronicle {
    pub week: String,
    pub start: String,
    pub end: String,
    pub thin: bool,
    pub story: String,
    pub what_changed: Vec<String>,
    pub numbers: Vec<String>,
    pub for_the_record: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<BragProposal>,
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Lowercase a text's leading letter so a logged line reads naturally mid-sentence ("the
/// through-line was adopted atomic writes…"). Leaves acronyms/paths that start non-alphabetic
/// untouched.
fn lower_lead(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether a logged achievement already references a win — by its link (a substring match, the
/// strongest signal) or by containing the win's text (case-insensitive). Trivially-short win text
/// is never matched by containment so a two-word goal can't be swallowed by an unrelated line.
fn references(logged: &LoggedItem, win: &Win) -> bool {
    if let Some(link) = win.link.as_deref() {
        if !link.is_empty() && logged.text.contains(link) {
            return true;
        }
    }
    let win_text = win.text.trim();
    if win_text.len() >= 8 {
        return logged
            .text
            .to_lowercase()
            .contains(&win_text.to_lowercase());
    }
    false
}

/// Extract the brag proposals: every win not already referenced by a logged achievement, in win
/// order, de-duplicated by text. Pure and deterministic — the heart of the brag-proposal
/// acceptance criterion. `logged` is the whole achievement corpus (not just this week's), so a win
/// logged earlier is never re-proposed.
pub fn brag_proposals(logged: &[LoggedItem], wins: &[Win]) -> Vec<BragProposal> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for w in wins {
        let text = w.text.trim();
        if text.len() < 4 {
            continue;
        }
        if logged.iter().any(|l| references(l, w)) {
            continue;
        }
        let key = text.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(BragProposal {
            text: text.to_string(),
            link: w.link.clone().filter(|l| !l.is_empty()),
        });
    }
    out
}

/// Compose the honest one-paragraph story from the strongest signals — prose, not a list of merges.
fn build_story(p: &Packet) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Opening: the subjective close, when the Interviewer supplied one.
    if let Some(iv) = p.interview.as_ref().filter(|i| !i.is_empty()) {
        if let Some(first) = iv.energized.first() {
            parts.push(format!("The week's energy was in {first}"));
        } else if let Some(first) = iv.dragged.first() {
            parts.push(format!("The week dragged on {first}"));
        }
    }

    // The shape of the work — merges and goals, as a sentence (each clause only when non-zero).
    let merged = p.metrics.map(|m| m.merged).unwrap_or(0);
    let goals = p.metrics.map(|m| m.goals_completed).unwrap_or(0);
    let mut shape: Vec<String> = Vec::new();
    if merged > 0 {
        shape.push(format!("{merged} PR{} merged", plural(merged)));
    }
    if goals > 0 {
        shape.push(format!("{goals} goal{} closed", plural(goals)));
    }
    if !shape.is_empty() {
        parts.push(shape.join(" and "));
    }

    // What the effort was about — the top logged achievement, verbatim-ish (prose, not a list).
    if let Some(top) = p.achievements.last() {
        parts.push(format!("the through-line was {}", lower_lead(&top.text)));
    } else if let Some(top) = p.wins.first() {
        parts.push(format!("the week's marker was {}", lower_lead(&top.text)));
    }

    // The staff's own record.
    let cards: usize = p.activity.iter().map(|a| a.cards).sum();
    if cards > 0 {
        parts.push(format!(
            "the staff logged {cards} card{} across {} agent{}",
            plural(cards),
            p.activity.len(),
            plural(p.activity.len())
        ));
    }

    if parts.is_empty() {
        return "A working week that left little on the record — the story is in the doing, not \
                the log."
            .to_string();
    }
    // Sentence-case join: capitalize the first, join the rest with "; ", end with a period.
    let mut sentence = parts.join("; ");
    if let Some(first) = sentence.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    sentence.push('.');
    sentence
}

/// The direction-aware "why it moved" clause for a count vs its previous window.
fn count_why(cur: usize, prev: usize, noun: &str) -> String {
    use std::cmp::Ordering;
    match cur.cmp(&prev) {
        Ordering::Greater => format!("{noun} up week-over-week"),
        Ordering::Less => format!("a quieter week for {noun}"),
        Ordering::Equal => format!("steady {noun}"),
    }
}

fn build_numbers(p: &Packet) -> Vec<String> {
    let mut out = Vec::new();
    let Some(m) = p.metrics.as_ref() else {
        return out;
    };

    if m.merged > 0 || m.prev_merged > 0 {
        out.push(format!(
            "{} PR{} merged (prev {}) — {}",
            m.merged,
            plural(m.merged),
            m.prev_merged,
            count_why(m.merged, m.prev_merged, "throughput")
        ));
    }

    if let Some(cur) = m.cycle_median_hours {
        let why = match m.prev_cycle_median_hours {
            Some(prev) if cur < prev => "reviews turned around faster",
            Some(prev) if cur > prev => "PRs sat longer before merge",
            Some(_) => "cycle time held",
            None => "first measured week",
        };
        let prev_clause = match m.prev_cycle_median_hours {
            Some(prev) => format!(" (prev {prev:.1}h)"),
            None => String::new(),
        };
        out.push(format!("median cycle time {cur:.1}h{prev_clause} — {why}"));
    }

    if m.goals_completed > 0 || m.goals_carried > 0 {
        let why = if m.goals_carried > m.goals_completed {
            "more started than finished"
        } else if m.goals_completed > 0 {
            "strong follow-through"
        } else {
            "a week of carries"
        };
        out.push(format!(
            "{} goal{} closed, {} carried — {why}",
            m.goals_completed,
            plural(m.goals_completed),
            m.goals_carried
        ));
    }

    out.truncate(3);
    out
}

/// "What changed" — the decisions, practices, and tools the week adopted. Sourced from logged
/// achievements (there is no dedicated decisions store yet); an honest single line when nothing was
/// recorded rather than invented filler.
fn build_what_changed(p: &Packet) -> Vec<String> {
    let mut out: Vec<String> = p
        .achievements
        .iter()
        .rev()
        .map(|a| a.text.clone())
        .collect();
    out.truncate(4);
    if out.is_empty() {
        out.push("Nothing recorded as a decision or practice change this week.".to_string());
    }
    out
}

/// "For the record" — what future-you (or a promo packet assembled privately, much later) will want
/// verbatim: the notable wins and quarter movement.
fn build_for_the_record(p: &Packet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in p.wins.iter().take(5) {
        match &w.link {
            Some(link) if !link.is_empty() => out.push(format!("{} → {link}", w.text)),
            _ => out.push(w.text.clone()),
        }
    }
    if let Some(q) = &p.quarter {
        out.push(format!(
            "Quarter {}: {}/{} goals on track.",
            q.qid, q.on_track, q.total
        ));
    }
    if out.is_empty() {
        out.push("No standout item logged — a steady, unremarkable week.".to_string());
    }
    out
}

/// "Thread to pull" — the one open question the week left behind. An at-risk quarter goal wins;
/// then a carried daily goal; then an honest forward-looking prompt.
fn build_thread(p: &Packet) -> Option<String> {
    if let Some(q) = &p.quarter {
        if let Some(at_risk) = &q.at_risk {
            return Some(format!("Will the at-risk quarter goal recover: {at_risk}?"));
        }
    }
    if let Some(m) = p.metrics.as_ref() {
        if m.goals_carried > 0 {
            return Some(format!(
                "{} goal{} carried into next week — what's blocking the finish?",
                m.goals_carried,
                plural(m.goals_carried)
            ));
        }
    }
    Some("What did this week set up that next week has to finish?".to_string())
}

/// The honest two-line degrade for a thin week — a real record of a quiet week, never padded.
fn thin_story(p: &Packet) -> String {
    let has_cards = p.activity.iter().any(|a| a.cards > 0);
    if has_cards {
        "A quiet week on the record — the staff kept watch but little shipped and no goals \
         closed. Heads-down work, or a week that simply didn't touch the log."
            .to_string()
    } else {
        "A quiet week — nothing shipped and no goals closed on the record. Rest, travel, or \
         offline work that didn't touch bn."
            .to_string()
    }
}

/// Build the chronicle from the packet — pure. A thin week collapses to the two-line story; a full
/// week assembles all five sections plus any brag proposals for the feed.
pub fn build_chronicle(p: &Packet) -> Chronicle {
    let thin = p.is_thin();
    let proposals = brag_proposals(&p.achievements, &p.wins);
    if thin {
        return Chronicle {
            week: p.week.clone(),
            start: p.start.clone(),
            end: p.end.clone(),
            thin: true,
            story: thin_story(p),
            what_changed: Vec::new(),
            numbers: Vec::new(),
            for_the_record: Vec::new(),
            thread: None,
            proposals,
        };
    }
    Chronicle {
        week: p.week.clone(),
        start: p.start.clone(),
        end: p.end.clone(),
        thin: false,
        story: build_story(p),
        what_changed: build_what_changed(p),
        numbers: build_numbers(p),
        for_the_record: build_for_the_record(p),
        thread: build_thread(p),
        proposals,
    }
}

fn push_section(body: &mut String, title: &str, lines: &[String]) {
    body.push_str(&format!("\n## {title}\n\n"));
    if lines.is_empty() {
        body.push_str("- —\n");
        return;
    }
    for l in lines {
        body.push_str(&format!("- {l}\n"));
    }
}

/// Render the chronicle to the markdown that lands at `<notes>/<machine>/chronicle/<week>.md`.
/// Section headers are clean `## ` units so `bn recall` (#249) can chunk section-by-section.
/// Proposals are deliberately absent — they are the feed card, never written into the private file.
pub fn render(c: &Chronicle) -> String {
    let mut out = format!("# Chronicle — {}\n\n", c.week);
    out.push_str(&format!(
        "_The story of the week · {} → {} · private_\n",
        c.start, c.end
    ));

    if c.thin {
        out.push('\n');
        out.push_str(&c.story);
        out.push('\n');
        return out;
    }

    out.push_str("\n## The story\n\n");
    out.push_str(&c.story);
    out.push('\n');

    push_section(&mut out, "What changed", &c.what_changed);
    push_section(&mut out, "The numbers that matter", &c.numbers);
    push_section(&mut out, "For the record", &c.for_the_record);

    out.push_str("\n## Thread to pull\n\n");
    match &c.thread {
        Some(t) => out.push_str(&format!("{t}\n")),
        None => out.push_str("—\n"),
    }
    out
}

/// Validate a rendered chronicle *before* it lands: it must carry the `# Chronicle` title, and — for
/// a full (non-thin) week — every one of the five [`SECTIONS`]. A thin week is intentionally short,
/// so only the title is required. Returns the list of what's missing so the caller can refuse the
/// write and say why.
pub fn validate(md: &str, thin: bool) -> Result<(), String> {
    if !md.trim_start().starts_with("# Chronicle") {
        return Err("chronicle is missing its title".to_string());
    }
    if thin {
        return Ok(());
    }
    let missing: Vec<&str> = SECTIONS
        .iter()
        .copied()
        .filter(|s| !md.contains(&format!("## {s}")))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "chronicle is missing section{}: {}",
            plural(missing.len()),
            missing.join(", ")
        ))
    }
}
