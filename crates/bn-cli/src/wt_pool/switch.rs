//! Switching a branch into a leased slot checkout in place.
//!
//! Split out of `wt_pool.rs` under the 1000-line ratchet.
use super::*;

/// Switch `branch` into an existing slot checkout (reusing its warm deps). Mirrors the
/// heads / remotes / new three-way logic in `create_worktree`, but via `git switch` in place.
#[cfg(test)]
pub(super) fn switch_into_slot(slot_s: &str, bare_s: &str, branch: &str) -> bool {
    switch_into_slot_from(slot_s, bare_s, branch, None)
}

pub(super) fn switch_into_slot_from(
    slot_s: &str,
    bare_s: &str,
    branch: &str,
    base: Option<&str>,
) -> bool {
    let exists = |kind: &str| {
        wt::git_out(&[
            "--git-dir",
            bare_s,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/{kind}/{branch}"),
        ])
        .is_some()
    };
    if exists("heads") {
        git_quiet(&["-C", slot_s, "switch", branch])
    } else if exists("remotes/origin") {
        git_quiet(&[
            "-C",
            slot_s,
            "switch",
            "-c",
            branch,
            &format!("origin/{branch}"),
        ])
    } else {
        let start = base
            .map(str::to_string)
            .unwrap_or_else(|| default_base(bare_s));
        git_quiet(&["-C", slot_s, "switch", "-c", branch, &start])
    }
}
