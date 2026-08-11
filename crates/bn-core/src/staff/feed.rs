//! The briefing-card **feed store** — where a validated card is written, listed, read back, and
//! marked read. Split out of `staff.rs`, which is over the file-size limit and may not grow.
//!
//! Two stores, deliberately: the cards themselves live under the person-scoped notes
//! (`<personal_root>/staff/feed/<date>/<agent>.json`) and sync across machines, while the
//! read-marks live in this machine's state dir. A card is one record of what the staff said; whether
//! *you* have read it is local to the box you read it on.

use super::{card_key, migrate_feed_layout, plausible_date, BriefingCard};
use crate::resolver::{state_dir, Config};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn card_path(cfg: &Config, date: &str, agent: &str) -> PathBuf {
    cfg.staff_feed_dir()
        .join(date)
        .join(format!("{agent}.json"))
}

/// Render the human `.md` twin written beside every card.
pub fn render_card_md(card: &BriefingCard) -> String {
    let mut out = format!(
        "# {}\n\n_{} · {}_\n\n{}\n",
        card.headline,
        card.agent,
        card.date,
        card.body_md.trim()
    );
    if !card.items.is_empty() {
        out.push_str("\n## Items\n");
        for item in &card.items {
            out.push_str(&format!(
                "- [{}] {}: {}",
                item.severity, item.kind, item.title
            ));
            if !item.link.is_empty() {
                out.push_str(&format!(" ({})", item.link));
            }
            out.push('\n');
        }
    }
    out
}

/// Write a validated card to the feed store (`.json` + rendered `.md`). Same agent + date
/// overwrites that day's card; different days accumulate (append-only by date).
pub fn write_card(cfg: &Config, card: &BriefingCard) -> std::io::Result<PathBuf> {
    migrate_feed_layout(cfg);
    let path = card_path(cfg, &card.date, &card.agent);
    crate::store::save_json_atomic(&path, card)?;
    std::fs::write(path.with_extension("md"), render_card_md(card))?;
    Ok(path)
}

fn read_marks_path() -> PathBuf {
    state_dir().join("staff").join("read.json")
}

/// The set of `<date>/<agent>` keys already viewed. Machine-local (state dir), never synced.
pub fn read_marks() -> HashSet<String> {
    std::fs::read_to_string(read_marks_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

/// Mark a card read (idempotent). Returns whether it was newly marked.
pub fn mark_read(date: &str, agent: &str) -> std::io::Result<bool> {
    let key = card_key(date, agent);
    let mut marks = read_marks();
    if !marks.insert(key) {
        return Ok(false);
    }
    let mut sorted: Vec<String> = marks.into_iter().collect();
    sorted.sort();
    let path = read_marks_path();
    crate::store::save_json_atomic(&path, &sorted)?;
    Ok(true)
}

/// One card as listed by `bn staff feed` — identity, headline, and its read-mark.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CardRef {
    pub agent: String,
    pub date: String,
    pub headline: String,
    pub read: bool,
    pub path: PathBuf,
}

/// Every card in the feed store, unread-first then newest-first then by agent. Unparseable
/// files are skipped (a half-synced card just shows up next time).
pub fn list_cards(cfg: &Config) -> Vec<CardRef> {
    migrate_feed_layout(cfg);
    let marks = read_marks();
    let mut out = Vec::new();
    let Ok(dates) = std::fs::read_dir(cfg.staff_feed_dir()) else {
        return out;
    };
    for d in dates.flatten() {
        let date = d.file_name().to_string_lossy().into_owned();
        if !plausible_date(&date) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(card) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| serde_json::from_str::<BriefingCard>(&t).ok())
            else {
                continue;
            };
            out.push(CardRef {
                read: marks.contains(&card_key(&card.date, &card.agent)),
                agent: card.agent,
                date: card.date,
                headline: card.headline,
                path: p,
            });
        }
    }
    out.sort_by(|a, b| {
        a.read
            .cmp(&b.read)
            .then(b.date.cmp(&a.date))
            .then(a.agent.cmp(&b.agent))
    });
    out
}

/// Load one stored card.
pub fn read_card(path: &Path) -> Option<BriefingCard> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The count of *today's* unread cards — the cheap source for the tmux 📮 badge (#265). Reads only
/// `<feed>/<today>/*.json` plus the machine-local read-marks: no git, no network, and — unlike
/// [`list_cards`] — it never walks the whole dated history, so it stays fast enough for a status
/// interval. Scoping to today is also the freshness discipline the badge needs: an unread card
/// from a previous day is a stale "ghost" the ambient badge must not keep shouting (the popup still
/// lists it under history). Zero when the staff feed is absent — silence is the default state.
pub fn unread_count(cfg: &Config, today: &str) -> usize {
    let marks = read_marks();
    let dir = cfg.staff_feed_dir().join(today);
    let Ok(files) = std::fs::read_dir(&dir) else {
        return 0;
    };
    files
        .flatten()
        .filter(|f| f.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|f| read_card(&f.path()))
        .filter(|card| card.date == today && !marks.contains(&card_key(&card.date, &card.agent)))
        .count()
}
