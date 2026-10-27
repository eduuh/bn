//! Leftover package directories: what upstream deleted but a slot's build output kept alive.
//!
//! When main deletes or moves a package, a reset removes its tracked files but not its ignored
//! build output (`node_modules/`, `lib/`), because `clean -df` never touches ignored files. The
//! directory survives, and once the package's own `.gitignore` is gone some of that output is no
//! longer ignored, so `git status` shows `?? packages/<name>/`. `dirty_count` then reads the
//! slot as dirty, every warm skips it, and `wt pool return` refuses it: on 2026-09-27 that parked
//! three of a cloud PC's monorepo slots for good.
//!
//! The return path makes these without noticing: it probes untracked paths *before* the reset (to
//! save a second 3-12 s status) and so never sees what the reset itself orphaned.
//!
//! Two narrow repairs, both for a directory that is a package root (`…/packages/<name>`), has no
//! tracked path under it in HEAD, and is not its own git repo:
//! * [`sweep_deleted_packages`] — after a reset moved HEAD, remove the package roots the move
//!   deleted. Cheap (a tree diff), and runs only on a slot every caller has already cleared or is
//!   deliberately discarding.
//! * [`clear_leftovers`] — for warm, on an idle slot that is dirty *only* because of such
//!   directories, remove them so the slot can be warmed. Any other dirt (a modified, staged or
//!   stray untracked file) leaves the slot untouched and dirty.
use super::*;

/// Whether `rel` (slot-relative, no trailing slash) names a package root: `…/packages/<name>`.
fn is_package_root(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    parts.len() >= 2
        && parts[parts.len() - 2] == "packages"
        && !parts[parts.len() - 1].is_empty()
        && parts
            .iter()
            .all(|p| !p.is_empty() && *p != "." && *p != "..")
}

/// The package roots a tracked file path sits in, shallowest first.
fn package_roots_of(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (0..parts.len().saturating_sub(2))
        .filter(|&i| parts[i] == "packages")
        .map(|i| parts[..=i + 1].join("/"))
        .collect()
}

/// Whether `rel` is a package root that may be deleted wholesale: nothing tracked under it in HEAD,
/// a real directory (not a symlink), not a nested git repo someone cloned there, and no
/// `package.json` at its root. That last one is what tells a leftover from a package someone is
/// writing: the manifest was tracked, so the reset that orphaned the directory removed it, while a
/// new package has one from its first minute. A git error answers "tracked", so a probe that
/// cannot run never deletes anything.
fn removable(slot_s: &str, rel: &str) -> bool {
    let dir = Path::new(slot_s).join(rel);
    is_package_root(rel)
        && std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir())
        && !dir.join(".git").exists()
        && !dir.join("package.json").exists()
        && wt::git_out(&["-C", slot_s, "ls-tree", "--name-only", "HEAD", "--", rel])
            .is_some_and(|out| out.is_empty())
}

fn remove(slot_s: &str, rel: &str) -> bool {
    std::fs::remove_dir_all(Path::new(slot_s).join(rel)).is_ok()
}

/// After a reset from `before` to `after`, remove the package roots that move deleted. Returns the
/// ones removed.
pub(super) fn sweep_deleted_packages(slot_s: &str, before: &str, after: &str) -> Vec<String> {
    if before == after {
        return Vec::new();
    }
    let args = [
        "-C",
        slot_s,
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=D",
        before,
        after,
    ];
    let Some(deleted) = wt::git_out(&args) else {
        return Vec::new();
    };
    let roots: BTreeSet<String> = deleted.lines().flat_map(package_roots_of).collect();
    roots
        .into_iter()
        .filter(|rel| removable(slot_s, rel) && remove(slot_s, rel))
        .collect()
}

/// The untracked package roots in `porcelain`, if they are *all* of its entries and every one is
/// [`removable`]. `None` as soon as any entry is something else — that slot holds real dirt.
fn leftover_dirs(slot_s: &str, porcelain: &str) -> Option<Vec<String>> {
    let mut dirs = Vec::new();
    for line in porcelain.lines().filter(|l| !l.trim().is_empty()) {
        let rel = line.strip_prefix("?? ")?.strip_suffix('/')?;
        if rel.starts_with('"') || !removable(slot_s, rel) {
            return None;
        }
        dirs.push(rel.to_string());
    }
    Some(dirs)
}

fn status(slot_s: &str) -> Option<String> {
    wt::git_out(&[
        "-C",
        slot_s,
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ])
}

