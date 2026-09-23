//! Does this checkout hold commits that exist nowhere else?
//!
//! Split out of `cleanup.rs` so the patch-equivalence logic is testable on its own — and because
//! counting "work only this disk has" is the single decision a sweep must not get wrong in the
//! direction of deleting.

use super::common::git_out;
use std::path::Path;

/// The upstream to test patch-equivalence against: the remote's default branch.
///
/// `origin/HEAD` is the honest answer when the symbolic ref is set; the two fallbacks cover a
/// clone where it never was. `None` when none of them resolve, which makes the caller keep the
/// by-SHA count rather than guess.
fn default_upstream(wt: &str) -> Option<String> {
    let head = git_out(&[
        "-C",
        wt,
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    if head.is_some() {
        return head;
    }
    ["origin/main", "origin/master"].into_iter().find_map(|r| {
        git_out(&["-C", wt, "rev-parse", "--verify", "--quiet", r])
            .filter(|s| !s.trim().is_empty())
            .map(|_| r.to_string())
    })
}

/// How many commits `git cherry` reports as genuinely absent upstream.
///
/// `git cherry` prefixes a commit with `+` when no equivalent patch exists upstream and `-` when
/// one does. Pure so the parse is tested rather than re-derived.
fn unmerged_in_cherry(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with('+'))
        .count()
}

/// Commits reachable from HEAD that are on no remote — the work a detached scratch checkout would
/// take with it. `commits_ahead` can't answer this: it counts against `main`, and a sub-agent's
/// worktree is cut from an arbitrary HEAD. Unreadable output counts as 1 (something to lose), so a
/// git failure keeps the worktree.
///
/// Counting by SHA alone over-reports, and the over-report is the common case rather than an edge
/// one. A squash merge replaces a branch's commits with a single new commit and the branch is
/// deleted on merge, so the originals are on no remote by SHA even though their content shipped.
/// Every worktree for a merged PR then looks like it is holding work that exists nowhere else, and
/// a sweep keeps 24GB of checkouts whose commits are all on `main`.
///
/// So: ask by SHA first, because it is one cheap call and answers zero most of the time. Only when
/// it reports something do we pay for `git cherry`, which compares by patch-id and therefore sees
/// through the squash. Anything that fails falls back to the by-SHA count — never to zero.
pub(super) fn local_only_commits(wt: &Path) -> usize {
    let wt = wt.to_string_lossy().to_string();
    let by_sha = match git_out(&["-C", &wt, "rev-list", "--count", "HEAD", "--not", "--remotes"]) {
        Some(n) => n.trim().parse().unwrap_or(1),
        None => return 1,
    };
    if by_sha == 0 {
        return 0;
    }
    let Some(upstream) = default_upstream(&wt) else {
        return by_sha;
    };
    match git_out(&["-C", &wt, "cherry", &upstream, "HEAD"]) {
        Some(out) => unmerged_in_cherry(&out),
        None => by_sha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cherry_counts_only_commits_absent_upstream() {
        // `-` is "an equivalent patch is already upstream" — the squash-merge case that used to
        // be counted as work this checkout alone was holding.
        let out = "- 4be2729c feat: add black tax finance bucket\n\
                   - dfbfb44e feat: track monthly usd to kes rates\n";
        assert_eq!(unmerged_in_cherry(out), 0);
    }

    #[test]
    fn cherry_counts_genuinely_local_commits() {
        let out = "+ b919da6d feat: something nobody has seen\n\
                   - 0fb004ac feat: already squashed onto main\n";
        assert_eq!(unmerged_in_cherry(out), 1);
    }

    #[test]
    fn cherry_with_no_output_is_nothing_to_lose() {
        assert_eq!(unmerged_in_cherry(""), 0);
    }

    #[test]
    fn cherry_tolerates_leading_whitespace() {
        assert_eq!(unmerged_in_cherry("  + abc123 a commit\n"), 1);
    }
}
