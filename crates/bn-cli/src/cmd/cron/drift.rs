use super::*;

/// Declared, host-allowed cron jobs with no registered timer or task.
pub(crate) fn unregistered_jobs(
    declared: &[String],
    registered: &std::collections::HashSet<String>,
) -> Vec<String> {
    declared
        .iter()
        .filter(|job| !registered.contains(*job))
        .cloned()
        .collect()
}

/// The fresh-machine drift: declared jobs that silently have no materialized scheduler.
///
/// "Declared" here has to mean exactly what `bn cron register` would materialize, so this reads
/// `desired_crons` — the same filter register uses — rather than a second, looser one of its own.
/// When the two disagree the operator gets a loop with no exit: `bn doctor` says "23 cron(s)
/// declared but not registered — run `bn cron register`", and `bn cron register` answers "0
/// declared (0 add, 0 update, 0 unchanged)". That is precisely what an unregistered machine used
/// to look like, because drift filtered on `job_allowed_here` alone while register additionally
/// requires `cron_enabled_here` (plus `repo_available_here` and the entry `only_on`). A machine
/// whose `[machine]` table says `cron = false` has cron disabled, so register is right to want
/// nothing — drift was the one advertising 23 jobs nothing would ever create.
/// `cron_disabled_notice` names that state instead, so agreeing here hides nothing.
pub(crate) fn cron_drift(cfg: &Config, ctx: Option<&Resolved>) -> Vec<String> {
    let registered: std::collections::HashSet<String> = timers::registered()
        .into_iter()
        .map(|timer| timer.job)
        .chain(winsched::registered().into_iter().map(|task| task.job))
        .chain(if launchd::have_launchctl() {
            launchd::registered()
                .into_iter()
                .map(|agent| agent.job)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        })
        .collect();
    cron_drift_against(cfg, ctx, &registered)
}

/// [`cron_drift`] against an already-read set of registered job names, for a caller that has
/// just listed the timers/tasks itself and should not pay for listing them again.
pub(crate) fn cron_drift_against(
    cfg: &Config,
    ctx: Option<&Resolved>,
    registered: &std::collections::HashSet<String>,
) -> Vec<String> {
    let declared: Vec<String> = desired_crons(cfg, ctx)
        .into_iter()
        .map(|cron| cron.entry.job)
        .collect();
    unregistered_jobs(&declared, registered)
}

/// Why this machine will register nothing, while the manifest nevertheless declares jobs for it.
///
/// Staying silent would be the wrong trade: the jobs are real and host-allowed, they are simply
/// never going to run, and nothing else in the output says so. `None` when cron is enabled here,
/// or when the manifest declares nothing for this host anyway — there is no problem to report.
pub(crate) fn cron_disabled_notice(cfg: &Config, ctx: Option<&Resolved>) -> Option<String> {
    if cfg.cron_enabled_here() {
        return None;
    }
    let declared = bn_core::crons::discover(cfg, ctx)
        .into_iter()
        .filter(|cron| cfg.job_allowed_here(&cron.entry.job))
        .count();
    if declared == 0 {
        return None;
    }
    let machine = cfg.machine();
    Some(format!(
        "cron is switched off on this machine ('{machine}') — {declared} declared job(s) will \
         never run. Remove `cron = false` from [machine] in {}, then re-run `bn cron register`.",
        bn_core::resolver::machine_config_path().display()
    ))
}

/// Print the disabled-machine notice, if this host has one — the shared `bn cron status` /
/// `bn doctor` shape.
pub(crate) fn print_cron_disabled_notice(cfg: &Config, ctx: Option<&Resolved>) {
    if let Some(notice) = cron_disabled_notice(cfg, ctx) {
        println!("⚠ {notice}");
    }
}

/// The `bn cron status` header: what is declared-but-unregistered, and — when the answer is
/// "nothing, because this host is switched off" — why. Kept together so the two can never again
/// be rendered from different notions of "declared".
pub(crate) fn print_cron_health(cfg: &Config, ctx: Option<&Resolved>, drift: &[String]) {
    if !drift.is_empty() {
        println!(
            "⚠ {} cron(s) declared but not registered on this machine — run `bn cron register`:",
            drift.len()
        );
        println!("    {}\n", drift.join(", "));
    }
    print_cron_disabled_notice(cfg, ctx);
}

