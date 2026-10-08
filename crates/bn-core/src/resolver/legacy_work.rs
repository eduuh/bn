//! Fold the retired `~/projects/branch-notes-work/bn` tree into the one notes root.
//!
//! bn used to fall back to that tree as its work root whenever `~/projects/notes/bn` was absent, so
//! the person-scoped stores (`tasks.txt`, `tasks/`, `links.md`, `references/`) landed in a
//! directory that is not a git repo and never synced. bn now reads a single root
//! (`~/projects/branch-notes`); this module carries whatever the old tree holds across, once.
//!
//! Crash-safe and idempotent: the legacy dir is first renamed to a fresh `<legacy>.migrated[-N]`
//! (an atomic claim, so two bn processes never both merge it), the merge only copies, and a
//! `MIGRATED` marker is written last. A run that died midway finds a claim without the marker and
//! merges it again, which converges because every step is a union. An older bn that recreates the
//! legacy dir gets it claimed and merged the same way. The claimed dirs stay as backups.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Line-oriented stores whose two copies are unioned rather than one winning.
const UNION_FILES: &[&str] = &["tasks.txt", "links.md"];

const MARKER: &str = "MIGRATED";

/// Merge `<legacy>/bn` into `root` and retire `legacy`. `legacy` is the `branch-notes-work` dir
/// itself, not its `bn/` child, so the whole retired tree is renamed aside.
pub(super) fn migrate(legacy: &Path, root: &Path) {
    if legacy.is_dir() {
        let _ = std::fs::rename(legacy, unused_claim(legacy));
    }
    for claimed in claims(legacy) {
        if claimed.join(MARKER).exists() {
            continue;
        }
        let src = claimed.join("bn");
        if src.is_dir() && merge_dir(&src, root, true).is_err() {
            // Leave the marker unwritten so the next run retries.
            continue;
        }
        let _ = std::fs::write(
            claimed.join(MARKER),
            format!("merged into {}\n", root.display()),
        );
    }
}

fn claim_prefix(legacy: &Path) -> String {
    format!(
        "{}.migrated",
        legacy.file_name().unwrap_or_default().to_string_lossy()
    )
}

/// `<legacy>.migrated`, or `<legacy>.migrated-N` for the first N not yet taken.
fn unused_claim(legacy: &Path) -> PathBuf {
    let prefix = claim_prefix(legacy);
    (1..)
        .map(|n| match n {
            1 => legacy.with_file_name(&prefix),
            n => legacy.with_file_name(format!("{prefix}-{n}")),
        })
        .find(|p| !p.exists())
        .unwrap_or_else(|| legacy.with_file_name(&prefix))
}

/// Every claimed dir beside `legacy`, oldest name first.
fn claims(legacy: &Path) -> Vec<PathBuf> {
    let prefix = claim_prefix(legacy);
    let Some(parent) = legacy.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix(&prefix)?;
            let n = match rest {
                "" => 1,
                _ => rest.strip_prefix('-')?.parse().ok()?,
            };
            e.path().is_dir().then(|| (n, e.path()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

/// Copy `src` into `dst` without overwriting. At the `top` level the [`UNION_FILES`] are unioned
/// instead; a nested file of the same name is an ordinary file.
fn merge_dir(src: &Path, dst: &Path, top: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            merge_dir(&from, &to, false)?;
        } else if top && to.is_file() && UNION_FILES.iter().any(|f| entry.file_name() == *f) {
            union_lines(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Rewrite `dst` as the legacy lines followed by the lines only `dst` has. Legacy first because it
/// was the live store: in `tasks.txt` its ids win when [`crate::todo::TodoFile::heal_ids`] meets a
/// collision, so `doc:` attachments keep pointing at the task they were written for.
fn union_lines(src: &Path, dst: &Path) -> std::io::Result<()> {
    let legacy = std::fs::read_to_string(src)?;
    let current = std::fs::read_to_string(dst)?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = String::new();
    for line in legacy.lines().chain(current.lines()) {
        if line.trim().is_empty() || seen.insert(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    let tmp = dst.with_extension("migrating");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_legacy_tree_is_merged_into_the_root_and_set_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("branch-notes-work");
        let root = tmp.path().join("branch-notes");
        write(&legacy.join("bn/tasks.txt"), "work a id:t2\nshared id:t3\n");
        write(&legacy.join("bn/links.md"), "- https://a\n");
        write(&legacy.join("bn/tasks/t2.md"), "doc");
        write(&legacy.join("bn/references/rust.md"), "legacy ref");
        write(&root.join("tasks.txt"), "shared id:t3\npersonal id:t2\n");
        write(&root.join("references/rust.md"), "kept ref");

        migrate(&legacy, &root);

        assert_eq!(
            read(&root.join("tasks.txt")),
            "work a id:t2\nshared id:t3\npersonal id:t2\n"
        );
        assert_eq!(read(&root.join("links.md")), "- https://a\n");
        assert_eq!(read(&root.join("tasks/t2.md")), "doc");
        assert_eq!(read(&root.join("references/rust.md")), "kept ref");
        assert!(!legacy.exists(), "nothing reads the legacy path any more");
        let claimed = tmp.path().join("branch-notes-work.migrated");
        assert!(claimed.join(MARKER).is_file());
        assert_eq!(
            read(&claimed.join("bn/tasks/t2.md")),
            "doc",
            "kept as backup"
        );
    }

    #[test]
    fn a_work_only_machine_gets_its_root_created() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("branch-notes-work");
        let root = tmp.path().join("branch-notes");
        write(&legacy.join("bn/tasks.txt"), "only id:t1\n");

        migrate(&legacy, &root);

        assert_eq!(read(&root.join("tasks.txt")), "only id:t1\n");
    }

    #[test]
    fn an_interrupted_migration_resumes_without_duplicating() {
        let tmp = tempfile::tempdir().unwrap();
        let claimed = tmp.path().join("branch-notes-work.migrated");
        let legacy = tmp.path().join("branch-notes-work");
        let root = tmp.path().join("branch-notes");
        write(&claimed.join("bn/tasks.txt"), "a id:t1\n");
        write(&root.join("tasks.txt"), "a id:t1\nb id:t2\n");

        migrate(&legacy, &root);
        migrate(&legacy, &root);

        assert_eq!(read(&root.join("tasks.txt")), "a id:t1\nb id:t2\n");
        assert!(claimed.join(MARKER).is_file());
    }

    #[test]
    fn a_finished_migration_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let claimed = tmp.path().join("branch-notes-work.migrated");
        let root = tmp.path().join("branch-notes");
        write(&claimed.join("bn/tasks.txt"), "a id:t1\n");
        write(&claimed.join(MARKER), "");

        migrate(&tmp.path().join("branch-notes-work"), &root);

        assert!(!root.exists());
    }

    /// An older bn that still writes to the legacy path recreates it; that copy is merged too.
    #[test]
    fn a_recreated_legacy_dir_is_merged_again() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("branch-notes-work");
        let root = tmp.path().join("branch-notes");
        write(&legacy.join("bn/tasks.txt"), "a id:t1\n");
        migrate(&legacy, &root);
        write(&legacy.join("bn/tasks.txt"), "a id:t1\nb id:t2\n");

        migrate(&legacy, &root);

        assert_eq!(read(&root.join("tasks.txt")), "a id:t1\nb id:t2\n");
        assert!(!legacy.exists());
        let second = tmp.path().join("branch-notes-work.migrated-2");
        assert!(second.join(MARKER).is_file());
    }
}
