//! The feed: an inbox for *informational crons* — jobs that don't *do* work but *surface* it.
//! A feed job fetches facts (open PRs to review, today's meetings) and writes them as items with
//! links; `bn feed` renders them grouped by source. Each source (one cron) owns a file under
//! `state_dir()/feed/<source>.json`, and a run REPLACES that file wholesale — so the feed mirrors
//! the current state, and merged PRs / past meetings drop off on the next run instead of piling
//! up. North star, same as the review queue: the script fetches, bn renders. Jobs only ever write
//! through `bn feed ingest <source>`; nothing here touches the network.

use crate::resolver::state_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One surfaced item: a line of text and an optional link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedItem {
    /// Stable identity within its source (e.g. `pr-5377937`). Lets a human see at a glance that an
    /// item is the same one as yesterday; not used for storage (a run replaces the whole source).
    #[serde(default)]
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub url: String,
}

/// One source's items plus when the job last wrote them (epoch seconds), for staleness display.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedSource {
    pub source: String,
    #[serde(default)]
    pub generated_at: i64,
    #[serde(default)]
    pub items: Vec<FeedItem>,
}

fn feed_dir() -> PathBuf {
    state_dir().join("feed")
}

/// A source name maps to one file. Sources are job names (`ado-reviews`, `meetings`) — keep them
/// to a filename-safe shape so a source can never escape the feed dir.
pub fn sanitize_source(source: &str) -> String {
    source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn source_path(source: &str) -> PathBuf {
    feed_dir().join(format!("{}.json", sanitize_source(source)))
}

/// Replace a source's items wholesale and stamp it now. Creates the feed dir on first write.
pub fn ingest(source: &str, items: Vec<FeedItem>) -> std::io::Result<()> {
    let rec = FeedSource {
        source: source.to_string(),
        generated_at: crate::util::now_epoch_secs(),
        items,
    };
    crate::store::save_json_atomic(&source_path(source), &rec)
}

/// Mirror a source's current items onto the attention list, reconciling against the feed's
/// replace-semantics so the active list tracks the passive inbox: raise a keyed attention item for
/// each current feed item (idempotent via [`crate::attention::add`]), and ack any *open* attention
/// item previously raised from this same source whose feed item has since dropped off (e.g. the PR
/// merged). Items are keyed `feed:<source>:<item.key>` so both halves agree on identity; a feed
/// item with no key is skipped (nothing stable to dedup or reconcile on). Returns
/// `(raised, acked)` — newly-raised items and resolved-because-gone items.
pub fn mirror_to_attention(
    source: &str,
    items: &[FeedItem],
    severity: &str,
    today: &str,
) -> std::io::Result<(usize, usize)> {
    let prefix = format!("feed:{source}:");
    let current: std::collections::HashSet<String> = items
        .iter()
        .filter(|i| !i.key.is_empty())
        .map(|i| format!("{prefix}{}", i.key))
        .collect();

    // Ack open items from this source whose feed item is gone — the auto-cleanup half.
    let mut acked = 0;
    for item in crate::attention::open_items(today) {
        if item.source != source {
            continue;
        }
        let Some(k) = item.key.as_deref() else {
            continue;
        };
        if k.starts_with(&prefix) && !current.contains(k) && crate::attention::ack(&item.id)? {
            acked += 1;
        }
    }

    // Raise (idempotent) one item per current feed item — the surfacing half.
    let mut raised = 0;
    for i in items {
        if i.key.is_empty() {
            continue;
        }
        let key = format!("{prefix}{}", i.key);
        let link = (!i.url.is_empty()).then_some(i.url.as_str());
        let out = crate::attention::add(
            source,
            &i.text,
            severity,
            None,
            None,
            link,
            Some(&key),
            today,
        )?;
        if !out.skipped {
            raised += 1;
        }
    }
    Ok((raised, acked))
}

/// Forget a source entirely (its file is removed). Missing file is fine — already gone.
pub fn clear(source: &str) -> std::io::Result<()> {
    match std::fs::remove_file(source_path(source)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Every source on disk, sorted by source name. Unreadable/unparseable files are skipped (a
/// half-written file just means the next run will fix it), never an error.
pub fn read_all() -> Vec<FeedSource> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(feed_dir()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(src) = serde_json::from_str::<FeedSource>(&text) {
                    out.push(src);
                }
            }
        }
    }
    out.sort_by(|a, b| a.source.cmp(&b.source));
    out
}

/// Parse ingest input from a job script. Accepts either a JSON array of items or JSONL (one item
/// object per line) — both are natural to emit from `jq`. Blank lines and lines that don't parse
/// as an item are skipped, so a stray log line in the pipe doesn't sink the whole batch.
pub fn parse_items(input: &str) -> Vec<FeedItem> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<FeedItem>>(trimmed) {
            return items;
        }
    }
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<FeedItem>(l).ok())
        .collect()
}


