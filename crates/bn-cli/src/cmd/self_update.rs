//! `self-update` — keep the installed `bn` in step with what is published, without ever going
//! backwards.
//!
//! Split out of `jobs.rs` (at the 1000-line ratchet) when the guard below was added. The job
//! itself is unchanged in shape: fast-forward `bn/main`, and when HEAD has moved past the last
//! install, delegate to whatever installs bn on this machine. That is the `publish-prod` job
//! when one resolves for bn/main (a script that compiles the checkout into the prod build dir
//! and re-links every anchor, so nothing ends up pointing at a checkout), else `install.sh
//! --core`, which downloads main's published release binaries and anchors at the checkout.
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
use std::path::{Path, PathBuf};

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

/// Resolve bn's `bn/main` worktree from the install anchor (`~/.config/bn/repo`, a symlink to the
/// checkout `install.sh` ran from), falling back to `$BN_MAIN_WORKTREE` and then the worktree-dir
/// convention. Used by `self-update` so the committed `install.sh` is resolved regardless of the
/// cron's arbitrary cwd, and it stays robust even when bn was installed from a feature worktree.
fn resolve_bn_main_worktree() -> String {
    let home = wt::home_str();
    let anchor = std::fs::canonicalize(format!("{home}/.config/bn/repo")).ok();
    anchor
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
        .unwrap_or_else(|| format!("{home}/projects/worktree/bn/main"))
}

/// How the update is applied once main has moved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Updater {
    /// A `publish-prod` job script resolved for bn/main (scope label, path). It compiles the
    /// main checkout into the prod build dir and re-links the anchors there, so the release
    /// guard does not apply: it always builds HEAD, never something older.
    Job(String, PathBuf),
    /// The repo's own `install.sh --core`: downloads main's published release.
    Installer(PathBuf),
}

/// Prefer the machine's `publish-prod` job over `install.sh` when one resolves. Pure so the
/// choice is unit-testable; the lookup itself happens in the caller.
fn updater(job: Option<(String, PathBuf)>, main_wt: &Path) -> Updater {
    match job {
        Some((scope, path)) => Updater::Job(scope, path),
        None => Updater::Installer(main_wt.join("install.sh")),
    }
}

/// `self-update` — keep the installed `bn` in lock-step with `bn/main` whenever main advances.
/// Cheap to run often: it fast-forwards main and only re-installs when HEAD actually moved past
/// the last install (tracked in a stamp file).
///
/// The install step is delegated, never reimplemented here. When a `publish-prod` job resolves
/// for bn/main (`jobs/bn/publish-prod.sh` in the notes tree, or a repo/global tier), it runs
/// from the main worktree exactly as `bn build publish-prod` would: it compiles the checkout
/// into the prod build dir and re-points every anchor (`~/.bn/config/{bn,repo,jobs,agents,
/// crons.toml}`, `~/.tmux.conf`) at that self-contained build. Otherwise the repo's own
/// `install.sh --core` remains the single source of truth: it downloads main's published release
/// binaries and wires the anchors at the checkout, so a box that has never seen cargo still
/// updates. Native port of `jobs/self-update.sh`: the git fan-out and stamp bookkeeping run
/// in-process; only the final install+wire step is shell. `--core` skips the tmux layer, whose
/// helpers are symlinks into the worktree and so already track main without re-linking.
pub(crate) fn job_self_update(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let home = wt::home_str();

    let bindir = std::env::var_os("BN_BIN_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{home}/.bin")));
    // Stamp lives at the legacy `~/.config/bn/.built-at` (where install.sh anchors bn), matching
    // the shell job so a first native run doesn't force a spurious rebuild.
    let cfg_base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{home}/.config")));
    let stamp = cfg_base.join("bn").join(".built-at");

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

    let bn_main = Resolved {
        repo: "bn".to_string(),
        branch: "main".to_string(),
    };
    let job = resolver::job_script(cfg, Some(&bn_main), "publish-prod")
        .map(|(scope, path)| (scope.label().to_string(), path));
    let (label, mut cmd) = match updater(job, Path::new(&main_wt)) {
        Updater::Job(scope, script) => {
            // Same invocation as `bn build publish-prod` from the main worktree: /bin/sh, never
            // the shebang, and the cron runner's PATH so cargo/npm/git resolve without a login
            // shell. No release guard: the job compiles HEAD, so it cannot install something older.
            let mut c = std::process::Command::new("/bin/sh");
            c.arg(&script)
                .current_dir(&main_wt)
                .env("PATH", crate::cmd::cron::job_path());
            (format!("publish-prod ({scope}) {}", script.display()), c)
        }
        Updater::Installer(installer) => {
            // install.sh reinstalls the *published* release, which can lag main: refuse to put an
            // older build over the installed one. It is not a git/gh/tmux binary, so spawning it
            // directly is fine (the subprocess guard only covers those).
            if let Some(why) = release_not_newer(&main_wt, &bindir.join("bn")) {
                native_job_line(
                    &format!("self-update: {why} — leaving the installed bn alone"),
                    false,
                );
                return 0;
            }
            let mut c = std::process::Command::new(&installer);
            c.arg("--core").current_dir(&main_wt);
            (format!("{} --core", installer.display()), c)
        }
    };
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
/// the right move (it is newer, or nothing about it can be determined and the old rule stands).
fn release_not_newer(main_wt: &str, installed: &Path) -> Option<String> {
    let installed_sha = installed_sha(installed)?;
    let release_sha = published_release_sha()?;
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

/// The commit the latest published release was built from, read off its name — the rolling
/// release is named `nightly (<sha>)`. `None` when gh cannot answer (offline, unauthenticated, a
/// release named some other way), which leaves the old rule in charge.
fn published_release_sha() -> Option<String> {
    let slug = std::env::var("BN_REPO_SLUG").unwrap_or_else(|_| "eduuh/bn".to_string());
    let name = crate::gh::gh_capture_bounded(
        &["release", "view", "-R", &slug, "--json", "name", "-q", ".name"],
        std::time::Duration::from_secs(20),
    )?;
    parse_release_sha(&name)
}

/// `nightly (8ba6942)` → `8ba6942`.
fn parse_release_sha(name: &str) -> Option<String> {
    let inner = name.rsplit('(').next()?.split(')').next()?.trim();
    let ok = inner.len() >= 7 && inner.chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| inner.to_string())
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn publish_prod_job_wins_over_install_sh() {
        let main = Path::new("/wt/bn/main");
        let job = Some(("repo".to_string(), PathBuf::from("/notes/jobs/bn/publish-prod.sh")));
        assert_eq!(
            updater(job, main),
            Updater::Job("repo".to_string(), PathBuf::from("/notes/jobs/bn/publish-prod.sh"))
        );
        assert_eq!(
            updater(None, main),
            Updater::Installer(PathBuf::from("/wt/bn/main/install.sh"))
        );
    }

    #[test]
    fn version_and_release_names_yield_their_shas() {
        assert_eq!(
            parse_version_sha("bn 0.5.0 (git 8ba69427374f, built 2026-09-07, x86_64)").as_deref(),
            Some("8ba69427374f")
        );
        assert_eq!(parse_version_sha("bn 0.5.0"), None);
        assert_eq!(parse_release_sha("nightly (8ba6942)").as_deref(), Some("8ba6942"));
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
