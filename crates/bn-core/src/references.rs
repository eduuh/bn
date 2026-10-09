//! Reusable **reference** documents — markdown the agents read before they work and write to as
//! they learn. Two scopes share one engine ([`Scope`]):
//!
//! * [`Scope::Global`] (#623) — per-topic coding standards / preferences that shape how agents
//!   write code and how reviews check it, the sibling of `skills` (reusable *workflow* docs) for
//!   reusable *standards* docs. Person-scoped and cross-machine, so the same standards apply on
//!   every host (`<work_root>/references`, see [`crate::resolver::references_dir`]).
//! * [`Scope::Repo`] — per-repo **reference notes**: what past sessions discovered about one
//!   repository (how it builds, where the seams are, which failures mislead). Machine-scoped
//!   beside the branch notes it outlives (`<notes>/<machine>/<repo>/reference`, see
//!   [`crate::resolver::Config::repo_references_dir`]).
//!
//! The engine stays small: a directory of `.md` files listed by name with a one-line summary
//! lifted from each file's first heading, resolved by a fuzzy (exact → prefix → substring) match.
//! Repo notes may nest (`architecture/runtime-concepts.md` is the name
//! `architecture/runtime-concepts`) and keep an `images/` sibling for screenshots, which the walk
//! skips. `bn ref` (CLI), the MCP `bn_ref` tools, the dispatch seed, and the review prompt all
//! read through here.

use crate::resolver::{references_dir, Config};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which reference store to read or write: the person-scoped standards store, or one repo's
/// notes. Constructed by the CLI/MCP layer from `--repo` (absent → [`Scope::Global`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `<work_root>/references` — my coding standards, the same on every machine.
    Global,
    /// `<notes>/<machine>/<repo>/reference` — one repo's discovery notes.
    Repo(String),
}

impl Scope {
    /// A repo scope with the repo name lowercased, matching what
    /// [`resolve`](crate::resolver::resolve) returns and what the notes tree is keyed by.
    pub fn repo(repo: &str) -> Self {
        Scope::Repo(repo.trim().to_lowercase())
    }

    /// The directory this scope's `.md` files live in.
    pub fn dir(&self) -> PathBuf {
        match self {
            Scope::Global => references_dir(),
            Scope::Repo(repo) => Config::load().repo_references_dir(repo),
        }
    }

    /// How the scope is named in output and JSON envelopes: `global`, or the repo's name.
    pub fn label(&self) -> &str {
        match self {
            Scope::Global => "global",
            Scope::Repo(repo) => repo,
        }
    }

    /// True for the person-scoped standards store — the two scopes get different scaffolds and
    /// different phrasing in the agent seed.
    fn is_global(&self) -> bool {
        matches!(self, Scope::Global)
    }
}

/// A single reference document: its `name` (the store-relative path stem, so a nested repo note
/// is `architecture/runtime-concepts`), the one-line `summary` lifted from the file's first
/// heading, and the on-disk `path`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub summary: String,
    pub path: PathBuf,
}

/// What an [`append`] did: `appended` is false when the exact fact was already recorded, so the
/// caller reports "already known" rather than stacking a duplicate bullet.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppendOutcome {
    pub appended: bool,
    pub path: PathBuf,
    pub heading: String,
}

/// The scaffold body a fresh reference starts with (via `bn ref add` / an editing `bn ref edit`
/// of a missing file). A minimal template the user fills in — its first heading doubles as the
/// list summary.
///
/// The two scopes scaffold differently because they hold different things: a standards doc is a
/// list of preferences, a repo note is a record of findings. The repo template carries the
/// evidence convention the notes are built around — every fact says where it came from and when
/// it was checked, so a stale or guessed claim is visible instead of hardening into folklore.
pub fn template(scope: &Scope, name: &str) -> String {
    if scope.is_global() {
        return format!(
            "# {name}\n\nMy {name} coding standards / preferences. Agents follow these when \
             writing code; reviewers check diffs against them.\n\n- \n"
        );
    }
    let repo = scope.label();
    let today = crate::util::today();
    format!(
        "---\ntype: reference\nstatus: active\nupdated: {today}\n---\n\n# {name}\n\nWhat we know \
         about `{repo}`: {name}. Durable facts only — things still true next month. \
         Branch-specific work belongs in the branch note.\n\nEvery bullet carries where it came \
         from and when it was checked, e.g. `(src: path/to/file, {today})` or `(observed, \
         {today})`. An unsourced bullet means nobody verified it.\n\n## Facts\n\n## Gotchas\n\n\
         ## Sources\n"
    )
}

