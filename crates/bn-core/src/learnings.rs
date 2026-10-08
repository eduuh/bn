//! `bn learnings` (#322) — the fleet-learning inspection view. Dispatched agents record what
//! they discover with `bn learned` (codebase/tooling facts) and `bn reflect` (one thing to
//! improve, incl. skill reflections `bn reflect "skill:<name> — …"`). Those land in the per-host
//! daily files ([`crate::daily`]), each stamped with the branch/issue/harness it came from
//! ([`crate::daily::LogProvenance`]). Because daily files live on the date axis — never under
//! `<repo>/<branch>` — they survive `bn wt cleanup` untouched, so the learnings outlive the
//! archived note; this module just gathers, attributes, groups, and reviews them.
//!
//! It is pure aggregation over the daily window (like [`crate::reports::weekly`]) plus a small
//! human-in-the-loop review ledger (`<daily_dir>/learnings-review.md`, modelled on the DXI
//! store): the overseer `promote`s a learning worth keeping or `dismiss`es a one-off, and the
//! view flags which is which. No new collection, no re-fetch.

use crate::daily::{self, LogEntry};
use crate::model::{Note, Scalar};
use crate::resolver::Config;
use crate::store::StoreError;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which day-log a learning came from: `bn learned` (a fact) or `bn reflect` (an improvement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Learned,
    Reflection,
}

impl Kind {
    fn tag(self) -> char {
        match self {
            Kind::Learned => 'l',
            Kind::Reflection => 'r',
        }
    }
    fn glyph(self) -> char {
        match self {
            Kind::Learned => '•',
            Kind::Reflection => '↳',
        }
    }
}

/// The author axis for filtering: recorded by a dispatched agent (a harness was driving bn) or
/// by me (a plain shell). `Any` (the default) applies no author filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Author {
    #[default]
    Any,
    Mine,
    Agent,
}

/// A human review decision recorded in the ledger. `Promoted` marks a learning worth keeping (the
/// "durable" flag the issue asks for); `Dismissed` retires a one-off so it stops surfacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Review {
    Promoted,
    Dismissed,
}

impl Review {
    /// The lowercase state word (`promoted`/`dismissed`) for the view trailer and CLI confirmation.
    pub fn label(self) -> &'static str {
        match self {
            Review::Promoted => "promoted",
            Review::Dismissed => "dismissed",
        }
    }
}

