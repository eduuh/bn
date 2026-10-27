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
        run_switch(slot_s, branch, &["-C", slot_s, "switch", branch])
    } else if exists("remotes/origin") {
        run_switch(
            slot_s,
            branch,
            &[
                "-C",
                slot_s,
                "switch",
                "-c",
                branch,
                &format!("origin/{branch}"),
            ],
        )
    } else {
        let start = base
            .map(str::to_string)
            .unwrap_or_else(|| default_base(bare_s));
        run_switch(
            slot_s,
            branch,
            &["-C", slot_s, "switch", "-c", branch, &start],
        )
    }
}

/// Run one `git switch`, and judge it by where HEAD landed rather than by git's exit code alone.
///
/// git runs the `post-checkout` hook *after* moving HEAD, and a failing hook turns the whole
/// command's exit code non-zero. A monorepo's hook calls `node`, which a non-login shell often lacks, so a
/// switch that had fully landed was reported as refused: the pool reset the slot, retried, and
/// failed the lease with the branch checked out and the tree clean. So when git fails but HEAD is on
/// `branch`, the switch counts, with one warning naming git's last stderr line. A switch that did not
/// land (branch held by another worktree, dirty tree) still fails. stderr is forwarded line by line
/// as it arrives, so git's and the hook's output still reach the terminal live.
fn run_switch(slot_s: &str, branch: &str, args: &[&str]) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(mut child) = crate::git::git_command(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let mut last = String::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            eprintln!("{line}");
            if !line.trim().is_empty() {
                last = line;
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    if ok {
        return true;
    }
    let head = wt::git_out(&["-C", slot_s, "symbolic-ref", "--quiet", "--short", "HEAD"]);
    if head.as_deref() != Some(branch) {
        return false;
    }
    ceprintln!(
        "{YELLOW}Switched to '{branch}', but a checkout hook failed: {}{NC}",
        last.trim()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::super::git_integration_tests::{git, head_ref, mk_bare};
    use super::*;

    /// A `post-checkout` hook that fails every time, like a monorepo's `node: not found` without nvm.
    /// Installed after the slot exists, because `worktree add` runs the hook too.
    fn install_failing_hook(bare: &str) {
        let hooks = Path::new(bare).join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'post-checkout: node: not found' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn a_failing_post_checkout_hook_does_not_fail_a_switch_that_landed() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare) = mk_bare();
        let slot = tmp.path().join("slot");
        let slot_s = slot.to_string_lossy().to_string();
        assert!(add_detached_slot(&bare, &slot_s));
        install_failing_hook(&bare);

        // An existing branch, a brand-new one, and re-switching onto the current branch
        // ("Already on …") all exit non-zero through the hook, yet all land.
        assert!(switch_into_slot_from(&slot_s, &bare, "feat", None));
        assert_eq!(head_ref(&slot), "feat");
        assert!(switch_into_slot_from(&slot_s, &bare, "feat", None));
        assert_eq!(head_ref(&slot), "feat");
        assert!(switch_into_slot_from(&slot_s, &bare, "fresh", None));
        assert_eq!(head_ref(&slot), "fresh");
    }

    #[test]
    fn a_switch_onto_a_branch_held_by_another_worktree_still_fails() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare) = mk_bare();
        let slot = tmp.path().join("slot");
        let slot_s = slot.to_string_lossy().to_string();
        assert!(add_detached_slot(&bare, &slot_s));
        let other = tmp.path().join("other");
        assert!(git(&[
            "--git-dir",
            &bare,
            "worktree",
            "add",
            &other.to_string_lossy(),
            "feat"
        ]));
        install_failing_hook(&bare);

        assert!(!switch_into_slot_from(&slot_s, &bare, "feat", None));
        assert_eq!(head_ref(&slot), "HEAD", "the slot stays detached");
    }
}
