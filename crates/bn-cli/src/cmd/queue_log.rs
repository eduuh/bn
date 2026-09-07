//! `bn queue log` — what the dispatcher has been doing.
//!
//! The terminal rendering of [`bn_core::queue::log`]. The fold, the kind table and the one-line
//! summaries all live in bn-core, so this surface and the dashboard's cannot disagree about what
//! the dispatcher did — which would be a worse failure than either being missing.
use crate::*;
use bn_core::queue::log::{self, Query, Row, Tone};

/// `bn queue log` — the dispatcher's recent history, oldest first so it reads as a narrative.
pub(crate) fn emit_log(
    cfg: &Config,
    since: &str,
    limit: usize,
    kind: Option<&str>,
    repo: Option<&str>,
    now: i64,
    json: bool,
) {
    let Some(window) = bn_core::focus::parse_duration(since) else {
        eprintln!("bn queue log: could not read --since '{since}' (try 30m, 4h, 2d)");
        return;
    };
    if let Some(k) = kind {
        if !log::is_kind(k) {
            eprintln!(
                "bn queue log: unknown --kind '{k}' (one of: {})",
                log::kind_names().join(", ")
            );
            return;
        }
    }
    let rows: Vec<Row> = log::read(&Query {
        since: now.saturating_sub(window),
        limit,
        kind,
        repo,
    })
    .into_iter()
    .filter(|row| {
        row.repo.is_empty()
            || row.repo.eq_ignore_ascii_case("machine")
            || cfg.repo_allowed_here(&row.repo)
    })
    .collect();

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-log",
                serde_json::json!({ "since": now - window, "now": now, "events": rows }),
            )
            .to_json()
        );
        return;
    }
    if rows.is_empty() {
        println!("no queue activity in the last {since}");
        // A silent dispatcher and an absent one look identical here, so say which this is.
        if !bn_core::resolver::dispatcher_alive(now) {
            println!("  (and no dispatcher is running — `bn queue status` for why)");
        }
        return;
    }
    println!(
        "\u{f017} dispatcher log — last {since} ({} events)",
        rows.len()
    );
    for r in &rows {
        println!(
            "  {}  {:<10} {}",
            bn_core::util::clock_hm(r.at),
            r.kind,
            line(r)
        );
    }
}

/// `<repo>/<id> · summary`, with the subject omitted when the event has none (a tick is about the
/// machine, not about one entry).
fn line(r: &Row) -> String {
    let mark = match r.tone {
        Tone::Bad => "\u{f071} ",
        _ => "",
    };
    if r.id.is_empty() {
        return format!("{mark}{}", r.summary);
    }
    format!("{mark}{}/{} · {}", r.repo, r.id, r.summary)
}
