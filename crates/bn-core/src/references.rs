//! Reusable **reference** documents (#623): per-topic coding standards / preferences that
//! shape how agents write code and how reviews check it — the sibling of `skills` (reusable
//! *workflow* docs) for reusable *standards* docs. One markdown file per topic
//! (`references/<name>.md`), person-scoped and cross-machine so the same standards apply on
//! every host (see [`crate::resolver::references_dir`]).
//!
//! The engine is intentionally tiny: a directory of `.md` files, listed by filename with a
//! one-line summary lifted from each file's first heading/non-empty line, and resolved by name
//! with a fuzzy (exact → prefix → substring) match. `bn ref` (CLI), the dispatch seed, and the
//! review prompt all read through here.

use crate::resolver::references_dir;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// A single reference document: its topic `name` (the file stem), the one-line `summary` lifted
/// from the file's first heading/non-empty line, and the on-disk `path`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub summary: String,
    pub path: PathBuf,
}

/// The scaffold body a fresh reference starts with (via `bn ref add` / an editing `bn ref edit`
/// of a missing file). A minimal template the user fills in — its first heading doubles as the
/// list summary.
pub fn template(name: &str) -> String {
    format!(
        "# {name}\n\nMy {name} coding standards / preferences. Agents follow these when writing \
         code; reviewers check diffs against them.\n\n- \n"
    )
}

/// One-line summary for the list view: the first markdown heading (leading `#`s stripped), else
/// the first non-empty line, trimmed. Empty when the file has no readable content.
fn summarize(body: &str) -> String {
    // Prefer a heading; fall back to the first non-empty line.
    for line in body.lines() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            if !h.is_empty() {
                return h.to_string();
            }
        }
    }
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The absolute path a reference `name` maps to: `<references_dir>/<name>.md`.
pub fn ref_path(name: &str) -> PathBuf {
    references_dir().join(format!("{name}.md"))
}

/// Read + summarize a single reference file into a [`Reference`]. `None` if it can't be read.
fn load(path: &Path) -> Option<Reference> {
    let name = path.file_stem()?.to_string_lossy().to_string();
    let body = std::fs::read_to_string(path).ok()?;
    Some(Reference {
        name,
        summary: summarize(&body),
        path: path.to_path_buf(),
    })
}

/// All references in the store, sorted by name. Only top-level `*.md` files count; a missing
/// directory yields an empty list (nothing configured yet).
pub fn list() -> Vec<Reference> {
    let dir = references_dir();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Some(r) = load(&p) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Just the reference names, sorted — the compact form the dispatch seed / review prompt list.
pub fn names() -> Vec<String> {
    list().into_iter().map(|r| r.name).collect()
}

/// Resolve a `query` to a single reference by name, fuzzily: exact match wins, else a unique
/// prefix match, else a unique substring match. `None` when nothing matches or a prefix/substring
/// is ambiguous (multiple candidates) — the caller reports the ambiguity.
pub fn resolve(query: &str) -> Option<Reference> {
    let all = list();
    let q = query.trim().to_lowercase();
    if let Some(r) = all.iter().find(|r| r.name.to_lowercase() == q) {
        return Some(r.clone());
    }
    let prefix: Vec<&Reference> = all
        .iter()
        .filter(|r| r.name.to_lowercase().starts_with(&q))
        .collect();
    if prefix.len() == 1 {
        return Some(prefix[0].clone());
    }
    let sub: Vec<&Reference> = all
        .iter()
        .filter(|r| r.name.to_lowercase().contains(&q))
        .collect();
    if sub.len() == 1 {
        return Some(sub[0].clone());
    }
    None
}

/// Read the full body of the reference matching `query` (via [`resolve`]) plus its metadata.
pub fn read(query: &str) -> Option<(Reference, String)> {
    let r = resolve(query)?;
    let body = std::fs::read_to_string(&r.path).ok()?;
    Some((r, body))
}

/// Ensure a reference file exists for `name`, scaffolding it from [`template`] if missing, and
/// return its path — the create-if-missing target for `bn ref edit` / `bn ref add`. `existed`
/// reports whether the file was already there (so `add` can flag a no-op).
pub fn ensure_file(name: &str) -> std::io::Result<(PathBuf, bool)> {
    let path = ref_path(name);
    if path.exists() {
        return Ok((path, true));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, template(name))?;
    Ok((path, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point `references_dir()` at a scratch dir for the duration of the test. Serialized via the
    /// shared env lock because `$BN_REFERENCES_DIR` is process-global.
    fn with_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
        use crate::test_support::{env_lock, EnvGuard};
        let _g = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("BN_REFERENCES_DIR", tmp.path());
        f(tmp.path())
    }

    fn write_ref(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
    }

    #[test]
    fn env_override_drives_the_store_path() {
        with_dir(|dir| {
            assert_eq!(references_dir(), dir);
            assert_eq!(ref_path("javascript"), dir.join("javascript.md"));
        });
    }

    #[test]
    fn list_sorts_and_summarizes_from_first_heading() {
        with_dir(|dir| {
            write_ref(dir, "react", "# React standards\n\n- hooks first\n");
            write_ref(dir, "javascript", "\n\nPlain first line summary\n");
            // Non-markdown files are ignored.
            std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();
            let refs = list();
            assert_eq!(refs.len(), 2);
            assert_eq!(refs[0].name, "javascript");
            assert_eq!(refs[0].summary, "Plain first line summary");
            assert_eq!(refs[1].name, "react");
            assert_eq!(refs[1].summary, "React standards");
            assert_eq!(names(), vec!["javascript", "react"]);
        });
    }

    #[test]
    fn resolve_matches_exact_prefix_and_substring() {
        with_dir(|dir| {
            write_ref(dir, "javascript", "# js\n");
            write_ref(dir, "rust", "# rust\n");
            // Exact.
            assert_eq!(resolve("rust").unwrap().name, "rust");
            // Prefix (unique).
            assert_eq!(resolve("java").unwrap().name, "javascript");
            // Substring (unique).
            assert_eq!(resolve("scrip").unwrap().name, "javascript");
            // Case-insensitive.
            assert_eq!(resolve("RUST").unwrap().name, "rust");
            // No match.
            assert!(resolve("python").is_none());
        });
    }

    #[test]
    fn resolve_returns_none_on_ambiguous_prefix() {
        with_dir(|dir| {
            write_ref(dir, "react", "# react\n");
            write_ref(dir, "reasonml", "# reasonml\n");
            // "rea" prefixes both → ambiguous → None.
            assert!(resolve("rea").is_none());
        });
    }

    #[test]
    fn read_returns_full_body() {
        with_dir(|dir| {
            write_ref(
                dir,
                "commit-style",
                "# Commit style\n\nUse conventional commits.\n",
            );
            let (r, body) = read("commit").unwrap();
            assert_eq!(r.name, "commit-style");
            assert!(body.contains("conventional commits"));
        });
    }

    #[test]
    fn ensure_file_scaffolds_when_missing_then_is_idempotent() {
        with_dir(|_dir| {
            let (path, existed) = ensure_file("go").unwrap();
            assert!(!existed);
            assert!(path.exists());
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.starts_with("# go"));
            // Second call sees the existing file.
            let (path2, existed2) = ensure_file("go").unwrap();
            assert!(existed2);
            assert_eq!(path, path2);
        });
    }
}