/// Record the notice for `bn cron register`, whose human output prints `warnings` and whose JSON
/// envelope carries them. Returns whether cron is disabled here, which also decides if the
/// "all timers already match" line would be a lie.
pub(crate) fn push_cron_disabled_warning(
    cfg: &Config,
    ctx: Option<&Resolved>,
    warnings: &mut Vec<String>,
) -> bool {
    match cron_disabled_notice(cfg, ctx) {
        Some(notice) => {
            warnings.push(notice);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_jobs_preserve_declared_order() {
        let declared = vec!["alpha".to_string(), "beta".to_string()];
        let registered = std::collections::HashSet::from(["beta".to_string()]);

        assert_eq!(unregistered_jobs(&declared, &registered), vec!["alpha"]);
    }

    /// A profile that says `cron = false`.
    fn cron_off() -> Option<bn_core::resolver::MachineProfile> {
        Some(bn_core::resolver::MachineProfile {
            cron: false,
            ..Default::default()
        })
    }

    /// A machine whose profile says `cron = false` has cron disabled, so `bn cron register`
    /// materializes nothing. Drift must agree, or `bn doctor` sends people to a command that
    /// answers "0 declared" — the loop this pairing exists to close.
    #[test]
    fn drift_is_empty_when_cron_is_disabled_on_this_machine() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, _cfg_dir) = seed_manifest(temp.path(), &["alpha", "beta"], cron_off());

        assert!(cron_drift(&cfg, None).is_empty());
    }

    #[test]
    fn disabled_machine_notice_names_the_host_and_the_job_count() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, _cfg_dir) = seed_manifest(temp.path(), &["alpha", "beta"], cron_off());

        let notice = cron_disabled_notice(&cfg, None).expect("a disabled machine explains itself");

        assert!(notice.contains("testbox"), "{notice}");
        assert!(notice.contains('2'), "{notice}");
    }

    /// The notice is for the switched-off case only — an enabled machine with unregistered jobs
    /// must keep getting the ordinary "run `bn cron register`" drift nudge, not this one.
    #[test]
    fn enabled_machine_gets_no_disabled_notice_and_still_drifts() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let profile = bn_core::resolver::MachineProfile {
            repos: vec!["*".to_string()],
            cron: true,
        };
        let (cfg, _cfg_dir) = seed_manifest(temp.path(), &["alpha", "beta"], Some(profile));

        assert!(cron_disabled_notice(&cfg, None).is_none());
        assert_eq!(cron_drift(&cfg, None), vec!["alpha", "beta"]);
    }

    /// Nothing declared for this host is not a problem worth a warning, enabled or not.
    #[test]
    fn an_empty_manifest_produces_no_notice() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, _cfg_dir) = seed_manifest(temp.path(), &[], None);

        assert!(cron_disabled_notice(&cfg, None).is_none());
    }

    /// The background reconcile re-registers only when what registration would install has moved:
    /// the same manifest fingerprints the same, and one more cron changes it.
    #[test]
    fn the_timers_fingerprint_moves_only_with_the_declared_crons() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, guard) = seed_manifest(temp.path(), &["alpha"], None);
        let one = crate::cmd::cron::reconcile::timers_fingerprint(&cfg, None);
        assert_eq!(one, crate::cmd::cron::reconcile::timers_fingerprint(&cfg, None));
        drop(guard);

        let (cfg, _guard) = seed_manifest(temp.path(), &["alpha", "beta"], None);
        assert_ne!(one, crate::cmd::cron::reconcile::timers_fingerprint(&cfg, None));
    }

    /// Point `config_dir()` at a scratch `crons.toml` declaring `jobs`, and return a Config whose
    /// machine profile is `profile` (`None` = the unregistered machine, cron disabled). The
    /// returned guard must stay alive for the length of the test — dropping it restores the real
    /// `BN_CONFIG_DIR` and the manifest disappears.
    fn seed_manifest(
        root: &std::path::Path,
        jobs: &[&str],
        profile: Option<bn_core::resolver::MachineProfile>,
    ) -> (Config, bn_core::test_support::EnvGuard) {
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).expect("config dir");
        let manifest: String = jobs
            .iter()
            .map(|job| {
                format!("[[cron]]\njob = \"{job}\"\non_calendar = \"*-*-* 03:00:00\"\n\n")
            })
            .collect();
        std::fs::write(cfg_dir.join("crons.toml"), manifest).expect("crons.toml");
        let guard = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", &cfg_dir);
        let cfg = Config {
            hostname: "testbox".to_string(),
            machine_profile: profile,
            ..Config::default()
        };
        (cfg, guard)
    }

    #[test]
    fn launchd_registered_jobs_are_not_reported_as_unregistered() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join("Library/LaunchAgents")).expect("launchagents dir");
        std::fs::write(
            home.join("Library/LaunchAgents/com.bn.loaded-job.plist"),
            "<?xml version=\"1.0\"?><plist><dict><key>Label</key><string>com.bn.loaded-job</string></dict></plist>",
        )
        .expect("launchd plist");
        let _home = bn_core::test_support::EnvGuard::set("HOME", &home);

        let declared = vec!["loaded-job".to_string(), "missing-job".to_string()];
        let registered: std::collections::HashSet<String> = launchd::registered()
            .into_iter()
            .map(|agent| agent.job)
            .collect();

        assert_eq!(unregistered_jobs(&declared, &registered), vec!["missing-job"]);
    }
}
