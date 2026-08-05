//! Today's agent (#577): synthesize the cron findings store (#576) into a reviewable list of
//! suggested tasks. It generalizes the `bn staff` / `bn chief` pattern — gather a deterministic
//! packet, produce a validated output, route it — but its packet is the cron findings and its
//! output is *suggested tasks* (written to the [`crate::suggestions`] store, reviewed on Today;
//! promotion to a real Command Center item is #578).
//!
//! The synthesis here is deterministic and offline: each recent finding item becomes one proposal,
//! carrying its source finding (source + key + url) and a plain-language rationale. That keeps the
//! agent verifiable and cheap enough to run on a cron without an LLM round-trip, while the packet
//! [`gather_packet_json`] builds is exactly what a headless-agent variant would consume later.
//! Re-runs are idempotent via the suggestions store's id dedupe, so a scheduled run over unchanged
//! findings adds nothing.

use crate::findings::{self, FindingsRun};
use crate::suggestions::{self, Suggestion};
use crate::util::{local_date_of, now_epoch_secs};
use serde::Serialize;

/// Default number of days of findings the agent looks back over.
pub const DEFAULT_LOOKBACK_DAYS: i64 = 7;
/// Default cap on suggestions synthesized from any one source's latest run (avoids one chatty
/// cron flooding the review list).
pub const DEFAULT_PER_SOURCE: usize = 5;

/// What one Today's-agent run did.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunReport {
    /// Distinct findings sources the packet drew from.
    pub sources: usize,
    /// Finding items considered across those sources.
    pub items: usize,
    /// Newly written suggestions (existing/dismissed ones are skipped — see the store's dedupe).
    pub added: usize,
}

/// The `since` local-date bound for a `lookback_days` window ending today (`today` is `YYYY-MM-DD`).
fn since_date(today: &str, lookback_days: i64) -> String {
    match crate::util::epoch_day(today) {
        Some(d) => {
            let start = (d - lookback_days.max(0)).max(0);
            crate::util::date_of(start * 86_400)
        }
        None => today.to_string(),
    }
}

/// The latest recorded run per source within the lookback window, newest source first. This is the
/// deterministic packet Today's agent reasons over: one representative (most recent) run per source
/// so re-synthesis tracks the *current* findings rather than the full append history.
pub fn gather(today: &str, lookback_days: i64) -> Vec<FindingsRun> {
    let since = since_date(today, lookback_days);
    let mut latest: std::collections::HashMap<String, FindingsRun> =
        std::collections::HashMap::new();
    for run in findings::query(None, Some(&since), 0) {
        latest
            .entry(run.source.clone())
            .and_modify(|cur| {
                if run.gathered_at > cur.gathered_at {
                    *cur = run.clone();
                }
            })
            .or_insert(run);
    }
    let mut runs: Vec<FindingsRun> = latest.into_values().collect();
    runs.sort_by(|a, b| {
        b.gathered_at
            .cmp(&a.gathered_at)
            .then(a.source.cmp(&b.source))
    });
    runs
}

