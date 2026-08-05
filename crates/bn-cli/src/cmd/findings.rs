//! `bn findings` — the cron findings store (#576): record + query durable, structured findings.
use crate::*;

/// `bn findings record <source>` — append one run's findings from JSON on stdin. Parses the same
/// `{key,text,url}` items as `feed ingest` (JSON array or JSONL); prints how many items landed so
/// a `bn run` window shows the result.
pub(crate) fn findings_record(source: &str, run_id: Option<&str>, raw: Option<&str>) {
    use std::io::Read;
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("bn findings record: failed to read stdin");
        std::process::exit(1);
    }
    let items = bn_core::feed::parse_items(&input);
    let n = items.len();
    match bn_core::findings::record(source, run_id, items, raw.map(str::to_string)) {
        Ok(rec) => println!(
            "findings '{source}': recorded run {} ({n} item{})",
            rec.run_id,
            if n == 1 { "" } else { "s" }
        ),
        Err(e) => {
            eprintln!("bn findings record: {e}");
            std::process::exit(1);
        }
    }
}

/// `bn findings query [--source] [--since|--date] [--limit]` — recorded findings, newest-first.
pub(crate) fn findings_query(
    source: Option<&str>,
    since: Option<&str>,
    date: Option<&str>,
    limit: usize,
    as_json: bool,
) {
    // `--date` is a single-day window: on/after date AND we drop anything past it below.
    let runs = bn_core::findings::query(source, date.or(since), limit);
    let runs: Vec<_> = match date {
        Some(d) => runs
            .into_iter()
            .filter(|r| bn_core::util::local_date_of(r.gathered_at) == d)
            .collect(),
        None => runs,
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "findings",
                serde_json::json!({ "count": runs.len(), "runs": runs })
            )
            .to_json()
        );
        return;
    }
    if runs.is_empty() {
        println!(
            "No findings — cron runs populate the store (e.g. `… | bn findings record <source>`)"
        );
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    for (i, run) in runs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "{}  run {} ({} item{}, {} ago)",
            run.source,
            run.run_id,
            run.items.len(),
            if run.items.len() == 1 { "" } else { "s" },
            ago((now - run.gathered_at).max(0))
        );
        for item in &run.items {
            println!("  ▸ {}", item.text);
            if !item.url.is_empty() {
                println!("      ↗ {}", item.url);
            }
        }
    }
}

/// `bn findings sources` — the sources that have recorded findings.
pub(crate) fn findings_sources(as_json: bool) {
    let sources = bn_core::findings::sources();
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "findings-sources",
                serde_json::json!({ "sources": sources })
            )
            .to_json()
        );
        return;
    }
    if sources.is_empty() {
        println!("No findings sources yet");
        return;
    }
    for s in sources {
        println!("{s}");
    }
}

/// `bn findings gather` (#897) — snapshot the local morning-planning caches into the findings
/// store: the PR review queue and your own open PRs, the inbound feed (meetings/email/teams/owe),
/// stale/blocked branches, last night's PR-ready dispatches, failing crons, the sentinel patrol,
/// the current quarter's open goals, and mined dojo cards. Additive — records one durable run per
/// non-empty source, reusing the existing caches (no network, no re-fetch); empty sources record
/// nothing.
pub(crate) fn findings_gather(cfg: &Config, as_json: bool) {
    let report = match bn_core::findings_gather::run(cfg) {
        Ok(r) => r,
        Err(e) => fail_json(
            as_json,
            "findings-gather",
            "gather_failed",
            &format!("bn findings gather: {e}"),
        ),
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "findings-gather",
                serde_json::to_value(&report).unwrap_or_default()
            )
            .to_json()
        );
        return;
    }
    println!(
        "findings gather: {} source{} recorded, {} item{}",
        report.recorded(),
        if report.recorded() == 1 { "" } else { "s" },
        report.total_items(),
        if report.total_items() == 1 { "" } else { "s" },
    );
    for s in &report.sources {
        let mark = if s.recorded { "▸" } else { "·" };
        println!(
            "  {mark} [{}] {} — {} item{}",
            s.tier,
            s.source,
            s.items,
            if s.items == 1 { "" } else { "s" }
        );
    }
}
