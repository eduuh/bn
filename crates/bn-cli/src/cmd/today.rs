//! `bn today` — Today's agent (#577): synthesize the cron findings store into reviewable
//! suggested tasks, and review them. The deterministic pipeline lives in
//! [`bn_core::today_agent`]; this is the thin CLI surface (run / list / accept / dismiss).
use crate::*;
use bn_core::suggestions::{self, Suggestion, SuggestionStatus};
use bn_core::today_agent;

/// Serialize a suggestion for the `--json` surfaces (matches the web `/api/suggestions` shape).
fn suggestion_json(s: &Suggestion) -> serde_json::Value {
    serde_json::json!({
        "id": s.id,
        "title": s.title,
        "rationale": s.rationale,
        "source": s.source,
        "source_key": s.source_key,
        "url": s.url,
        "created_at": s.created_at,
        "status": s.status.as_str(),
    })
}

fn print_suggestion(s: &Suggestion) {
    println!("  [{}] {}  ({})", s.status.as_str(), s.title, s.id);
    if !s.rationale.is_empty() {
        println!("      {}", s.rationale);
    }
    if !s.url.is_empty() {
        println!("      ↗ {}", s.url);
    }
}

/// `bn today run` (and bare `bn today`) — the manual trigger. Runs the agent, then reports what it
/// added and lists the currently open (suggested) proposals.
pub(crate) fn today_run(lookback_days: Option<i64>, per_source: Option<usize>, as_json: bool) {
    let lookback = lookback_days.unwrap_or(today_agent::DEFAULT_LOOKBACK_DAYS);
    let per_source = per_source.unwrap_or(today_agent::DEFAULT_PER_SOURCE);
    let report = match today_agent::run(&today(), lookback, per_source) {
        Ok(r) => r,
        Err(e) => {
            fail_json(as_json, "today", "run_failed", &format!("bn today: {e}"));
        }
    };
    let open = suggestions::list(Some(SuggestionStatus::Suggested), 0);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today",
                serde_json::json!({
                    "report": report,
                    "suggestions": open.iter().map(suggestion_json).collect::<Vec<_>>(),
                })
            )
            .to_json()
        );
        return;
    }
    println!(
        "Today's agent: {} source{}, {} finding{} → {} new suggestion{}",
        report.sources,
        if report.sources == 1 { "" } else { "s" },
        report.items,
        if report.items == 1 { "" } else { "s" },
        report.added,
        if report.added == 1 { "" } else { "s" },
    );
    if open.is_empty() {
        println!("No open suggestions — record cron findings (`bn findings record`) to seed them.");
        return;
    }
    println!("\nSuggested tasks ({} open):", open.len());
    for s in &open {
        print_suggestion(s);
    }
}

/// `bn today list [--status] [--limit]` — read the suggestions store back.
pub(crate) fn today_list(status: Option<&str>, limit: usize, as_json: bool) {
    let filter = match status {
        Some(s) => match SuggestionStatus::parse(s) {
            Some(st) => Some(st),
            None => fail_json(
                as_json,
                "today",
                "bad_status",
                &format!("bn today list: unknown status '{s}' (use suggested|accepted|dismissed)"),
            ),
        },
        None => None,
    };
    let items = suggestions::list(filter, limit);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-list",
                serde_json::json!({
                    "count": items.len(),
                    "suggestions": items.iter().map(suggestion_json).collect::<Vec<_>>(),
                })
            )
            .to_json()
        );
        return;
    }
    if items.is_empty() {
        println!("No suggestions — run `bn today` to synthesize them from the cron findings.");
        return;
    }
    for s in &items {
        print_suggestion(s);
    }
}

/// `today-agent` cron (#577) — the scheduled trigger. Opt-in via `[today].enabled`; a clean no-op
/// when off (a declined gate isn't a failure). Runs the same offline pipeline as `bn today`.
pub(crate) fn job_today_agent(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().today_enabled {
        println!("today-agent: Today's agent disabled — set [today].enabled to opt in");
        return 0;
    }
    match today_agent::run(
        &today(),
        today_agent::DEFAULT_LOOKBACK_DAYS,
        today_agent::DEFAULT_PER_SOURCE,
    ) {
        Ok(r) => {
            println!(
                "today-agent: {} source(s), {} finding(s) → {} new suggestion(s)",
                r.sources, r.items, r.added
            );
            0
        }
        Err(e) => {
            eprintln!("today-agent: {e}");
            1
        }
    }
}

/// `bn today accept|dismiss <id>` — move one suggestion in its review lifecycle.
pub(crate) fn today_set_status(id: &str, status: SuggestionStatus, as_json: bool) {
    match suggestions::set_status(id, status) {
        Ok(true) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "today-status",
                        serde_json::json!({ "id": id, "status": status.as_str() })
                    )
                    .to_json()
                );
            } else {
                println!("suggestion {id}: {}", status.as_str());
            }
        }
        Ok(false) => fail_json(
            as_json,
            "today",
            "not_found",
            &format!("bn today: no suggestion '{id}'"),
        ),
        Err(e) => fail_json(as_json, "today", "write_failed", &format!("bn today: {e}")),
    }
}
