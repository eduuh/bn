//! `bn weakness gather` — the evidence for what to practise next, printed as JSON.
//!
//! The I/O edge over [`bn_core::weakness`]: read the recurring themes a post-merge review kept
//! naming, the dojo area never drilled, the tech inventory, the learn notes that already exist,
//! and the last few weeks' choices. It chooses nothing; the `staff-curriculum` agent does.
//!
//! The pure brain lives in [`bn_core::weakness`] (packet → plan → card). This module is the IO
//! half: it gathers the packet from the live stores (the #253 dojo mastery map, recurring
//! reflection themes, a cheap tech-inventory scan, prior Curriculum cards), applies the plan's
//! idempotent side-effects (the Monday daily goal, the seeded learn note, the dojo drills), writes
//! the briefing card, and mirrors it to the staff feed — the same shape as [`crate::cmd::chief`].
//!
//! Degraded mode. The Mentor staff agent (epic #266) that would emit recurring-theme cards is not
//! shipped yet, so the theme signal is sourced from `bn reflect` skill-themes as a proxy; when even
//! that and the dojo map are thin, the plan falls back to the tech inventory. The habit matters
//! more than perfect targeting, so a card always ships.
use crate::*;

use bn_core::weakness::{self, Packet, ThemeTag};
use bn_core::dojo::WeakArea;
use std::collections::BTreeMap;
use std::path::Path;

// --- gather -------------------------------------------------------------------------------------

/// The grouping theme for a learning entry — the skill name for a `skill:` reflection, else `None`
/// (repo/general entries are too generic to be recurring-gap tags).
fn reflection_theme(e: &bn_core::learnings::LearningEntry) -> Option<String> {
    e.skill.clone()
}

/// Recurring-theme tags from the week's reflections (the Mentor-degraded proxy): the count of
/// `skill:` reflections per skill this week vs the prior week.
fn gather_themes(cfg: &Config, today: &str) -> Vec<ThemeTag> {
    let d7 = bn_core::util::add_days(today, -7).unwrap_or_else(|| today.to_string());
    let d14 = bn_core::util::add_days(today, -14).unwrap_or_else(|| today.to_string());
    let mut this: BTreeMap<String, u32> = BTreeMap::new();
    let mut prior: BTreeMap<String, u32> = BTreeMap::new();
    for e in bn_core::learnings::all_entries(cfg, today) {
        let Some(tag) = reflection_theme(&e) else {
            continue;
        };
        if e.date.as_str() > d7.as_str() && e.date.as_str() <= today {
            *this.entry(tag).or_insert(0) += 1;
        } else if e.date.as_str() > d14.as_str() && e.date.as_str() <= d7.as_str() {
            *prior.entry(tag).or_insert(0) += 1;
        }
    }
    this.into_iter()
        .map(|(tag, count)| ThemeTag {
            prior: prior.get(&tag).copied().unwrap_or(0),
            tag,
            count,
        })
        .collect()
}

/// The weak area selected by dojo's canonical mastery policy.
fn gather_weak_area(cfg: &Config) -> Option<WeakArea> {
    let store = bn_core::dojo::DojoStore::from_config(cfg);
    let cards = match store.load_cards() {
        Ok(cards) => cards,
        Err(err) => {
            eprintln!("bn curriculum: {err}");
            return None;
        }
    };
    let states = match store.review_states() {
        Ok(states) => states,
        Err(err) => {
            eprintln!("bn curriculum: {err}");
            return None;
        }
    };
    let map = bn_core::dojo::mastery(&cards, &states);
    bn_core::dojo::weakest_area(&map)
}

/// Map a file extension to a coarse language label, or `None` when it isn't source we count.
fn ext_language(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" => "C++",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "rb" => "Ruby",
        "sh" | "bash" => "Shell",
        "lua" => "Lua",
        _ => return None,
    })
}

/// A cheap, bounded language scan of `root` (skipping vendored / build / VCS dirs), aggregated into
/// a tech inventory most-used-first. Best-effort: an unreadable tree just yields fewer facts.
fn scan_tech(root: &Path, file_cap: usize) -> Vec<weakness::TechFact> {
    let repo = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if seen >= file_cap {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    ".git" | "target" | "node_modules" | "vendor" | "dist" | "build" | ".venv"
                ) {
                    continue;
                }
                stack.push(path);
            } else {
                seen += 1;
                if let Some(lang) = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(ext_language)
                {
                    *counts.entry(lang).or_insert(0) += 1;
                }
            }
        }
    }
    let mut facts: Vec<weakness::TechFact> = counts
        .into_iter()
        .map(|(language, files)| weakness::TechFact {
            language: language.to_string(),
            repos: if repo.is_empty() {
                vec![]
            } else {
                vec![repo.clone()]
            },
            files,
        })
        .collect();
    facts.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.language.cmp(&b.language))
    });
    facts
}

