//! `bn dojo` — the gamified learning engine (#253).
//!
//! A teaching layer that *derives lessons from the codebase and its history* — real bug-fix
//! commits, feature work, branch-note decisions, module structure — and drills you on them with
//! spaced repetition so the habit sticks. All raw material is mined from what already exists; no
//! authored content lives here.
//!
//! This module is the pure engine: the card model, the SM-2 scheduler, the append-only stores
//! (`cards.jsonl` / `reviews.jsonl` / `sessions.jsonl` under `<notes>/<machine>/dojo/`), the
//! streak/XP/level math, the mastery map, and the deterministic card-mining formatters. It never
//! prints for humans and never shells out directly — external `git` reads flow through the
//! [`crate::runner::CommandRunner`] seam so the whole surface is unit-testable. The CLI layer
//! (`bn dojo`) drives the interactive session and rendering on top of these values.

use crate::resolver::Config;
use crate::runner::CommandRunner;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Initial SM-2 ease factor for a freshly-seen card (the classic 2.5).
pub const INITIAL_EASE: f64 = 2.5;
/// SM-2 floor for the ease factor — it never drops below this no matter how often a card lapses.
pub const MIN_EASE: f64 = 1.3;
/// Interval (days) a card lands on its very first "Good" answer.
pub const FIRST_INTERVAL_DAYS: f64 = 1.0;
/// Interval (days) a card lands on its second consecutive "Good" answer.
pub const SECOND_INTERVAL_DAYS: f64 = 6.0;
/// Interval (days) a lapsed card ("Again") is re-scheduled to — deliberately tiny (~10 min) so
/// missed cards resurface within the same or next session, the "resurface sooner" contract.
pub const LAPSE_INTERVAL_DAYS: f64 = 10.0 / 1440.0;
/// Extra multiplier applied on an "Easy" answer, on top of the ease factor.
pub const EASY_BONUS: f64 = 1.3;
/// Default number of cards a single `bn dojo` session presents.
pub const DEFAULT_DAILY_TARGET: usize = 5;

// ---------------------------------------------------------------------------
// Card model
// ---------------------------------------------------------------------------

/// The kind of drill a card poses — its provenance and how it's presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// A debugging drill built from a real past `fix:` commit: symptom + pre-fix code shown, the
    /// actual fix hidden until you answer.
    BugMystery,
    /// A walkthrough of a recent `feat:` commit: what changed and why, quizzed on the design.
    Feature,
    /// Decision recall from a branch-note `decisions:` entry: "why did we choose X?"
    Decision,
    /// An architecture tour / "where would X live?" question derived from module structure.
    Architecture,
    /// A generic quick-fire recall question.
    Quiz,
}

impl CardKind {
    /// Every kind, in display order — the canonical list for iteration and validation.
    pub const ALL: [CardKind; 5] = [
        CardKind::BugMystery,
        CardKind::Feature,
        CardKind::Decision,
        CardKind::Architecture,
        CardKind::Quiz,
    ];

    /// Short machine slug (used in card ids and CLI filters).
    pub fn slug(self) -> &'static str {
        match self {
            CardKind::BugMystery => "bug",
            CardKind::Feature => "feature",
            CardKind::Decision => "decision",
            CardKind::Architecture => "arch",
            CardKind::Quiz => "quiz",
        }
    }

    /// Human-readable label for rendering.
    pub fn label(self) -> &'static str {
        match self {
            CardKind::BugMystery => "bug mystery",
            CardKind::Feature => "feature walkthrough",
            CardKind::Decision => "decision recall",
            CardKind::Architecture => "architecture tour",
            CardKind::Quiz => "quiz",
        }
    }

    /// Parse a CLI slug back into a kind (accepts the slug or a few friendly aliases).
    pub fn from_slug(s: &str) -> Option<CardKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bug" | "bugs" | "mystery" => Some(CardKind::BugMystery),
            "feature" | "feat" => Some(CardKind::Feature),
            "decision" | "decisions" | "why" => Some(CardKind::Decision),
            "arch" | "architecture" | "map" | "tour" => Some(CardKind::Architecture),
            "quiz" | "recall" => Some(CardKind::Quiz),
            _ => None,
        }
    }
}

/// One learning card — a self-contained prompt/answer pair with provenance. Persisted to
/// `cards.jsonl`; the `id` is stable across runs so re-mining the same source never duplicates it
/// and reviews keep referring to the same card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// Stable id: `<kind-slug>-<hash(repo, kind, source)>`.
    pub id: String,
    /// Bare repo name this card is drawn from (drives per-repo rotation + mastery).
    pub repo: String,
    /// The drill kind.
    pub kind: CardKind,
    /// Subsystem / module this card exercises, for the mastery map. `None` for repo-wide cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// What the learner is shown (the fix/answer is never embedded here).
    pub prompt: String,
    /// The answer key, revealed only after the learner has committed to an answer.
    pub answer: String,
    /// Provenance string used for dedup: a git sha, a note path, a PR url, a module name.
    pub source: String,
    /// When the card was mined (epoch secs).
    pub created: i64,
}

impl Card {
    /// Build a card, deriving its stable id from `(repo, kind, source)`.
    pub fn new(
        repo: impl Into<String>,
        kind: CardKind,
        area: Option<String>,
        prompt: impl Into<String>,
        answer: impl Into<String>,
        source: impl Into<String>,
        created: i64,
    ) -> Card {
        let repo = repo.into();
        let source = source.into();
        Card {
            id: make_id(&repo, kind, &source),
            repo,
            kind,
            area,
            prompt: prompt.into(),
            answer: answer.into(),
            source,
            created,
        }
    }
}

/// Stable card id from its natural key. Uses FNV-1a so the value is identical across runs and
/// bn versions (unlike the std hasher, which is not stability-guaranteed).
pub fn make_id(repo: &str, kind: CardKind, source: &str) -> String {
    let key = format!("{repo}\u{1f}{}\u{1f}{source}", kind.slug());
    format!("{}-{:012x}", kind.slug(), fnv1a(&key) & 0xffff_ffff_ffff)
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// Grades + SM-2 scheduling
// ---------------------------------------------------------------------------

/// A self-graded answer outcome, mapped onto the SM-2 quality scale. `Again` is a lapse; the rest
/// are passes of increasing confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// Missed it — the card lapses and resurfaces almost immediately.
    Again,
    /// Got it, but it was a struggle.
    Hard,
    /// Got it comfortably.
    Good,
    /// Trivial — back it off aggressively.
    Easy,
}

impl Grade {
    /// SM-2 quality value (0..5). Anything `< 3` is treated as a failed recall.
    pub fn quality(self) -> u8 {
        match self {
            Grade::Again => 1,
            Grade::Hard => 3,
            Grade::Good => 4,
            Grade::Easy => 5,
        }
    }

    /// XP earned for answering a card at this grade. Even a miss earns a little — showing up is
    /// the habit the streak protects.
    pub fn xp(self) -> u64 {
        match self {
            Grade::Again => 2,
            Grade::Hard => 5,
            Grade::Good => 10,
            Grade::Easy => 15,
        }
    }

    /// Whether this grade counts as a successful recall (SM-2 `q >= 3`).
    pub fn passed(self) -> bool {
        self.quality() >= 3
    }

