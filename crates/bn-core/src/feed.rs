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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir;

    #[test]
    fn ingest_then_read_roundtrips() {
        with_state_dir(|| {
            let items = vec![FeedItem {
                key: "pr-1".into(),
                text: "Fix areaPath — Steph".into(),
                url: "https://example/pr/1".into(),
            }];
            ingest("ado-reviews", items.clone()).unwrap();
            let all = read_all();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].source, "ado-reviews");
            assert_eq!(all[0].items, items);
        });
    }

    #[test]
    fn ingest_replaces_rather_than_appends() {
        with_state_dir(|| {
            ingest(
                "s",
                vec![FeedItem {
                    key: "a".into(),
                    text: "old".into(),
                    url: String::new(),
                }],
            )
            .unwrap();
            ingest(
                "s",
                vec![FeedItem {
                    key: "b".into(),
                    text: "new".into(),
                    url: String::new(),
                }],
            )
            .unwrap();
            let all = read_all();
            assert_eq!(
                all[0].items.len(),
                1,
                "a re-run mirrors current state, not history"
            );
            assert_eq!(all[0].items[0].text, "new");
        });
    }

    #[test]
    fn read_all_sorts_sources() {
        with_state_dir(|| {
            ingest("meetings", vec![]).unwrap();
            ingest("ado-reviews", vec![]).unwrap();
            let names: Vec<_> = read_all().into_iter().map(|s| s.source).collect();
            assert_eq!(names, vec!["ado-reviews", "meetings"]);
        });
    }

    #[test]
    fn parse_items_accepts_array_and_jsonl() {
        let arr = r#"[{"key":"a","text":"A","url":"u"},{"key":"b","text":"B"}]"#;
        assert_eq!(parse_items(arr).len(), 2);
        let jsonl = "{\"key\":\"a\",\"text\":\"A\"}\n\n{\"text\":\"B\",\"url\":\"u\"}\n";
        let items = parse_items(jsonl);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].text, "B");
    }

    #[test]
    fn parse_items_skips_garbage_lines() {
        let mixed = "fetching...\n{\"text\":\"real\"}\nnot json\n";
        let items = parse_items(mixed);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "real");
    }

    fn item(key: &str, text: &str) -> FeedItem {
        FeedItem {
            key: key.into(),
            text: text.into(),
            url: format!("https://example/pr/{key}"),
        }
    }

    #[test]
    fn mirror_raises_then_reconciles_gone_items() {
        with_state_dir(|| {
            // First run: two PRs surface as warn attention items.
            let first = vec![item("pr-1", "Fix A"), item("pr-2", "Fix B")];
            let (raised, acked) =
                mirror_to_attention("ado-reviews", &first, "warn", "2026-06-28").unwrap();
            assert_eq!((raised, acked), (2, 0));
            let open = crate::attention::open_items("2026-06-28");
            assert_eq!(open.len(), 2);
            assert!(open.iter().all(|i| i.severity == "warn"));
            assert!(open.iter().all(|i| i.source == "ado-reviews"));

            // Second run: pr-1 still open (idempotent, no dup), pr-2 merged (gone → acked),
            // pr-3 new.
            let second = vec![item("pr-1", "Fix A"), item("pr-3", "Fix C")];
            let (raised, acked) =
                mirror_to_attention("ado-reviews", &second, "warn", "2026-06-28").unwrap();
            assert_eq!((raised, acked), (1, 1));
            let keys: Vec<_> = crate::attention::open_items("2026-06-28")
                .into_iter()
                .filter_map(|i| i.key)
                .collect();
            assert_eq!(keys, vec!["feed:ado-reviews:pr-1", "feed:ado-reviews:pr-3"]);
        });
    }

    #[test]
    fn mirror_only_touches_its_own_source() {
        with_state_dir(|| {
            // A manual item and another source's mirror must survive reconciliation.
            crate::attention::add(
                "manual",
                "by hand",
                "urgent",
                None,
                None,
                None,
                None,
                "2026-06-28",
            )
            .unwrap();
            mirror_to_attention("meetings", &[item("m-1", "Standup")], "info", "2026-06-28")
                .unwrap();
            // ado-reviews ingests an empty set: nothing of its own to ack, others untouched.
            let (raised, acked) =
                mirror_to_attention("ado-reviews", &[], "warn", "2026-06-28").unwrap();
            assert_eq!((raised, acked), (0, 0));
            assert_eq!(crate::attention::open_items("2026-06-28").len(), 2);
        });
    }

    #[test]
    fn mirror_skips_keyless_items() {
        with_state_dir(|| {
            let items = vec![FeedItem {
                key: String::new(),
                text: "no key".into(),
                url: String::new(),
            }];
            let (raised, acked) = mirror_to_attention("s", &items, "warn", "2026-06-28").unwrap();
            assert_eq!((raised, acked), (0, 0));
            assert!(crate::attention::open_items("2026-06-28").is_empty());
        });
    }

    #[test]
    fn sanitize_source_blocks_path_escape() {
        assert_eq!(sanitize_source("../etc/passwd"), "---etc-passwd");
        assert_eq!(sanitize_source("ado-reviews"), "ado-reviews");
    }
}
