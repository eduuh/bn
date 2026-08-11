//! Whether a dispatch branch is already taken — the guard that decides between reusing a name
//! and refusing the dispatch.
//!
//! Its own file because the answer is three git questions rather than one, and because the
//! distinction it draws (a branch someone is working on vs. the pointer a finished one left
//! behind) is worth testing against a real repository.
use std::path::Path;

/// Whether `branch` is genuinely taken — somebody is working on it *now*, or its work has not
/// landed yet.
///
/// **Why this is not "a ref by this name exists".** That was the original test, and it conflates a
/// live dispatch with the pointer a finished one leaves behind. A dispatch branch outlives its
/// agent: the worktree is removed and the PR merges, but `refs/heads/dispatch/t17` stays until
/// somebody prunes it. Under the ref-exists rule that dead pointer refused every future dispatch of
/// the same item, forever — and because a failed launch returns the queue entry to pending, the
/// dispatcher re-ran `bn dispatch` against it on a loop. Six of the eight items held on the machine
/// this was written for were blocked by branches already merged into the base, one of them for over
/// nine hours.
///
/// So occupancy is two live facts instead: a worktree has the branch checked out, or the ref
/// carries commits the base does not. Both are true exactly while the work is real, and both stop
/// being true when it finishes — which is what makes the refusal recoverable without `--fresh`.
///
/// A ref that cannot be compared against `base` (a base that does not resolve, git unavailable) is
/// treated as occupied: unable to tell landed work from live work, refusing is the safe half of the
/// mistake, since `--fresh` and `--branch` both still get you past it.
pub(super) fn dispatch_branch_occupied(
    bare: &Path,
    worktree_dir: &Path,
    repo: &str,
    branch: &str,
    base: &str,
) -> bool {
    if worktree_dir
        .join(repo)
        .join(branch.replace('/', "-"))
        .exists()
    {
        return true;
    }
    let bare_s = bare.to_string_lossy().to_string();
    if branch_checked_out(&bare_s, branch) {
        return true;
    }
    ["heads", "remotes/origin"]
        .iter()
        .map(|kind| format!("refs/{kind}/{branch}"))
        .filter(|r| ref_exists(&bare_s, r))
        .any(|r| !merged_into(&bare_s, &r, base))
}

