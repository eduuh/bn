//! One-shot note-store maintenance: convert legacy split notes to the single-file
//! format (`migrate_all`) and relocate old-layout note dirs into the machine-subtree
//! layout (`migrate_layout`). Maintenance code kept off the hot note-command path
//! (`commands`).

use crate::resolver::Config;
use crate::store::{self, StoreError};

/// Outcome of a bulk migration.
#[derive(Debug, Default)]
pub struct MigrateReport {
    pub migrated: Vec<String>,
    pub skipped: usize,
    pub failed: Vec<String>,
}

/// Convert every legacy note (split `note.yaml` sidecar or md-only `## Todos` sections)
/// under the storage roots into the single-file format, retiring `note.yaml`→`.bak`.
/// Already-single-file notes are left untouched. Walks recursively so archived notes
/// (deeper than `<repo>/<branch>`) are migrated too.
pub fn migrate_all(cfg: &Config) -> MigrateReport {
    let mut report = MigrateReport::default();
    let mut note_files = Vec::new();
    for root in cfg.each_notes_root() {
        find_note_files(root, &mut note_files);
    }
    note_files.sort();
    for note_md in note_files {
        match migrate_one(&note_md) {
            Ok(true) => report.migrated.push(note_md.display().to_string()),
            Ok(false) => report.skipped += 1,
            Err(e) => report.failed.push(format!("{}: {e}", note_md.display())),
        }
    }
    report
}

/// Migrate one note if it is legacy. Returns Ok(true) if converted, Ok(false) if it was
/// already single-file (left untouched).
fn migrate_one(note_md: &std::path::Path) -> Result<bool, StoreError> {
    let sibling = note_md.with_extension("yaml");
    let text = std::fs::read_to_string(note_md)?;
    let is_single_file = !sibling.exists()
        && store::split_frontmatter(&text)
            .map(|(front, _)| front.lines().any(|l| l.trim_start().starts_with("todos:")))
            .unwrap_or(false);
    if is_single_file {
        return Ok(false);
    }
    // Locked: a migration rewrites the whole note, so an unlocked read-then-save here would drop
    // any concurrent edit made between the two.
    let _lock = store::NoteLock::acquire(note_md);
    let note = store::read(note_md)?;
    store::save(&note, note_md)?;
    Ok(true)
}

fn find_note_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_note_files(&p, out);
        } else if p.file_name().and_then(|n| n.to_str()) == Some("note.md") {
            out.push(p);
        }
    }
}

