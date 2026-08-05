//! `bn dojo` — architecture-tour learning cards: session/map/stats/config.
use crate::*;

fn dojo_store_read<T>(result: std::io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!("bn dojo: {err}");
            None
        }
    }
}

/// `bn night status` (and bare `bn night`) — the shift's config + current state.
/// Build a session [`Selector`] from the `[dojo]` rotation plus optional overrides. A `--repo`
/// override wins over the configured allowlist; an empty allowlist means "every repo with cards".
pub(crate) fn dojo_selector(
    repo: Option<String>,
    kind: Option<bn_core::dojo::CardKind>,
    area: Option<String>,
) -> bn_core::dojo::Selector {
    let jc = resolver::JobConfig::load();
    let repos = match repo {
        Some(r) => vec![r],
        None => jc.dojo_repos,
    };
    bn_core::dojo::Selector { repos, kind, area }
}

/// The bare repo name of the current directory, best-effort (`None` outside a repo). Used to seed
/// a live architecture tour when no cards have been mined yet.
pub(crate) fn dojo_current_repo(cfg: &Config) -> Option<String> {
    resolver::resolve(cfg, &std::env::current_dir().ok()?)
        .ok()
        .map(|r| r.repo)
}

/// Seed a live architecture tour: list the current repo's `.rs` modules via `git ls-files` and
/// mine one architecture card per module into the store. Returns how many new cards were added
/// (0 if not in a repo / nothing to mine). The graceful-degrade path for an empty deck.
pub(crate) fn dojo_seed_arch_tour(cfg: &Config, store: &bn_core::dojo::DojoStore) -> usize {
    use bn_core::runner::{CommandRunner, RealRunner};
    let Some(repo) = dojo_current_repo(cfg) else {
        return 0;
    };
    let files: Vec<String> = RealRunner
        .git(&["ls-files", "*.rs"])
        .ok_stdout()
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    if files.is_empty() {
        return 0;
    }
    let now = bn_core::util::now_epoch_secs();
    let cards = bn_core::dojo::mine_architecture_from_paths(&repo, &files, now);
    store.append_cards(&cards).unwrap_or(0)
}

/// One card as a JSON object — the scriptable shape emitted by `--json` sessions.
pub(crate) fn dojo_card_json(card: &bn_core::dojo::Card) -> serde_json::Value {
    serde_json::json!({
        "id": card.id,
        "repo": card.repo,
        "kind": card.kind.slug(),
        "area": card.area,
        "prompt": card.prompt,
        "answer": card.answer,
    })
}