fn ref_exists(bare: &str, reference: &str) -> bool {
    crate::git::git_command(&["--git-dir", bare, "show-ref", "--verify", "--quiet", reference])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Whether any worktree of the bare repo has `branch` checked out.
///
/// Distinct from the directory probe above, which only knows the path dispatch *would* have chosen.
/// A pooled worktree holds its branch at `<repo>/_pool/<n>`, so that probe misses it entirely — and
/// missing it is what let a second dispatch onto a busy pool slot get as far as the worktree layer
/// before failing.
fn branch_checked_out(bare: &str, branch: &str) -> bool {
    let Ok(out) = crate::git::git_command(&["--git-dir", bare, "worktree", "list", "--porcelain"])
        .output()
    else {
        return false;
    };
    let want = format!("branch refs/heads/{branch}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.trim_end() == want)
}

/// Whether `reference` is fully contained in `base` — i.e. its work has landed and the ref is now
/// just a leftover pointer.
fn merged_into(bare: &str, reference: &str, base: &str) -> bool {
    // Compare against the base as the bare repo actually holds it. A dispatch base is a local
    // branch here (`main`), but falling back to the remote-tracking ref keeps this working on a
    // bare clone that only ever fetched.
    let base_ref = ["refs/heads", "refs/remotes/origin"]
        .iter()
        .map(|kind| format!("{kind}/{base}"))
        .find(|r| ref_exists(bare, r));
    let Some(base_ref) = base_ref else {
        return false;
    };
    crate::git::git_command(&[
        "--git-dir",
        bare,
        "merge-base",
        "--is-ancestor",
        reference,
        &base_ref,
    ])
    .status()
    .map(|status| status.success())
    .unwrap_or(false)
}


#[cfg(test)]
mod tests {
    //! Against a real bare repo, because the distinction under test is a git fact. The bug these
    //! pin down was not in the logic that reads the answer — it was that the old test asked
    //! "does a ref exist", which a finished dispatch answers yes to forever.
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A bare repo with `main`, plus a work tree to commit through.
    fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare = tmp.path().join("r.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        git(tmp.path(), &["init", "-q", "--bare", "-b", "main", "r.git"]);
        git(tmp.path(), &["init", "-q", "-b", "main", "work"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-q", "origin", "main"]);
        (tmp, work)
    }

    fn bare_of(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        tmp.path().join("r.git")
    }

    #[test]
    fn a_branch_nobody_has_ever_used_is_free() {
        let (tmp, _work) = repo();
        assert!(!dispatch_branch_occupied(
            &bare_of(&tmp),
            &tmp.path().join("worktrees"),
            "r",
            "dispatch/new",
            "main",
        ));
    }

    /// The regression. `dispatch/t17` shipped and merged hours ago; its ref is all that is left.
    /// Under the old rule this refused every future dispatch of t17, and the queue retried it every
    /// ten minutes for as long as the machine was up.
    #[test]
    fn a_merged_branch_is_not_occupied_by_its_leftover_ref() {
        let (tmp, work) = repo();
        git(&work, &["checkout", "-q", "-b", "dispatch/t17"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "work"]);
        git(&work, &["checkout", "-q", "main"]);
        git(&work, &["merge", "-q", "--ff-only", "dispatch/t17"]);
        git(&work, &["push", "-q", "origin", "main", "dispatch/t17"]);

        let bare = bare_of(&tmp);
        // The ref is really there — this is not passing because the branch is absent.
        assert!(ref_exists(
            &bare.to_string_lossy(),
            "refs/heads/dispatch/t17"
        ));
        assert!(!dispatch_branch_occupied(
            &bare,
            &tmp.path().join("worktrees"),
            "r",
            "dispatch/t17",
            "main",
        ));
    }

    /// The half that must keep working: unlanded commits mean somebody is still on it.
    #[test]
    fn a_branch_with_work_the_base_lacks_is_occupied() {
        let (tmp, work) = repo();
        git(&work, &["checkout", "-q", "-b", "dispatch/live"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "wip"]);
        git(&work, &["push", "-q", "origin", "dispatch/live"]);

        assert!(dispatch_branch_occupied(
            &bare_of(&tmp),
            &tmp.path().join("worktrees"),
            "r",
            "dispatch/live",
            "main",
        ));
    }

    /// Checked out somewhere is occupied even when the branch is level with the base — that is the
    /// pooled-worktree case, which lives at `<repo>/_pool/<n>` and so is invisible to the path probe.
    #[test]
    fn a_branch_checked_out_in_a_worktree_is_occupied_even_when_merged() {
        let (tmp, work) = repo();
        git(&work, &["branch", "repo-setup"]);
        git(&work, &["push", "-q", "origin", "repo-setup"]);
        let pool = tmp.path().join("pool2");
        git(
            &bare_of(&tmp),
            &["worktree", "add", "-q", pool.to_str().unwrap(), "repo-setup"],
        );

        assert!(dispatch_branch_occupied(
            &bare_of(&tmp),
            &tmp.path().join("worktrees"),
            "r",
            "repo-setup",
            "main",
        ));
    }

    /// The path dispatch itself would have used still counts, without asking git anything.
    #[test]
    fn the_directory_dispatch_would_create_still_counts() {
        let (tmp, _work) = repo();
        let worktrees = tmp.path().join("worktrees");
        std::fs::create_dir_all(worktrees.join("r").join("dispatch-thing")).expect("mkdir");

        assert!(dispatch_branch_occupied(
            &bare_of(&tmp),
            &worktrees,
            "r",
            "dispatch/thing",
            "main",
        ));
    }
}
