//! The dispatch base: the repo's default (or `--base`) branch, fetched and present as a local
//! worktree so a new dispatch branch can be cut from it.
use crate::*;

#[derive(Debug)]
pub(crate) struct DispatchBase {
    pub(crate) branch: String,
    pub(crate) bootstrapped: bool,
    pub(crate) origin_backed: bool,
    /// The base checkout could not be fast-forwarded (usually your own uncommitted edits). The
    /// dispatch still forks from `origin/<base>`, so this is advice, not a failure.
    pub(crate) stale_checkout: Option<String>,
}

pub(crate) fn dispatch_base_bootstrap_message(repo: &str, base: &DispatchBase) -> Option<String> {
    if let Some(why) = &base.stale_checkout {
        return Some(format!(
            "bn dispatch: left {repo}'s {} checkout as it is ({why}); forking from origin/{}",
            base.branch, base.branch
        ));
    }
    (base.bootstrapped && base.origin_backed).then(|| {
        format!(
            "bn dispatch: bootstrapped {} worktree for {repo} from origin/{}",
            base.branch, base.branch
        )
    })
}

/// The worktree that already has `branch` checked out, wherever it lives. A repo whose default is
/// `master` is often checked out in a dir named `main`, and git refuses a second checkout.
fn existing_checkout(bare: &str, branch: &str) -> Option<std::path::PathBuf> {
    let out = crate::git::git_command(&bare_git_args(bare, &["worktree", "list", "--porcelain"]))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let want = format!("branch refs/heads/{branch}");
    let mut path = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(std::path::PathBuf::from(p));
        } else if line == want {
            return path.filter(|p| p.is_dir());
        }
    }
    None
}

pub(super) fn bare_git_args<'a>(bare: &'a str, tail: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["-c", "safe.bareRepository=all", "--git-dir", bare];
    args.extend_from_slice(tail);
    args
}

/// Ensure the dispatch base exists as a current local worktree. Fetching and resolving happen
/// against the bare clone so callers work identically from a repo worktree or the orchestrator.
pub(crate) fn ensure_dispatch_base(
    cfg: &Config,
    repo: &str,
    bare: &std::path::Path,
    requested: Option<&str>,
) -> Result<DispatchBase, String> {
    if !bare.is_dir() {
        return Err(format!(
            "bare repo is missing at {} — clone it with `bn wt clone <url>`",
            bare.display()
        ));
    }

    let bare_s = bare.to_string_lossy().to_string();
    let run = |args: &[&str]| -> Result<std::process::Output, String> {
        crate::git::git_command(args)
            .output()
            .map_err(|e| format!("couldn't run git for {}: {e}", bare.display()))
    };
    let checked = |args: &[&str], action: &str| -> Result<std::process::Output, String> {
        let out = run(args)?;
        if out.status.success() {
            Ok(out)
        } else {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if detail.is_empty() {
                format!("{action} failed for {}", bare.display())
            } else {
                format!("{action} failed for {}: {detail}", bare.display())
            })
        }
    };

    let probe = checked(
        &bare_git_args(&bare_s, &["rev-parse", "--is-bare-repository"]),
        "bare repo validation",
    )?;
    if String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Err(format!(
            "{} is not a valid bare repository — recreate it with `bn wt clone <url>`",
            bare.display()
        ));
    }

    checked(
        &bare_git_args(
            &bare_s,
            &[
                "-c",
                "credential.useHttpPath=true",
                "fetch",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        ),
        "fetch from origin",
    )?;

    let requested = requested.map(str::trim).filter(|s| !s.is_empty());
    let branch = match requested {
        Some(branch) => branch.to_string(),
        None => {
            let symbolic = run(&bare_git_args(
                &bare_s,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            ))?;
            if symbolic.status.success() {
                String::from_utf8_lossy(&symbolic.stdout)
                    .trim()
                    .strip_prefix("origin/")
                    .unwrap_or_default()
                    .to_string()
            } else {
                let remote_head = checked(
                    &bare_git_args(&bare_s, &["ls-remote", "--symref", "origin", "HEAD"]),
                    "remote default-branch lookup",
                )?;
                String::from_utf8_lossy(&remote_head.stdout)
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("ref: refs/heads/")
                            .and_then(|rest| rest.split_whitespace().next())
                    })
                    .unwrap_or_default()
                    .to_string()
            }
        }
    };
    if branch.is_empty() {
        return Err(format!(
            "couldn't determine origin's default branch for {} — set origin/HEAD or pass --base",
            bare.display()
        ));
    }

    let origin_ref = format!("refs/remotes/origin/{branch}");
    let origin_backed = run(&bare_git_args(
        &bare_s,
        &["show-ref", "--verify", "--quiet", &origin_ref],
    ))?
    .status
    .success();
    let local_ref = format!("refs/heads/{branch}");
    let local_exists = run(&bare_git_args(
        &bare_s,
        &["show-ref", "--verify", "--quiet", &local_ref],
    ))?
    .status
    .success();
    if !origin_backed && (requested.is_none() || !local_exists) {
        return Err(format!(
            "origin/{branch} does not exist in {} — fetch it or pass an existing local --base",
            bare.display()
        ));
    }

    let conventional = cfg
        .worktree_dir
        .join(repo)
        .join(crate::wt::sanitize_branch(&branch));
    let base_path = if conventional.exists() {
        conventional
    } else {
        existing_checkout(&bare_s, &branch).unwrap_or(conventional)
    };
    let base_s = base_path.to_string_lossy().to_string();
    let mut bootstrapped = false;
    if base_path.exists() {
        let current = checked(
            &["-C", &base_s, "symbolic-ref", "--quiet", "--short", "HEAD"],
            "base worktree validation",
        )?;
        let current = String::from_utf8_lossy(&current.stdout).trim().to_string();
        if current != branch {
            return Err(format!(
                "base worktree {} is on '{current}', expected '{branch}' — move it aside or check out '{branch}'",
                base_path.display()
            ));
        }
    } else {
        std::fs::create_dir_all(base_path.parent().unwrap_or(&cfg.worktree_dir))
            .map_err(|e| format!("couldn't create {}: {e}", base_path.display()))?;
        let mut args = bare_git_args(&bare_s, &["worktree", "add"]);
        if !local_exists {
            args.extend(["-b", branch.as_str()]);
        }
        args.extend([base_s.as_str()]);
        if !local_exists {
            args.push(origin_ref.as_str());
        } else {
            args.push(branch.as_str());
        }
        checked(&args, "base worktree bootstrap")?;
        bootstrapped = true;
    }

    // The new branch forks from origin/<base> regardless, so a checkout that will not
    // fast-forward (your uncommitted edits in it) is reported, never a reason to refuse the work.
    let mut stale_checkout = None;
    if origin_backed {
        let ff = run(&[
            "-C",
            &base_s,
            "merge",
            "--ff-only",
            &format!("origin/{branch}"),
        ])?;
        if !ff.status.success() {
            let detail = String::from_utf8_lossy(&ff.stderr);
            let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("fast-forward failed");
            stale_checkout = Some(first.trim().to_string());
        }
    }

    Ok(DispatchBase {
        branch,
        bootstrapped,
        origin_backed,
        stale_checkout,
    })
}

#[cfg(test)]
#[path = "base_tests.rs"]
mod tests;