/// `bn dojo` (+ `quiz` / `bug`) — run today's session: pick due/new cards for the selector,
/// present each (prompt → reveal → self-grade), append the reviews, then record the session and
/// update the streak/XP. Degrades to a live architecture tour when the deck has nothing ready, and
/// stays non-interactive (a scriptable peek) under `--json` or a non-tty.
pub(crate) fn emit_dojo_session(
    cfg: &Config,
    sel: bn_core::dojo::Selector,
    max: Option<usize>,
    as_json: bool,
) {
    use bn_core::clock::{Clock, RealClock};
    use bn_core::dojo::{self, Card, CardKind, ReviewEvent, SessionRecord};
    use std::io::IsTerminal;

    let clock = RealClock;
    let store = dojo::DojoStore::from_config(cfg);
    let jc = resolver::JobConfig::load();
    let target = max
        .filter(|n| *n > 0)
        .unwrap_or(jc.dojo_daily_target)
        .max(1);
    let now = clock.now_epoch_secs();

    let Some(mut cards) = dojo_store_read(store.load_cards()) else {
        return;
    };
    let Some(mut states) = dojo_store_read(store.review_states()) else {
        return;
    };
    let mut picks: Vec<Card> = dojo::select_session(&cards, &states, &sel, now, target)
        .into_iter()
        .cloned()
        .collect();

    // Graceful degrade: nothing ready → seed + run a live architecture tour instead.
    let mut degraded = false;
    if picks.is_empty() && dojo_seed_arch_tour(cfg, &store) > 0 {
        degraded = true;
        let Some(reloaded_cards) = dojo_store_read(store.load_cards()) else {
            return;
        };
        cards = reloaded_cards;
        let Some(reloaded_states) = dojo_store_read(store.review_states()) else {
            return;
        };
        states = reloaded_states;
        let tour = dojo::Selector {
            kind: Some(CardKind::Architecture),
            ..sel
        };
        picks = dojo::select_session(&cards, &states, &tour, now, target)
            .into_iter()
            .cloned()
            .collect();
    }

    if picks.is_empty() {
        if as_json {
            println!(
                "{}",
                Envelope::success("dojo", serde_json::json!({ "cards": [], "ready": false }),)
                    .to_json()
            );
        } else {
            println!("bn dojo: no cards ready yet.");
            println!("  Mine some with `bn run dojo-cards` (or enable the nightly cron:");
            println!("  set `[dojo] enabled = true` and list repos under `[dojo] repos`).");
        }
        return;
    }

    // Non-interactive surfaces: emit the queued session without blocking on stdin.
    if as_json {
        let arr: Vec<serde_json::Value> = picks.iter().map(dojo_card_json).collect();
        println!(
            "{}",
            Envelope::success(
                "dojo",
                serde_json::json!({
                    "ready": true,
                    "degraded": degraded,
                    "count": arr.len(),
                    "cards": arr,
                }),
            )
            .to_json()
        );
        return;
    }
    if !std::io::stdin().is_terminal() {
        println!(
            "bn dojo: {} card(s) queued (non-interactive peek):",
            picks.len()
        );
        for (i, c) in picks.iter().enumerate() {
            println!(
                "\n[{}/{}] {} — {}",
                i + 1,
                picks.len(),
                c.kind.label(),
                c.repo
            );
            println!("{}", c.prompt);
        }
        println!("\nRun `bn dojo` in a terminal to answer + grade these.");
        return;
    }

    // Interactive session.
    use std::io::Write;
    if degraded {
        println!("No mined cards ready — running a live architecture tour instead.\n");
    }
    println!(
        "🥋 bn dojo — {} card(s). For each: read, press Enter to reveal, then grade yourself.",
        picks.len()
    );
    let mut answered = 0u32;
    let mut xp = 0u64;
    let mut per_repo: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    'session: for (i, card) in picks.iter().enumerate() {
        println!(
            "\n──────────── [{}/{}] {} · {} ────────────",
            i + 1,
            picks.len(),
            card.kind.label(),
            card.repo
        );
        println!("{}", card.prompt);
        print!("\n(press Enter to reveal the answer, or `q` to stop) ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        if line.trim().eq_ignore_ascii_case("q") {
            break;
        }
        println!("\n── answer ──\n{}", card.answer);

        // Self-grade loop.
        let grade = loop {
            print!("\nHow did you do?  [a]gain  [h]ard  [g]ood  [e]asy  ([q] stop) > ");
            std::io::stdout().flush().ok();
            let mut g = String::new();
            if std::io::stdin().read_line(&mut g).unwrap_or(0) == 0 {
                break 'session;
            }
            let g = g.trim();
            if g.eq_ignore_ascii_case("q") {
                break 'session;
            }
            if let Some(grade) = dojo::Grade::from_input(g) {
                break grade;
            }
            println!("  (enter a, h, g, or e)");
        };

        let ev = ReviewEvent {
            card_id: card.id.clone(),
            grade,
            ts: clock.now_epoch_secs(),
        };
        if let Err(e) = store.append_review(&ev) {
            eprintln!("bn dojo: could not record review: {e}");
        }
        answered += 1;
        xp += grade.xp();
        *per_repo.entry(card.repo.clone()).or_insert(0) += grade.xp();
    }

    if answered == 0 {
        println!("\nSession ended — no cards graded, nothing recorded.");
        return;
    }

    let date = clock.today();
    let session = SessionRecord {
        date: date.clone(),
        ts: clock.now_epoch_secs(),
        cards: answered,
        xp,
    };
    if let Err(e) = store.append_session(&session) {
        eprintln!("bn dojo: could not record session: {e}");
    }
    dojo_emit_session_metric(cfg, &store, &date, answered, xp);

    let Some(streak) = dojo_store_read(store.streak(&date)) else {
        return;
    };
    let Some(all_reviews) = dojo_store_read(store.load_reviews()) else {
        return;
    };
    let total = dojo::total_xp(&all_reviews);
    let level = dojo::level_for(total);
    println!("\n✅ Session complete — {answered} card(s), +{xp} XP.");
    println!(
        "   🔥 streak: {} day(s) (longest {})   ·   level: {} ({} XP total)",
        streak.current, streak.longest, level.label, total
    );
    if let Some(to_next) = level.xp_to_next {
        println!("   {to_next} XP to the next level.");
    }
}

