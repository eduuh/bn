//! `self-update` — keep the installed `bn` in step with what is published, without ever going
//! backwards.
//!
//! Split out of `jobs.rs` (at the 1000-line ratchet) when the guard below was added. The shape:
//! fast-forward `bn/main`, and when HEAD has moved past the last install, run the repo's own
//! `install.sh --core`, which downloads main's published release binaries and anchors them at the
//! checkout. That is the only install path. It used to prefer a machine-local `publish-prod` job
//! that compiled the checkout instead; strict release retired it, because no machine should need
//! a working cargo to run bn.
//!
//! The guard exists because "main moved" and "there is something newer to install" stopped being
//! the same statement. install.sh installs the *published* release, and the release is built by a
//! workflow that needs a runner. With the runner offline the release stood a month behind main,
//! and every six hours this job saw HEAD ≠ stamp, ran install.sh, and put that month-old binary
//! over whatever was on the box — including a build of the current checkout installed three
//! minutes earlier — then stamped main's HEAD as if it had installed it. So the job now asks the
//! release what it is before installing it, and refuses to replace an installed bn with one that
//! is the same or older.
use crate::*;
use std::path::Path;

/// Locate bn's `bn/main` worktree in `git worktree list --porcelain` output — the first
/// `worktree <path>` line whose path ends in `/bn/main`. Pure so it's unit-testable.
fn parse_main_worktree(porcelain: &str) -> Option<String> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(str::trim)
        .find(|p| p.ends_with("/bn/main"))
        .map(str::to_string)
}

