//! Low-level parsing, env, and git helpers shared by the resolver submodules.

use crate::runner::{CommandRunner, RealRunner};
use std::env;
use std::path::Path;

/// Parse the work-repos allowlist (bn:73-82): strip `#`-comments, delete ALL whitespace
/// in each line, drop empties. Missing/unreadable file ⇒ empty (everything personal).
pub(super) fn parse_work_repos(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|line| {
            let no_comment = line.split('#').next().unwrap_or("");
            no_comment
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

pub(super) fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// Split a comma/space/newline-separated env list into trimmed, non-empty entries. Mirrors how
/// `$BN_REGULAR_CLONE_REPOS` is parsed so list-valued env overrides behave consistently.
pub(super) fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse a weekday into `0=Sun .. 6=Sat`: a name (`sun`/`sunday`/… case-insensitive,
/// any prefix ≥3 chars) or a bare number `0..6`. `None` if unrecognized.
pub(super) fn parse_weekday(s: &str) -> Option<u32> {
    let s = s.trim().to_lowercase();
    if let Ok(n) = s.parse::<u32>() {
        return (n <= 6).then_some(n);
    }
    const NAMES: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    NAMES
        .iter()
        .position(|name| name.starts_with(&s) && s.len() >= 3)
        .map(|i| i as u32)
}

/// Cross-platform home dir: `$HOME` on unix, `%USERPROFILE%` on Windows (via `dirs`).
pub(super) fn home() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Machine hostname, cross-platform: `gethostname` reads it directly (no `hostname` binary,
/// which native Windows lacks). `$BN_HOSTNAME` overrides upstream in [`Config::resolve`].
pub(super) fn read_hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string()
}

pub(super) fn git_current_branch(dir: &Path) -> Option<String> {
    git(dir, &["branch", "--show-current"])
}

pub(super) fn git_toplevel(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--show-toplevel"])
}

/// The git *common* dir for `dir` (`git rev-parse --git-common-dir`): the bare repo shared by
/// all linked worktrees, or `.git`/`.bare` for an in-place layout. Authoritative repo identity
/// — unlike the worktree directory path, it's the same wherever the worktree lives. `None`
/// outside a git work tree.
pub(super) fn git_common_dir(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--git-common-dir"])
}

/// Run `git -C <dir> <args>`, returning trimmed stdout if the command succeeds and is
/// non-empty. Any failure (not a repo, git missing) ⇒ None. Routes through the shared
/// [`crate::runner`] seam so it's the same testable path the feature reads use.
pub(super) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let dir_s = dir.to_string_lossy();
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
    full.push("-C");
    full.push(dir_s.as_ref());
    full.extend_from_slice(args);
    RealRunner.git(&full).ok_stdout()
}