/// Emit a `dojo_session` productivity metric so `bn insights` can report streak + habit drift.
/// Best-effort: a write failure is swallowed (the session already recorded to the dojo store).
pub(crate) fn dojo_emit_session_metric(
    cfg: &Config,
    store: &bn_core::dojo::DojoStore,
    date: &str,
    cards: u32,
    xp: u64,
) {
    let Some(streak) = dojo_store_read(store.streak(date)) else {
        return;
    };
    let mut fields = serde_json::Map::new();
    fields.insert("cards".into(), serde_json::json!(cards));
    fields.insert("xp".into(), serde_json::json!(xp));
    fields.insert("streak".into(), serde_json::json!(streak.current));
    let _ = bn_core::events::append_metric_at(
        &cfg.metrics_log_path(),
        "dojo_session",
        bn_core::util::now_epoch_secs(),
        fields,
    );
}

/// `bn dojo map [<area>]` — an architecture tour. With an area, drills that subsystem's cards
/// (seeding a live tour if none are mined yet); without one, lists the areas you can tour.
pub(crate) fn emit_dojo_map(
    cfg: &Config,
    area: Option<String>,
    repo: Option<String>,
    as_json: bool,
) {
    use bn_core::dojo::{self, CardKind};
    let store = dojo::DojoStore::from_config(cfg);

    let Some(area) = area else {
        // No area → list tourable areas from the mastery map.
        let Some(cards) = dojo_store_read(store.load_cards()) else {
            return;
        };
        if cards.is_empty() {
            dojo_seed_arch_tour(cfg, &store);
        }
        let Some(cards) = dojo_store_read(store.load_cards()) else {
            return;
        };
        let Some(states) = dojo_store_read(store.review_states()) else {
            return;
        };
        let map = dojo::mastery(&cards, &states);
        if as_json {
            let arr: Vec<serde_json::Value> = map
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "repo": a.repo,
                        "area": a.area,
                        "cards": a.cards,
                        "reviewed": a.reviewed,
                        "coverage": a.coverage(),
                    })
                })
                .collect();
            println!(
                "{}",
                Envelope::success("dojo", serde_json::json!({ "areas": arr })).to_json()
            );
            return;
        }
        if map.is_empty() {
            println!("bn dojo map: no areas yet — run `bn dojo` inside a repo to seed a tour.");
            return;
        }
        println!("🗺️  Areas you can tour (`bn dojo map <area>`):");
        for a in &map {
            let seen = if a.reviewed == 0 {
                " · never drilled".to_string()
            } else {
                format!(" · {:.0}% covered", a.coverage() * 100.0)
            };
            println!("  {} / {}  ({} card(s){})", a.repo, a.area, a.cards, seen);
        }
        return;
    };

    // With an area: seed if needed, then run a focused session on that area.
    let Some(cards) = dojo_store_read(store.load_cards()) else {
        return;
    };
    if cards.iter().all(|c| {
        c.area
            .as_deref()
            .map(|a| !a.eq_ignore_ascii_case(&area))
            .unwrap_or(true)
    }) {
        dojo_seed_arch_tour(cfg, &store);
    }
    let sel = dojo_selector(repo, Some(CardKind::Architecture), Some(area));
    emit_dojo_session(cfg, sel, None, as_json);
}

