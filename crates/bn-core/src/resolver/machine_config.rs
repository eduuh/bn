//! Machine registration and repository authorization.

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

    /// Whether this machine has an explicit `[machines.<name>]` registration.
    pub fn machine_registered(&self) -> bool {
        self.machine_profile.is_some()
    }

    /// Whether this machine may see or execute work for `repo`.
    ///
    /// Full `owner/repo` grants win. A trailing-name grant is accepted only when that basename is
    /// unique in the profile, so two owners' `foo` repositories cannot accidentally authorize one
    /// another.
    pub fn repo_allowed_here(&self, repo: &str) -> bool {
        let Some(profile) = &self.machine_profile else {
            return false;
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

    pub fn queue_enabled_here(&self) -> bool {
        self.machine_profile.as_ref().is_some_and(|p| p.queue)
    }

    pub fn cron_enabled_here(&self) -> bool {
        self.machine_profile.as_ref().is_some_and(|p| p.cron)
    }

    pub fn web_enabled_here(&self) -> bool {
        self.machine_profile.as_ref().is_some_and(|p| p.web)
    }

    /// Whether managed services may be destructively reconciled from this configuration.
    pub fn destructive_reconcile_allowed(&self) -> bool {
        self.config_file_loaded
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

    #[test]
    fn missing_profile_denies_everything() {
        let cfg = Config {
            hostname: "unregistered".to_string(),
            machine_profile: None,
            ..Config::default()
        };

        assert!(!cfg.machine_registered());
        assert!(!cfg.repo_allowed_here("owner/bn"));
        assert!(!cfg.queue_enabled_here());
        assert!(!cfg.cron_enabled_here());
        assert!(!cfg.web_enabled_here());
    }

    #[test]
    fn profile_flags_default_off_and_repos_are_normalized() {
        let file: ConfigFile = toml::from_str(
            r#"
            [machines.MyLaptop]
            repos = [" bn ", "owner/arena/", "BN"]
            "#,
        )
        .expect("machine profile parses");
        let profile = file
            .machine_profile("MyLaptop")
            .map(Config::normalize_machine_profile)
            .expect("machine profile resolves");

        assert_eq!(profile.repos, vec!["bn", "owner/arena"]);
        assert!(!profile.queue);
        assert!(!profile.cron);
        assert!(!profile.web);
        assert!(file.machine_profile("mylaptop").is_none());
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
                queue: true,
                cron: false,
                web: true,
            }),
            ..Config::default()
        };

        assert!(cfg.repo_allowed_here("first/shared"));
        assert!(!cfg.repo_allowed_here("third/shared"));
        assert!(!cfg.repo_allowed_here("shared"));
        assert!(cfg.repo_allowed_here("owner/solo"));
        assert!(cfg.queue_enabled_here());
        assert!(!cfg.cron_enabled_here());
        assert!(cfg.web_enabled_here());
    }

    #[test]
    fn malformed_config_denies_permissions_without_destructive_reconcile() {
        let _lock = crate::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[machines.broken\nqueue = true").expect("write malformed config");
        let _config = crate::test_support::EnvGuard::set("BN_CONFIG", &path);
        let _hostname = crate::test_support::EnvGuard::set("BN_HOSTNAME", "broken");

        let cfg = Config::load();

        assert!(!cfg.machine_registered());
        assert!(!cfg.queue_enabled_here());
        assert!(!cfg.web_enabled_here());
        assert!(!cfg.destructive_reconcile_allowed());
    }

    #[test]
    fn parsed_unregistered_config_authorizes_destructive_reconcile() {
        let _lock = crate::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[machines.other]\nrepos = [\"acme/api\"]").expect("write config");
        let _config = crate::test_support::EnvGuard::set("BN_CONFIG", &path);
        let _hostname = crate::test_support::EnvGuard::set("BN_HOSTNAME", "this-machine");

        let cfg = Config::load();

        assert!(!cfg.machine_registered());
        assert!(cfg.destructive_reconcile_allowed());
    }
}
