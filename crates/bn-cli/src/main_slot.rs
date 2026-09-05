//! The **main slot** — `<root>/worktree/<repo>/_pool/1` — and the two rules that make it work:
//! it is the checkout you land in when you pick "main", and the `worktree/<repo>/main` checkout is
//! not somewhere you go at all.
//!
//! The split exists because those two checkouts want incompatible things. `worktree/<repo>/main`
//! is the stow source, the shape the repo's `pre-commit` hook refuses commits in, and what every
//! tool that wants "a clean checkout of main" reads — it has to *exist*, on the `main` branch,
//! current and clean. But it was also the obvious place to land, so it collected edits, and a
//! dirty main can neither fast-forward nor serve as a trustworthy base: work cut from it starts
//! stale, and `main-sync` cannot advance it.
//!
//! So main-branch work moves one directory over. Slot [`bn_core::agent::MAIN_POOL_SLOT`] is
//! already reserved — [`bn_core::pool::pick_slot`] hands workers `2..=size` and never this one —
//! so it is free to be the place you sit while reading main, running a build, or cutting the next
//! branch. Git cannot check `main` out twice, so the slot sits **detached** at the same commit:
//! the same files, none of the branch's obligations.
//!
//! Every repo has one, unconditionally — pooling has no register and no opt-out. The pickers ask
//! [`resolve`] for a repo's landing spot; the `main-sync` job ([`crate::cmd::main_sync`]) keeps
//! both checkouts current.

use std::path::{Path, PathBuf};

/// Where a repo's main slot lives: `<worktree_dir>/<repo>/_pool/1`.
pub(crate) fn path(worktree_dir: &Path, repo: &str) -> PathBuf {
    worktree_dir
        .join(repo)
        .join("_pool")
        .join(bn_core::agent::MAIN_POOL_SLOT.to_string())
}

/// The repo's main worktree — `<worktree_dir>/<repo>/main`. Named `main` whatever the repo's
/// default branch is: that is the directory convention the whole layout (and
/// [`bn_core::agent::is_main_worktree_path`]) is keyed on, not a claim about the branch inside it.
pub(crate) fn main_worktree(worktree_dir: &Path, repo: &str) -> PathBuf {
    worktree_dir.join(repo).join("main")
}

/// A live main slot for `repo`, creating it if it isn't on disk yet.
///
/// `None` when the repo cannot have one — no bare clone, or a `worktree add` that failed — and
/// every caller has a fallback for that case rather than a failure, because a missing main slot
/// must never be the thing that stops you opening a repo.
pub(crate) fn ensure(bare: &Path, worktree_dir: &Path, repo: &str) -> Option<PathBuf> {
    if !bare.is_dir() {
        return None;
    }
    let slot = path(worktree_dir, repo);
    if crate::wt_pool::is_live_slot(&slot) {
        return Some(slot);
    }
    // A directory that isn't a live checkout is the debris of an interrupted `worktree add` — the
    // files are there but the `.git` link isn't, so every git command in it fails. Tear it down
    // rather than hand it back.
    let bare_s = bare.to_string_lossy().to_string();
    if slot.exists() {
        crate::wt_pool::force_remove_worktree(&bare_s, &slot);
    }
    let _ = std::fs::create_dir_all(slot.parent()?);
    crate::wt_pool::add_detached_slot(&bare_s, &slot.to_string_lossy()).then_some(slot)
}

/// Where picking `repo` (or its `main` branch) should land you: the main slot, else the main
/// worktree, else the bare dir. The first two may be missing on a repo bn has not finished laying
/// out yet, and the caller still needs somewhere real to open a shell.
///
/// **Creates the slot** if the repo hasn't got one, so this is for callers that are about to open
/// something. Anything that merely wants to *name* the path — the picker's frecency scoring, a
/// preview pane — must call [`landing`] instead: those run on every repaint, and cutting a
/// worktree as a side effect of drawing a list is not a side effect anyone asked for.
pub(crate) fn resolve(bare_dir: &Path, worktree_dir: &Path, repo: &str) -> PathBuf {
    let bare = bare_dir.join(format!("{repo}.git"));
    match ensure(&bare, worktree_dir, repo) {
        Some(slot) => slot,
        None => landing(bare_dir, worktree_dir, repo),
    }
}

/// [`resolve`] without the creation: the same three candidates, but a main slot that isn't on disk
/// yet is simply passed over. Read-only, so it is safe on a hot path.
pub(crate) fn landing(bare_dir: &Path, worktree_dir: &Path, repo: &str) -> PathBuf {
    let slot = path(worktree_dir, repo);
    if slot.is_dir() {
        return slot;
    }
    let main = main_worktree(worktree_dir, repo);
    if main.is_dir() {
        return main;
    }
    bare_dir.join(format!("{repo}.git"))
}

/// Whether a picker row naming `branch` means "take me to main" rather than "open a worktree".
/// The main worktree is not a destination, so `main`/`master` is a request for the main slot.
pub(crate) fn is_main_selection(branch: &str) -> bool {
    bn_core::agent::is_reserved_branch(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_slot_is_slot_one_under_pool() {
        let p = path(Path::new("/w"), "bn");
        assert!(p.ends_with("bn/_pool/1"), "{}", p.display());
    }

    #[test]
    fn main_and_master_are_requests_for_the_main_slot() {
        assert!(is_main_selection("main"));
        assert!(is_main_selection("master"));
        assert!(!is_main_selection("feature/main-thing"));
    }

    #[test]
    fn landing_never_creates_anything() {
        // The picker scores every repo on every repaint through this. Cutting a worktree as a
        // side effect of drawing a list is exactly the bug the read-only split exists to prevent.
        let tmp = tempfile::tempdir().unwrap();
        let (bare_dir, worktree_dir) = (tmp.path().join("bare"), tmp.path().join("worktree"));
        assert_eq!(
            landing(&bare_dir, &worktree_dir, "bn"),
            bare_dir.join("bn.git")
        );
        assert!(!worktree_dir.exists());
    }

    #[test]
    fn resolve_falls_back_to_the_main_worktree_then_the_bare_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (bare_dir, worktree_dir) = (root.join("bare"), root.join("worktree"));

        // No bare clone at all: nothing to create a slot from, and no main worktree either.
        assert_eq!(
            resolve(&bare_dir, &worktree_dir, "bn"),
            bare_dir.join("bn.git")
        );

        // A main worktree exists but the repo has no usable bare clone — land there.
        let main = main_worktree(&worktree_dir, "bn");
        std::fs::create_dir_all(&main).unwrap();
        assert_eq!(resolve(&bare_dir, &worktree_dir, "bn"), main);
    }

    /// The one remaining way a repo has no main slot: bn has no clone of it. There is no pooling
    /// opt-out to check any more.
    #[test]
    fn a_repo_with_no_bare_clone_gets_no_main_slot() {
        let _g = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare").join("never-cloned.git");
        assert_eq!(
            ensure(&bare, &tmp.path().join("worktree"), "never-cloned"),
            None
        );
    }
}
