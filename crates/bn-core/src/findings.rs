//! The cron findings store (#576): crons persist the information they *gather*, not just log it.
//!
//! Where [`crate::feed`] mirrors the *current* state of a source (each run REPLACES the source's
//! items, so merged PRs / past meetings drop off), the findings store keeps *history*: every run
//! appends one durable, queryable record — `{source, run_id, gathered_at, items, raw?}`. A cron is
//! a script OR a headless agent; its sources are APIs or MCPs. Script crons pipe JSON into
//! `bn findings record <source>` (same `{key,text,url}` item shape as `feed ingest`); agents call
//! the same path. Findings are queryable by source and date via `bn findings query` and the web
//! `GET /api/findings`, so "Today's agent" (#577) and the dashboard can read them as an input
//! packet.
//!
//! Each source owns one append-only JSONL file under `state_dir()/findings/<source>.jsonl`, one
//! run per line — the same torn-write-tolerant substrate as the event log: a half-written line is
//! skipped on read, never an error. Nothing here touches the network.

use crate::feed::{sanitize_source, FeedItem};
use crate::resolver::state_dir;
use crate::util::{local_date_of, now_epoch_secs};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// One run's findings: what a cron gathered, when, from where. `items` reuses the feed
/// `{key,text,url}` shape; `raw` optionally carries the untransformed payload (e.g. the API
/// response) for later re-parsing without a re-fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindingsRun {
    pub source: String,
    /// Stable identity for this run within its source. Auto-assigned when not supplied.
    pub run_id: String,
    /// When the run gathered these findings (epoch seconds).
    pub gathered_at: i64,
    #[serde(default)]
    pub items: Vec<FeedItem>,
    /// The untransformed payload, if the caller chose to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

fn findings_dir() -> PathBuf {
    state_dir().join("findings")
}

fn source_path(source: &str) -> PathBuf {
    findings_dir().join(format!("{}.jsonl", sanitize_source(source)))
}

/// A run_id unique enough to distinguish two runs of the same source, even within one second:
/// epoch-seconds plus the sub-second nanos of the wall clock.
fn next_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{:09}", now_epoch_secs(), nanos)
}

/// Append one run's findings for `source`. `run_id`/`gathered_at`, when `None`, are stamped now.
/// Creates the findings dir on first write. Returns the stored record.
pub fn record(
    source: &str,
    run_id: Option<&str>,
    items: Vec<FeedItem>,
    raw: Option<String>,
) -> std::io::Result<FindingsRun> {
    let rec = FindingsRun {
        source: source.to_string(),
        run_id: run_id.map(str::to_string).unwrap_or_else(next_run_id),
        gathered_at: now_epoch_secs(),
        items,
        raw,
    };
    std::fs::create_dir_all(findings_dir())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(source_path(source))?;
    writeln!(f, "{}", serde_json::to_string(&rec)?)?;
    Ok(rec)
}

/// Every run recorded for one source, in file (chronological) order. Missing file → empty;
/// unparseable lines are skipped so a torn append never poisons the read.
pub fn read_source(source: &str) -> Vec<FindingsRun> {
    let Ok(file) = std::fs::File::open(source_path(source)) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<FindingsRun>(&l).ok())
        .collect()
}