    /// Parse an interactive keypress / word into a grade (`a`/`h`/`g`/`e` or the full word).
    pub fn from_input(s: &str) -> Option<Grade> {
        match s.trim().to_ascii_lowercase().as_str() {
            "a" | "again" | "miss" | "missed" | "no" | "n" => Some(Grade::Again),
            "h" | "hard" => Some(Grade::Hard),
            "g" | "good" | "got" | "yes" | "y" => Some(Grade::Good),
            "e" | "easy" => Some(Grade::Easy),
            _ => None,
        }
    }
}

/// One logged answer — the append-only unit written to `reviews.jsonl`. Folding these in `ts`
/// order reconstructs the SM-2 [`ReviewState`] for every card, so the log is the source of truth
/// (no mutable state file to git-conflict on sync).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewEvent {
    /// The card that was answered.
    pub card_id: String,
    /// The grade given.
    pub grade: Grade,
    /// When it was answered (epoch secs).
    pub ts: i64,
}

/// The folded SM-2 state for a single card — its current ease, interval, and next-due time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewState {
    /// The card this state belongs to.
    pub card_id: String,
    /// SM-2 ease factor (>= [`MIN_EASE`]); higher = intervals grow faster.
    pub ease: f64,
    /// Current inter-repetition interval in days.
    pub interval_days: f64,
    /// Consecutive successful recalls (reset to 0 on a lapse).
    pub reps: u32,
    /// Lifetime count of lapses (`Again` answers) — a weak-spot signal for the mastery map.
    pub lapses: u32,
    /// When the card is next due (epoch secs).
    pub due: i64,
    /// When it was last reviewed (epoch secs).
    pub last_reviewed: i64,
}

/// Apply one `grade` to a card's previous SM-2 state (or `None` for a first-ever review) at time
/// `now`, returning the updated state. Standard SM-2 with a lapse that reschedules to a tiny
/// interval so missed cards come back fast, while repeated passes grow the interval by the ease
/// factor (mastered cards back off).
pub fn schedule(prev: Option<&ReviewState>, card_id: &str, grade: Grade, now: i64) -> ReviewState {
    let prev_ease = prev.map(|s| s.ease).unwrap_or(INITIAL_EASE);
    let prev_reps = prev.map(|s| s.reps).unwrap_or(0);
    let prev_interval = prev.map(|s| s.interval_days).unwrap_or(0.0);
    let prev_lapses = prev.map(|s| s.lapses).unwrap_or(0);

    // SM-2 ease update: EF' = EF + (0.1 - (5-q)(0.08 + (5-q)0.02)), floored at MIN_EASE.
    let q = grade.quality() as f64;
    let diff = 5.0 - q;
    let ease = (prev_ease + (0.1 - diff * (0.08 + diff * 0.02))).max(MIN_EASE);

    let (reps, interval, lapses) = if !grade.passed() {
        // Lapse: reset the streak, bump the lapse counter, resurface almost immediately.
        (0, LAPSE_INTERVAL_DAYS, prev_lapses + 1)
    } else {
        let interval = match prev_reps {
            0 => FIRST_INTERVAL_DAYS,
            1 => SECOND_INTERVAL_DAYS,
            _ => {
                let base = if prev_interval > 0.0 {
                    prev_interval
                } else {
                    SECOND_INTERVAL_DAYS
                };
                base * ease
            }
        };
        let interval = if matches!(grade, Grade::Easy) {
            interval * EASY_BONUS
        } else {
            interval
        };
        (prev_reps + 1, interval, prev_lapses)
    };

    ReviewState {
        card_id: card_id.to_string(),
        ease,
        interval_days: interval,
        reps,
        lapses,
        due: now + (interval * 86_400.0).round() as i64,
        last_reviewed: now,
    }
}

/// Fold a slice of [`ReviewEvent`]s into the current [`ReviewState`] per card. Events are applied
/// in timestamp order (ties broken by input order), so the result is deterministic regardless of
/// how the JSONL lines were interleaved on disk.
pub fn fold_reviews(events: &[ReviewEvent]) -> BTreeMap<String, ReviewState> {
    let mut ordered: Vec<&ReviewEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.ts);
    let mut states: BTreeMap<String, ReviewState> = BTreeMap::new();
    for ev in ordered {
        let next = schedule(states.get(&ev.card_id), &ev.card_id, ev.grade, ev.ts);
        states.insert(ev.card_id.clone(), next);
    }
    states
}

// ---------------------------------------------------------------------------
// Streak / XP / levels
// ---------------------------------------------------------------------------

/// A completed session's summary line — one per day you show up. Appended to `sessions.jsonl`;
/// the streak is derived from the set of distinct dates present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Local calendar date (`YYYY-MM-DD`) the session completed on — the streak's unit.
    pub date: String,
    /// When it completed (epoch secs).
    pub ts: i64,
    /// How many cards were answered.
    pub cards: u32,
    /// XP earned in this session.
    pub xp: u64,
}

/// A computed streak: consecutive-day run ending today or yesterday, plus the all-time longest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Streak {
    /// Current consecutive-day streak (0 if the last session was more than a day ago).
    pub current: u32,
    /// The longest consecutive-day run ever recorded.
    pub longest: u32,
    /// The most recent session date, if any.
    pub last: Option<String>,
}

/// Compute the [`Streak`] from a set of session `dates` (any order, dups allowed) relative to
/// `today` (local `YYYY-MM-DD`). Timezone-safe: it works purely on local date strings, so a
/// session logged late at night counts for its own local day. The current streak survives a
/// same-day gap (last session was today *or* yesterday) and breaks on any larger gap.
pub fn compute_streak(dates: &[String], today: &str) -> Streak {
    let mut days: Vec<i64> = dates.iter().filter_map(|d| util::epoch_day(d)).collect();
    days.sort_unstable();
    days.dedup();
    if days.is_empty() {
        return Streak::default();
    }

    // Longest run of consecutive days anywhere in the history.
    let mut longest = 1u32;
    let mut run = 1u32;
    for w in days.windows(2) {
        if w[1] - w[0] == 1 {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
    }

    let Some(&last_day) = days.last() else {
        return Streak::default();
    };
    let last = util::epoch_day(today).map(|t| t - last_day);

    // Current streak only counts if the last session is today or yesterday.
    let current = match last {
        Some(0) | Some(1) => {
            let mut c = 1u32;
            for w in days.windows(2).rev() {
                if w[1] - w[0] == 1 {
                    c += 1;
                } else {
                    break;
                }
            }
            c
        }
        _ => 0,
    };

    Streak {
        current,
        longest: longest.max(current),
        last: dates
            .iter()
            .filter(|d| util::epoch_day(d) == Some(last_day))
            .max()
            .cloned(),
    }
}

/// A labeled mastery level for a given XP total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    /// Zero-based level index.
    pub index: u32,
    /// The label ("apprentice", "journeyman", …).
    pub label: &'static str,
    /// XP accumulated into the current level.
    pub xp_into: u64,
    /// XP still needed to reach the next level, or `None` at the cap.
    pub xp_to_next: Option<u64>,
}

/// Cumulative XP thresholds and their labels. The nth entry is the XP at which level n begins.
const LEVELS: [(u64, &str); 7] = [
    (0, "novice"),
    (50, "apprentice"),
    (150, "journeyman"),
    (400, "adept"),
    (900, "expert"),
    (1800, "master"),
    (3500, "grandmaster"),
];

