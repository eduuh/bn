//! `bn curriculum` — the Curriculum Director (#260): the Sunday weekly learning plan.
//!
//! The pure brain lives in [`bn_core::curriculum`] (packet → plan → card). This module is the IO
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

use bn_core::curriculum::{self, ChosenTopic, Packet, Plan, Resource, ThemeTag};
use bn_core::dojo::WeakArea;
use std::collections::BTreeMap;
use std::path::Path;

/// What the run actually changed — for the summary line and the idempotency tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The Monday learning goal was scheduled (false = already present).
    pub monday_goal: bool,
    /// The learn note was seeded (false = already present).
    pub learn_note: bool,
    /// Dojo drills newly appended (0 = all already present).
    pub dojo_added: usize,
}

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
fn scan_tech(root: &Path, file_cap: usize) -> Vec<curriculum::TechFact> {
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
    let mut facts: Vec<curriculum::TechFact> = counts
        .into_iter()
        .map(|(language, files)| curriculum::TechFact {
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
            .strip_prefix(curriculum::GOAL_PREFIX)?
            .strip_suffix(curriculum::GOAL_SUFFIX)
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
fn gather_prior_cards(cfg: &Config, today: &str) -> Vec<curriculum::PriorCard> {
    let mut refs: Vec<bn_core::staff::CardRef> = bn_core::staff::list_cards(cfg)
        .into_iter()
        .filter(|c| c.agent == "curriculum")
        .collect();
    refs.sort_by(|a, b| b.date.cmp(&a.date));
    refs.into_iter()
        .filter_map(|r| {
            let card = bn_core::staff::read_card(&r.path)?;
            let topic = parse_card_topic(&card)?;
            let title = format!(
                "{}{topic}{}",
                curriculum::GOAL_PREFIX,
                curriculum::GOAL_SUFFIX
            );
            Some(curriculum::PriorCard {
                completed: goal_completed(cfg, today, &title),
                date: r.date,
                topic,
            })
        })
        .take(4)
        .collect()
}

/// Gather the deterministic packet for `today` from the live stores.
pub(crate) fn gather_packet(cfg: &Config, today: &str) -> Packet {
    let themes = gather_themes(cfg, today);
    let weak_area = gather_weak_area(cfg);
    let tech = std::env::current_dir()
        .map(|d| scan_tech(&d, 6000))
        .unwrap_or_default();
    let existing_notes: Vec<String> = themes.iter().map(|t| t.tag.clone()).collect();
    let prior_cards = gather_prior_cards(cfg, today);
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

// --- apply (idempotent side-effects) ------------------------------------------------------------

/// Apply the plan's side-effects idempotently: schedule the Monday goal (unless already queued /
/// on that day), seed the learn note (unless an entry for this topic exists), and append the dojo
/// drills (deduped by stable id). Returns what actually changed.
pub(crate) fn apply_plan(cfg: &Config, today: &str, now: i64, plan: &Plan) -> Applied {
    let mut applied = Applied::default();

    // 1) Monday learning goal — idempotent by exact title across the pending queue and the day.
    let title = plan.goal_title();
    let queued = bn_core::daily::scheduled_pending(cfg, today)
        .iter()
        .any(|s| s.text == title);
    let on_day = bn_core::daily::read_window(cfg, &plan.monday, &plan.monday)
        .iter()
        .any(|d| d.front.goals.iter().any(|g| g.text == title));
    if !queued
        && !on_day
        && bn_core::daily::add(cfg, today, &title, None, Some(&plan.monday)).is_ok()
    {
        applied.monday_goal = true;
    }

    // 2) Learn note — one `bn learned` entry carrying the topic + resource + outline, idempotent by
    //    a `curriculum: <topic>` text prefix so a re-run this week doesn't pile up.
    let learn_text = plan.learn_text();
    let prefix = format!("curriculum: {}", plan.chosen.topic);
    let exists = bn_core::learnings::all_entries(cfg, today)
        .iter()
        .any(|e| e.text.starts_with(&prefix));
    if !exists {
        let prov = bn_core::daily::LogProvenance {
            repo: (!plan.chosen.repo.is_empty()).then(|| plan.chosen.repo.clone()),
            branch: None,
            issue: None,
            harness: None,
        };
        if bn_core::daily::learned(cfg, today, &learn_text, &prov).is_ok() {
            applied.learn_note = true;
        }
    }

    // 3) Dojo drills — append_cards de-dups by the stable id, so re-seeding the same week is a
    //    no-op.
    let cards = plan.dojo_cards(now);
    applied.dojo_added = bn_core::dojo::DojoStore::from_config(cfg)
        .append_cards(&cards)
        .unwrap_or(0);

    applied
}

/// The feed items mirrored for a Curriculum card: the headline, then each routed item.
fn curriculum_feed_items(card: &bn_core::staff::BriefingCard) -> Vec<bn_core::feed::FeedItem> {
    let mut items = vec![bn_core::feed::FeedItem {
        key: "curriculum-headline".to_string(),
        text: card.headline.clone(),
        url: String::new(),
    }];
    for (i, it) in card.items.iter().enumerate() {
        items.push(bn_core::feed::FeedItem {
            key: format!("curriculum-{i}"),
            text: it.title.clone(),
            url: it.link.clone(),
        });
    }
    items
}

fn persist_curriculum_outputs(
    cfg: &Config,
    card: &bn_core::staff::BriefingCard,
) -> std::io::Result<()> {
    bn_core::staff::write_card(cfg, card)?;
    bn_core::feed::ingest("curriculum", curriculum_feed_items(card))
}

/// A resource finder — a harness-backed search that fills in the one best resource for a topic.
/// `None` from it (or no finder) → the deterministic self-study fallback.
type ResourceFinder<'a> = dyn Fn(&ChosenTopic) -> Option<Resource> + 'a;

/// The shared pipeline for `bn curriculum` and the `staff-curriculum` cron: gather → select topic →
/// (optionally) find the resource → build the plan → (when `apply`) apply the idempotent
/// side-effects, write the card, and mirror it to the feed. Returns the card and what it changed.
pub(crate) fn run_curriculum(
    cfg: &Config,
    today: &str,
    now: i64,
    apply: bool,
    finder: Option<&ResourceFinder>,
) -> std::io::Result<(bn_core::staff::BriefingCard, Applied)> {
    let packet = gather_packet(cfg, today);
    let monday = next_monday(today);
    let chosen = curriculum::select_topic(&packet);
    let resource = finder.and_then(|f| f(&chosen));
    let plan = curriculum::build_plan(&packet, &monday, resource);
    let card = curriculum::build_card(&plan);
    let applied = if apply {
        persist_curriculum_outputs(cfg, &card)?;
        apply_plan(cfg, today, now, &plan)
    } else {
        Applied::default()
    };
    Ok((card, applied))
}

/// A one-line summary of what a run changed, for the human-facing output.
fn applied_summary(a: &Applied) -> String {
    let goal = if a.monday_goal {
        "scheduled Monday goal"
    } else {
        "Monday goal already set"
    };
    let note = if a.learn_note {
        "seeded learn note"
    } else {
        "learn note present"
    };
    format!("{goal}; {note}; {} dojo drill(s) added", a.dojo_added)
}

// --- CLI entrypoints ----------------------------------------------------------------------------

/// `bn curriculum gather` — print the deterministic packet JSON (the agent-facing gather output).
pub(crate) fn emit_curriculum_gather(cfg: &Config) -> i32 {
    println!("{}", gather_packet(cfg, &today()).to_json());
    0
}

/// `bn curriculum` / `bn curriculum plan` — the read-only weekly preview: build the card without
/// applying any side-effects. `--json` emits the full card envelope.
pub(crate) fn emit_curriculum_plan(cfg: &Config, as_json: bool) {
    let today = today();
    let (card, _) = run_curriculum(cfg, &today, bn_core::util::now_epoch_secs(), false, None)
        .unwrap_or_else(|e| fail(&format!("bn curriculum plan: {e}")));
    if as_json {
        println!(
            "{}",
            Envelope::success("curriculum", serde_json::json!({ "card": card })).to_json()
        );
        return;
    }
    print!("{}", bn_core::staff::render_card_md(&card));
}

/// `bn curriculum run` — run the full pipeline and apply its idempotent side-effects. `--enrich`
/// asks a harness to find the one best resource (a network read); without it the deterministic
/// self-study fallback is used.
pub(crate) fn emit_curriculum_run(cfg: &Config, enrich: bool, as_json: bool) {
    let today = today();
    let finder = enrich.then(|| harness_resource_finder(cfg));
    let (card, applied) = run_curriculum(
        cfg,
        &today,
        bn_core::util::now_epoch_secs(),
        true,
        finder.as_deref(),
    )
    .unwrap_or_else(|e| {
        fail_json(
            as_json,
            "curriculum",
            "persist",
            &format!("bn curriculum run: could not persist outputs: {e}"),
        )
    });
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "curriculum",
                serde_json::json!({
                    "card": card,
                    "monday_goal": applied.monday_goal,
                    "learn_note": applied.learn_note,
                    "dojo_added": applied.dojo_added,
                }),
            )
            .to_json()
        );
        return;
    }
    print!("{}", bn_core::staff::render_card_md(&card));
    println!("\n— {}", applied_summary(&applied));
}

/// `staff-curriculum` cron — the Sunday 18:00 weekly plan. Deterministic (no enrichment) so the
/// scheduled run is reliable and network-independent; `bn curriculum run --enrich` is the manual
/// resource-search path.
pub(crate) fn job_staff_curriculum(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let (card, applied) =
        match run_curriculum(cfg, &today(), bn_core::util::now_epoch_secs(), true, None) {
            Ok(output) => output,
            Err(e) => {
                eprintln!("curriculum: could not persist outputs: {e}");
                return 1;
            }
        };
    println!("curriculum: {}", card.headline);
    println!("  {}", applied_summary(&applied));
    0
}

// --- harness-backed resource finder (the one legitimate network read) ---------------------------

/// Extract the first balanced top-level `{ … }` JSON object from harness output.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0i32;
    for (i, c) in raw[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a harness resource reply `{ "title", "url", "summary", "fallback"? }` into a [`Resource`],
/// keeping only a github/https resource link and dropping anything malformed.
fn parse_resource_reply(raw: &str) -> Option<Resource> {
    #[derive(serde::Deserialize)]
    struct Reply {
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        fallback: Option<String>,
    }
    let json = extract_json_object(raw)?;
    let r: Reply = serde_json::from_str(json).ok()?;
    if r.title.trim().is_empty() {
        return None;
    }
    Some(Resource {
        title: r.title.trim().to_string(),
        url: r.url.trim().to_string(),
        summary: r.summary.trim().to_string(),
        fallback: r.fallback.filter(|s| !s.trim().is_empty()),
    })
}

/// Build a harness-backed resource finder: a short, tightly-scoped headless dispatch asking for the
/// one best resource on the topic as strict JSON. Best-effort — any spawn/parse failure or empty
/// reply yields `None`, and the caller falls back to the deterministic self-study resource.
fn harness_resource_finder(_cfg: &Config) -> Box<ResourceFinder<'static>> {
    Box::new(move |chosen: &ChosenTopic| {
        let prompt = format!(
            "Find the single best learning resource for the topic \"{}\" ({}). It must be one \
             article, paper, or documentation chapter readable in under two hours. Reply with \
             ONLY a JSON object and nothing else: {{\"title\":\"...\",\"url\":\"...\",\
             \"summary\":\"one sentence\",\"fallback\":\"a second option or null\"}}.",
            chosen.topic, chosen.why
        );
        let cmd = staff_dispatch_command("copilot", &prompt, None, None)?;
        let (ok, stdout, _combined) =
            dispatch_capture(cmd, Some(std::time::Duration::from_secs(240)));
        if !ok {
            return None;
        }
        parse_resource_reply(&stdout)
    })
}