/// Outcome of a layout migration: each entry is a human-readable line.
#[derive(Debug, Default)]
pub struct LayoutReport {
    /// `<old> -> <new>` for each moved (or, in dry-run, would-move) branch dir.
    pub moved: Vec<String>,
    /// Reasons a candidate was left alone (collision, unreadable, …).
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

/// Move every OLD-layout note dir (`<root>/<repo>/<branch>/`) into the machine-subtree
/// layout (`<root>/<machine>/<repo>/<branch>/`). The destination `<machine>` comes from the
/// note's frontmatter `machine` field (empty/absent → this host), so notes belonging to
/// other machines land in THEIR subtree — the synced repo holds every machine's notes.
///
/// Old layout is identified positionally: `note.md` sitting exactly two path components below
/// a notes root (`<repo>/<branch>/note.md`). Anything deeper is already migrated or under
/// `archive/` and is left untouched. The whole branch directory moves (note.md + sibling
/// design docs + jobs/).
///
/// On a collision (a machine-subtree copy already exists, from a prior partial migration):
/// without `force`, skip and leave both in place. With `force`, reconcile by recency —
/// whichever `note.md` is newer wins: a newer old-layout copy replaces the stale
/// machine-subtree dir; a newer (or equal) machine copy is kept and the old-layout orphan is
/// removed. Everything is git-tracked, so a wrong reconciliation is recoverable.
pub fn migrate_layout(cfg: &Config, dry_run: bool, force: bool) -> LayoutReport {
    let mut report = LayoutReport::default();
    for root in cfg.each_notes_root() {
        let mut note_files = Vec::new();
        find_note_files(root, &mut note_files);
        note_files.sort();
        for note_md in note_files {
            let Ok(rel) = note_md.strip_prefix(root) else {
                continue;
            };
            let comps: Vec<_> = rel.components().collect();
            // Old layout == exactly [repo, branch, note.md]; deeper is already-migrated/archive.
            if comps.len() != 3 {
                continue;
            }
            let repo = comps[0].as_os_str().to_string_lossy().into_owned();
            let branch = comps[1].as_os_str().to_string_lossy().into_owned();
            if repo == "archive" {
                continue;
            }

            // Destination machine is taken from the note itself; only the path supplies
            // repo/branch (dir names must keep matching the resolver, which derives them from
            // cwd/git, not from frontmatter).
            let machine = match store::read(&note_md) {
                Ok(n) if !n.front.machine.is_empty() => n.front.machine,
                Ok(_) => cfg.machine().to_string(),
                Err(e) => {
                    report
                        .skipped
                        .push(format!("{} (unreadable: {e})", note_md.display()));
                    continue;
                }
            };

            let Some(src_dir) = note_md.parent().map(|path| path.to_path_buf()) else {
                report
                    .skipped
                    .push(format!("{} (missing parent directory)", note_md.display()));
                continue;
            };
            let dst_dir = root.join(&machine).join(&repo).join(&branch);

            if dst_dir.exists() {
                if !force {
                    report.skipped.push(format!(
                        "{} -> {} (dest exists)",
                        src_dir.display(),
                        dst_dir.display()
                    ));
                    continue;
                }
                // Reconcile by recency.
                let src_newer = note_mtime(&note_md) > note_mtime(&dst_dir.join("note.md"));
                if src_newer {
                    let line = format!(
                        "{} -> {} (replaced stale)",
                        src_dir.display(),
                        dst_dir.display()
                    );
                    if dry_run {
                        report.moved.push(line);
                        continue;
                    }
                    if let Err(e) = std::fs::remove_dir_all(&dst_dir) {
                        report
                            .failed
                            .push(format!("{}: rm stale dst: {e}", dst_dir.display()));
                        continue;
                    }
                    match std::fs::rename(&src_dir, &dst_dir) {
                        Ok(()) => report.moved.push(line),
                        Err(e) => report.failed.push(format!("{}: {e}", src_dir.display())),
                    }
                } else {
                    let line = format!(
                        "{} (kept newer machine copy, removed orphan)",
                        src_dir.display()
                    );
                    if dry_run {
                        report.skipped.push(line);
                        continue;
                    }
                    match std::fs::remove_dir_all(&src_dir) {
                        Ok(()) => report.skipped.push(line),
                        Err(e) => report
                            .failed
                            .push(format!("{}: rm orphan: {e}", src_dir.display())),
                    }
                }
                continue;
            }

            let line = format!("{} -> {}", src_dir.display(), dst_dir.display());
            if dry_run {
                report.moved.push(line);
                continue;
            }
            if let Some(parent) = dst_dir.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    report.failed.push(format!(
                        "{}: mkdir {}: {e}",
                        src_dir.display(),
                        parent.display()
                    ));
                    continue;
                }
            }
            match std::fs::rename(&src_dir, &dst_dir) {
                Ok(()) => report.moved.push(line),
                Err(e) => report.failed.push(format!("{}: {e}", src_dir.display())),
            }
        }
    }
    report
}