/// The [`Level`] for an XP total — its label plus progress into the next threshold.
pub fn level_for(xp: u64) -> Level {
    let mut idx = 0usize;
    for (i, (threshold, _)) in LEVELS.iter().enumerate() {
        if xp >= *threshold {
            idx = i;
        }
    }
    let (floor, label) = LEVELS[idx];
    let xp_to_next = LEVELS.get(idx + 1).map(|(next, _)| next - xp);
    Level {
        index: idx as u32,
        label,
        xp_into: xp - floor,
        xp_to_next,
    }
}

/// Total XP earned across all `events`, using each card's grade reward.
pub fn total_xp(events: &[ReviewEvent]) -> u64 {
    events.iter().map(|e| e.grade.xp()).sum()
}

/// XP earned per repo, joining review events to their cards. Reviews whose card is unknown (e.g.
/// the card was pruned) are dropped, so the total always ties out to the known cards.
pub fn xp_by_repo(events: &[ReviewEvent], cards: &[Card]) -> BTreeMap<String, u64> {
    let repo_of: BTreeMap<&str, &str> = cards
        .iter()
        .map(|c| (c.id.as_str(), c.repo.as_str()))
        .collect();
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for ev in events {
        if let Some(repo) = repo_of.get(ev.card_id.as_str()) {
            *out.entry((*repo).to_string()).or_insert(0) += ev.grade.xp();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Mastery map
// ---------------------------------------------------------------------------

/// Per-area coverage: how many cards exist for an area, how many you've actually been drilled on,
/// and how shaky you are on them. The daily prompt generator leans on this — an area with
/// `reviewed == 0` is "never been drilled on X".
#[derive(Debug, Clone, PartialEq)]
pub struct MasteryArea {
    /// Repo the area belongs to.
    pub repo: String,
    /// Area / subsystem label (falls back to the repo name when a card has no area).
    pub area: String,
    /// Cards mined for this area.
    pub cards: u32,
    /// Distinct cards reviewed at least once.
    pub reviewed: u32,
    /// Total lapses recorded across this area's cards (the weak-spot signal).
    pub lapses: u32,
    /// Average ease across reviewed cards (`None` if none reviewed). Lower = weaker.
    pub avg_ease: Option<f64>,
}

impl MasteryArea {
    /// Coverage ratio in `0.0..=1.0` — reviewed cards over total cards.
    pub fn coverage(&self) -> f64 {
        if self.cards == 0 {
            0.0
        } else {
            self.reviewed as f64 / self.cards as f64
        }
    }
}

/// A mastery area selected as a concrete weak spot for another subsystem to consume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeakArea {
    /// Repo the area belongs to.
    pub repo: String,
    /// Subsystem / module label.
    pub area: String,
    /// Total lapses recorded across the area's cards.
    pub lapses: u32,
    /// Cards mined for the area.
    pub cards: u32,
    /// Distinct cards drilled at least once.
    pub reviewed: u32,
}

impl WeakArea {
    fn from_mastery(area: &MasteryArea) -> Self {
        Self {
            repo: area.repo.clone(),
            area: area.area.clone(),
            lapses: area.lapses,
            cards: area.cards,
            reviewed: area.reviewed,
        }
    }
}

/// Build the mastery map: one [`MasteryArea`] per `(repo, area)`, sorted repo then area. Joins
/// cards to their folded review states so coverage, lapses and average ease line up.
pub fn mastery(cards: &[Card], states: &BTreeMap<String, ReviewState>) -> Vec<MasteryArea> {
    struct Acc {
        cards: u32,
        reviewed: u32,
        lapses: u32,
        ease_sum: f64,
        ease_n: u32,
    }
    let mut acc: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for c in cards {
        let area = c.area.clone().unwrap_or_else(|| c.repo.clone());
        let e = acc.entry((c.repo.clone(), area)).or_insert(Acc {
            cards: 0,
            reviewed: 0,
            lapses: 0,
            ease_sum: 0.0,
            ease_n: 0,
        });
        e.cards += 1;
        if let Some(st) = states.get(&c.id) {
            e.reviewed += 1;
            e.lapses += st.lapses;
            e.ease_sum += st.ease;
            e.ease_n += 1;
        }
    }
    acc.into_iter()
        .map(|((repo, area), a)| MasteryArea {
            repo,
            area,
            cards: a.cards,
            reviewed: a.reviewed,
            lapses: a.lapses,
            avg_ease: (a.ease_n > 0).then(|| a.ease_sum / a.ease_n as f64),
        })
        .collect()
}

/// The weakest areas first — most lapses, then lowest average ease, then least coverage. Areas
/// with cards you've never been drilled on (`reviewed == 0`) sort as weak (unknown = risky).
pub fn weakest_areas(mastery: &[MasteryArea], limit: usize) -> Vec<MasteryArea> {
    let mut v: Vec<MasteryArea> = mastery.to_vec();
    v.sort_by(weakness_order);
    v.truncate(limit);
    v
}

/// Select the canonical dojo weak spot, or `None` when every reviewed area is clean.
///
/// Areas with lapses are weak, as are areas with cards that have never been drilled. Ranking uses
/// the same lapse, ease, and coverage policy as [`weakest_areas`].
pub fn weakest_area(mastery: &[MasteryArea]) -> Option<WeakArea> {
    let mut candidates: Vec<&MasteryArea> = mastery
        .iter()
        .filter(|area| area.lapses > 0 || (area.cards > 0 && area.reviewed == 0))
        .collect();
    candidates.sort_by(|a, b| weakness_order(a, b));
    candidates.first().map(|area| WeakArea::from_mastery(area))
}

fn weakness_order(a: &MasteryArea, b: &MasteryArea) -> std::cmp::Ordering {
    b.lapses
        .cmp(&a.lapses)
        .then_with(|| {
            // Lower ease is weaker; treat "never reviewed" as the weakest ease bucket.
            let ea = a.avg_ease.unwrap_or(0.0);
            let eb = b.avg_ease.unwrap_or(0.0);
            ea.partial_cmp(&eb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| {
            a.coverage()
                .partial_cmp(&b.coverage())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| a.area.cmp(&b.area))
}

// ---------------------------------------------------------------------------
// Session card selection
// ---------------------------------------------------------------------------

/// Filter that shapes which cards a session draws from.
#[derive(Debug, Clone, Default)]
pub struct Selector {
    /// Repo allowlist (bare names). Empty = all repos with cards.
    pub repos: Vec<String>,
    /// Restrict to a single kind (e.g. `bn dojo bug`). `None` = any kind.
    pub kind: Option<CardKind>,
    /// Restrict to a single area (e.g. `bn dojo map <area>`). `None` = any area.
    pub area: Option<String>,
}

impl Selector {
    fn allows(&self, card: &Card) -> bool {
        if !self.repos.is_empty() && !self.repos.iter().any(|r| r == &card.repo) {
            return false;
        }
        if let Some(k) = self.kind {
            if card.kind != k {
                return false;
            }
        }
        if let Some(a) = &self.area {
            let matches = card
                .area
                .as_deref()
                .map(|ca| ca.eq_ignore_ascii_case(a))
                .unwrap_or(false);
            if !matches {
                return false;
            }
        }
        true
    }
}

/// Pick up to `limit` cards for a session at time `now`, honoring the [`Selector`].
///
/// Ordering: cards already **due** (`due <= now`) come first, soonest-due first (lapsed cards,
/// scheduled minutes out, naturally lead); then **new** cards never reviewed, oldest-mined first.
/// Within each bucket the result is interleaved by repo so a multi-repo rotation doesn't drill one
/// repo to exhaustion before touching the next — the "multi-repo rotation" contract.
pub fn select_session<'a>(
    cards: &'a [Card],
    states: &BTreeMap<String, ReviewState>,
    sel: &Selector,
    now: i64,
    limit: usize,
) -> Vec<&'a Card> {
    let mut due: Vec<&Card> = Vec::new();
    let mut fresh: Vec<&Card> = Vec::new();
    for c in cards.iter().filter(|c| sel.allows(c)) {
        match states.get(&c.id) {
            Some(st) if st.due <= now => due.push(c),
            Some(_) => {} // not due yet
            None => fresh.push(c),
        }
    }
    due.sort_by(|a, b| {
        let da = states.get(&a.id).map(|s| s.due).unwrap_or(now);
        let db = states.get(&b.id).map(|s| s.due).unwrap_or(now);
        da.cmp(&db).then_with(|| a.id.cmp(&b.id))
    });
    fresh.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));

    let mut ordered = interleave_by_repo(due);
    ordered.extend(interleave_by_repo(fresh));
    ordered.truncate(limit);
    ordered
}

/// Round-robin a card list across its repos, preserving each repo's relative order. Keeps a
/// session varied instead of front-loading one repo.
fn interleave_by_repo(cards: Vec<&Card>) -> Vec<&Card> {
    let mut buckets: Vec<(String, Vec<&Card>)> = Vec::new();
    for c in cards {
        match buckets.iter_mut().find(|(r, _)| r == &c.repo) {
            Some((_, v)) => v.push(c),
            None => buckets.push((c.repo.clone(), vec![c])),
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let mut pushed = false;
        for (_, v) in buckets.iter() {
            if let Some(c) = v.get(i) {
                out.push(*c);
                pushed = true;
            }
        }
        if !pushed {
            break;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Card mining (deterministic formatters over structured git/note input)
// ---------------------------------------------------------------------------

/// A single commit with enough patch context to build a card. The cron gathers these via the
/// runner (`git log` + `git show`); the mining formatters below are pure over this struct so they
/// stay unit-testable without a real repo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitPatch {
    /// Commit sha (short or full).
    pub sha: String,
    /// Commit subject line (conventional-commit prefix still attached).
    pub subject: String,
    /// Files the commit touched.
    pub files: Vec<String>,
    /// The unified diff (`git show` body), used to derive the pre-fix view and the answer key.
    pub diff: String,
}

/// Strip a conventional-commit `type(scope):` prefix and a trailing `(#123)` PR ref from a
/// subject, leaving the human summary. Mirrors the cleaning used across sync/reports.
pub fn clean_subject(subject: &str) -> String {
    let mut s = subject.trim();
    if let Some((prefix, rest)) = s.split_once(':') {
        let prefix = prefix.trim();
        let kind = prefix.split('(').next().unwrap_or(prefix);
        let is_cc = !kind.is_empty()
            && kind.chars().all(|c| c.is_ascii_lowercase())
            && (prefix == kind || prefix.ends_with(')'));
        if is_cc {
            s = rest.trim();
        }
    }
    if let Some(idx) = s.rfind("(#") {
        if s[idx..].ends_with(')') && s[idx + 2..s.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            s = s[..idx].trim();
        }
    }
    s.to_string()
}

/// The conventional-commit type of a subject (`fix`, `feat`, …), lowercased, or `None`.
pub fn commit_type(subject: &str) -> Option<String> {
    let (prefix, _) = subject.trim().split_once(':')?;
    let kind = prefix.split('(').next().unwrap_or(prefix).trim();
    (!kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase())).then(|| kind.to_string())
}

/// The "before the fix" view of a diff: context and removed (`-`) lines only, with the added
/// (`+`) lines dropped so the fix itself stays hidden. Diff/hunk headers are kept for orientation.
/// Truncated to `max_lines` so a huge patch doesn't swamp the prompt.
pub fn pre_change_view(diff: &str, max_lines: usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in diff.lines() {
        // Drop added lines (the fix) but keep removed + context + headers.
        if line.starts_with('+') && !line.starts_with("+++") {
            continue;
        }
        out.push(line);
        if out.len() >= max_lines {
            out.push("… (truncated)");
            break;
        }
    }
    out.join("\n")
}

/// Truncate a body to `max_lines`, appending an ellipsis marker when clipped.
fn clip(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.trim_end().to_string();
    }
    let mut kept = lines[..max_lines].join("\n");
    kept.push_str("\n… (truncated)");
    kept
}

/// The first path segment that looks like a module (`crates/x/src/foo.rs` → `foo`,
/// `src/bar/baz.rs` → `bar`), used as a card's mastery area. `None` for top-level or unclear paths.
pub fn area_of_path(path: &str) -> Option<String> {
    let p = path.trim();
    // crates/<crate>/src/<module>.rs → <module>
    if let Some(rest) = p.split("/src/").nth(1) {
        let seg = rest.split('/').next().unwrap_or(rest);
        let stem = seg.strip_suffix(".rs").unwrap_or(seg);
        if !stem.is_empty() && stem != "main" && stem != "lib" {
            return Some(stem.to_string());
        }
    }
    None
}

/// The dominant area across a commit's files (the most common module), for tagging a mined card.
fn dominant_area(files: &[String]) -> Option<String> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for f in files {
        if let Some(a) = area_of_path(f) {
            *counts.entry(a).or_insert(0) += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(a, _)| a)
}

/// Build a **bug-mystery** card from a `fix:` commit, or `None` if the commit isn't a fix. The
/// prompt shows the symptom (cleaned subject), the touched files, and the pre-fix code; the answer
/// reveals the real diff.
pub fn mine_bug_mystery(repo: &str, c: &CommitPatch, created: i64) -> Option<Card> {
    if commit_type(&c.subject).as_deref() != Some("fix") {
        return None;
    }
    let symptom = clean_subject(&c.subject);
    let files = if c.files.is_empty() {
        "(unknown)".to_string()
    } else {
        c.files.join(", ")
    };
    let pre = pre_change_view(&c.diff, 40);
    let prompt = format!(
        "🐛 Bug mystery — {repo}\n\nSymptom (what was reported / went wrong):\n  {symptom}\n\nFiles touched: {files}\n\nThe code before the fix:\n{pre}\n\nQ: What was the bug, and how would you fix it?"
    );
    let answer = format!("The actual fix ({}):\n\n{}", c.sha, clip(&c.diff, 80));
    Some(Card::new(
        repo,
        CardKind::BugMystery,
        dominant_area(&c.files),
        prompt,
        answer,
        &c.sha,
        created,
    ))
}

/// Build a **feature-walkthrough** card from a `feat:` commit, or `None` otherwise. The prompt
/// asks the learner to explain the design; the answer shows what actually changed.
pub fn mine_feature(repo: &str, c: &CommitPatch, created: i64) -> Option<Card> {
    if commit_type(&c.subject).as_deref() != Some("feat") {
        return None;
    }
    let summary = clean_subject(&c.subject);
    let files = if c.files.is_empty() {
        "(unknown)".to_string()
    } else {
        c.files.join(", ")
    };
    let prompt = format!(
        "✨ Feature walkthrough — {repo}\n\nA feature shipped: {summary}\n\nFiles touched: {files}\n\nQ: What does this feature do, why was it built this way, and where does its logic live?"
    );
    let answer = format!("What changed ({}):\n\n{}", c.sha, clip(&c.diff, 80));
    Some(Card::new(
        repo,
        CardKind::Feature,
        dominant_area(&c.files),
        prompt,
        answer,
        &c.sha,
        created,
    ))
}

/// Build a **decision-recall** card from a branch-note decision entry. `source` is a stable
/// provenance string (e.g. the note path + a slug of the text).
pub fn mine_decision(
    repo: &str,
    area: Option<String>,
    decision: &str,
    source: &str,
    created: i64,
) -> Card {
    let text = decision.trim();
    // Pose the decision back as a "why" question by hiding the rationale after the first clause.
    let head = text.split([':', '—', '-']).next().unwrap_or(text).trim();
    let prompt = format!(
        "🧭 Decision recall — {repo}\n\nWe made this call: \"{head}\"\n\nQ: Why did we decide this? What was the reasoning / trade-off?"
    );
    let answer = format!("The recorded decision:\n\n{text}");
    Card::new(
        repo,
        CardKind::Decision,
        area,
        prompt,
        answer,
        source,
        created,
    )
}

/// Build an **architecture-tour** card for a module, given its sibling modules. Poses a
/// "where would X live?" style question and answers with the module's place among its siblings.
pub fn mine_architecture(repo: &str, module: &str, siblings: &[String], created: i64) -> Card {
    let others: Vec<&str> = siblings
        .iter()
        .filter(|s| s.as_str() != module)
        .map(String::as_str)
        .take(12)
        .collect();
    let prompt = format!(
        "🗺️  Architecture tour — {repo}\n\nThe `{module}` module is one of this codebase's subsystems.\n\nQ: What is `{module}` responsible for, and which neighbouring modules would you reach for related work?"
    );
    let answer = format!(
        "`{module}` sits alongside: {}.\n\n(Tour the module to confirm its responsibilities.)",
        if others.is_empty() {
            "(no recorded siblings)".to_string()
        } else {
            others.join(", ")
        }
    );
    Card::new(
        repo,
        CardKind::Architecture,
        Some(module.to_string()),
        prompt,
        answer,
        format!("module:{module}"),
        created,
    )
}

/// Distinct module names discovered across a set of file paths, sorted. Each maps a
/// `.../src/<module>.rs` (or `<module>/mod.rs`) path to its module via [`area_of_path`].
pub fn modules_from_paths(paths: &[String]) -> Vec<String> {
    let mut mods: Vec<String> = paths.iter().filter_map(|p| area_of_path(p)).collect();
    mods.sort();
    mods.dedup();
    mods
}

/// Build one architecture-tour card per module discovered in `paths`, each listing its sibling
/// modules. The live "map tour" `bn dojo` falls back to when no cards have been mined yet.
pub fn mine_architecture_from_paths(repo: &str, paths: &[String], created: i64) -> Vec<Card> {
    let modules = modules_from_paths(paths);
    modules
        .iter()
        .map(|m| mine_architecture(repo, m, &modules, created))
        .collect()
}

// ---------------------------------------------------------------------------
// Live gathering through the runner (used by the dojo-cards cron)
// ---------------------------------------------------------------------------

/// Read commits since `since` (`YYYY-MM-DD`) through the runner, keeping only `fix:`/`feat:`
/// commits (capped at `limit`, newest first), enriched with their files + diff via `git show`.
/// Best-effort: a git failure yields an empty list rather than an error.
pub fn gather_commits(runner: &dyn CommandRunner, since: &str, limit: usize) -> Vec<CommitPatch> {
    let since_arg = format!("--since={since}");
    let log = runner.git(&["log", &since_arg, "--no-merges", "--pretty=format:%H\x1f%s"]);
    let Some(stdout) = log.ok_stdout() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in stdout.lines() {
        if out.len() >= limit {
            break;
        }
        let Some((sha, subject)) = line.split_once('\x1f') else {
            continue;
        };
        let subject = subject.trim();
        match commit_type(subject).as_deref() {
            Some("fix") | Some("feat") => {}
            _ => continue,
        }
        let files = runner
            .git(&["show", "--name-only", "--pretty=format:", sha])
            .ok_stdout()
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let diff = runner
            .git(&["show", "--unified=3", "--pretty=format:", sha])
            .ok_stdout()
            .unwrap_or_default();
        out.push(CommitPatch {
            sha: sha.to_string(),
            subject: subject.to_string(),
            files,
            diff,
        });
    }
    out
}

/// Mine cards from a batch of gathered commits: one bug-mystery per `fix:`, one walkthrough per
/// `feat:`. Deterministic over its input, so it's tested directly.
pub fn mine_from_commits(repo: &str, commits: &[CommitPatch], created: i64) -> Vec<Card> {
    let mut out = Vec::new();
    for c in commits {
        if let Some(card) = mine_bug_mystery(repo, c, created) {
            out.push(card);
        } else if let Some(card) = mine_feature(repo, c, created) {
            out.push(card);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Store (append-only JSONL under <notes>/<machine>/dojo/)
// ---------------------------------------------------------------------------

/// The on-disk dojo store: three append-only JSONL logs in one directory. Reads fold the logs;
/// writes append, so the files sync across machines without git-conflicting (each machine has its
/// own `<machine>` subtree).
#[derive(Debug, Clone)]
pub struct DojoStore {
    dir: PathBuf,
}

impl DojoStore {
    /// A store rooted at `dir` (created on first write).
    pub fn new(dir: PathBuf) -> DojoStore {
        DojoStore { dir }
    }

    /// The production store: `<personal_root>/<machine>/dojo`.
    pub fn from_config(cfg: &Config) -> DojoStore {
        DojoStore::new(cfg.dojo_dir())
    }

    /// The directory this store lives in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn cards_path(&self) -> PathBuf {
        self.dir.join("cards.jsonl")
    }
    fn reviews_path(&self) -> PathBuf {
        self.dir.join("reviews.jsonl")
    }
    fn sessions_path(&self) -> PathBuf {
        self.dir.join("sessions.jsonl")
    }

    /// Every mined card, de-duplicated by id (last write wins). Missing file → empty.
    pub fn load_cards(&self) -> io::Result<Vec<Card>> {
        let mut by_id: BTreeMap<String, Card> = BTreeMap::new();
        for card in read_jsonl::<Card>(&self.cards_path())? {
            by_id.insert(card.id.clone(), card);
        }
        Ok(by_id.into_values().collect())
    }

    /// Append cards whose id isn't already present, returning how many were newly added. Idempotent
    /// re-mining: known ids are skipped. Cards are also de-duplicated within `new`.
    pub fn append_cards(&self, new: &[Card]) -> io::Result<usize> {
        let mut known: std::collections::BTreeSet<String> =
            self.load_cards()?.into_iter().map(|c| c.id).collect();
        let mut added = 0;
        for card in new {
            if known.insert(card.id.clone()) {
                append_jsonl(&self.cards_path(), card)?;
                added += 1;
            }
        }
        Ok(added)
    }

    /// Every logged review event, in file order.
    pub fn load_reviews(&self) -> io::Result<Vec<ReviewEvent>> {
        read_jsonl::<ReviewEvent>(&self.reviews_path())
    }

    /// The folded SM-2 state per card (convenience over `fold_reviews(&self.load_reviews())`).
    pub fn review_states(&self) -> io::Result<BTreeMap<String, ReviewState>> {
        Ok(fold_reviews(&self.load_reviews()?))
    }

    /// Append one graded answer.
    pub fn append_review(&self, ev: &ReviewEvent) -> io::Result<()> {
        append_jsonl(&self.reviews_path(), ev)
    }

    /// Every recorded session.
    pub fn load_sessions(&self) -> io::Result<Vec<SessionRecord>> {
        read_jsonl::<SessionRecord>(&self.sessions_path())
    }

    /// Append one completed session.
    pub fn append_session(&self, s: &SessionRecord) -> io::Result<()> {
        append_jsonl(&self.sessions_path(), s)
    }

    /// The streak computed from recorded session dates relative to `today`.
    pub fn streak(&self, today: &str) -> io::Result<Streak> {
        let dates: Vec<String> = self.load_sessions()?.into_iter().map(|s| s.date).collect();
        Ok(compute_streak(&dates, today))
    }
}

/// Read a JSONL file into a vec of `T`. Missing file → empty; malformed lines are errors.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<T>(line).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}:{}: invalid JSONL: {err}", path.display(), index + 1),
                )
            })
        })
        .collect()
}

