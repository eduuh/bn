//! Ref → path resolution for the `tat` picker: which directory a picker row actually opens.
//!
//! Split out of `tat.rs` so the picker's row-building and tmux plumbing stay separate from the
//! layout rules. The one rule worth knowing: a bare repo's landing spot is its **main slot**
//! (`_pool/1`), never the `main` worktree — see [`crate::main_slot`].

use super::{ref_repo, Roots};
use std::path::PathBuf;

/// Is `ref` a bare repo (i.e. `<ref-root>/bare/<repo>.git` exists)?
pub(super) fn is_bare_repo(roots: &Roots, r: &str) -> bool {
    roots
        .bare_dir(r)
        .join(format!("{}.git", ref_repo(r)))
        .is_dir()
}

/// Where picking a bare repo lands you: its **main slot** (`_pool/1`), not the `main` worktree.
/// See [`crate::main_slot`] for why main-branch work happens one directory over. Creates the slot
/// if the repo hasn't got one, so this is only for callers about to open a window there.
pub(super) fn bare_repo_main_path(roots: &Roots, r: &str) -> PathBuf {
    crate::main_slot::resolve(&roots.bare_dir(r), &roots.worktree_dir(r), ref_repo(r))
}

/// `ref` → path. `ref/branch` → a worktree path; a bare `ref` → the project root checkout.
pub(super) fn resolve_project_path(roots: &Roots, r: &str) -> PathBuf {
    let refpart = r.split_once('/').map(|(a, _)| a).unwrap_or(r);
    match r.split_once('/') {
        Some((_, branch)) => roots
            .worktree_dir(refpart)
            .join(ref_repo(refpart))
            .join(branch),
        None => roots.ref_root(r).join(ref_repo(r)),
    }
}

/// The checkout path for a ref, used for frecency scoring (bare → its landing spot, else the
/// project path). Read-only: this runs for every ref on every repaint, so it names the main slot
/// without creating it — see [`crate::main_slot::landing`].
pub(super) fn project_path(roots: &Roots, r: &str) -> PathBuf {
    if is_bare_repo(roots, r) {
        crate::main_slot::landing(&roots.bare_dir(r), &roots.worktree_dir(r), ref_repo(r))
    } else {
        resolve_project_path(roots, r)
    }
}
