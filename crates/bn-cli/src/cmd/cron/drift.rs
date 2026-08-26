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
    let declared: Vec<String> = bn_core::crons::discover(cfg, ctx)
        .into_iter()
        .filter(|cron| cfg.job_allowed_here(&cron.entry.job))
        .map(|cron| cron.entry.job)
        .collect();
    unregistered_jobs(&declared, &registered)
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