/// Warm's dirty gate for an idle slot (already checked: not leased, no live occupant). True when
/// the slot is clean, possibly after removing leftover package directories; false when it holds
/// anything else, or git cannot say.
pub(super) fn clear_leftovers(n: u32, slot_s: &str) -> bool {
    let Some(before) = status(slot_s) else {
        return false;
    };
    if before.is_empty() {
        return true;
    }
    let Some(dirs) = leftover_dirs(slot_s, &before) else {
        return false;
    };
    let _lock =
        slot_parts(Path::new(slot_s)).and_then(|(_, n, pdir)| slot_lock::lock_slot(&pdir, n));
    for rel in &dirs {
        if remove(slot_s, rel) {
            eprintln!("{YELLOW}[{n}] removed leftover {rel}/ (not in HEAD){NC}");
        }
    }
    status(slot_s).is_some_and(|s| s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::super::git_integration_tests::{git, mk_bare};
    use super::*;

    #[test]
    fn package_roots_are_recognised() {
        assert!(is_package_root("packages/name-pronunciation"));
        assert!(is_package_root("packages/foo"));
        assert!(!is_package_root("packages"));
        assert!(!is_package_root("docs/notes"));
        assert!(!is_package_root("packages/foo/lib"));
        assert!(!is_package_root("packages/.."));
        assert_eq!(package_roots_of("packages/foo/src/a.ts"), ["packages/foo"]);
        assert_eq!(
            package_roots_of("packages/a/packages/b/x"),
            ["packages/a", "packages/a/packages/b"]
        );
        assert!(package_roots_of("packages/README.md").is_empty());
    }

    fn commit_all(dir: &str, msg: &str) {
        assert!(git(&["-C", dir, "add", "-A"]));
        assert!(git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-C",
            dir,
            "commit",
            "-qm",
            msg
        ]));
    }

    /// A bare repo whose main has `packages/gone` (with its own `.gitignore` for `lib/`)
    /// and then a commit deleting it. Returns the tempdir, bare path and the two commits.
    fn repo_with_deleted_package() -> (tempfile::TempDir, String, String, String) {
        let (tmp, bare) = mk_bare();
        let other = tmp.path().join("other");
        let o = other.to_string_lossy().to_string();
        assert!(git(&[
            "--git-dir",
            &bare,
            "worktree",
            "add",
            "-q",
            &o,
            "main"
        ]));
        let pkg = other.join("packages/gone");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(pkg.join("src/index.ts"), "x\n").unwrap();
        std::fs::write(pkg.join(".gitignore"), "lib/\n").unwrap();
        std::fs::create_dir_all(other.join("packages/kept")).unwrap();
        std::fs::write(other.join("packages/kept/a.ts"), "k\n").unwrap();
        commit_all(&o, "add packages");
        let with = wt::git_out(&["-C", &o, "rev-parse", "HEAD"]).unwrap();
        std::fs::remove_dir_all(&pkg).unwrap();
        commit_all(&o, "delete gone");
        let without = wt::git_out(&["-C", &o, "rev-parse", "HEAD"]).unwrap();
        assert!(git(&[
            "--git-dir",
            &bare,
            "worktree",
            "remove",
            "--force",
            &o
        ]));
        (tmp, bare, with, without)
    }

    /// A slot at `with`, holding the package's build output, as a tenant leaves it.
    fn built_slot(tmp: &tempfile::TempDir, bare: &str, with: &str) -> (PathBuf, String) {
        let slot = tmp.path().join("_pool").join("2");
        let s = slot.to_string_lossy().to_string();
        assert!(add_detached_slot(bare, &s));
        assert!(git(&["-C", &s, "reset", "-q", "--hard", with]));
        let pkg = slot.join("packages/gone");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(pkg.join("lib/index.js"), "built\n").unwrap();
        std::fs::create_dir_all(slot.join("target")).unwrap(); // root-ignored
        std::fs::write(slot.join("target/cache"), "keep\n").unwrap();
        (slot, s)
    }

    /// The bug as it happened: a plain `reset --hard` onto a main that deleted the package leaves
    /// its unignored build output behind, and the slot reads dirty.
    #[test]
    fn a_reset_past_a_deleted_package_leaves_it_dirty_without_the_sweep() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare, with, without) = repo_with_deleted_package();
        let (_slot, s) = built_slot(&tmp, &bare, &with);
        assert!(git(&["-C", &s, "reset", "-q", "--hard", &without]));
        assert_eq!(
            status(&s).unwrap(),
            "?? packages/gone/",
            "the leftover is the only dirt"
        );
    }

    #[test]
    fn warm_reset_sweeps_a_package_the_new_base_deleted() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare, with, _without) = repo_with_deleted_package();
        let (slot, s) = built_slot(&tmp, &bare, &with);
        // The return path: probed clean before the reset, so it passes an empty untracked list.
        warm_reset_with(&s, &bare, Some(Vec::new()));
        assert!(!slot.join("packages/gone").exists());
        assert!(slot.join("packages/kept/a.ts").exists());
        assert!(slot.join("target/cache").exists(), "root build cache kept");
        assert_eq!(wt::dirty_count(&s), 0);
    }

    #[test]
    fn warm_clears_an_idle_slot_dirty_only_with_leftover_packages() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare, with, without) = repo_with_deleted_package();
        let (slot, s) = built_slot(&tmp, &bare, &with);
        assert!(git(&["-C", &s, "reset", "-q", "--hard", &without]));
        assert!(clear_leftovers(2, &s));
        assert!(!slot.join("packages/gone").exists());
        assert!(slot.join("target/cache").exists());
    }

    #[test]
    fn warm_keeps_a_slot_with_any_other_dirt() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare, with, without) = repo_with_deleted_package();
        let (slot, s) = built_slot(&tmp, &bare, &with);
        assert!(git(&["-C", &s, "reset", "-q", "--hard", &without]));
        let gone = slot.join("packages/gone");

        // A stray untracked file beside the leftover: nothing is removed.
        std::fs::write(slot.join("notes.md"), "mine\n").unwrap();
        assert!(!clear_leftovers(2, &s));
        assert!(gone.exists() && slot.join("notes.md").exists());
        std::fs::remove_file(slot.join("notes.md")).unwrap();

        // A modified tracked file.
        std::fs::write(slot.join("a.txt"), "edit\n").unwrap();
        assert!(!clear_leftovers(2, &s));
        assert!(gone.exists());
        assert!(git(&["-C", &s, "checkout", "--", "a.txt"]));

        // A staged new file.
        std::fs::write(slot.join("new.txt"), "n\n").unwrap();
        assert!(git(&["-C", &s, "add", "new.txt"]));
        assert!(!clear_leftovers(2, &s));
        assert!(gone.exists());
        assert!(git(&["-C", &s, "rm", "-q", "--cached", "new.txt"]));
        std::fs::remove_file(slot.join("new.txt")).unwrap();

        // An untracked directory that is not a package root.
        std::fs::create_dir_all(slot.join("docs/draft")).unwrap();
        std::fs::write(slot.join("docs/draft/x.md"), "d\n").unwrap();
        assert!(!clear_leftovers(2, &s));
        assert!(gone.exists() && slot.join("docs/draft/x.md").exists());
        std::fs::remove_dir_all(slot.join("docs")).unwrap();

        // A new package someone is writing: untracked, not in HEAD, but it has a manifest.
        std::fs::create_dir_all(slot.join("packages/fresh/src")).unwrap();
        std::fs::write(slot.join("packages/fresh/package.json"), "{}\n").unwrap();
        std::fs::write(slot.join("packages/fresh/src/a.ts"), "wip\n").unwrap();
        assert!(!clear_leftovers(2, &s));
        assert!(gone.exists() && slot.join("packages/fresh/package.json").exists());
        std::fs::remove_dir_all(slot.join("packages/fresh")).unwrap();

        // A nested git repo under a package path.
        assert!(git(&["init", "-q", &gone.to_string_lossy()]));
        assert!(!clear_leftovers(2, &s));
        assert!(gone.join(".git").exists());
    }

    #[test]
    fn a_new_untracked_package_with_a_tracked_sibling_path_is_kept() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare, _with, without) = repo_with_deleted_package();
        let slot = tmp.path().join("_pool").join("3");
        let s = slot.to_string_lossy().to_string();
        assert!(add_detached_slot(&bare, &s));
        assert!(git(&["-C", &s, "reset", "-q", "--hard", &without]));
        // Untracked work inside a package that *is* tracked shows as its files, not the root.
        std::fs::write(slot.join("packages/kept/new.ts"), "wip\n").unwrap();
        assert!(!clear_leftovers(3, &s));
        assert!(slot.join("packages/kept/new.ts").exists());
    }
}
