//! Per-worktree gh-account pinning policy (#456).
//!
//! `gh`'s *active account* is global: it lives in one config dir (default `~/.config/gh`, or
//! `$GH_CONFIG_DIR`), and `gh auth switch --user <x>` rewrites the `user:` there. Under multi-repo
//! orchestration that's a cross-lane hazard — the work account 404s a private repo, so a dispatched
//! agent flips to the personal account with `gh auth switch`, and every *sibling* lane sharing the
//! one config dir is silently flipped under it, breaking its in-flight `gh` ops. bn's own gh reads
//! (PR status polling, merges) run against that same shared config, so they break too.
//!
//! The fix is to pin each worktree to its *own* `GH_CONFIG_DIR`, seeded from the shared config, so
//! an agent's `gh auth switch` is scoped to that worktree alone and can't touch a sibling. This
//! module is the **pure** policy — where a worktree's pinned dir lives and which files carry the
//! auth state to seed it — so it's unit-tested without touching gh or the filesystem; the CLI does
//! the copy and threads `GH_CONFIG_DIR` into the launched agent's environment.

use std::path::{Path, PathBuf};

/// The gh config files that carry the auth + general state a pinned dir must be seeded with.
/// `hosts.yml` holds the per-host account selection — the `user:` a `gh auth switch` rewrites —
/// and `config.yml` holds general settings (aliases, editor, protocol). The OAuth token itself
/// lives in the OS keyring (gh's secure storage) keyed by host and is shared across config dirs,
/// so copying these two replicates the *account selection* without duplicating the secret. When
/// gh is in insecure mode the token is inline in `hosts.yml`, which the copy still carries.
pub const GH_STATE_FILES: [&str; 2] = ["hosts.yml", "config.yml"];

/// A worktree's pinned `GH_CONFIG_DIR`: `<base>/<repo>/<branch>`. `base` must be a machine-LOCAL
/// root (e.g. under `~/.bn`) — never the synced notes tree, since in gh's insecure mode the seeded
/// `hosts.yml` carries a token.
///
/// Keyed by `(repo, branch)` so every worktree is isolated: a `gh auth switch` inside one worktree
/// writes only to its own dir, leaving sibling lanes — each resolving to a *different* dir — wholly
/// untouched. `repo` and `branch` are folded to a single path segment each (any `/` or `\` → `-`),
/// matching the worktree dir's own `branch.replace('/', "-")` sanitization so `feature/x` maps to
/// one directory, not a nested tree.
pub fn worktree_gh_config_dir(base: &Path, repo: &str, branch: &str) -> PathBuf {
    base.join(sanitize_segment(repo))
        .join(sanitize_segment(branch))
}

/// Fold a repo/branch name into one filesystem-safe path segment: any path separator (`/` or `\`)
/// becomes `-`. Mirrors the worktree layout's slash-flattening so the pinned dir is a sibling-keyed
/// leaf, not an accidental subtree.
fn sanitize_segment(s: &str) -> String {
    s.replace(['/', '\\'], "-")
}