/// Parse the topic a stored Curriculum card proposed, out of its Monday goal item.
fn parse_card_topic(card: &bn_core::staff::BriefingCard) -> Option<String> {
    card.items.iter().find_map(|it| {
        if it.kind != "goal" {
            return None;
        }
        it.title
            .strip_prefix(weakness::GOAL_PREFIX)?
            .strip_suffix(weakness::GOAL_SUFFIX)
            .map(str::to_string)
    })
}

/// Whether a goal with exactly `title` was ever marked done in the last ~40 days — the completion
/// signal for a prior topic.
fn goal_completed(cfg: &Config, today: &str, title: &str) -> bool {
    let start = bn_core::util::add_days(today, -40).unwrap_or_else(|| today.to_string());
    bn_core::daily::read_window(cfg, &start, today)
        .iter()
        .any(|d| {
            d.front
                .goals
                .iter()
                .any(|g| g.text == title && g.done == Some(true))
        })
}

/// The last few Curriculum cards (newest first, ≤ 4), each flattened to what continuity needs.
/// The agent whose past cards are this gather's continuity signal.
///
/// A card is filed under the roster name that produced it, and the reader here is the agent named
/// by `for_agent`. Hardcoding a name would make the collector blind the moment the agent were
/// renamed — which is exactly what happened once: cards began landing as `staff-curriculum` while
/// this still looked for `curriculum`, so every week read as the first week and continuity — the
/// carry-or-drop decision that stops a topic riding unfinished for a month — silently stopped
/// working.
fn gather_prior_cards(cfg: &Config, for_agent: &str, today: &str) -> Vec<weakness::PriorCard> {
    let mut refs: Vec<bn_core::staff::CardRef> = bn_core::staff::list_cards(cfg)
        .into_iter()
        .filter(|c| c.agent == for_agent)
        .collect();
    refs.sort_by(|a, b| b.date.cmp(&a.date));
    refs.into_iter()
        .filter_map(|r| {
            let card = bn_core::staff::read_card(&r.path)?;
            let topic = parse_card_topic(&card)?;
            let title = format!(
                "{}{topic}{}",
                weakness::GOAL_PREFIX,
                weakness::GOAL_SUFFIX
            );
            Some(weakness::PriorCard {
                completed: goal_completed(cfg, today, &title),
                date: r.date,
                topic,
            })
        })
        .take(4)
        .collect()
}

/// Gather the deterministic packet for `today` from the live stores.
pub(crate) fn gather_packet(cfg: &Config, for_agent: &str, today: &str) -> Packet {
    let themes = gather_themes(cfg, today);
    let weak_area = gather_weak_area(cfg);
    let tech = std::env::current_dir()
        .map(|d| scan_tech(&d, 6000))
        .unwrap_or_default();
    let existing_notes: Vec<String> = themes.iter().map(|t| t.tag.clone()).collect();
    let prior_cards = gather_prior_cards(cfg, for_agent, today);
    Packet::assemble(
        today,
        cfg.machine(),
        themes,
        weak_area,
        tech,
        existing_notes,
        prior_cards,
    )
}

/// The Monday to schedule the goal for: the upcoming Monday (today itself when today is a Monday).
fn next_monday(today: &str) -> String {
    let idx = bn_core::util::weekday_index(today).unwrap_or(0) as i64; // 0 = Sunday, 1 = Monday
    let add = (1 - idx).rem_euclid(7);
    bn_core::util::add_days(today, add).unwrap_or_else(|| today.to_string())
}

/// `bn weakness gather` — print the packet as JSON. Always exits 0: thin evidence is a real
/// answer, and the packet says so through its `signal` rather than by failing.
pub(crate) fn emit_weakness_gather(cfg: &Config, for_agent: &str) -> i32 {
    let today = today();
    let mut packet = gather_packet(cfg, for_agent, &today);
    packet.monday = next_monday(&today);
    println!("{}", packet.to_json());
    0
}