/// Modification time of a note file, or `UNIX_EPOCH` if it can't be read (so a missing/stale
/// file always loses the recency comparison).
fn note_mtime(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::add;
    use crate::resolver::Resolved;
    use crate::test_support::config as cfg;

    #[test]
    fn migrate_converts_legacy_skips_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // legacy split note
        let nd = dir.path().join("demo").join("main");
        std::fs::create_dir_all(&nd).unwrap();
        std::fs::write(nd.join("note.md"), "# demo / main\n\n## Goal\n\ng\n").unwrap();
        std::fs::write(nd.join("note.yaml"), "repo: demo\nbranch: main\ncreated: 2026-01-01\nstatus: active\nmachine: h\ntags: []\ncollaborators: []\ntodos:\n  - id: t1\n    text: legacy\n    done: false\n    added: \"2026-01-01\"\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n").unwrap();
        // already single-file note (created via add)
        add(
            &c,
            &Resolved {
                repo: "fresh".into(),
                branch: "main".into(),
            },
            "todo",
            "x",
            None,
            "2026-06-20",
        )
        .unwrap();

        let report = migrate_all(&c);
        assert_eq!(
            report.migrated.len(),
            1,
            "only the legacy split note migrates"
        );
        assert!(report.skipped >= 1, "single-file note skipped");
        assert!(report.failed.is_empty());
        assert!(!nd.join("note.yaml").exists());
        assert!(nd.join("note.yaml.bak").exists());
        let migrated = store::read(&nd.join("note.md")).unwrap();
        assert_eq!(migrated.front.todos[0].id, "t1");
    }

    /// Write a minimal single-file note at an arbitrary dir with a given `machine` value.
    fn put_note(dir: &std::path::Path, repo: &str, branch: &str, machine: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mc = if machine.is_empty() {
            "machine:".to_string()
        } else {
            format!("machine: {machine}")
        };
        let body = format!(
            "---\nrepo: {repo}\nbranch: {branch}\ncreated: 2026-06-01\nstatus: active\n{mc}\ntags: []\ncollaborators: []\ntodos: []\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n---\n# {repo} / {branch}\n\n## Goal\n\n\n"
        );
        std::fs::write(dir.join("note.md"), body).unwrap();
    }

    #[test]
    fn migrate_layout_moves_by_frontmatter_machine() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path()); // hostname "testhost"
        let root = dir.path();

        // own host
        put_note(&root.join("bn").join("main"), "bn", "main", "testhost");
        // other host → goes to ITS subtree
        put_note(&root.join("bn").join("feat"), "bn", "feat", "OTHERHOST");
        // empty machine → falls back to this host
        put_note(&root.join("bn").join("nomachine"), "bn", "nomachine", "");
        // a sibling design doc must travel with its dir
        std::fs::write(root.join("bn").join("main").join("design.md"), "doc").unwrap();
        // already migrated (depth-3) → must NOT move
        put_note(
            &root.join("testhost").join("bn").join("done"),
            "bn",
            "done",
            "testhost",
        );
        // collision: dest already exists → skip, leave source
        put_note(
            &root.join("testhost").join("bn").join("collide"),
            "bn",
            "collide",
            "testhost",
        );
        put_note(
            &root.join("bn").join("collide"),
            "bn",
            "collide",
            "testhost",
        );

        let r = migrate_layout(&c, false, false);
        assert_eq!(r.failed.len(), 0, "{:#?}", r.failed);
        assert!(root
            .join("testhost")
            .join("bn")
            .join("main")
            .join("note.md")
            .exists());
        assert!(root
            .join("testhost")
            .join("bn")
            .join("main")
            .join("design.md")
            .exists());
        assert!(root
            .join("OTHERHOST")
            .join("bn")
            .join("feat")
            .join("note.md")
            .exists());
        assert!(root
            .join("testhost")
            .join("bn")
            .join("nomachine")
            .join("note.md")
            .exists());
        // collision left the old-layout source in place
        assert!(root.join("bn").join("collide").join("note.md").exists());
        assert!(r.skipped.iter().any(|s| s.contains("dest exists")));
        // old-layout sources that moved are gone
        assert!(!root.join("bn").join("main").exists());
        assert!(!root.join("bn").join("feat").exists());
    }

    #[test]
    fn migrate_layout_dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        put_note(
            &dir.path().join("bn").join("main"),
            "bn",
            "main",
            "testhost",
        );
        let r = migrate_layout(&c, true, false);
        assert_eq!(r.moved.len(), 1);
        assert!(dir.path().join("bn").join("main").join("note.md").exists());
        assert!(!dir.path().join("testhost").join("bn").join("main").exists());
    }

    #[test]
    fn migrate_layout_skips_unreadable_note() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let d = dir.path().join("x").join("y");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("note.md"), "no frontmatter fence here\n").unwrap();
        let r = migrate_layout(&c, false, false);
        assert_eq!(r.moved.len(), 0);
        assert_eq!(r.failed.len(), 0);
        assert!(r.skipped.iter().any(|s| s.contains("unreadable")));
        assert!(d.join("note.md").exists(), "left in place");
    }

    #[test]
    fn migrate_layout_force_reconciles_collisions_by_recency() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path()); // hostname "testhost"
        let root = dir.path();

        // Distinguish copies by a valid-frontmatter Goal marker so the reconciliation can
        // still read them (a marker-less body would be "unreadable" and skipped). Write the
        // "older" copies first, sleep past 1s mtime granularity, then the "newer" copies.
        let put_marked = |dir: &std::path::Path, marker: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("note.md"),
                format!("---\nrepo: bn\nbranch: x\ncreated: 2026-06-01\nstatus: active\nmachine: testhost\ntags: []\ncollaborators: []\ntodos: []\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n---\n# bn / x\n\n## Goal\n\n{marker}\n"),
            )
            .unwrap();
        };
        // A: machine copy older; B: old-layout copy older.
        put_marked(&root.join("testhost").join("bn").join("a"), "STALE-A");
        put_marked(&root.join("bn").join("b"), "STALE-B");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // A: old-layout NEWER → it should replace the stale machine copy.
        put_marked(&root.join("bn").join("a"), "WIN-A");
        // B: machine copy NEWER → keep it, remove the old-layout orphan.
        put_marked(&root.join("testhost").join("bn").join("b"), "WIN-B");

        let r = migrate_layout(&c, false, true);
        assert_eq!(r.failed.len(), 0, "{:#?}", r.failed);

        // A: machine-subtree copy now holds the newer old-layout content; old orphan gone.
        let a = std::fs::read_to_string(root.join("testhost").join("bn").join("a").join("note.md"))
            .unwrap();
        assert!(a.contains("WIN-A"), "old-layout (newer) should win: {a}");
        assert!(!root.join("bn").join("a").exists());
        // B: machine copy kept (newer content), old orphan removed.
        let b = std::fs::read_to_string(root.join("testhost").join("bn").join("b").join("note.md"))
            .unwrap();
        assert!(b.contains("WIN-B"), "machine (newer) should win: {b}");
        assert!(!root.join("bn").join("b").exists());
    }
}
