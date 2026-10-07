//! What this machine may do: repository authorization and the resident runtimes.
//!
//! Everything is allowed by default. A `[machine]` table — usually in the machine's own
//! `<notes_root>/<machine>/config.toml` — only narrows it, so a machine nobody configured still
//! runs its cron and dashboard instead of silently doing nothing.

use super::{Config, MachineProfile};

impl Config {
    /// Whether `job` is allowed to run on this host.
    pub fn job_allowed_here(&self, job: &str) -> bool {
        match self.job_only_on.get(job) {
            Some(machines) => machines.iter().any(|machine| machine == self.machine()),
            None => true,
        }
    }

    /// Whether `repo` is in the work allowlist (`[work].work_repos`).
    pub fn is_work_repo(&self, repo: &str) -> bool {
        let base = repo.rsplit('/').next().unwrap_or(repo);
        self.work_repos
            .iter()
            .any(|work| work == base || work == repo)
    }

    /// Whether this machine may see or execute work for `repo`.
    ///
    /// With no `[machine]` table, or one that lists `*`, every repository is allowed. Otherwise
    /// full `owner/repo` grants win, and a trailing-name grant is accepted only when that basename
    /// is unique in the list, so two owners' `foo` repositories cannot accidentally authorize one
    /// another.
    pub fn repo_allowed_here(&self, repo: &str) -> bool {
        let Some(profile) = &self.machine_profile else {
            return true;
        };
        if profile.repos.iter().any(|allowed| allowed == "*") {
            return true;
        }
        let repo = repo.trim().trim_end_matches(['/', '\\']).replace('\\', "/");
        if profile
            .repos
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&repo))
        {
            return true;
        }
        let requested_has_owner = repo.contains('/');
        let base = repo.rsplit('/').next().unwrap_or(&repo);
        profile
            .repos
            .iter()
            .filter(|allowed| {
                if requested_has_owner && allowed.contains(['/', '\\']) {
                    return false;
                }
                allowed
                    .rsplit(['/', '\\'])
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(base))
            })
            .count()
            == 1
    }

    /// Whether an authorized repository is physically available in this machine's bn-managed
    /// bare-repository inventory. Scheduled work uses this stricter gate: assignment alone must
    /// not keep firing a cron after its checkout has been removed from the machine.
    pub fn repo_available_here(&self, repo: &str) -> bool {
        if !self.repo_allowed_here(repo) {
            return false;
        }
        let base = repo
            .trim()
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(repo)
            .to_lowercase();
        crate::lifecycle::cloned_repos(self).contains(&base)
    }

    /// Whether scheduled jobs run here: on unless `[machine]` says `cron = false`.
    pub fn cron_enabled_here(&self) -> bool {
        self.machine_profile.as_ref().is_none_or(|p| p.cron)
    }

    /// Whether a repo should appear in cross-repo views on this machine.
    pub fn is_visible_repo(&self, repo: &str) -> bool {
        self.repo_allowed_here(repo) && (!self.work_only || self.is_work_repo(repo))
    }

    pub(super) fn normalize_machine_profile(mut profile: MachineProfile) -> MachineProfile {
        let mut repos = Vec::new();
        for repo in profile.repos {
            let repo = repo.trim().trim_end_matches('/').replace('\\', "/");
            if !repo.is_empty()
                && !repos
                    .iter()
                    .any(|seen: &String| seen.eq_ignore_ascii_case(&repo))
            {
                repos.push(repo);
            }
        }
        profile.repos = repos;
        profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::config_file::ConfigFile;
    use crate::test_support::EnvGuard;

    #[test]
    fn scheduled_repo_requires_assignment_and_a_local_clone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = Config {
            worktree_dir: temp.path().join("worktree"),
            machine_profile: Some(MachineProfile {
                repos: vec!["owner/api".to_string()],
                ..MachineProfile::default()
            }),
            ..Default::default()
        };

        assert!(!cfg.repo_available_here("owner/api"));
        std::fs::create_dir_all(temp.path().join("bare").join("api.git"))
            .expect("create bare repo marker");
        assert!(cfg.repo_available_here("owner/api"));
        assert!(!cfg.repo_available_here("owner/web"));
    }

    #[test]
    fn no_profile_allows_every_repository_and_runtime() {
        let cfg = Config {
            hostname: "unconfigured".to_string(),
            machine_profile: None,
            ..Config::default()
        };

        assert!(cfg.repo_allowed_here("owner/bn"));
        assert!(cfg.cron_enabled_here());
    }

    #[test]
    fn profile_flags_default_on_and_repos_are_normalized() {
        let file: ConfigFile = toml::from_str(
            r#"
            [machine]
            repos = [" bn ", "owner/arena/", "BN"]
            "#,
        )
        .expect("machine table parses");
        let profile = file
            .machine_profile()
            .map(Config::normalize_machine_profile)
            .expect("machine table resolves");

        assert_eq!(profile.repos, vec!["bn", "owner/arena"]);
        assert!(profile.cron);
    }

    #[test]
    fn a_profile_narrows_only_what_it_names() {
        let file: ConfigFile =
            toml::from_str("[machine]\ncron = false\n").expect("machine table parses");
        let cfg = Config {
            machine_profile: file
                .machine_profile()
                .map(Config::normalize_machine_profile),
            ..Config::default()
        };

        assert!(cfg.repo_allowed_here("owner/bn"));
        assert!(!cfg.cron_enabled_here());
    }

    #[test]
    fn repository_matching_prefers_full_slug_and_rejects_ambiguous_basenames() {
        let cfg = Config {
            machine_profile: Some(MachineProfile {
                repos: vec![
                    "first/shared".to_string(),
                    "second/shared".to_string(),
                    "solo".to_string(),
                ],
                cron: false,
            }),
            ..Config::default()
        };

        assert!(cfg.repo_allowed_here("first/shared"));
        assert!(!cfg.repo_allowed_here("third/shared"));
        assert!(!cfg.repo_allowed_here("shared"));
        assert!(cfg.repo_allowed_here("owner/solo"));
        assert!(!cfg.cron_enabled_here());
    }

    #[test]
    fn a_malformed_or_missing_config_switches_nothing_off() {
        let _lock = crate::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let malformed = temp.path().join("config.toml");
        std::fs::write(&malformed, "[machine\ncron = false").expect("write malformed");
        let _hostname = EnvGuard::set("BN_HOSTNAME", "broken");

        // A file bn cannot read says nothing, so every service stays on and nothing already
        // installed is uninstalled on its account.
        for path in [malformed, temp.path().join("missing.toml")] {
            let _config = EnvGuard::set("BN_CONFIG", &path);
            let cfg = Config::load();
            assert!(cfg.cron_enabled_here(), "{}", path.display());
        }
    }
}
