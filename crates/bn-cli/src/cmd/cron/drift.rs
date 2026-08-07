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
}
