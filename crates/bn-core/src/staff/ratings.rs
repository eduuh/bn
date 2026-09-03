//! Ratings and engagement — the staff feedback loop (#268).
//!
//! Its own module beside `cost` for the same reason that one is: `staff.rs` is at the
//! file-size ratchet's ceiling, and whether a card earned its cost is a self-contained question
//! from how the card was gathered or routed.
use super::*;

// --- ratings + engagement (staff feedback loop, #268) --------------------------------------------

/// A one-keystroke reaction to a briefing card: was this agent's card worth its cost?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Up,
    Down,
}

impl Rating {
    pub fn as_str(self) -> &'static str {
        match self {
            Rating::Up => "up",
            Rating::Down => "down",
        }
    }

    /// Parse the CLI token — `up`/`down` plus the obvious one-keystroke shorthands (`u`/`+`,
    /// `d`/`-`) so the reaction stays a single keypress.
    pub fn parse(s: &str) -> Option<Rating> {
        match s.trim().to_ascii_lowercase().as_str() {
            "up" | "u" | "+" => Some(Rating::Up),
            "down" | "d" | "-" => Some(Rating::Down),
            _ => None,
        }
    }
}

/// One stored rating of a card, keyed `<date>/<agent>` in the store (a re-rate overwrites). The
/// `note` is optional free text; down-vote notes also accumulate in the agent's tuning log as
/// prompt-iteration material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatingRecord {
    pub agent: String,
    pub date: String,
    pub rating: Rating,
    #[serde(default)]
    pub note: String,
    pub ts: i64,
}

fn ratings_path() -> PathBuf {
    state_dir().join("staff").join("ratings.json")
}