/// `bn dojo stats` — streak, XP + level per repo, deck size, and weakest areas.
pub(crate) fn emit_dojo_stats(cfg: &Config, as_json: bool) {
    use bn_core::dojo;
    let store = dojo::DojoStore::from_config(cfg);
    let today = today();
    let Some(cards) = dojo_store_read(store.load_cards()) else {
        return;
    };
    let Some(reviews) = dojo_store_read(store.load_reviews()) else {
        return;
    };
    let Some(states) = dojo_store_read(store.review_states()) else {
        return;
    };
    let now = bn_core::util::now_epoch_secs();

    let Some(streak) = dojo_store_read(store.streak(&today)) else {
        return;
    };
    let total = dojo::total_xp(&reviews);
    let level = dojo::level_for(total);
    let by_repo = dojo::xp_by_repo(&reviews, &cards);
    let map = dojo::mastery(&cards, &states);
    let weakest = dojo::weakest_areas(&map, 5);
    let due = cards
        .iter()
        .filter(|c| states.get(&c.id).map(|s| s.due <= now).unwrap_or(true))
        .count();

    if as_json {
        let repos: Vec<serde_json::Value> = by_repo
            .iter()
            .map(|(repo, xp)| {
                let lvl = dojo::level_for(*xp);
                serde_json::json!({ "repo": repo, "xp": xp, "level": lvl.label })
            })
            .collect();
        let weak: Vec<serde_json::Value> = weakest
            .iter()
            .map(|a| {
                serde_json::json!({
                    "repo": a.repo,
                    "area": a.area,
                    "lapses": a.lapses,
                    "coverage": a.coverage(),
                    "reviewed": a.reviewed,
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success(
                "dojo",
                serde_json::json!({
                    "streak": streak.current,
                    "longest_streak": streak.longest,
                    "last_session": streak.last,
                    "total_xp": total,
                    "level": level.label,
                    "cards": cards.len(),
                    "due": due,
                    "per_repo": repos,
                    "weakest_areas": weak,
                }),
            )
            .to_json()
        );
        return;
    }

    println!("🥋 dojo stats");
    println!(
        "  🔥 streak:  {} day(s)  (longest {}{})",
        streak.current,
        streak.longest,
        streak
            .last
            .as_deref()
            .map(|d| format!(", last {d}"))
            .unwrap_or_default()
    );
    println!("  ⭐ level:   {} — {total} XP total", level.label);
    if let Some(n) = level.xp_to_next {
        println!("             {n} XP to next level");
    }
    println!("  🃏 deck:    {} card(s), {due} due now", cards.len());
    if by_repo.is_empty() {
        println!("  per repo:  (no reviews yet — run `bn dojo`)");
    } else {
        println!("  per repo:");
        for (repo, xp) in &by_repo {
            println!("    {repo}: {} — {xp} XP", dojo::level_for(*xp).label);
        }
    }
    if !weakest.is_empty() {
        println!("  weakest areas:");
        for a in &weakest {
            let tag = if a.reviewed == 0 {
                "never drilled".to_string()
            } else {
                format!(
                    "{} lapse(s), {:.0}% covered",
                    a.lapses,
                    a.coverage() * 100.0
                )
            };
            println!("    {} / {} — {tag}", a.repo, a.area);
        }
    }
}

/// `bn dojo config` — the rotation: which repos are in play and how many cards each holds.
pub(crate) fn emit_dojo_config(cfg: &Config, as_json: bool) {
    use bn_core::dojo;
    let jc = resolver::JobConfig::load();
    let store = dojo::DojoStore::from_config(cfg);
    let Some(cards) = dojo_store_read(store.load_cards()) else {
        return;
    };
    let Some(states) = dojo_store_read(store.review_states()) else {
        return;
    };
    let now = bn_core::util::now_epoch_secs();

    // Card + due counts per repo.
    let mut counts: std::collections::BTreeMap<String, (u32, u32)> =
        std::collections::BTreeMap::new();
    for c in &cards {
        let e = counts.entry(c.repo.clone()).or_insert((0, 0));
        e.0 += 1;
        if states.get(&c.id).map(|s| s.due <= now).unwrap_or(true) {
            e.1 += 1;
        }
    }

    if as_json {
        let repos: Vec<serde_json::Value> = counts
            .iter()
            .map(|(repo, (n, due))| {
                serde_json::json!({
                    "repo": repo,
                    "cards": n,
                    "due": due,
                    "in_rotation": jc.dojo_repos.is_empty() || jc.dojo_repos.contains(repo),
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success(
                "dojo",
                serde_json::json!({
                    "enabled": jc.dojo_enabled,
                    "repos": jc.dojo_repos,
                    "daily_target": jc.dojo_daily_target,
                    "card_repos": repos,
                }),
            )
            .to_json()
        );
        return;
    }

    println!("🥋 dojo config");
    println!(
        "  mining cron: {}",
        if jc.dojo_enabled {
            "enabled".to_string()
        } else {
            "disabled — set [dojo] enabled = true (or $BN_DOJO_ENABLED=1)".to_string()
        }
    );
    if jc.dojo_repos.is_empty() {
        println!("  rotation:    all repos with cards (allowlist an [dojo] repos to pin it)");
    } else {
        println!("  rotation:    {}", jc.dojo_repos.join(", "));
    }
    println!(
        "  daily target: {} card(s) per session",
        jc.dojo_daily_target
    );
    if counts.is_empty() {
        println!("  cards:       none yet — run `bn run dojo-cards` to mine some");
    } else {
        println!("  cards:");
        for (repo, (n, due)) in &counts {
            let rot = if jc.dojo_repos.is_empty() || jc.dojo_repos.contains(repo) {
                ""
            } else {
                " (not in rotation)"
            };
            println!("    {repo}: {n} card(s), {due} due{rot}");
        }
    }
}
