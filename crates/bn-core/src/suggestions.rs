//! The suggestions store (#577): Today's agent synthesizes the cron findings store (#576) into a
//! reviewable list of **suggested tasks** — proposals, each carrying its source finding and a
//! rationale — for the owner to review. This is *not* yet a Command Center item; promotion to a
//! real work item is #578. The store is the durable, reviewable substrate between the two: the
//! agent writes suggestions here, the owner reviews them on Today (web) or `bn today`, and each
//! carries a `status` so an accepted/dismissed proposal doesn't nag again.
//!
//! Unlike the findings store (append-only history), suggestions are *mutable* — a review marks one
//! accepted or dismissed — so they live in one read-modify-write JSON file under
//! `state_dir()/suggestions.json` rather than an append log. Each suggestion's `id` is derived from
//! its source + source item key, so re-running the agent over unchanged findings is idempotent: an
//! existing suggestion (in any state) is never re-created, so a dismissed proposal stays dismissed.
//! Nothing here touches the network.

use crate::resolver::state_dir;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a suggested task sits in its review lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionStatus {
    /// Freshly synthesized, awaiting review.
    Suggested,
    /// The owner wants to act on it (promotion to a work item is #578).
    Accepted,
    /// The owner declined it — stays recorded so the agent never re-proposes it.
    Dismissed,
}

impl SuggestionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionStatus::Suggested => "suggested",
            SuggestionStatus::Accepted => "accepted",
            SuggestionStatus::Dismissed => "dismissed",
        }
    }

    /// Parse a CLI/API status token; `None` for an unknown word.
    pub fn parse(s: &str) -> Option<SuggestionStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "suggested" => Some(SuggestionStatus::Suggested),
            "accepted" => Some(SuggestionStatus::Accepted),
            "dismissed" => Some(SuggestionStatus::Dismissed),
            _ => None,
        }
    }
}

/// One proposed task: what to do (`title`), why (`rationale`), and where it came from (`source` +
/// `source_key`, plus an optional `url` to the finding). `id` is deterministic on
/// `source`+`source_key` so re-synthesis is idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    /// The findings source this was synthesized from.
    pub source: String,
    /// The finding item's key within that source (the dedupe axis).
    #[serde(default)]
    pub source_key: String,
    #[serde(default)]
    pub url: String,
    /// When this suggestion was first created (epoch seconds).
    pub created_at: i64,
    pub status: SuggestionStatus,
}

impl Suggestion {
    /// Build a fresh `Suggested` proposal, stamping `id` (from source+key) and `created_at` now.
    pub fn new(
        source: &str,
        source_key: &str,
        title: impl Into<String>,
        rationale: impl Into<String>,
        url: impl Into<String>,
    ) -> Suggestion {
        Suggestion {
            id: derive_id(source, source_key),
            title: title.into(),
            rationale: rationale.into(),
            source: source.to_string(),
            source_key: source_key.to_string(),
            url: url.into(),
            created_at: now_epoch_secs(),
            status: SuggestionStatus::Suggested,
        }
    }
}

/// A filename/id-safe token: ASCII alphanumerics, `-`, `_` kept; everything else → `-`. Same shape
/// as [`crate::feed::sanitize_source`], reused so ids stay stable and printable.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.trim_matches('-').to_string()
}

/// The deterministic id for a `(source, source_key)` pair — the dedupe key across re-runs.
pub fn derive_id(source: &str, source_key: &str) -> String {
    let s = sanitize(source);
    let k = sanitize(source_key);
    if k.is_empty() {
        s
    } else {
        format!("{s}-{k}")
    }
}

fn store_path() -> PathBuf {
    state_dir().join("suggestions.json")
}

/// Every suggestion on disk, in stored order. Missing file → empty; a corrupt file reads as empty
/// rather than an error (the agent simply re-synthesizes on the next run).
pub fn read() -> Vec<Suggestion> {
    match std::fs::read_to_string(store_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Persist the whole list (create the state dir on first write).
fn write(list: &[Suggestion]) -> std::io::Result<()> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(list)?)
}