/// Append one JSON-serializable record as a line, creating the parent dir on demand.
fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(repo: &str, kind: CardKind, source: &str, created: i64) -> Card {
        Card::new(repo, kind, None, "prompt", "answer", source, created)
    }

    // ---- ids ----

    #[test]
    fn card_id_is_stable_and_kind_prefixed() {
        let a = make_id("bn", CardKind::BugMystery, "abc123");
        let b = make_id("bn", CardKind::BugMystery, "abc123");
        assert_eq!(a, b, "same key → same id");
        assert!(a.starts_with("bug-"), "id carries the kind slug: {a}");
        // Different source / kind / repo all diverge.
        assert_ne!(a, make_id("bn", CardKind::BugMystery, "def456"));
        assert_ne!(a, make_id("bn", CardKind::Feature, "abc123"));
        assert_ne!(a, make_id("other", CardKind::BugMystery, "abc123"));
    }

    // ---- SM-2 scheduling math ----

    #[test]
    fn first_good_review_schedules_one_day_out() {
        let now = 1_000_000;
        let st = schedule(None, "c1", Grade::Good, now);
        assert_eq!(st.reps, 1);
        assert_eq!(st.lapses, 0);
        assert!((st.interval_days - FIRST_INTERVAL_DAYS).abs() < 1e-9);
        assert_eq!(st.due, now + 86_400);
    }

    #[test]
    fn consecutive_goods_grow_the_interval_mastered_backs_off() {
        let now = 0;
        let s1 = schedule(None, "c", Grade::Good, now);
        let s2 = schedule(Some(&s1), "c", Grade::Good, now);
        let s3 = schedule(Some(&s2), "c", Grade::Good, now);
        assert!(s1.interval_days < s2.interval_days, "1d → 6d");
        assert!(
            s3.interval_days > s2.interval_days,
            "6d → 6d*ease, mastered cards back off: {} !> {}",
            s3.interval_days,
            s2.interval_days
        );
    }

    #[test]
    fn lapse_resurfaces_sooner_than_a_pass_and_bumps_lapses() {
        let now = 10_000;
        let mastered = schedule(None, "c", Grade::Good, now);
        let mastered = schedule(Some(&mastered), "c", Grade::Good, now);
        // Now miss it.
        let lapsed = schedule(Some(&mastered), "c", Grade::Again, now);
        assert_eq!(lapsed.reps, 0, "lapse resets the rep counter");
        assert_eq!(lapsed.lapses, 1, "lapse counter bumped");
        assert!(
            lapsed.due - now < mastered.due - now,
            "missed card is due much sooner than the mastered schedule"
        );
        assert!(
            lapsed.due - now < 86_400,
            "lapsed card resurfaces within a day"
        );
        assert!(lapsed.ease < mastered.ease, "ease drops on a lapse");
    }

    #[test]
    fn ease_never_drops_below_floor() {
        let now = 0;
        let mut st = schedule(None, "c", Grade::Again, now);
        for _ in 0..20 {
            st = schedule(Some(&st), "c", Grade::Again, now);
        }
        assert!(
            st.ease >= MIN_EASE,
            "ease floored at {MIN_EASE}, got {}",
            st.ease
        );
    }

    #[test]
    fn easy_backs_off_faster_than_good() {
        let now = 0;
        let g = schedule(None, "c", Grade::Good, now);
        let good_next = schedule(Some(&g), "c", Grade::Good, now);
        let easy_next = schedule(Some(&g), "c", Grade::Easy, now);
        assert!(
            easy_next.interval_days > good_next.interval_days,
            "easy earns a longer interval than good"
        );
    }

    #[test]
    fn fold_reviews_applies_in_timestamp_order() {
        // Deliberately out of order in the slice; fold must sort by ts.
        let evs = vec![
            ReviewEvent {
                card_id: "c".into(),
                grade: Grade::Good,
                ts: 200,
            },
            ReviewEvent {
                card_id: "c".into(),
                grade: Grade::Good,
                ts: 100,
            },
        ];
        let states = fold_reviews(&evs);
        let st = states.get("c").unwrap();
        assert_eq!(st.reps, 2, "two passes folded regardless of slice order");
        assert_eq!(st.last_reviewed, 200, "last_reviewed is the latest ts");
    }

    // ---- streak edge cases ----

    #[test]
    fn streak_counts_consecutive_days_ending_today() {
        let dates = vec![
            "2026-07-04".into(),
            "2026-07-05".into(),
            "2026-07-06".into(),
        ];
        let s = compute_streak(&dates, "2026-07-06");
        assert_eq!(s.current, 3);
        assert_eq!(s.longest, 3);
        assert_eq!(s.last.as_deref(), Some("2026-07-06"));
    }

    #[test]
    fn streak_survives_yesterday_but_breaks_on_a_gap() {
        // Last session yesterday → streak still live.
        let dates = vec!["2026-07-04".into(), "2026-07-05".into()];
        assert_eq!(compute_streak(&dates, "2026-07-06").current, 2);
        // A two-day gap → current resets to 0, but longest is remembered.
        let gapped = vec!["2026-07-01".into(), "2026-07-02".into()];
        let s = compute_streak(&gapped, "2026-07-06");
        assert_eq!(s.current, 0, "stale streak resets");
        assert_eq!(s.longest, 2, "longest run preserved");
    }

    #[test]
    fn streak_dedups_multiple_sessions_same_day() {
        // Three sessions in one day + one the day before = a 2-day streak, not 4.
        let dates = vec![
            "2026-07-05".into(),
            "2026-07-06".into(),
            "2026-07-06".into(),
            "2026-07-06".into(),
        ];
        let s = compute_streak(&dates, "2026-07-06");
        assert_eq!(s.current, 2, "same-day sessions don't double-count");
    }

    #[test]
    fn streak_empty_history_is_zero() {
        assert_eq!(compute_streak(&[], "2026-07-06"), Streak::default());
    }

    #[test]
    fn streak_longest_can_exceed_current() {
        // A long past run, then a fresh single day today.
        let dates = vec![
            "2026-06-01".into(),
            "2026-06-02".into(),
            "2026-06-03".into(),
            "2026-06-04".into(),
            "2026-07-06".into(),
        ];
        let s = compute_streak(&dates, "2026-07-06");
        assert_eq!(s.current, 1);
        assert_eq!(s.longest, 4);
    }

    // ---- xp / levels ----

    #[test]
    fn levels_climb_with_xp() {
        assert_eq!(level_for(0).label, "novice");
        assert_eq!(level_for(49).label, "novice");
        assert_eq!(level_for(50).label, "apprentice");
        assert_eq!(level_for(150).label, "journeyman");
        let top = level_for(999_999);
        assert_eq!(top.label, "grandmaster");
        assert_eq!(top.xp_to_next, None, "no next level at the cap");
    }

    #[test]
    fn level_progress_tracks_into_and_to_next() {
        let l = level_for(100); // apprentice starts at 50, journeyman at 150
        assert_eq!(l.label, "apprentice");
        assert_eq!(l.xp_into, 50);
        assert_eq!(l.xp_to_next, Some(50));
    }

    #[test]
    fn xp_by_repo_joins_reviews_to_cards() {
        let cards = vec![
            card("bn", CardKind::Quiz, "s1", 0),
            card("arena", CardKind::Quiz, "s2", 0),
        ];
        let evs = vec![
            ReviewEvent {
                card_id: cards[0].id.clone(),
                grade: Grade::Good,
                ts: 1,
            }, // bn +10
            ReviewEvent {
                card_id: cards[1].id.clone(),
                grade: Grade::Again,
                ts: 2,
            }, // arena +2
            ReviewEvent {
                card_id: "ghost".into(),
                grade: Grade::Easy,
                ts: 3,
            }, // dropped
        ];
        let by = xp_by_repo(&evs, &cards);
        assert_eq!(by.get("bn"), Some(&10));
        assert_eq!(by.get("arena"), Some(&2));
        assert_eq!(by.len(), 2, "unknown card ignored");
        assert_eq!(total_xp(&evs), 27, "total counts every review");
    }

    // ---- mastery ----

    #[test]
    fn mastery_groups_by_area_and_flags_undrilled() {
        let cards = vec![
            Card::new(
                "bn",
                CardKind::Architecture,
                Some("mcp".into()),
                "p",
                "a",
                "module:mcp",
                0,
            ),
            Card::new(
                "bn",
                CardKind::BugMystery,
                Some("store".into()),
                "p",
                "a",
                "sha1",
                0,
            ),
            Card::new(
                "bn",
                CardKind::BugMystery,
                Some("store".into()),
                "p",
                "a",
                "sha2",
                0,
            ),
        ];
        // Review only one store card (miss it once).
        let evs = vec![ReviewEvent {
            card_id: cards[1].id.clone(),
            grade: Grade::Again,
            ts: 100,
        }];
        let states = fold_reviews(&evs);
        let m = mastery(&cards, &states);
        let mcp = m.iter().find(|a| a.area == "mcp").unwrap();
        assert_eq!(mcp.reviewed, 0, "mcp never drilled");
        let store = m.iter().find(|a| a.area == "store").unwrap();
        assert_eq!(store.cards, 2);
        assert_eq!(store.reviewed, 1);
        assert_eq!(store.lapses, 1);
        // Weakest surfaces the lapsed store area first.
        let weak = weakest_areas(&m, 1);
        assert_eq!(weak[0].area, "store");
    }

    #[test]
    fn weakest_area_owns_ranking_and_weak_signal_threshold() {
        let mastery = vec![
            MasteryArea {
                repo: "bn".into(),
                area: "clean".into(),
                cards: 2,
                reviewed: 2,
                lapses: 0,
                avg_ease: Some(1.3),
            },
            MasteryArea {
                repo: "bn".into(),
                area: "unknown".into(),
                cards: 3,
                reviewed: 0,
                lapses: 0,
                avg_ease: None,
            },
            MasteryArea {
                repo: "bn".into(),
                area: "lapsed".into(),
                cards: 2,
                reviewed: 1,
                lapses: 2,
                avg_ease: Some(2.5),
            },
        ];

        assert_eq!(weakest_area(&mastery).unwrap().area, "lapsed");
        assert_eq!(weakest_area(&mastery[..2]).unwrap().area, "unknown");
        assert_eq!(weakest_area(&mastery[..1]), None);
    }

    // ---- selection / rotation ----

    #[test]
    fn select_prefers_due_then_new_and_honors_repo_filter() {
        let now = 1_000_000;
        let mut cards = vec![
            card("bn", CardKind::Quiz, "due", 0),
            card("bn", CardKind::Quiz, "fresh", 5), // never reviewed
            card("other", CardKind::Quiz, "excluded", 0),
        ];
        // Make the first card due (reviewed in the past, short interval elapsed).
        let past = now - 10 * 86_400;
        let evs = vec![ReviewEvent {
            card_id: cards[0].id.clone(),
            grade: Grade::Again,
            ts: past,
        }];
        let states = fold_reviews(&evs);
        let sel = Selector {
            repos: vec!["bn".into()],
            ..Default::default()
        };
        let picked = select_session(&cards, &states, &sel, now, 10);
        let ids: Vec<&str> = picked.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(
            ids,
            vec!["due", "fresh"],
            "due first, then new; other repo excluded"
        );
        // A kind filter with no matches yields nothing.
        cards.push(card("bn", CardKind::BugMystery, "bug", 0));
        let bug_only = Selector {
            kind: Some(CardKind::BugMystery),
            ..Default::default()
        };
        let only = select_session(&cards, &states, &bug_only, now, 10);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].kind, CardKind::BugMystery);
    }

    #[test]
    fn select_interleaves_repos_for_rotation() {
        let now = 0;
        let cards = vec![
            card("bn", CardKind::Quiz, "bn1", 0),
            card("bn", CardKind::Quiz, "bn2", 1),
            card("arena", CardKind::Quiz, "arena1", 2),
        ];
        let states = BTreeMap::new(); // all fresh
        let picked = select_session(&cards, &states, &Selector::default(), now, 10);
        let repos: Vec<&str> = picked.iter().map(|c| c.repo.as_str()).collect();
        // Round-robin: bn, arena, bn — not bn, bn, arena.
        assert_eq!(repos, vec!["bn", "arena", "bn"]);
    }

    // ---- mining formatters ----

    #[test]
    fn commit_type_and_clean_subject() {
        assert_eq!(commit_type("fix: broken thing").as_deref(), Some("fix"));
        assert_eq!(
            commit_type("feat(dojo): add cards").as_deref(),
            Some("feat")
        );
        assert_eq!(commit_type("no prefix here"), None);
        assert_eq!(
            clean_subject("fix(store): drop torn line (#42)"),
            "drop torn line"
        );
    }

    #[test]
    fn bug_mystery_hides_the_fix_in_the_prompt() {
        let c = CommitPatch {
            sha: "abc1234".into(),
            subject: "fix(store): skip torn jsonl lines".into(),
            files: vec!["crates/bn-core/src/store.rs".into()],
            diff: "@@ -1,3 +1,3 @@\n context\n-    let bad = parse(line).unwrap();\n+    let Ok(ok) = parse(line) else { continue };\n more".into(),
        };
        let card = mine_bug_mystery("bn", &c, 999).unwrap();
        assert_eq!(card.kind, CardKind::BugMystery);
        assert_eq!(
            card.area.as_deref(),
            Some("store"),
            "area from the file path"
        );
        assert!(
            card.prompt.contains("skip torn jsonl lines"),
            "symptom shown"
        );
        assert!(
            card.prompt.contains("let bad = parse(line).unwrap();"),
            "pre-fix (removed) line shown"
        );
        assert!(
            !card.prompt.contains("let Ok(ok) = parse(line)"),
            "the fix (added line) must be hidden in the prompt"
        );
        assert!(
            card.answer.contains("let Ok(ok) = parse(line)"),
            "fix revealed in the answer"
        );
        assert!(card.answer.contains("abc1234"), "answer cites the commit");
        // A non-fix commit isn't a bug mystery.
        let feat = CommitPatch {
            subject: "feat: new thing".into(),
            ..c
        };
        assert!(mine_bug_mystery("bn", &feat, 0).is_none());
    }

    #[test]
    fn feature_card_only_from_feat_commits() {
        let c = CommitPatch {
            sha: "def5678".into(),
            subject: "feat(agent): supervise idle panes".into(),
            files: vec!["crates/bn-core/src/agent.rs".into()],
            diff: "@@\n+ new code".into(),
        };
        let card = mine_feature("bn", &c, 0).unwrap();
        assert_eq!(card.kind, CardKind::Feature);
        assert_eq!(card.area.as_deref(), Some("agent"));
        assert!(card.prompt.contains("supervise idle panes"));
        assert!(mine_feature(
            "bn",
            &CommitPatch {
                subject: "fix: x".into(),
                ..c
            },
            0
        )
        .is_none());
    }

    #[test]
    fn mine_from_commits_routes_by_type() {
        let commits = vec![
            CommitPatch {
                sha: "1".into(),
                subject: "fix: a".into(),
                ..Default::default()
            },
            CommitPatch {
                sha: "2".into(),
                subject: "feat: b".into(),
                ..Default::default()
            },
            CommitPatch {
                sha: "3".into(),
                subject: "chore: c".into(),
                ..Default::default()
            },
        ];
        let cards = mine_from_commits("bn", &commits, 0);
        assert_eq!(cards.len(), 2, "chore is skipped");
        assert_eq!(cards[0].kind, CardKind::BugMystery);
        assert_eq!(cards[1].kind, CardKind::Feature);
    }

    #[test]
    fn decision_and_architecture_cards_build() {
        let d = mine_decision(
            "bn",
            Some("resolver".into()),
            "launchd over cron on macOS: survives logout, better for laptops",
            "note:demo",
            0,
        );
        assert_eq!(d.kind, CardKind::Decision);
        assert!(d.prompt.contains("launchd over cron on macOS"));
        assert!(
            d.answer.contains("survives logout"),
            "rationale in the answer"
        );

        let a = mine_architecture(
            "bn",
            "mcp",
            &["mcp".into(), "server".into(), "prompts".into()],
            0,
        );
        assert_eq!(a.kind, CardKind::Architecture);
        assert_eq!(a.area.as_deref(), Some("mcp"));
        assert!(
            a.answer.contains("server"),
            "siblings listed, self excluded"
        );
        assert!(
            !a.answer.contains("mcp,"),
            "module itself excluded from siblings"
        );
    }

    #[test]
    fn area_of_path_extracts_module() {
        assert_eq!(
            area_of_path("crates/bn-core/src/store.rs").as_deref(),
            Some("store")
        );
        assert_eq!(
            area_of_path("crates/bn-cli/src/wt.rs").as_deref(),
            Some("wt")
        );
        assert_eq!(
            area_of_path("crates/bn-cli/src/main.rs"),
            None,
            "main is not an area"
        );
        assert_eq!(area_of_path("README.md"), None);
    }

    #[test]
    fn pre_change_view_drops_added_lines() {
        let diff = "@@ -1 +1 @@\n-old\n+new\n context";
        let view = pre_change_view(diff, 100);
        assert!(view.contains("-old"));
        assert!(view.contains(" context"));
        assert!(!view.contains("+new"));
    }

    // ---- store round-trip ----

    #[test]
    fn store_round_trips_cards_reviews_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = DojoStore::new(dir.path().join("dojo"));
        assert!(
            store.load_cards().unwrap().is_empty(),
            "empty store reads clean"
        );

        let cards = vec![
            card("bn", CardKind::Quiz, "s1", 10),
            card("bn", CardKind::BugMystery, "s2", 20),
        ];
        assert_eq!(store.append_cards(&cards).unwrap(), 2, "both new");
        // Re-appending is idempotent (dedup by id).
        assert_eq!(store.append_cards(&cards).unwrap(), 0, "no dups on re-mine");
        let loaded = store.load_cards().unwrap();
        assert_eq!(loaded.len(), 2);

        let ev = ReviewEvent {
            card_id: cards[0].id.clone(),
            grade: Grade::Good,
            ts: 100,
        };
        store.append_review(&ev).unwrap();
        assert_eq!(store.load_reviews().unwrap(), vec![ev]);
        assert!(store.review_states().unwrap().contains_key(&cards[0].id));

        let sess = SessionRecord {
            date: "2026-07-06".into(),
            ts: 100,
            cards: 1,
            xp: 10,
        };
        store.append_session(&sess).unwrap();
        assert_eq!(store.load_sessions().unwrap(), vec![sess]);
        assert_eq!(store.streak("2026-07-06").unwrap().current, 1);
    }

    #[test]
    fn missing_store_files_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = DojoStore::new(dir.path().join("dojo"));
        assert!(store.load_cards().unwrap().is_empty());
        assert!(store.load_reviews().unwrap().is_empty());
        assert!(store.load_sessions().unwrap().is_empty());
    }

    #[test]
    fn corrupt_cards_and_reviews_report_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = DojoStore::new(dir.path().to_path_buf());
        store
            .append_cards(&[card("bn", CardKind::Quiz, "ok", 0)])
            .unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(store.cards_path())
            .unwrap();
        writeln!(f, "{{ this is not valid json").unwrap();
        let err = store.load_cards().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("cards.jsonl:2"), "{err}");

        let review = ReviewEvent {
            card_id: "card-1".into(),
            grade: Grade::Good,
            ts: 100,
        };
        store.append_review(&review).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(store.reviews_path())
            .unwrap();
        writeln!(f, "not json").unwrap();
        let err = store.load_reviews().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("reviews.jsonl:2"), "{err}");
    }

    #[test]
    fn architecture_from_paths_builds_one_card_per_module() {
        let paths = vec![
            "crates/bn-core/src/store.rs".to_string(),
            "crates/bn-core/src/dojo.rs".to_string(),
            "crates/bn-cli/src/main.rs".to_string(), // main → no module
            "README.md".to_string(),                 // no module
        ];
        assert_eq!(modules_from_paths(&paths), vec!["dojo", "store"]);
        let cards = mine_architecture_from_paths("bn", &paths, 0);
        assert_eq!(cards.len(), 2, "one card per real module");
        assert!(cards.iter().all(|c| c.kind == CardKind::Architecture));
        // Siblings cross-reference each other.
        let store = cards
            .iter()
            .find(|c| c.area.as_deref() == Some("store"))
            .unwrap();
        assert!(store.answer.contains("dojo"), "sibling module listed");
    }

    #[test]
    fn gather_commits_filters_fix_feat_through_the_runner() {
        use crate::runner::FakeRunner;
        let log = "aaa\x1ffix: real bug\nbbb\x1ffeat: shiny\nccc\x1fchore: noise";
        let r = FakeRunner::new()
            .on(
                "git",
                &[
                    "log",
                    "--since=2026-01-01",
                    "--no-merges",
                    "--pretty=format:%H\x1f%s",
                ],
                log,
            )
            .on_program("git", ""); // show calls return empty (files/diff optional)
        let commits = gather_commits(&r, "2026-01-01", 10);
        assert_eq!(commits.len(), 2, "chore filtered out");
        assert_eq!(commits[0].subject, "fix: real bug");
        assert_eq!(commits[1].subject, "feat: shiny");
    }
}
