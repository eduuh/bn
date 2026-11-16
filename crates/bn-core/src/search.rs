//! Full-text search across retained repository reference documents.

use crate::resolver::Config;
use serde::Serialize;
use std::path::Path;

/// One matching line in a reference document.
#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub repo: String,
    pub branch: String,
    pub path: String,
    /// The retained artifact kind.
    pub kind: String,
    /// Artifact status for consumers of the search projection.
    pub status: String,
    pub line: usize,
    pub snippet: String,
}

const SNIPPET_MAX: usize = 200;

/// Case-insensitive substring search over retained repo references in each configured store.
/// `limit` caps total hits (0 = uncapped).
pub fn search(cfg: &Config, query: &str, limit: usize) -> Vec<SearchHit> {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for root in cfg.each_notes_root() {
        for (repo, path) in crate::references::notes_under_root(root) {
            scan_file(
                &path,
                "reference",
                &repo,
                "reference",
                "active",
                &needle,
                &mut hits,
            );
            if limit > 0 && hits.len() >= limit {
                hits.truncate(limit);
                return hits;
            }
        }
    }
    hits
}

/// Append a hit for every line in `path` containing `needle` (already lowercased).
fn scan_file(
    path: &Path,
    kind: &str,
    repo: &str,
    branch: &str,
    status: &str,
    needle: &str,
    hits: &mut Vec<SearchHit>,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        if line_contains_ci(line, needle) {
            hits.push(SearchHit {
                repo: repo.to_string(),
                branch: branch.to_string(),
                path: path.display().to_string(),
                kind: kind.to_string(),
                status: status.to_string(),
                line: i + 1,
                snippet: truncate(line.trim()),
            });
        }
    }
}

/// Case-insensitive substring test against an already-lowercased `needle`. For the common
/// all-ASCII case this scans `haystack`'s bytes without allocating a lowercased copy per line;
/// it only falls back to a Unicode `to_lowercase` allocation when either side is non-ASCII (to
/// preserve the original full-Unicode folding behaviour).
fn line_contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let (hay, ndl) = (haystack.as_bytes(), needle.as_bytes());
        if ndl.len() > hay.len() {
            return false;
        }
        // needle is already lowercase, so only the haystack byte needs folding.
        hay.windows(ndl.len())
            .any(|w| w.iter().zip(ndl).all(|(h, n)| h.to_ascii_lowercase() == *n))
    } else {
        haystack.to_lowercase().contains(needle)
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= SNIPPET_MAX {
        return s.to_string();
    }
    let cut: String = s.chars().take(SNIPPET_MAX).collect();
    format!("{cut}…")
}
