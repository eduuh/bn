//! `bn reviews`/`bn feed` — review queue + activity feed.
use crate::*;

pub(crate) fn emit_reviews(cfg: &bn_core::resolver::Config, as_json: bool) {
    use bn_core::clock::Clock;
    use bn_core::reviews::SlaState;
    let cache = bn_core::reviews::read();
    if as_json {
        println!(
            "{}",
            Envelope::success("reviews", payload::reviews_data(&cache)).to_json()
        );
        return;
    }
    if cache.prs.is_empty() {
        println!("No PRs in the review queue — run `bn run pr-queue` to populate it");
        return;
    }
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let sla = cfg.sla_first_review_hours;
    let me = bn_core::reviews::my_login();
    // review PRs first, then learning, each newest-updated first
    let mut prs: Vec<&bn_core::reviews::ReviewPr> = cache.prs.iter().collect();
    prs.sort_by(|a, b| a.tag.cmp(&b.tag).then(b.updated_at.cmp(&a.updated_at)));
    for p in prs {
        // Mark review-tagged PRs that aren't actually awaiting you, so they don't read as work.
        let mark = if p.tag == "review" && !p.awaiting_review(me.as_deref()) {
            if me
                .as_deref()
                .map(|m| p.author.eq_ignore_ascii_case(m))
                .unwrap_or(false)
            {
                "  (yours)"
            } else if p.acted() {
                "  (you reviewed)"
            } else {
                ""
            }
        } else {
            ""
        };
        println!("[{}] {}#{} @{}{mark}", p.tag, p.repo, p.number, p.author);
        println!("  {}", p.title);
        // Wait time + SLA state only for PRs genuinely awaiting your review.
        if p.awaiting_review(me.as_deref()) {
            if let (Some(w), Some(state)) = (p.wait_hours(now), p.sla_state(now, sla)) {
                let glyph = match state {
                    SlaState::Over => "over",
                    SlaState::Near => "near",
                    SlaState::Ok => "ok",
                };
                let glyph = format!("{} {glyph}", bn_core::glyph::CLOCK);
                println!("  {glyph} · waited {} · SLA {}", dur(w), dur(sla));
            }
        }
        println!("  {}", p.url);
    }
    if !cache.generated_at.is_empty() {
        println!("(cached {})", cache.generated_at);
    }
}

/// `bn feed` — render every feed source (informational-cron inbox), grouped by source with a
/// staleness hint and each item's link.
pub(crate) fn emit_feed(as_json: bool) {
    let sources = bn_core::feed::read_all();
    // The event-driven half of the inbox: agent completions awaiting review. Read from the
    // attention event log (source of truth), not the polled feed store — a review VIEW, not a
    // second store. Kept out of the actionable attention list; surfaced here to be reviewed.
    let completed = bn_core::attention::completed_items(&today());
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "feed",
                serde_json::json!({ "sources": sources, "completed": completed })
            )
            .to_json()
        );
        return;
    }
    if sources.is_empty() && completed.is_empty() {
        println!("Feed empty — feed crons populate it (e.g. `bn run ado-reviews`)");
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    for (i, src) in sources.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "{}  ({}, updated {} ago)",
            src.source,
            src.items.len(),
            ago((now - src.generated_at).max(0))
        );
        for item in &src.items {
            println!("  ▸ {}", item.text);
            if !item.url.is_empty() {
                println!("      ↗ {}", item.url);
            }
        }
    }
    if !completed.is_empty() {
        if !sources.is_empty() {
            println!();
        }
        println!("completed · awaiting review  ({})", completed.len());
        for item in &completed {
            println!("  {} {}  [{}]", bn_core::glyph::CHECK, item.title, item.id);
            if let Some(link) = item.link.as_deref().filter(|l| !l.is_empty()) {
                println!("      ↗ {link}");
            }
        }
        println!("  ↳ review, then `bn attn ack <id>` to clear");
    }
}

/// `bn feed ingest <source>` — replace a source's items from JSON on stdin. The write path feed
/// cron scripts pipe into; prints how many items landed so a `bn run` window shows the result.
/// With `--attn`, also mirrors the items onto the attention list (reconciling against current
/// state) so review-queue findings surface in `bn next`/doctor, not just the passive feed.
pub(crate) fn feed_ingest(source: &str, attn: bool, severity: &str) {
    use std::io::Read;
    if attn && !bn_core::attention::SEVERITIES.contains(&severity) {
        eprintln!(
            "bn feed ingest: invalid --severity '{severity}' (want {})",
            bn_core::attention::SEVERITIES.join(" | ")
        );
        std::process::exit(2);
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("bn feed ingest: failed to read stdin");
        std::process::exit(1);
    }
    let items = bn_core::feed::parse_items(&input);
    let n = items.len();
    if let Err(e) = bn_core::feed::ingest(source, items.clone()) {
        eprintln!("bn feed ingest: {e}");
        std::process::exit(1);
    }
    println!("feed '{source}': {n} item{}", if n == 1 { "" } else { "s" });
    if attn {
        match bn_core::feed::mirror_to_attention(source, &items, severity, &today()) {
            Ok((raised, acked)) => {
                println!("attention '{source}': +{raised} raised, {acked} resolved");
            }
            Err(e) => {
                eprintln!("bn feed ingest: attention mirror failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// `bn feed clear <source>` — forget a source.
pub(crate) fn feed_clear(source: &str) {
    match bn_core::feed::clear(source) {
        Ok(()) => println!("feed '{source}': cleared"),
        Err(e) => {
            eprintln!("bn feed clear: {e}");
            std::process::exit(1);
        }
    }
}

/// `bn feed rate <card> up|down [--note "…"]` — react to a staff briefing card. Resolves `<card>`
/// (agent name → newest card, a YYYY-MM-DD date, or an exact <date>/<agent>), stores the rating
/// beside the read-marks (marking the card read), and confirms. The rating feeds the `bn insights`
/// staff performance review; a down-vote note also accrues as prompt-tuning material.
pub(crate) fn feed_rate(
    cfg: &bn_core::resolver::Config,
    card: &str,
    rating: &str,
    note: Option<&str>,
    as_json: bool,
) {
    use bn_core::staff;
    let Some(reaction) = staff::Rating::parse(rating) else {
        eprintln!("bn feed rate: invalid rating '{rating}' (want up | down)");
        std::process::exit(2);
    };
    let target = match staff::resolve_card(cfg, card) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bn feed rate: {e}");
            std::process::exit(1);
        }
    };
    let rec = match staff::rate_card(&target.date, &target.agent, reaction, note.unwrap_or("")) {
        Ok(rec) => rec,
        Err(e) => {
            eprintln!("bn feed rate: {e}");
            std::process::exit(1);
        }
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "feed_rate",
                serde_json::json!({
                    "agent": target.agent,
                    "date": target.date,
                    "rating": reaction.as_str(),
                    "note": rec.note,
                    "headline": target.headline,
                }),
            )
            .to_json()
        );
        return;
    }
    let glyph = match reaction {
        staff::Rating::Up => "▲",
        staff::Rating::Down => "▼",
    };
    println!(
        "{glyph} {} · {}  \"{}\"",
        target.agent, target.date, target.headline
    );
    if !rec.note.is_empty() {
        println!("  note: {}", rec.note);
    }
}
