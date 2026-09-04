//! `ensure-tmux` — hold a repository's tmux session open so WSL does not idle out.
//!
//! Split out of `jobs.rs` under the 1000-line ratchet, and a natural seam rather than an arbitrary
//! one: this is the only job whose whole purpose is a tmux session, and it is the one place the
//! "every session is a repository" rule is enforced against a knob a person can set.
use crate::*;

/// Why `ensure-tmux` has no session to ensure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnsureTmuxSkip {
    /// `$BN_TMUX_SESSION` named something that is not a repo on this machine.
    UnknownRepo { asked: String, known: Vec<String> },
    /// No bn-managed clone at all — a machine that has not been set up yet.
    NoRepos,
}

/// Which repo's session the keepalive should ensure. Pure.
///
/// `$BN_TMUX_SESSION` is a **repo name**, not a free-form session name — that reinterpretation is
/// the whole point. It used to default to `main`, a session belonging to no repository, which is
/// exactly the thing this keepalive should not be conjuring. Validating the override against the
/// machine's clones is what makes "every session is a repo" enforceable rather than aspirational:
/// a typo is refused with the list of real answers instead of silently creating a fake session.
///
/// Order: the override, else bn (whose crons this keepalive exists to let fire), else any clone.
pub(crate) fn ensure_tmux_repo(
    asked: Option<&str>,
    cloned: &std::collections::BTreeSet<String>,
) -> Result<String, EnsureTmuxSkip> {
    if let Some(asked) = asked.map(str::trim).filter(|s| !s.is_empty()) {
        return cloned
            .iter()
            .find(|repo| repo.eq_ignore_ascii_case(asked))
            .cloned()
            .ok_or_else(|| EnsureTmuxSkip::UnknownRepo {
                asked: asked.to_string(),
                known: cloned.iter().cloned().collect(),
            });
    }
    if let Some(bn) = cloned.iter().find(|repo| *repo == "bn") {
        return Ok(bn.clone());
    }
    cloned.iter().next().cloned().ok_or(EnsureTmuxSkip::NoRepos)
}

/// `jobs/ensure-tmux.sh` — ensure the keepalive tmux session exists.
///
/// The session exists so WSL keeps a tmux server alive and does not idle the distro out from under
/// the systemd timers. *Which* session was never the point, so it used to be a bare `main` — a
/// session named after no repository, and one of the four this change retires. It now ensures a
/// repo's own session through the ordinary `wt` path, which is a no-op when that session is already
/// open.
///
/// A machine with no bn-managed clone has no session to ensure, and that is not a failure: a fresh
/// box is what `setup-bn` is for, and a red cron row would say nothing useful. The Windows task
/// still wakes the distro every 30 minutes, so the cost is that WSL may idle out *between* wakes
/// rather than staying up continuously.
pub(crate) fn job_ensure_tmux(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let asked = std::env::var("BN_TMUX_SESSION").ok();
    let cloned = bn_core::lifecycle::cloned_repos(cfg);
    let repo = match ensure_tmux_repo(asked.as_deref(), &cloned) {
        Ok(repo) => repo,
        Err(EnsureTmuxSkip::UnknownRepo { asked, known }) => {
            eprintln!(
                "ensure-tmux: BN_TMUX_SESSION names a repo, and '{asked}' is not one on this                  machine. Known: {}",
                known.join(", ")
            );
            return 1;
        }
        Err(EnsureTmuxSkip::NoRepos) => {
            println!("ensure-tmux: no bn-managed repo clone on this machine — no session to ensure");
            return 0;
        }
    };
    if crate::proc::tmux_run(&["has-session", "-t", &format!("={repo}")]) {
        println!("tmux session '{repo}' already running");
        return 0;
    }
    let home = crate::wt::respawn_fallback(&cfg.worktree_dir, &repo);
    crate::wt::open_worktree_window(&repo, "main", &home, false);
    if crate::proc::tmux_run(&["has-session", "-t", &format!("={repo}")]) {
        println!("started detached tmux session '{repo}'");
        0
    } else {
        eprintln!("ensure-tmux: failed to start tmux session '{repo}'");
        1
    }
}

#[cfg(test)]
mod ensure_tmux_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn repos(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bn_hosts_the_keepalive_when_nothing_is_asked_for() {
        assert_eq!(
            ensure_tmux_repo(None, &repos(&["bigmono", "bn", "dotfiles"])),
            Ok("bn".to_string())
        );
    }

    #[test]
    fn any_clone_will_do_when_bn_is_not_one_of_them() {
        // The job only needs *a* tmux server alive; it must not invent a session to get one.
        assert_eq!(
            ensure_tmux_repo(None, &repos(&["bigmono", "dotfiles"])),
            Ok("bigmono".to_string())
        );
    }

    #[test]
    fn the_override_names_a_repo_and_is_matched_case_insensitively() {
        let cloned = repos(&["bigmono", "bn"]);
        assert_eq!(ensure_tmux_repo(Some("BigMono"), &cloned), Ok("bigmono".to_string()));
        assert_eq!(ensure_tmux_repo(Some("  bn "), &cloned), Ok("bn".to_string()));
        // Empty is not an answer; fall through to the default.
        assert_eq!(ensure_tmux_repo(Some(""), &cloned), Ok("bn".to_string()));
    }

    /// The knob is where the invariant becomes enforceable: a session name that is not a repo is
    /// refused outright rather than conjured.
    #[test]
    fn an_override_that_is_not_a_repo_is_refused_with_the_real_answers() {
        assert_eq!(
            ensure_tmux_repo(Some("main"), &repos(&["bn", "dotfiles"])),
            Err(EnsureTmuxSkip::UnknownRepo {
                asked: "main".to_string(),
                known: vec!["bn".to_string(), "dotfiles".to_string()],
            })
        );
    }

    /// A machine nobody has set up yet is not a broken cron.
    #[test]
    fn a_machine_with_no_clones_has_nothing_to_ensure() {
        assert_eq!(
            ensure_tmux_repo(None, &repos(&[])),
            Err(EnsureTmuxSkip::NoRepos)
        );
    }
}