/// True if `p` is an executable file (unix exec bit), mirroring the shell job's `[ -x ... ]`.
fn is_executable_file(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Resolve bn's `bn/main` worktree: the conventional `<worktree_dir>/bn/main`, else the main
/// worktree of the checkout the `<config_dir>/repo` install anchor points at, else
/// `$BN_MAIN_WORKTREE`. Used by `self-update` so the committed `install.sh` is resolved regardless
/// of the cron's arbitrary cwd, and it stays robust even when bn was installed from a feature
/// worktree.
fn resolve_bn_main_worktree() -> String {
    let conventional = bn_core::resolver::worktree_dir().join("bn").join("main");
    if conventional.is_dir() {
        return conventional.to_string_lossy().into_owned();
    }
    std::fs::canonicalize(bn_core::resolver::config_dir().join("repo"))
        .ok()
        .and_then(|a| {
            let a = a.to_string_lossy().into_owned();
            crate::git::git_capture(&["-C", &a, "worktree", "list", "--porcelain"], None, true)
        })
        .and_then(|out| parse_main_worktree(&out))
        .or_else(|| {
            std::env::var("BN_MAIN_WORKTREE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| conventional.to_string_lossy().into_owned())
}

/// `self-update` — keep the installed `bn` in lock-step with `bn/main` whenever main advances.
/// Cheap to run often: it fast-forwards main and only re-installs when HEAD actually moved past
/// the last install (tracked in a stamp file).
///
/// The install step is delegated, never reimplemented here: the repo's own `install.sh --core`
/// downloads main's published release binaries and wires the anchors at the checkout, so a box
/// that has never seen cargo still updates. Native port of `jobs/self-update.sh`: the git fan-out
/// and stamp bookkeeping run in-process; only the final install+wire step is shell. `--core` skips
/// the tmux layer, whose helpers are symlinks into the worktree and so already track main without
/// re-linking.
pub(crate) fn job_self_update(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // The same anchors every other bn path resolves: `~/.bn/bin` for the binary and the config dir
    // for the install stamp. This used to hardcode `~/.bin` and `~/.config/bn`, the pre-`~/.bn`
    // layout, and only kept working where install.sh had left a compatibility symlink behind.
    let bindir = bn_core::resolver::bin_dir();
    let stamp = bn_core::resolver::config_dir().join(".built-at");

    // Resolve bn's main worktree — robust even when bn was installed
    // from a feature worktree.
    let main_wt = resolve_bn_main_worktree();

    if !Path::new(&main_wt).is_dir() {
        native_job_line(
            &format!("self-update: bn main worktree not found ({main_wt})"),
            true,
        );
        return 1;
    }

    // Best-effort fast-forward of main (matches the shell's `|| true`).
    let _ = crate::git::git_ok(&["-C", &main_wt, "fetch", "--quiet", "origin", "main"]);
    let _ = crate::git::git_ok(&["-C", &main_wt, "merge", "--ff-only", "origin/main"]);

    let head = match crate::git::git_capture(
        &["-C", &main_wt, "rev-parse", "--short", "HEAD"],
        None,
        true,
    ) {
        Some(h) if !h.is_empty() => h,
        _ => {
            native_job_line(
                &format!("self-update: cannot resolve HEAD in {main_wt}"),
                true,
            );
            return 1;
        }
    };

    // Skip the (re)install when the installed binary already matches the current HEAD.
    let built = std::fs::read_to_string(&stamp)
        .ok()
        .map(|s| s.trim().to_string());
    if built.as_deref() == Some(head.as_str()) && is_executable_file(&bindir.join("bn")) {
        return 0;
    }

    // One way to install bn, on every machine: download what CI published. This used to prefer a
    // machine-local `publish-prod` job that compiled the checkout, which made the installed binary
    // depend on whether that box had a working cargo — and meant a laptop was compiling bn to run
    // it. Strict release removed the reason for it: main is published as a versioned release only
    // after the full CI gate passes on that commit, so there is nothing a local build can offer
    // that the release does not, except a way to drift.
    let installer = Path::new(&main_wt).join("install.sh");

    // install.sh reinstalls the *published* release, which can lag main by however long CI takes:
    // refuse to put an older build over the installed one. It is not a git/gh/tmux binary, so
    // spawning it directly is fine (the subprocess guard only covers those).
    if let Some(why) = release_not_newer(&main_wt, &bindir.join("bn")) {
        native_job_line(
            &format!("self-update: {why} — leaving the installed bn alone"),
            false,
        );
        return 0;
    }
    let mut cmd = std::process::Command::new(&installer);
    cmd.arg("--core").current_dir(&main_wt);
    let label = format!("{} --core", installer.display());
    // Capture rather than inherit: a cron has no terminal, so inherited output goes nowhere and a
    // failed build leaves `bn cron`/`bn doctor` showing a bare "FAIL" with no cause. Capturing lets
    // the compiler error ride into the run's `output_tail`, which is the only durable record of why
    // an unattended update stopped keeping `bn` in lock-step with main.
    match cmd.output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stderr).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            native_job_line(
                &format!("self-update: {label} failed ({})", out.status),
                true,
            );
            // The last few lines carry the actual installer error.
            for line in tail_lines(&combined, 12).lines() {
                native_job_line(&format!("  {line}"), true);
            }
            return out.status.code().unwrap_or(1);
        }
        Err(e) => {
            native_job_line(&format!("self-update: cannot run {label}: {e}"), true);
            return 1;
        }
    }

    let _ = stamp.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(&stamp, format!("{head}\n"));
    native_job_line(&format!("self-update: ran {label} @ {head}"), false);
    crate::cmd::self_update_check::verify(&bindir.join("bn"))
}

/// Why the published release must not replace the installed `bn`, or `None` when installing it is
/// the right move (it is newer, or there is no bn with a known commit installed to protect).
///
/// When the release's commit cannot be learned, this refuses. It used to install then, and that
/// is how source builds got replaced by an older release: a timer runs without the terminal's gh
/// sign-in, gh cannot answer, and v0.6.2 went over a newer build (seen 2026-09-27 on
/// cloud-pc-AB12C, whose `bn run` then queued jobs instead of running them).
fn release_not_newer(main_wt: &str, installed: &Path) -> Option<String> {
    let installed_sha = installed_sha(installed)?;
    let Some(release_sha) = published_release_sha() else {
        return Some(format!(
            "cannot tell which commit the published release is (gh offline or signed out), \
             so it may be older than installed {installed_sha}"
        ));
    };
    let ancestor = |older: &str, newer: &str| {
        crate::git::git_ok(&[
            "-C",
            main_wt,
            "merge-base",
            "--is-ancestor",
            older,
            newer,
        ])
    };
    match compare(&installed_sha, &release_sha, ancestor) {
        Newer::Release => None,
        Newer::Same => Some(format!(
            "the published release ({release_sha}) is what is installed"
        )),
        Newer::Installed => Some(format!(
            "installed {installed_sha} is ahead of the published release ({release_sha})"
        )),
    }
}

/// Which of the two is the newer build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newer {
    Release,
    Same,
    Installed,
}

