//! The one base-branch resolver: *what should this branch be diffed against?*
//!
//! Before this module the answer was spelled three ways — [`crate::related`]'s worktree ladder,
//! `wt_pool`'s bare-repo ladder, and ~20 hardcoded `origin/main` literals. They disagreed, which
//! matters the moment two of them describe the same change to a human: a `--stat` preview computed
//! against `origin/main` beside a diff view computed against `origin/HEAD` is a picker that lies.
//!
//! Resolution is entirely offline — never `git remote show origin`, which hits the network:
//!   1. `origin/HEAD`'s short name — the remote's default branch, when recorded locally.
//!   2. the current branch's upstream (e.g. `origin/develop`) — the real default on a repo whose
//!      default branch is custom *and* whose `origin/HEAD` was never set (`git remote set-head`).
//!   3. `origin/main`, `main`, `origin/master`, `master`, first that resolves.
//!
//! `None` when none resolve, so a caller degrades with a diagnostic instead of diffing against a
//! literal `main` that doesn't exist — which silently yields an empty result rather than an error.

use crate::runner::CommandRunner;
use std::path::Path;

/// Where to resolve a base ref. Both forms scope the underlying `git` calls explicitly rather than
/// relying on the process cwd, so one resolver serves a checked-out worktree and a bare clone.
#[derive(Debug, Clone, Copy)]
pub enum BaseScope<'a> {
    /// A checked-out worktree — scoped with `git -C <dir>`.
    Worktree(&'a Path),
    /// A bare repo (`<root>/bare/<repo>.git`) — scoped with `git --git-dir <dir>`.
    BareRepo(&'a Path),
}

impl BaseScope<'_> {
    /// The argv prefix that scopes a `git` call to this location.
    fn prefix(&self) -> Vec<String> {
        match self {
            BaseScope::Worktree(p) => vec!["-C".to_string(), p.display().to_string()],
            BaseScope::BareRepo(p) => vec!["--git-dir".to_string(), p.display().to_string()],
        }
    }
}

/// Candidate refs tried in order once `origin/HEAD` and the upstream have both declined.
const FALLBACKS: [&str; 4] = ["origin/main", "main", "origin/master", "master"];

/// Resolve the base ref to diff against at `at`, or `None` when the repo offers no answer.
pub fn base_ref(runner: &dyn CommandRunner, at: BaseScope<'_>) -> Option<String> {
    let scope = at.prefix();
    let git = |args: &[&str]| {
        let mut argv: Vec<&str> = scope.iter().map(String::as_str).collect();
        argv.extend_from_slice(args);
        runner.git(&argv)
    };

    if let Some(sym) = git(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .ok_stdout()
    .filter(|s| !s.is_empty())
    {
        if git(&["rev-parse", "--verify", "--quiet", &sym]).success {
            return Some(sym);
        }
    }

    // A bare repo has no meaningful `@{upstream}`; the call simply fails there and costs one spawn.
    if let Some(upstream) = git(&["rev-parse", "--abbrev-ref", "@{upstream}"]).ok_stdout() {
        if git(&["rev-parse", "--verify", "--quiet", &upstream]).success {
            return Some(upstream);
        }
    }

    FALLBACKS
        .iter()
        .find(|r| git(&["rev-parse", "--verify", "--quiet", r]).success)
        .map(|r| (*r).to_string())
}

/// The three-dot range this feature diffs over: changes on `HEAD` since it diverged from `base`,
/// ignoring commits `base` collected afterwards. Two-dot would report main's own new commits as
/// "your changes" on any worktree that has fallen behind.
pub fn range_to_head(base: &str) -> String {
    format!("{base}...HEAD")
}
