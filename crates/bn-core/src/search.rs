//! Full-text search across every branch note and the design docs saved beside them —
//! active *and* archived. Branch notes are organized by branch, which is the wrong axis
//! for recall: months later you look for a topic, not the branch you happened to be on.
//! This walks the whole corpus so durable knowledge stays findable.

use crate::resolver::Config;
use crate::store;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One matching line in a note or a sibling design doc.
#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub repo: String,
    pub branch: String,
    pub path: String,
    /// `note` for the branch note itself, `doc` for a design/investigation file beside it.
    pub kind: String,
    /// The owning note's status (`active`/`closed`) so callers can rank live work first.
    pub status: String,
    pub line: usize,
    pub snippet: String,
}

const SNIPPET_MAX: usize = 200;

/// Case-insensitive substring search over all note roots. Walks active and archived notes
/// plus the `*.md` docs saved in the same directory, attributing each hit to the owning
/// note's repo/branch (read from frontmatter, so `archive/` nesting doesn't confuse it).
/// `limit` caps total hits (0 = uncapped).
pub fn search(cfg: &Config, query: &str, limit: usize) -> Vec<SearchHit> {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for root in cfg.each_notes_root() {
        let mut note_files = Vec::new();
        find_notes(root, &mut note_files);
        note_files.sort();
        for note_md in note_files {
            let Ok(note) = store::read(&note_md) else {
                continue;
            };
            let repo = note.front.repo.as_str();
            let branch = note.front.branch.as_str();
            let status = note.front.status.as_str();
            let reached = |hits: &[SearchHit]| limit > 0 && hits.len() >= limit;
            scan_file(&note_md, "note", repo, branch, status, &needle, &mut hits);
            if reached(&hits) {
                hits.truncate(limit);
                return hits;
            }
            if let Some(dir) = note_md.parent() {
                for doc in sibling_docs(dir) {
                    scan_file(&doc, "doc", repo, branch, status, &needle, &mut hits);
                    if reached(&hits) {
                        hits.truncate(limit);
                        return hits;
                    }
                }
            }
        }
    }
    hits
}

/// Recursively collect every `note.md` under `dir`, skipping VCS/dependency dirs.
fn find_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, ".git" | "node_modules") {
                continue;
            }
            find_notes(&path, out);
        } else if path.file_name().and_then(|s| s.to_str()) == Some("note.md") {
            out.push(path);
        }
    }
}

/// The `*.md` design/investigation docs in `dir`, excluding the note and its achievements log.
fn sibling_docs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut docs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("md")
                && !matches!(
                    p.file_name().and_then(|s| s.to_str()),
                    Some("note.md") | Some("achievements.md")
                )
        })
        .collect();
    docs.sort();
    docs
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::config as cfg;

    fn write_note(root: &Path, sub: &str, repo: &str, branch: &str, status: &str, body: &str) {
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let front = format!(
            "---\nrepo: {repo}\nbranch: {branch}\ncreated: 2026-06-20\nstatus: {status}\nmachine: testhost\ntags: []\ncollaborators: []\ntodos: []\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n---\n{body}\n"
        );
        std::fs::write(dir.join("note.md"), front).unwrap();
    }

    #[test]
    fn finds_across_active_archive_and_docs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // active note mentioning telemetry
        write_note(
            root,
            "bn/feat-x",
            "bn",
            "feat-x",
            "active",
            "## Goal\n\nWire telemetry markers\n",
        );
        // archived note also mentioning it, nested under archive/
        write_note(
            root,
            "archive/bn/old-feat",
            "bn",
            "old-feat",
            "closed",
            "Investigated TELEMETRY paths\n",
        );
        // a sibling design doc next to the active note
        std::fs::write(
            root.join("bn/feat-x/architecture.md"),
            "# Arch\n\nThe telemetry pipeline emits markers.\n",
        )
        .unwrap();

        let hits = search(&cfg(root), "telemetry", 0);
        // 1 active note + 1 archived note + 1 doc = 3
        assert_eq!(hits.len(), 3, "{hits:#?}");
        assert!(hits
            .iter()
            .any(|h| h.kind == "note" && h.status == "active"));
        assert!(hits
            .iter()
            .any(|h| h.kind == "note" && h.status == "closed" && h.branch == "old-feat"));
        assert!(hits
            .iter()
            .any(|h| h.kind == "doc" && h.path.ends_with("architecture.md")));
    }

    #[test]
    fn case_insensitive_and_attributes_repo_branch_from_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "tracker/main",
            "tracker",
            "main",
            "active",
            "Audit NPM Dependencies\n",
        );
        let hits = search(&cfg(root), "npm dependencies", 0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].repo, "tracker");
        assert_eq!(hits[0].branch, "main");
        assert_eq!(
            hits[0].line, 16,
            "matched line within the raw note (after 15 frontmatter lines)"
        );
    }

    #[test]
    fn empty_query_returns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(search(&cfg(dir.path()), "   ", 0).is_empty());
    }

    #[test]
    fn limit_caps_total_hits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "bn/a",
            "bn",
            "a",
            "active",
            "match\nmatch\nmatch\nmatch\n",
        );
        let hits = search(&cfg(root), "match", 2);
        assert_eq!(hits.len(), 2);
    }
}