/// The pure half of [`release_not_newer`]: `is_ancestor(older, newer)` is git's word on whether
/// `older` is reachable from `newer`. A release that is neither the same commit nor an ancestor of
/// what is installed — unrelated, or ahead — is treated as newer, which is the old behaviour and
/// the safe direction when the history cannot say.
fn compare(installed: &str, release: &str, is_ancestor: impl Fn(&str, &str) -> bool) -> Newer {
    if same_commit(installed, release) {
        Newer::Same
    } else if is_ancestor(release, installed) {
        Newer::Installed
    } else {
        Newer::Release
    }
}

/// Whether two abbreviated shas name one commit: one is a prefix of the other.
fn same_commit(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The git sha the installed binary reports in `bn --version`.
fn installed_sha(bn: &Path) -> Option<String> {
    let out = std::process::Command::new(bn).arg("--version").output().ok()?;
    parse_version_sha(&String::from_utf8_lossy(&out.stdout))
}

/// `bn 0.5.0 (git 8ba69427374f, built 2026-09-07, …)` → `8ba69427374f`.
fn parse_version_sha(version: &str) -> Option<String> {
    let rest = version.split("(git ").nth(1)?;
    let sha: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (sha.len() >= 7).then_some(sha)
}

/// The commit the latest published release was built from.
///
/// This used to be scraped out of the release *name*, back when the rolling release was titled
/// `nightly (<sha>)`. Releases are versioned now (`v0.5.1`), so there is no sha in the name to
/// read and the guard would have quietly gone dead — `parse_release_sha` returning `None` on
/// every release means "the history cannot say", which installs unconditionally. Ask GitHub for
/// the commit instead of inferring it from a title: `targetCommitish` is what the release
/// workflow pinned with `gh release create --target <sha>`.
///
/// `None` when gh cannot answer (offline, unauthenticated, or a release pinned to a branch name
/// rather than a commit), which makes [`release_not_newer`] leave the installed bn alone.
fn published_release_sha() -> Option<String> {
    let slug = std::env::var("BN_REPO_SLUG").unwrap_or_else(|_| "eduuh/bn".to_string());
    let commitish = crate::gh::gh_capture_bounded(
        &[
            "release",
            "view",
            "-R",
            &slug,
            "--json",
            "targetCommitish",
            "-q",
            ".targetCommitish",
        ],
        std::time::Duration::from_secs(20),
    )?;
    parse_release_sha(&commitish)
}

/// A release's `targetCommitish` is only usable here when it is an actual commit. GitHub returns
/// the branch name (`main`) for a release created without `--target`, and a branch tip is not the
/// commit the assets were built from — comparing against it would be worse than not comparing.
fn parse_release_sha(commitish: &str) -> Option<String> {
    let sha = commitish.trim();
    let ok = sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| sha.to_string())
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn version_and_release_names_yield_their_shas() {
        assert_eq!(
            parse_version_sha("bn 0.5.0 (git 8ba69427374f, built 2026-09-07, x86_64)").as_deref(),
            Some("8ba69427374f")
        );
        assert_eq!(parse_version_sha("bn 0.5.0"), None);
        assert_eq!(parse_release_sha("8ba69427374f").as_deref(), Some("8ba69427374f"));
        // A branch name, not a commit: refuse it rather than compare against a moving tip.
        assert_eq!(parse_release_sha("main"), None);
        assert_eq!(parse_release_sha("v0.4.1"), None);
    }

    /// The failure this guards against: a month-old release replacing a build of the current
    /// checkout because main had moved since the last stamp.
    #[test]
    fn a_release_behind_the_installed_build_is_refused() {
        let ancestor = |older: &str, newer: &str| older == "8ba6942" && newer == "3dfedcafaeb0";
        assert_eq!(compare("3dfedcafaeb0", "8ba6942", ancestor), Newer::Installed);
    }

    #[test]
    fn the_same_commit_at_either_abbreviation_is_nothing_to_install() {
        assert_eq!(compare("8ba69427374f", "8ba6942", |_, _| false), Newer::Same);
    }

    /// Unrelated or ahead: install, as before. History that cannot say is not a reason to stay
    /// behind.
    #[test]
    fn a_release_git_cannot_place_behind_the_install_still_installs() {
        assert_eq!(compare("3dfedca", "20ffccc", |_, _| false), Newer::Release);
    }
}