/// The deterministic input packet as JSON — the same shape a headless-agent variant would be fed:
/// `{generated_at, date, sources:[{source, run_id, gathered_at, items:[{key,text,url}]}]}`.
pub fn gather_packet_json(today: &str, lookback_days: i64) -> String {
    let runs = gather(today, lookback_days);
    let packet = serde_json::json!({
        "generated_at": now_epoch_secs(),
        "date": today,
        "sources": runs.iter().map(|r| serde_json::json!({
            "source": r.source,
            "run_id": r.run_id,
            "gathered_at": r.gathered_at,
            "items": r.items,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".to_string())
}

/// Deterministically turn the gathered runs into proposed tasks: each finding item → one
/// suggestion (title = the finding headline, rationale = which cron surfaced it and when), capped
/// at `per_source` per source. Items with an empty key fall back to their index so the id stays
/// stable for that source's slot.
pub fn synthesize(runs: &[FindingsRun], per_source: usize) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for run in runs {
        let date = local_date_of(run.gathered_at);
        for (i, item) in run.items.iter().enumerate() {
            if per_source > 0 && i >= per_source {
                break;
            }
            let text = item.text.trim();
            if text.is_empty() {
                continue;
            }
            let key = if item.key.trim().is_empty() {
                format!("i{i}")
            } else {
                item.key.trim().to_string()
            };
            let rationale = format!(
                "Surfaced by the '{}' cron on {} — review whether this is worth acting on.",
                run.source, date
            );
            out.push(Suggestion::new(
                &run.source,
                &key,
                text,
                rationale,
                &item.url,
            ));
        }
    }
    out
}

/// The full pipeline shared by `bn today` and the `today-agent` cron: gather the findings packet →
/// synthesize proposals → merge them into the suggestions store (idempotently). Returns a summary
/// of what it did. Fully offline; a store write error propagates so a cron surfaces it.
pub fn run(today: &str, lookback_days: i64, per_source: usize) -> std::io::Result<RunReport> {
    let runs = gather(today, lookback_days);
    let fresh = synthesize(&runs, per_source);
    let items = runs.iter().map(|r| r.items.len()).sum();
    let added = suggestions::upsert(fresh)?;
    Ok(RunReport {
        sources: runs.len(),
        items,
        added,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedItem;
    use crate::test_support::{with_state_dir, EnvGuard};

    fn item(key: &str, text: &str) -> FeedItem {
        FeedItem {
            key: key.into(),
            text: text.into(),
            url: format!("https://x/{key}"),
        }
    }

    const TODAY: &str = "2026-07-16";

    #[test]
    fn run_synthesizes_suggestions_from_findings() {
        with_state_dir(|| {
            findings::record(
                "gh-prs",
                Some("r1"),
                vec![item("pr-1", "Review PR 1")],
                None,
            )
            .unwrap();
            findings::record(
                "issues",
                Some("r2"),
                vec![item("i-9", "Triage issue 9")],
                None,
            )
            .unwrap();
            let report = run(TODAY, DEFAULT_LOOKBACK_DAYS, DEFAULT_PER_SOURCE).unwrap();
            assert_eq!(report.sources, 2);
            assert_eq!(report.items, 2);
            assert_eq!(report.added, 2);
            let stored = suggestions::list(None, 0);
            assert_eq!(stored.len(), 2);
            assert!(stored
                .iter()
                .any(|suggestion| suggestion.title == "Review PR 1"));
            assert!(stored.iter().all(|s| !s.rationale.is_empty()));
        });
    }

    #[test]
    fn run_is_idempotent_over_unchanged_findings() {
        with_state_dir(|| {
            findings::record("s", Some("r1"), vec![item("k", "Do the thing")], None).unwrap();
            assert_eq!(run(TODAY, 7, 5).unwrap().added, 1);
            // Same latest run → same ids → nothing new.
            assert_eq!(run(TODAY, 7, 5).unwrap().added, 0);
        });
    }

    #[test]
    fn gather_takes_only_the_latest_run_per_source() {
        with_state_dir(|| {
            let g = EnvGuard::set("BN_NOW", "1700000000");
            findings::record("s", Some("old"), vec![item("a", "old finding")], None).unwrap();
            g.set_value("1700086400");
            findings::record("s", Some("new"), vec![item("b", "new finding")], None).unwrap();
            drop(g);
            let runs = gather(TODAY, 3650);
            assert_eq!(runs.len(), 1, "one representative run per source");
            assert_eq!(runs[0].run_id, "new");
        });
    }

    #[test]
    fn synthesize_caps_per_source_and_skips_empty_text() {
        let run = FindingsRun {
            source: "s".into(),
            run_id: "r".into(),
            gathered_at: now_epoch_secs(),
            items: vec![
                item("a", "one"),
                item("b", ""),
                item("c", "three"),
                item("d", "four"),
            ],
            raw: None,
        };
        let out = synthesize(std::slice::from_ref(&run), 2);
        // per_source=2 → only the first two slots considered; the empty one is dropped.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "one");
    }
}