/// Merge freshly synthesized suggestions into the store, keeping existing ones untouched: only a
/// proposal whose `id` isn't already present is appended (in any state — so a dismissed one is
/// never resurrected). Returns how many were newly added.
pub fn upsert(fresh: Vec<Suggestion>) -> std::io::Result<usize> {
    let mut list = read();
    let existing: std::collections::HashSet<String> = list.iter().map(|s| s.id.clone()).collect();
    let mut added = 0;
    for s in fresh {
        if existing.contains(&s.id) {
            continue;
        }
        list.push(s);
        added += 1;
    }
    if added > 0 {
        write(&list)?;
    }
    Ok(added)
}

/// Move one suggestion to a new status. Returns `true` if it existed and changed (or already held
/// that status), `false` if no suggestion with `id` is stored.
pub fn set_status(id: &str, status: SuggestionStatus) -> std::io::Result<bool> {
    let mut list = read();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else {
        return Ok(false);
    };
    s.status = status;
    write(&list)?;
    Ok(true)
}

/// Suggestions newest-first, optionally filtered by status and capped. `limit == 0` is uncapped.
pub fn list(status: Option<SuggestionStatus>, limit: usize) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = read();
    if let Some(want) = status {
        out.retain(|s| s.status == want);
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    if limit > 0 {
        out.truncate(limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir;

    fn sug(source: &str, key: &str, title: &str) -> Suggestion {
        Suggestion::new(source, key, title, "because", format!("https://x/{key}"))
    }

    #[test]
    fn upsert_then_read_roundtrips() {
        with_state_dir(|| {
            let added = upsert(vec![sug("gh-prs", "pr-1", "Review PR 1")]).unwrap();
            assert_eq!(added, 1);
            let all = read();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].title, "Review PR 1");
            assert_eq!(all[0].status, SuggestionStatus::Suggested);
            assert_eq!(all[0].id, "gh-prs-pr-1");
        });
    }

    #[test]
    fn upsert_is_idempotent_on_id() {
        with_state_dir(|| {
            assert_eq!(upsert(vec![sug("s", "k", "one")]).unwrap(), 1);
            // Same source+key → same id → not re-added, even with different title text.
            assert_eq!(upsert(vec![sug("s", "k", "one changed")]).unwrap(), 0);
            let all = read();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].title, "one", "existing suggestion is left untouched");
        });
    }

    #[test]
    fn dismissed_suggestion_is_never_resurrected() {
        with_state_dir(|| {
            upsert(vec![sug("s", "k", "one")]).unwrap();
            assert!(set_status("s-k", SuggestionStatus::Dismissed).unwrap());
            // Re-synthesis over the same finding must not undo the dismissal.
            assert_eq!(upsert(vec![sug("s", "k", "one")]).unwrap(), 0);
            let all = read();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].status, SuggestionStatus::Dismissed);
        });
    }

    #[test]
    fn set_status_reports_missing() {
        with_state_dir(|| {
            assert!(!set_status("nope", SuggestionStatus::Accepted).unwrap());
        });
    }

    #[test]
    fn list_filters_by_status_and_orders_newest_first() {
        with_state_dir(|| {
            let now = crate::test_support::EnvGuard::set("BN_NOW", "1000");
            upsert(vec![sug("s", "old", "old")]).unwrap();
            now.set_value("2000");
            upsert(vec![sug("s", "new", "new")]).unwrap();
            drop(now);
            set_status("s-old", SuggestionStatus::Accepted).unwrap();

            let suggested = list(Some(SuggestionStatus::Suggested), 0);
            assert_eq!(suggested.len(), 1);
            assert_eq!(suggested[0].id, "s-new");

            let all = list(None, 0);
            let ids: Vec<_> = all.iter().map(|s| s.id.clone()).collect();
            assert_eq!(ids, vec!["s-new", "s-old"], "newest first");
            assert_eq!(list(None, 1).len(), 1);
        });
    }

    #[test]
    fn status_parse_roundtrips() {
        for st in [
            SuggestionStatus::Suggested,
            SuggestionStatus::Accepted,
            SuggestionStatus::Dismissed,
        ] {
            assert_eq!(SuggestionStatus::parse(st.as_str()), Some(st));
        }
        assert_eq!(SuggestionStatus::parse("bogus"), None);
    }
}