/// Every source with a findings file on disk, sorted by source name.
pub fn sources() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(findings_dir()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Query recorded findings, newest-first. Filters:
/// - `source`: restrict to one source (else every source is unioned).
/// - `since_date`: only runs whose local-date `gathered_at` is on/after this `YYYY-MM-DD`.
/// - `limit`: most-recent-N cap (0 = uncapped).
pub fn query(source: Option<&str>, since_date: Option<&str>, limit: usize) -> Vec<FindingsRun> {
    let mut runs: Vec<FindingsRun> = match source {
        Some(s) => read_source(s),
        None => sources().iter().flat_map(|s| read_source(s)).collect(),
    };
    if let Some(since) = since_date {
        runs.retain(|r| local_date_of(r.gathered_at).as_str() >= since);
    }
    runs.sort_by(|a, b| {
        b.gathered_at
            .cmp(&a.gathered_at)
            .then(b.run_id.cmp(&a.run_id))
    });
    if limit > 0 {
        runs.truncate(limit);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir;

    fn item(key: &str, text: &str) -> FeedItem {
        FeedItem {
            key: key.into(),
            text: text.into(),
            url: format!("https://example/{key}"),
        }
    }

    #[test]
    fn record_then_read_roundtrips() {
        with_state_dir(|| {
            let stored = record(
                "gh-prs",
                Some("run-1"),
                vec![item("pr-1", "Fix A")],
                Some("{\"raw\":true}".into()),
            )
            .unwrap();
            assert_eq!(stored.run_id, "run-1");
            let runs = read_source("gh-prs");
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0], stored);
            assert_eq!(runs[0].items[0].text, "Fix A");
            assert_eq!(runs[0].raw.as_deref(), Some("{\"raw\":true}"));
        });
    }

    #[test]
    fn record_appends_history_rather_than_replacing() {
        with_state_dir(|| {
            record("s", Some("a"), vec![item("1", "old")], None).unwrap();
            record("s", Some("b"), vec![item("2", "new")], None).unwrap();
            let runs = read_source("s");
            assert_eq!(
                runs.len(),
                2,
                "each run is kept — the store is history, not a mirror"
            );
            assert_eq!(runs[0].run_id, "a");
            assert_eq!(runs[1].run_id, "b");
        });
    }

    #[test]
    fn auto_run_id_is_assigned_when_absent() {
        with_state_dir(|| {
            let stored = record("s", None, vec![], None).unwrap();
            assert!(!stored.run_id.is_empty());
        });
    }

    #[test]
    fn query_filters_by_source() {
        with_state_dir(|| {
            record("a", Some("a1"), vec![], None).unwrap();
            record("b", Some("b1"), vec![], None).unwrap();
            let only_a = query(Some("a"), None, 0);
            assert_eq!(only_a.len(), 1);
            assert_eq!(only_a[0].source, "a");
            assert_eq!(query(None, None, 0).len(), 2, "no source → union of all");
        });
    }

    #[test]
    fn query_filters_by_since_date_and_orders_newest_first() {
        with_state_dir(|| {
            // Three runs on distinct days by stepping BN_NOW under one guard (env_lock is held by
            // `with_state_dir`).
            let now = crate::test_support::EnvGuard::set("BN_NOW", "1000000"); // 1970-01-12
            record("s", Some("old"), vec![], None).unwrap();
            now.set_value("1700000000"); // 2023-11-14
            record("s", Some("mid"), vec![], None).unwrap();
            now.set_value("1800000000"); // 2027-01-15
            record("s", Some("new"), vec![], None).unwrap();
            drop(now);

            let recent = query(Some("s"), Some("2023-01-01"), 0);
            let ids: Vec<_> = recent.into_iter().map(|r| r.run_id).collect();
            assert_eq!(
                ids,
                vec!["new", "mid"],
                "dropped the 1970 run, newest first"
            );
        });
    }

    #[test]
    fn query_respects_limit() {
        with_state_dir(|| {
            for i in 0..5 {
                record("s", Some(&format!("r{i}")), vec![], None).unwrap();
            }
            assert_eq!(query(Some("s"), None, 2).len(), 2);
            assert_eq!(query(Some("s"), None, 0).len(), 5, "0 = uncapped");
        });
    }

    #[test]
    fn sources_lists_and_sorts() {
        with_state_dir(|| {
            record("zulu", None, vec![], None).unwrap();
            record("alpha", None, vec![], None).unwrap();
            assert_eq!(sources(), vec!["alpha", "zulu"]);
        });
    }

    #[test]
    fn read_source_skips_torn_lines() {
        with_state_dir(|| {
            record("s", Some("good"), vec![], None).unwrap();
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(source_path("s"))
                .unwrap();
            writeln!(f, "not json").unwrap();
            drop(f);
            let runs = read_source("s");
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].run_id, "good");
        });
    }
}