/// One-line summary for the list view: the first markdown heading (leading `#`s stripped), else
/// the first non-empty line, trimmed. YAML frontmatter is stripped first, so a note that opens
/// with `---` summarizes as its heading rather than as a fence or a stray field. Empty when the
/// file has no readable content.
fn summarize(body: &str) -> String {
    let body = crate::store::split_frontmatter(body).map_or(body, |(_, rest)| rest);
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

/// The absolute path a reference `name` maps to in `scope`: `<dir>/<name>.md`. A nested name
/// (`architecture/runtime-concepts`) maps to the matching subdirectory. Path segments that would
/// escape the store (`..`, absolute paths) are dropped — a name is a location inside the store,
/// never a way out of it.
pub fn ref_path(scope: &Scope, name: &str) -> PathBuf {
    let mut path = scope.dir();
    let segments: Vec<&str> = name
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    if segments.is_empty() {
        return path.join("untitled.md");
    }
    for seg in &segments[..segments.len() - 1] {
        path.push(seg);
    }
    path.join(format!("{}.md", segments[segments.len() - 1]))
}

fn reference_name(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let name = rel
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    (!name.is_empty()).then_some(name)
}

/// Read + summarize a single reference file into a [`Reference`], named by its path relative to
/// the store root. `None` if it can't be read.
fn load(root: &Path, path: &Path) -> Option<Reference> {
    let name = reference_name(root, path)?;
    let body = std::fs::read_to_string(path).ok()?;
    Some(Reference {
        name,
        summary: summarize(&body),
        path: path.to_path_buf(),
    })
}

/// Directory names the walk never descends into: attachments and editor/VCS noise. Screenshots
/// live beside the notes that embed them, so `images/` is content, not a topic.
fn is_skipped_dir(name: &str) -> bool {
    name == "images" || name == "attachments" || name.starts_with('.')
}

/// Recursively collect `*.md` files under `dir` into `out`. Hand-rolled rather than pulling in a
/// walker crate — the trees are a few dozen files deep at most.
fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !is_skipped_dir(&name) {
                collect_md(&path, out);
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

/// Every repo reference note under a notes `root`, as `(repo, path)` pairs sorted by path.
///
/// Repo notes sit at `<root>/<machine>/<repo>/reference/**.md` — a repo-level directory with no
/// owning `note.md`, so the note walkers in [`crate::search`] and [`crate::recall`] step straight
/// past them. Both call this to fold the notes into their corpus; without it the durable knowledge
/// would be the one thing `bn search` and `bn recall` couldn't find.
pub fn notes_under_root(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let Ok(machines) = std::fs::read_dir(root) else {
        return out;
    };
    for machine in machines.flatten() {
        if !machine.path().is_dir() || machine.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(repos) = std::fs::read_dir(machine.path()) else {
            continue;
        };
        for repo_entry in repos.flatten() {
            let dir = repo_entry.path().join("reference");
            if !dir.is_dir() {
                continue;
            }
            let repo = repo_entry.file_name().to_string_lossy().to_string();
            let mut files = Vec::new();
            collect_md(&dir, &mut files);
            out.extend(files.into_iter().map(|p| (repo.clone(), p)));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// All references in `scope`, sorted by name. Global references include shipped defaults,
/// overridden by personal files. An explicit `BN_REFERENCES_DIR` reads only that store.
/// Repo notes may nest, so this walks subdirectories (skipping `images/`).
pub fn list_in(scope: &Scope) -> Vec<Reference> {
    let mut dirs = Vec::new();
    let explicit = std::env::var("BN_REFERENCES_DIR").is_ok_and(|dir| !dir.trim().is_empty());
    if scope.is_global() && !explicit {
        if let Some(dir) = crate::resolver::shipped_config("references") {
            dirs.push(dir);
        }
    }
    dirs.push(scope.dir());
    let mut references = BTreeMap::new();
    for dir in dirs {
        let mut files = Vec::new();
        collect_md(&dir, &mut files);
        for path in files {
            if let Some(name) = reference_name(&dir, &path) {
                references.insert(name.to_lowercase(), (dir.clone(), path));
            }
        }
    }
    let mut out: Vec<Reference> = references
        .into_values()
        .filter_map(|(dir, path)| load(&dir, &path))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Just the reference names in `scope`, sorted — the compact form the dispatch seed / review
/// prompt list, so an agent learns what exists without paying for any body.
pub fn names_in(scope: &Scope) -> Vec<String> {
    list_in(scope).into_iter().map(|r| r.name).collect()
}

/// Resolve a `query` to a single reference in `scope`, fuzzily: exact match wins, else a unique
/// prefix match, else a unique substring match. A nested note also matches on its bare stem
/// (`runtime-concepts` finds `architecture/runtime-concepts`). `None` when nothing matches or a
/// prefix/substring is ambiguous — the caller reports the ambiguity.
pub fn resolve_in(scope: &Scope, query: &str) -> Option<Reference> {
    let all = list_in(scope);
    let q = query.trim().trim_matches('/').to_lowercase();
    if q.is_empty() {
        return None;
    }
    if let Some(r) = all.iter().find(|r| r.name.to_lowercase() == q) {
        return Some(r.clone());
    }
    // A nested note is addressable by its bare stem when that stem is unique.
    let stem: Vec<&Reference> = all
        .iter()
        .filter(|r| r.name.rsplit('/').next().unwrap_or(&r.name).to_lowercase() == q)
        .collect();
    if stem.len() == 1 {
        return Some(stem[0].clone());
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

/// Read the full body of the reference in `scope` matching `query` (via [`resolve_in`]) plus its
/// metadata.
pub fn read_in(scope: &Scope, query: &str) -> Option<(Reference, String)> {
    let r = resolve_in(scope, query)?;
    let body = std::fs::read_to_string(&r.path).ok()?;
    Some((r, body))
}

/// Split `name#section` into its parts. No `#` means the whole document.
pub fn split_ref(query: &str) -> (&str, Option<&str>) {
    match query.split_once('#') {
        Some((name, section)) if !section.trim().is_empty() => (name.trim(), Some(section.trim())),
        Some((name, _)) => (name.trim(), None),
        None => (query.trim(), None),
    }
}

/// A body's `## ` sections in order: the heading text and the section's text (heading line
/// included, up to the next `## ` or `# `). Headings inside fenced code are not sections.
pub fn sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for (line, code) in crate::markdown::scan(body) {
        let heading = (!code).then(|| line.strip_prefix("## ")).flatten();
        let top = !code && line.starts_with("# ");
        if heading.is_some() || top {
            out.extend(current.take());
        }
        if let Some(h) = heading {
            current = Some((h.trim().to_string(), String::new()));
        }
        if let Some((_, text)) = current.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out.extend(current);
    out
}

/// The section of `body` whose heading matches `section`, ignoring case and treating `-`, `_`
/// and spaces alike (`key-files` finds `## Key Files`).
pub fn section_of(body: &str, section: &str) -> Option<String> {
    let norm = |s: &str| {
        s.trim()
            .trim_start_matches('#')
            .trim()
            .to_lowercase()
            .replace(['-', '_'], " ")
    };
    let want = norm(section);
    sections(body)
        .into_iter()
        .find(|(h, _)| norm(h) == want)
        .map(|(_, text)| text)
}

/// Ensure a reference file exists for `name` in `scope`, scaffolding it from [`template`] if
/// missing, and return its path — the create-if-missing target for `bn ref edit` / `bn ref add`.
/// A shipped default is copied into the personal store before customization.
/// `existed` reports whether the file was already there (so `add` can flag a no-op).
pub fn ensure_file_in(scope: &Scope, name: &str) -> std::io::Result<(PathBuf, bool)> {
    let path = ref_path(scope, name);
    if path.exists() {
        return Ok((path, true));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = match resolve_in(scope, name).filter(|reference| reference.name == name) {
        Some(reference) => std::fs::read_to_string(reference.path)?,
        None => template(scope, name),
    };
    std::fs::write(&path, body)?;
    Ok((path, false))
}

/// Normalize a section token (`gotchas`, `Gotchas`, `## Gotchas`) to the `## Heading` form
/// [`crate::render::append_to_section`] matches on. Hyphens and underscores become spaces so
/// `key-files` reads as `## Key Files`.
fn heading_for(section: &str) -> String {
    let s = section.trim();
    if let Some(rest) = s.strip_prefix('#') {
        return format!("## {}", rest.trim_start_matches('#').trim());
    }
    let title = s
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("## {title}")
}

/// Record one fact in `scope`'s `name` note, under `section` (default `Facts`), as a bullet.
///
/// The file is scaffolded from [`template`] when missing, so an agent mid-debug never has to
/// create a note before writing to it. **Deduped document-wide**: if that exact bullet already
/// appears anywhere in the note, nothing is written and `appended` comes back false — the same
/// fact learned twice should not become two bullets, and a fact recorded under one heading
/// should not reappear under another. On a real append the frontmatter's `updated:` is stamped
/// with today, so staleness is visible in the file itself.
pub fn append(
    scope: &Scope,
    name: &str,
    section: Option<&str>,
    text: &str,
) -> std::io::Result<AppendOutcome> {
    let heading = heading_for(section.unwrap_or("Facts"));
    let (path, _existed) = ensure_file_in(scope, name)?;
    let body = std::fs::read_to_string(&path)?;
    let line = format!("- {}", text.trim());
    if body.lines().any(|l| l.trim() == line) {
        return Ok(AppendOutcome {
            appended: false,
            path,
            heading,
        });
    }
    let updated = stamp_updated(&crate::render::append_to_section(&body, &heading, &line));
    std::fs::write(&path, &updated)?;
    Ok(AppendOutcome {
        appended: true,
        path,
        heading,
    })
}

/// Rewrite an `updated: <date>` frontmatter field to today, leaving everything else alone. A note
/// with no frontmatter (a hand-written one, or a global standards doc) is returned unchanged —
/// this stamps what is already there rather than imposing a schema.
fn stamp_updated(doc: &str) -> String {
    let Some((front, _)) = crate::store::split_frontmatter(doc) else {
        return doc.to_string();
    };
    if !front
        .lines()
        .any(|l| l.trim_start().starts_with("updated:"))
    {
        return doc.to_string();
    }
    let today = crate::util::today();
    let mut out = String::with_capacity(doc.len());
    let mut in_front = false;
    let mut done = false;
    for (i, line) in doc.lines().enumerate() {
        let is_fence = line.trim_end() == "---";
        if i == 0 && is_fence {
            in_front = true;
        } else if in_front && is_fence {
            in_front = false;
        } else if in_front && !done && line.trim_start().starts_with("updated:") {
            out.push_str(&format!("updated: {today}\n"));
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Global-scope conveniences: the pre-#623 call shape, kept so the specialist resolver and the
// dispatch seed read the standards store without naming a scope.
// ---------------------------------------------------------------------------------------------

/// Every standards reference, sorted by name.
pub fn list() -> Vec<Reference> {
    list_in(&Scope::Global)
}

/// Just the standards reference names, sorted.
pub fn names() -> Vec<String> {
    names_in(&Scope::Global)
}

/// Fuzzily resolve `query` against the standards store.
pub fn resolve(query: &str) -> Option<Reference> {
    resolve_in(&Scope::Global, query)
}

/// Read a standards reference's full body plus its metadata.
pub fn read(query: &str) -> Option<(Reference, String)> {
    read_in(&Scope::Global, query)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sections_are_addressable_and_fences_are_not_headings() {
        let body = "---\ntype: reference\n---\n# build\n\nintro\n\n## Facts\n- a\n\n```md\n## Not a heading\n```\n\n## Key Files\n- b\n";
        let secs = super::sections(body);
        let names: Vec<&str> = secs.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(names, ["Facts", "Key Files"]);
        let facts = super::section_of(body, "facts").unwrap();
        assert!(facts.starts_with("## Facts") && facts.contains("## Not a heading"));
        assert!(!facts.contains("Key Files"));
        assert!(super::section_of(body, "key-files")
            .unwrap()
            .contains("- b"));
        assert_eq!(super::section_of(body, "gotchas"), None);
        assert_eq!(super::split_ref("build#facts"), ("build", Some("facts")));
        assert_eq!(super::split_ref("build#"), ("build", None));
        assert_eq!(super::split_ref("build"), ("build", None));
    }

    use super::*;

    /// Point the global store at a scratch dir. `$BN_REFERENCES_DIR` is the documented override
    /// [`references_dir`] honours, so this needs no config on disk.
    fn with_global_store<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _lock = crate::test_support::env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::test_support::EnvGuard::set("BN_REFERENCES_DIR", tmp.path());
        f(tmp.path())
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn with_shipped_store(f: impl FnOnce(&Path, &Path)) {
        let _lock = crate::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _config =
            crate::test_support::EnvGuard::set("BN_CONFIG", tmp.path().join("config.toml"));
        let _config_dir =
            crate::test_support::EnvGuard::set("BN_CONFIG_DIR", tmp.path().join("config"));
        let _notes = crate::test_support::EnvGuard::set("BN_NOTES_DIR", tmp.path().join("notes"));
        let _refs = crate::test_support::EnvGuard::remove("BN_REFERENCES_DIR");
        let shipped = tmp.path().join("config/repo/config/references");
        std::fs::create_dir_all(&shipped).unwrap();
        let personal = Scope::Global.dir();
        assert!(personal.starts_with(tmp.path()));
        f(&personal, &shipped);
    }

    #[test]
    fn shipped_references_are_available_without_personal_copies() {
        with_shipped_store(|personal, shipped| {
            write(
                shipped,
                "writing-voice.md",
                "# Writing voice\n\nPlain English.\n",
            );
            let (reference, body) = read("writing-voice").expect("shipped reference");
            assert_eq!(reference.path, shipped.join("writing-voice.md"));
            assert!(body.contains("Plain English."));
            assert_eq!(names(), ["writing-voice"]);
            assert!(!personal.exists());
            assert!(names_in(&Scope::repo("bn")).is_empty());
        });
    }

    #[test]
    fn personal_references_override_shipped_defaults_without_duplicates() {
        with_shipped_store(|personal, shipped| {
            write(shipped, "writing-voice.md", "# Shipped voice\n");
            write(personal, "writing-voice.md", "# My voice\n");
            let (reference, body) = read("writing-voice").unwrap();
            assert_eq!(reference.path, personal.join("writing-voice.md"));
            assert_eq!(body, "# My voice\n");
            assert_eq!(names(), ["writing-voice"]);
        });
    }

    #[test]
    fn unreadable_personal_references_do_not_silently_use_shipped_defaults() {
        with_shipped_store(|personal, shipped| {
            write(shipped, "writing-voice.md", "# Shipped voice\n");
            std::fs::create_dir_all(personal).unwrap();
            std::fs::write(personal.join("writing-voice.md"), [0xff]).unwrap();
            assert!(read("writing-voice").is_none());
        });
    }

    #[test]
    fn editing_a_shipped_reference_copies_it_into_the_personal_store() {
        with_shipped_store(|personal, shipped| {
            let body = "# Writing voice\n\n## Style\n- Plain English.\n";
            write(shipped, "writing-voice.md", body);
            let (path, existed) = ensure_file_in(&Scope::Global, "writing-voice").unwrap();
            assert!(!existed);
            assert_eq!(path, personal.join("writing-voice.md"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
            append(
                &Scope::Global,
                "writing-voice",
                Some("style"),
                "Keep it short.",
            )
            .unwrap();
            assert!(std::fs::read_to_string(path)
                .unwrap()
                .contains("Keep it short."));
            assert_eq!(
                std::fs::read_to_string(shipped.join("writing-voice.md")).unwrap(),
                body
            );
        });
    }

    #[test]
    fn explicit_reference_directory_excludes_shipped_defaults() {
        with_shipped_store(|personal, shipped| {
            write(shipped, "writing-voice.md", "# Writing voice\n");
            let _refs = crate::test_support::EnvGuard::set("BN_REFERENCES_DIR", personal);
            assert!(list().is_empty());
            assert!(read("writing-voice").is_none());
        });
    }

    #[test]
    fn list_is_empty_when_the_store_does_not_exist_yet() {
        let _lock = crate::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _guard =
            crate::test_support::EnvGuard::set("BN_REFERENCES_DIR", tmp.path().join("nope"));
        assert!(list_in(&Scope::Global).is_empty());
    }

    #[test]
    fn nested_notes_are_named_by_their_relative_path_and_images_are_skipped() {
        with_global_store(|root| {
            write(root, "build-and-run.md", "# build-and-run\n");
            write(
                root,
                "architecture/runtime-concepts.md",
                "# Runtime concepts\n",
            );
            write(root, "images/shot.png", "not markdown");
            write(root, "images/caption.md", "# should be skipped\n");
            let names = names_in(&Scope::Global);
            assert_eq!(
                names,
                vec![
                    "architecture/runtime-concepts".to_string(),
                    "build-and-run".to_string()
                ]
            );
        });
    }

    #[test]
    fn summary_skips_frontmatter_and_uses_the_first_heading() {
        with_global_store(|root| {
            write(
                root,
                "n.md",
                "---\ntype: reference\nupdated: 2026-09-03\n---\n\n# How it builds\n\nbody\n",
            );
            assert_eq!(list_in(&Scope::Global)[0].summary, "How it builds");
        });
    }

    #[test]
    fn resolve_matches_exact_then_bare_stem_then_prefix() {
        with_global_store(|root| {
            write(root, "architecture/runtime-concepts.md", "# rc\n");
            write(root, "websocket-debugging.md", "# ws\n");
            let s = Scope::Global;
            assert_eq!(
                resolve_in(&s, "architecture/runtime-concepts")
                    .unwrap()
                    .name,
                "architecture/runtime-concepts"
            );
            // Bare stem of a nested note.
            assert_eq!(
                resolve_in(&s, "runtime-concepts").unwrap().name,
                "architecture/runtime-concepts"
            );
            // Prefix.
            assert_eq!(
                resolve_in(&s, "websocket").unwrap().name,
                "websocket-debugging"
            );
            assert!(resolve_in(&s, "nothing-like-this").is_none());
        });
    }

    #[test]
    fn ref_path_cannot_escape_the_store() {
        with_global_store(|root| {
            let p = ref_path(&Scope::Global, "../../etc/passwd");
            assert_eq!(p, root.join("etc").join("passwd.md"));
        });
    }

    #[test]
    fn append_creates_the_note_then_dedupes_the_same_fact() {
        with_global_store(|root| {
            let s = Scope::Global;
            let first = append(
                &s,
                "build-and-run",
                Some("gotchas"),
                "port 3001 serves chunks",
            )
            .expect("append");
            assert!(first.appended);
            assert_eq!(first.heading, "## Gotchas");
            assert!(root.join("build-and-run.md").exists());

            let again = append(
                &s,
                "build-and-run",
                Some("gotchas"),
                "port 3001 serves chunks",
            )
            .expect("append");
            assert!(!again.appended, "the same fact must not be recorded twice");

            // Deduping is document-wide: the same fact under a different heading is still a dupe.
            let elsewhere = append(
                &s,
                "build-and-run",
                Some("facts"),
                "port 3001 serves chunks",
            )
            .expect("append");
            assert!(!elsewhere.appended);

            let body = std::fs::read_to_string(root.join("build-and-run.md")).unwrap();
            assert_eq!(body.matches("port 3001 serves chunks").count(), 1);
        });
    }

    #[test]
    fn append_stamps_the_updated_field_when_the_note_has_one() {
        with_global_store(|root| {
            write(
                root,
                "n.md",
                "---\ntype: reference\nupdated: 2020-01-01\n---\n\n# n\n\n## Facts\n",
            );
            append(&Scope::Global, "n", Some("facts"), "a fact").unwrap();
            let body = std::fs::read_to_string(root.join("n.md")).unwrap();
            assert!(body.contains(&format!("updated: {}", crate::util::today())));
            assert!(!body.contains("2020-01-01"));
            assert!(body.contains("- a fact"));
        });
    }

    #[test]
    fn heading_normalizes_section_tokens() {
        assert_eq!(heading_for("gotchas"), "## Gotchas");
        assert_eq!(heading_for("key-files"), "## Key Files");
        assert_eq!(heading_for("## Sources"), "## Sources");
    }
}