/// Every card rating, keyed `<date>/<agent>`. Machine-local (state dir), beside the read-marks and
/// never synced. Unreadable/unparseable → empty (a fresh machine has rated nothing).
pub fn read_ratings() -> std::collections::HashMap<String, RatingRecord> {
    std::fs::read_to_string(ratings_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn tuning_path(agent: &str) -> PathBuf {
    state_dir()
        .join("staff")
        .join("tuning")
        .join(format!("{agent}.md"))
}

/// Append a down-vote note to the agent's tuning log (append-only, machine-local) — the raw
/// material for iterating its skill prompt (#268). Best-effort; a write failure never fails a rate.
fn append_tuning_note(agent: &str, date: &str, note: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = tuning_path(agent);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "- {date}: {note}")
}

/// Record a reaction to a card (idempotent overwrite of that card's prior rating) and mark it read
/// — you cannot react to a card you never saw. A down-vote note is also appended to the agent's
/// tuning log. Returns the stored record.
pub fn rate_card(
    date: &str,
    agent: &str,
    rating: Rating,
    note: &str,
) -> std::io::Result<RatingRecord> {
    let rec = RatingRecord {
        agent: agent.to_string(),
        date: date.to_string(),
        rating,
        note: note.trim().to_string(),
        ts: crate::util::now_epoch_secs(),
    };
    let mut all = read_ratings();
    all.insert(card_key(date, agent), rec.clone());
    let path = ratings_path();
    crate::store::save_json_atomic(&path, &all)?;
    crate::util::best_effort(mark_read(date, agent), "staff: mark rated card read");
    if rating == Rating::Down && !rec.note.is_empty() {
        crate::util::best_effort(
            append_tuning_note(agent, date, &rec.note),
            "staff: append tuning note",
        );
    }
    Ok(rec)
}

/// Resolve a `bn feed rate <card>` selector to exactly one card. Accepts an exact `<date>/<agent>`,
/// an agent name (its newest card), or a `YYYY-MM-DD` date (that day's card when unambiguous). On
/// no match or an ambiguous date, returns a human-readable error naming the candidates.
pub fn resolve_card(cfg: &Config, sel: &str) -> Result<CardRef, String> {
    let cards = list_cards(cfg);
    if cards.is_empty() {
        return Err("no briefing cards yet — staff agents write one per run".to_string());
    }
    if let Some(c) = cards.iter().find(|c| card_key(&c.date, &c.agent) == sel) {
        return Ok(c.clone());
    }
    if let Some(newest) = cards
        .iter()
        .filter(|c| c.agent == sel)
        .max_by(|a, b| a.date.cmp(&b.date))
    {
        return Ok(newest.clone());
    }
    let by_date: Vec<&CardRef> = cards.iter().filter(|c| c.date == sel).collect();
    match by_date.as_slice() {
        [] => Err(format!(
            "no card matches '{sel}' — try an agent name, a YYYY-MM-DD date, or <date>/<agent>"
        )),
        [c] => Ok((*c).clone()),
        many => Err(format!(
            "'{sel}' matches {} cards — qualify as <date>/<agent>: {}",
            many.len(),
            many.iter()
                .map(|c| card_key(&c.date, &c.agent))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The staff performance-review window: a fixed ~month, independent of `bn insights --since`.
/// Staff cadence is ~daily, so 30 days is a meaningful engagement sample, and the fire-me signal
/// is defined over "~a month" of sustained ignore/down-rating.
pub const REVIEW_WINDOW_DAYS: i64 = 30;

/// At/below this read-rate over the window, an agent's cards are effectively ignored.
const FIRE_IGNORE_READ_PCT: f64 = 0.25;
/// At/above this net down-vote count (down − up), an agent is being actively down-rated.
const FIRE_NET_DOWNVOTES: i64 = 3;
/// Minimum cards in the window before the fire-me flag can fire — never fire a barely-run agent.
const FIRE_MIN_CARDS: usize = 4;

/// One agent's engagement over the review window (#268): how many of its cards you read, how many
/// of the items it routed you acted on, your explicit up/down ratings, and — when sustained ignore
/// or down-rating crosses the threshold — the fire-me flag proposing its removal.
#[derive(Debug, Clone, Serialize)]
pub struct AgentReview {
    pub agent: String,
    pub cards_total: usize,
    pub cards_read: usize,
    /// `cards_read / cards_total`; `None` when the agent has no cards in the window.
    pub read_pct: Option<f64>,
    pub items_routed: usize,
    pub items_acted: usize,
    /// `items_acted / items_routed`; `None` when the agent routed nothing in the window.
    pub acted_pct: Option<f64>,
    pub up: usize,
    pub down: usize,
    /// Newest in-window card date, for recency.
    pub last_card: Option<String>,
    /// Sustained ignore or down-rating over the window — proposes the agent's removal.
    pub fire_me: bool,
}

#[derive(Default)]
struct EngagementAcc {
    cards_total: usize,
    cards_read: usize,
    items_routed: usize,
    items_acted: usize,
    up: usize,
    down: usize,
    last_card: Option<String>,
}

/// Build the per-agent staff performance review over the fixed [`REVIEW_WINDOW_DAYS`] window as of
/// `today`. Pure read: folds the card store + read-marks, the ratings store, and the attention
/// event log (items an agent routed vs the ones you acted on). Agents are those that produced a
/// card or a rating in the window; worst-engaged (and fire-me) first. The window boundary is
/// midnight of `today − REVIEW_WINDOW_DAYS`, applied to card dates, rating times, and raise times
/// alike.
pub fn performance_review(cfg: &Config, today: &str) -> Vec<AgentReview> {
    let start =
        crate::util::add_days(today, -REVIEW_WINDOW_DAYS).unwrap_or_else(|| today.to_string());
    let start_secs = crate::util::epoch_day(&start).unwrap_or(0) * 86_400;

    let mut by: std::collections::BTreeMap<String, EngagementAcc> = Default::default();

    // Cards + read-marks, windowed by card date.
    for c in list_cards(cfg) {
        if c.date.as_str() < start.as_str() {
            continue;
        }
        let a = by.entry(c.agent.clone()).or_default();
        a.cards_total += 1;
        if c.read {
            a.cards_read += 1;
        }
        if a.last_card.as_deref().is_none_or(|d| c.date.as_str() > d) {
            a.last_card = Some(c.date.clone());
        }
    }

    // Explicit ratings, windowed by rating time.
    for rec in read_ratings().values() {
        if rec.ts < start_secs {
            continue;
        }
        let a = by.entry(rec.agent.clone()).or_default();
        match rec.rating {
            Rating::Up => a.up += 1,
            Rating::Down => a.down += 1,
        }
    }

    // Items routed vs acted, from the attention log — attributable per agent via the `staff-<name>`
    // source the router stamps. Only enrich agents already active (carded/rated) in the window.
    for o in crate::attention::outcomes() {
        if o.created < start_secs {
            continue;
        }
        let Some(agent) = o.source.strip_prefix("staff-") else {
            continue;
        };
        if let Some(a) = by.get_mut(agent) {
            a.items_routed += 1;
            if o.acted() {
                a.items_acted += 1;
            }
        }
    }

    let mut out: Vec<AgentReview> = by
        .into_iter()
        .map(|(agent, a)| {
            let read_pct = (a.cards_total > 0).then(|| a.cards_read as f64 / a.cards_total as f64);
            let acted_pct =
                (a.items_routed > 0).then(|| a.items_acted as f64 / a.items_routed as f64);
            let ignored = read_pct.is_some_and(|p| p <= FIRE_IGNORE_READ_PCT);
            let downrated = a.down as i64 - a.up as i64 >= FIRE_NET_DOWNVOTES;
            let fire_me = a.cards_total >= FIRE_MIN_CARDS && (ignored || downrated);
            AgentReview {
                agent,
                cards_total: a.cards_total,
                cards_read: a.cards_read,
                read_pct,
                items_routed: a.items_routed,
                items_acted: a.items_acted,
                acted_pct,
                up: a.up,
                down: a.down,
                last_card: a.last_card,
                fire_me,
            }
        })
        .collect();

    // Worst-engaged first: fire-me, then lowest read-rate, then most down-votes, then name.
    out.sort_by(|x, y| {
        y.fire_me
            .cmp(&x.fire_me)
            .then(
                x.read_pct
                    .unwrap_or(1.0)
                    .total_cmp(&y.read_pct.unwrap_or(1.0)),
            )
            .then(y.down.cmp(&x.down))
            .then(x.agent.cmp(&y.agent))
    });
    out
}