/// One aggregated learning/reflection, joined with its provenance, derived skill, and any review
/// decision. `key` is a stable content hash (see [`entry_key`]) — the handle `promote`/`dismiss`
/// take, unchanged across runs so a decision sticks.
#[derive(Debug, Clone, Serialize)]
pub struct LearningEntry {
    pub key: String,
    pub kind: Kind,
    pub date: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Skill parsed from a `skill:<name> …` prefix (skill reflections), else `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// Review decision from the ledger, if the overseer has acted on this entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<Review>,
    /// True when a harness authored the entry (agent-recorded), false when recorded at a shell.
    pub agent: bool,
}

impl LearningEntry {
    /// The grouping theme: the skill if it's a skill reflection, else the repo, else `general`.
    fn theme(&self) -> String {
        if let Some(s) = &self.skill {
            format!("skill:{s}")
        } else if let Some(r) = &self.repo {
            r.clone()
        } else {
            "general".to_string()
        }
    }
}

/// One theme group (a skill, a repo, or `general`) with its entries, newest last.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub theme: String,
    pub entries: Vec<LearningEntry>,
}

/// The assembled inspection view over `[start..=end]`: theme groups plus headline counts.
#[derive(Debug, Clone, Serialize)]
pub struct LearningsView {
    pub start: String,
    pub end: String,
    pub groups: Vec<Group>,
    pub total: usize,
    pub learned: usize,
    pub reflections: usize,
    pub agent: usize,
    pub mine: usize,
    pub promoted: usize,
    pub dismissed: usize,
    pub unreviewed: usize,
}

/// Filter for [`collect`]: `since` overrides the anchored weekly window with a rolling N days
/// (`None` = the [`crate::reports::window_start`] anchor); `all` widens to the whole daily
/// history. `repo`/`skill` narrow by provenance/theme; `author` by agent-vs-mine.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub since: Option<i64>,
    pub all: bool,
    pub repo: Option<String>,
    pub skill: Option<String>,
    pub author: Author,
}

// ---------------------------------------------------------------------------
// Parsing / provenance helpers
// ---------------------------------------------------------------------------

/// FNV-1a — stable across runs and bn versions (unlike the std hasher), so a review decision keyed
/// on the hash keeps matching the same entry. Same constants as [`crate::dojo`].
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Stable key for a learning: `<kind><8 hex>` over its identifying fields. Deterministic, so
/// `promote`/`dismiss` can name an entry and the decision survives re-runs.
fn entry_key(kind: Kind, date: &str, repo: Option<&str>, text: &str) -> String {
    let natural = format!(
        "{}\u{1f}{date}\u{1f}{}\u{1f}{text}",
        kind.tag(),
        repo.unwrap_or("")
    );
    format!("{}{:08x}", kind.tag(), fnv1a(&natural) & 0xffff_ffff)
}

/// Parse a `skill:<name> …` prefix (as written by `bn reflect "skill:<name> — …"`) into the bare
/// skill name, tolerating a trailing `:`/`-`/em-dash. `None` when the text isn't a skill entry.
pub fn parse_skill(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("skill:")?;
    let token = rest.split_whitespace().next().unwrap_or("");
    let name = token.trim_end_matches([':', '-', '—']);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The issue/work-item a branch note points at, for learning provenance: the first `source:`
/// collaboration line (an issue/PR URL), else the note's `work_item`. `None` when neither is set.
pub fn note_issue(note: &Note) -> Option<String> {
    for c in &note.front.collab {
        if let Some(rest) = c.text.trim().strip_prefix("source:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    note.front.work_item.as_ref().map(Scalar::display)
}

fn map_entry(kind: Kind, e: &LogEntry) -> LearningEntry {
    let skill = parse_skill(&e.text);
    LearningEntry {
        key: entry_key(kind, &e.created, e.repo.as_deref(), &e.text),
        kind,
        date: e.created.clone(),
        text: e.text.clone(),
        repo: e.repo.clone(),
        branch: e.branch.clone(),
        issue: e.issue.clone(),
        agent: e.harness.is_some(),
        harness: e.harness.clone(),
        skill,
        review: None,
    }
}

// ---------------------------------------------------------------------------
// Lessons: skill reflections fed back into the skill they name
// ---------------------------------------------------------------------------

/// How far back a served skill looks for reflections on itself.
pub const LESSON_WINDOW_DAYS: i64 = 30;
/// How many reflections a served skill carries, newest first.
pub const LESSON_LIMIT: usize = 5;
/// Longest a single lesson line may run before it is cut.
const LESSON_MAX_CHARS: usize = 300;

/// Recent skill reflections grouped by skill, newest first and at most [`LESSON_LIMIT`] each.
/// Dismissed entries are left out. Read once and pass to [`lessons_section`] per skill, so a
/// caller rendering many skills scans the daily history once.
pub fn recent_lessons(cfg: &Config, today: &str) -> BTreeMap<String, Vec<LearningEntry>> {
    let start = util::add_days(today, -LESSON_WINDOW_DAYS).unwrap_or_else(|| today.to_string());
    let reviews = read_reviews(cfg);
    let mut by_skill: BTreeMap<String, Vec<LearningEntry>> = BTreeMap::new();
    for d in daily::read_window(cfg, &start, today) {
        for r in &d.front.reflections {
            let mut e = map_entry(Kind::Reflection, r);
            e.review = reviews.get(&e.key).map(|r| r.state);
            if e.review == Some(Review::Dismissed) {
                continue;
            }
            if let Some(skill) = e.skill.clone() {
                by_skill.entry(skill).or_default().push(e);
            }
        }
    }
    for entries in by_skill.values_mut() {
        entries.sort_by(|a, b| b.date.cmp(&a.date));
        entries.truncate(LESSON_LIMIT);
    }
    by_skill
}

/// The "Recent lessons" section appended to a served skill, or an empty string when no recent
/// run left a reflection on it. The `skill:<name> —` prefix is stripped from each line.
pub fn lessons_section(lessons: &BTreeMap<String, Vec<LearningEntry>>, skill: &str) -> String {
    let Some(entries) = lessons.get(skill).filter(|e| !e.is_empty()) else {
        return String::new();
    };
    let mut out = format!(
        "\n\n## Recent lessons from runs of this skill\n\nWhat earlier runs of `{skill}` noted with \
         `bn reflect`, newest first. Treat them as hints from the field, not rules; where one \
         contradicts the steps above, the steps win.\n\n"
    );
    for e in entries {
        let text = e.text.trim_start();
        let text = text
            .strip_prefix("skill:")
            .and_then(|rest| rest.strip_prefix(skill))
            .map(|rest| rest.trim_start_matches([' ', ':', '-', '—']))
            .unwrap_or(text)
            .replace('\n', " ");
        let mut line: String = text.chars().take(LESSON_MAX_CHARS).collect();
        if line.len() < text.len() {
            line.push('…');
        }
        out.push_str(&format!("- {} · {line}\n", e.date));
    }
    out
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// Every learning/reflection across the whole daily history (no window, no filter), review state
/// joined in, newest last. Backs entry resolution for `promote`/`dismiss` (which must reach older
/// entries too) and is the unfiltered source [`collect`] narrows.
pub fn all_entries(cfg: &Config, today: &str) -> Vec<LearningEntry> {
    let reviews = read_reviews(cfg);
    let start = earliest_day(cfg).unwrap_or_else(|| today.to_string());
    let mut out = Vec::new();
    for d in daily::read_window(cfg, &start, today) {
        for l in &d.front.learnings {
            out.push(map_entry(Kind::Learned, l));
        }
        for r in &d.front.reflections {
            out.push(map_entry(Kind::Reflection, r));
        }
    }
    for e in &mut out {
        e.review = reviews.get(&e.key).map(|r| r.state);
    }
    out
}

/// Assemble the windowed, filtered, grouped inspection view.
pub fn collect(cfg: &Config, today: &str, filter: &Filter) -> LearningsView {
    let start = if filter.all {
        earliest_day(cfg).unwrap_or_else(|| today.to_string())
    } else {
        crate::reports::window_start(cfg, today, filter.since)
    };

    let reviews = read_reviews(cfg);
    let skill_q = filter.skill.as_ref().map(|s| s.to_lowercase());
    let repo_q = filter.repo.as_ref().map(|s| s.to_lowercase());

    let mut kept: Vec<LearningEntry> = Vec::new();
    for d in daily::read_window(cfg, &start, today) {
        let mapped = d
            .front
            .learnings
            .iter()
            .map(|e| map_entry(Kind::Learned, e))
            .chain(
                d.front
                    .reflections
                    .iter()
                    .map(|e| map_entry(Kind::Reflection, e)),
            );
        for mut e in mapped {
            match filter.author {
                Author::Agent if !e.agent => continue,
                Author::Mine if e.agent => continue,
                _ => {}
            }
            if let Some(rq) = &repo_q {
                if e.repo.as_deref().map(str::to_lowercase).as_deref() != Some(rq.as_str()) {
                    continue;
                }
            }
            if let Some(sq) = &skill_q {
                match &e.skill {
                    Some(s) if s.to_lowercase().contains(sq.as_str()) => {}
                    _ => continue,
                }
            }
            e.review = reviews.get(&e.key).map(|r| r.state);
            kept.push(e);
        }
    }

    let total = kept.len();
    let learned = kept.iter().filter(|e| e.kind == Kind::Learned).count();
    let reflections = total - learned;
    let agent = kept.iter().filter(|e| e.agent).count();
    let mine = total - agent;
    let promoted = kept
        .iter()
        .filter(|e| e.review == Some(Review::Promoted))
        .count();
    let dismissed = kept
        .iter()
        .filter(|e| e.review == Some(Review::Dismissed))
        .count();
    let unreviewed = total - promoted - dismissed;

    // Group by theme (skill: > repo > general), themes sorted, entries oldest→newest within.
    let mut by_theme: BTreeMap<String, Vec<LearningEntry>> = BTreeMap::new();
    for e in kept {
        by_theme.entry(e.theme()).or_default().push(e);
    }
    let groups = by_theme
        .into_iter()
        .map(|(theme, entries)| Group { theme, entries })
        .collect();

    LearningsView {
        start,
        end: today.to_string(),
        groups,
        total,
        learned,
        reflections,
        agent,
        mine,
        promoted,
        dismissed,
        unreviewed,
    }
}

/// The earliest `YYYY-MM-DD.md` day file present (min filename), or `None` when there are none.
/// Bounds the `--all` / resolution scan so it never walks from the epoch.
fn earliest_day(cfg: &Config) -> Option<String> {
    let dir = cfg.daily_dir();
    let mut earliest: Option<String> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(date) = name.strip_suffix(".md") else {
            continue;
        };
        if !util::is_iso_date(date) {
            continue;
        }
        match &earliest {
            Some(cur) if cur.as_str() <= date => {}
            _ => earliest = Some(date.to_string()),
        }
    }
    earliest
}

// ---------------------------------------------------------------------------
// Review ledger
// ---------------------------------------------------------------------------

/// One recorded review decision. Keyed by the entry's stable [`entry_key`]; `at` stamps when the
/// decision was made and `note` carries an optional rationale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub key: String,
    pub state: Review,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The review store's frontmatter: a flat list of decisions (latest write wins per key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct ReviewFront {
    #[serde(default)]
    reviews: Vec<ReviewRecord>,
}

/// Path of the review ledger (`<daily_dir>/learnings-review.md`) — one file beside the day files,
/// modelled on the DXI store. Never synced into a day file (it's overseer metadata, not a goal).
fn review_path(cfg: &Config) -> std::path::PathBuf {
    cfg.daily_dir().join("learnings-review.md")
}

/// Read every review decision, keyed by entry key (last write wins on duplicates). Empty when the
/// ledger is missing/unreadable.
pub fn read_reviews(cfg: &Config) -> BTreeMap<String, ReviewRecord> {
    let path = review_path(cfg);
    let mut map = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return map;
    };
    if let Ok(front) = crate::store::parse_frontmatter::<ReviewFront>(&text) {
        for r in front.reviews {
            map.insert(r.key.clone(), r);
        }
    }
    map
}

/// Why a `promote`/`dismiss` couldn't be applied.
#[derive(Debug)]
pub enum ReviewError {
    /// No learning matched the key/substring.
    NotFound,
    /// The substring matched more than one entry — the caller must disambiguate.
    Ambiguous(Vec<LearningEntry>),
    /// Persisting the ledger failed.
    Write(StoreError),
}

/// Resolve a `promote`/`dismiss` argument to a single learning: an exact `key` first, else a
/// unique case-insensitive text substring across the full history (like `bn done`). Ambiguous
/// substrings return every candidate so the caller can list them.
pub fn resolve(cfg: &Config, today: &str, query: &str) -> Result<LearningEntry, ReviewError> {
    let entries = all_entries(cfg, today);
    if let Some(hit) = entries.iter().find(|e| e.key == query) {
        return Ok(hit.clone());
    }
    let q = query.to_lowercase();
    let matches: Vec<LearningEntry> = entries
        .into_iter()
        .filter(|e| e.text.to_lowercase().contains(&q))
        .collect();
    match matches.len() {
        0 => Err(ReviewError::NotFound),
        1 => matches.into_iter().next().ok_or(ReviewError::NotFound),
        _ => Err(ReviewError::Ambiguous(matches)),
    }
}

/// Record (or overwrite) the review decision for the learning matched by `query`. Idempotent per
/// key: re-reviewing replaces the prior decision. Returns the resolved entry with its new state.
pub fn review(
    cfg: &Config,
    today: &str,
    query: &str,
    state: Review,
    note: Option<&str>,
) -> Result<LearningEntry, ReviewError> {
    let mut entry = resolve(cfg, today, query)?;
    let record = ReviewRecord {
        key: entry.key.clone(),
        state,
        at: today.to_string(),
        note: note
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    let mut reviews = read_reviews(cfg);
    reviews.insert(record.key.clone(), record);
    let mut list: Vec<ReviewRecord> = reviews.into_values().collect();
    list.sort_by(|a, b| a.key.cmp(&b.key));
    write_reviews(cfg, &list).map_err(ReviewError::Write)?;
    entry.review = Some(state);
    Ok(entry)
}

fn write_reviews(cfg: &Config, reviews: &[ReviewRecord]) -> Result<(), StoreError> {
    let front = ReviewFront {
        reviews: reviews.to_vec(),
    };
    let fm = serde_yaml::to_string(&front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let text = format!("---\n{fm}---\n\n# Learnings review ledger\n");
    let path = review_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Render the inspection view for the terminal.
pub fn render(v: &LearningsView) -> String {
    let mut out = format!("Learnings — {} … {}\n", v.start, v.end);
    out.push_str(&format!(
        "  {} total · {} learned, {} reflect · {} agent, {} mine · {} promoted, {} dismissed, {} unreviewed\n",
        v.total, v.learned, v.reflections, v.agent, v.mine, v.promoted, v.dismissed, v.unreviewed
    ));
    if v.groups.is_empty() {
        out.push_str("\n  (no learnings in this window — try --since N or --all)\n");
        return out;
    }
    for g in &v.groups {
        out.push_str(&format!("\n{}  ({})\n", g.theme, g.entries.len()));
        for e in &g.entries {
            out.push_str(&format!("  {} {}  {}", e.kind.glyph(), e.key, e.text));
            out.push_str(&provenance_suffix(e));
            out.push('\n');
        }
    }
    out
}

/// The compact `· repo/branch · #issue · harness · date [review]` trailer on an entry line.
fn provenance_suffix(e: &LearningEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    match (&e.repo, &e.branch) {
        (Some(r), Some(b)) => parts.push(format!("{r}/{b}")),
        (Some(r), None) => parts.push(r.clone()),
        _ => {}
    }
    if let Some(i) = &e.issue {
        parts.push(short_issue(i));
    }
    if let Some(h) = &e.harness {
        parts.push(h.clone());
    }
    parts.push(e.date.clone());
    let mut suffix = format!("  · {}", parts.join(" · "));
    if let Some(r) = e.review {
        suffix.push_str(&format!("  [{}]", r.label()));
    }
    suffix
}

/// Shorten an issue reference for the trailer: a `.../issues/322` or `.../pull/12` URL → `#322`,
/// a bare number → `#n`, anything else kept verbatim.
fn short_issue(issue: &str) -> String {
    for marker in ["/issues/", "/pull/"] {
        if let Some(idx) = issue.rfind(marker) {
            let tail = &issue[idx + marker.len()..];
            let num: String = tail.chars().take_while(char::is_ascii_digit).collect();
            if !num.is_empty() {
                return format!("#{num}");
            }
        }
    }
    if issue.chars().all(|c| c.is_ascii_digit()) {
        format!("#{issue}")
    } else {
        issue.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daily::LogProvenance;

    fn reflect(cfg: &Config, day: &str, text: &str) {
        daily::reflect(cfg, day, text, &LogProvenance::default()).unwrap();
    }

    #[test]
    fn a_skill_carries_its_recent_reflections_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(root.path());
        reflect(&cfg, "2026-08-01", "skill:explore — too old to carry");
        reflect(
            &cfg,
            "2026-10-01",
            "skill:explore — boot the smallest honest surface",
        );
        reflect(
            &cfg,
            "2026-10-05",
            "skill:explore: add an /api timeout sweep",
        );
        reflect(
            &cfg,
            "2026-10-06",
            "an untagged reflection reaches no skill",
        );
        reflect(
            &cfg,
            "2026-10-06",
            "skill:investigate — check git log first",
        );

        let lessons = recent_lessons(&cfg, "2026-10-08");
        let section = lessons_section(&lessons, "explore");
        let first = section
            .find("2026-10-05 · add an /api timeout sweep")
            .unwrap();
        let second = section
            .find("2026-10-01 · boot the smallest honest surface")
            .unwrap();
        assert!(first < second, "newest first:\n{section}");
        assert!(!section.contains("too old") && !section.contains("untagged"));
        assert!(
            !section.contains("skill:explore"),
            "prefix stripped:\n{section}"
        );
        assert!(lessons_section(&lessons, "investigate").contains("check git log first"));
        assert_eq!(lessons_section(&lessons, "plan"), "");
    }

    #[test]
    fn a_dismissed_reflection_is_not_carried() {
        let root = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(root.path());
        reflect(&cfg, "2026-10-01", "skill:explore — keep this one");
        reflect(&cfg, "2026-10-02", "skill:explore — a one-off to drop");
        review(
            &cfg,
            "2026-10-08",
            "a one-off to drop",
            Review::Dismissed,
            None,
        )
        .unwrap();

        let section = lessons_section(&recent_lessons(&cfg, "2026-10-08"), "explore");
        assert!(section.contains("keep this one") && !section.contains("one-off"));
    }
}
