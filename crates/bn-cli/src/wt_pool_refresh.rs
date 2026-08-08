use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// How long the box must have been quiet before an idle-time pool refresh runs.
const POOL_REFRESH_IDLE_MINS: i64 = 20;

/// Don't re-warm a repo whose slots were refreshed within this window.
const POOL_REFRESH_MIN_AGE_SECS: i64 = 4 * 3600;

/// Minimum gap between opportunistic warm attempts.
const POOL_REFRESH_ATTEMPT_MIN_AGE_SECS: i64 = 30 * 60;

/// What an idle-time pool refresh should do this tick.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PoolRefreshPlan {
    /// Nothing to warm (box active, no pools, or all fresh) — the reason is logged, not an error.
    Skip(String),
    /// Warm these repos (de-duped, config order preserved).
    Warm(Vec<String>),
}

pub(crate) fn plan_pool_refresh(cfg: &Config, now: i64) -> PoolRefreshPlan {
    let events = bn_core::events::read_all();
    if events.iter().any(|e| {
        e.kind == "cron_run"
            && e.fields.get("job").and_then(|v| v.as_str()) == Some("pool-warm")
            && now.saturating_sub(e.ts) < POOL_REFRESH_ATTEMPT_MIN_AGE_SECS
    }) {
        return PoolRefreshPlan::Skip("pool-warm attempted recently".to_string());
    }
    plan_idle_pool_refresh(
        pool_refresh_allowed(cfg, now),
        &crate::wt_pool::pooled_repos(),
        &last_pool_warm_by_repo(&events),
        now,
        POOL_REFRESH_MIN_AGE_SECS,
    )
}

fn pool_refresh_allowed(cfg: &Config, now: i64) -> bool {
    if bn_core::idle::is_idle(cfg, now, POOL_REFRESH_IDLE_MINS) {
        return true;
    }
    let mut spec = bn_core::backlog::AddSpec {
        kind: bn_core::backlog::BacklogKind::Cron,
        priority: -50,
        source: "cron:pool-warm".to_string(),
        exec: bn_core::backlog::ExecSpec::Job {
            job: "pool-warm".to_string(),
            repo: None,
            branch: None,
        },
        ..Default::default()
    };
    spec.heavy = true;
    let entry =
        bn_core::backlog::BacklogEntry::new(&cfg.queue_home_repo(), "run pool-warm", &spec, now);
    bn_core::queue::window::allows_opportunistic(&entry, &cfg.queue.windows, now)
}

/// Freshest `pool_warm` timestamp per repo from the machine-local event log.
fn last_pool_warm_by_repo(evs: &[bn_core::events::Event]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for e in evs.iter().filter(|e| e.kind == "pool_warm") {
        if let Some(repo) = e.fields.get("repo").and_then(|v| v.as_str()) {
            let slot = m.entry(repo.to_string()).or_insert(e.ts);
            if e.ts > *slot {
                *slot = e.ts;
            }
        }
    }
    m
}

/// Decide which pooled repos to warm on an idle tick.
fn plan_idle_pool_refresh(
    idle: bool,
    repos: &[String],
    last_warm: &BTreeMap<String, i64>,
    now: i64,
    min_age: i64,
) -> PoolRefreshPlan {
    if repos.is_empty() {
        return PoolRefreshPlan::Skip("no pooled repos configured".into());
    }
    if !idle {
        return PoolRefreshPlan::Skip("box is active — deferring pool refresh".into());
    }
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for r in repos {
        if !seen.insert(r.as_str()) {
            continue;
        }
        let stale = last_warm.get(r).is_none_or(|ts| now - *ts >= min_age);
        if stale {
            targets.push(r.clone());
        }
    }
    if targets.is_empty() {
        return PoolRefreshPlan::Skip("all pools refreshed recently".into());
    }
    PoolRefreshPlan::Warm(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pools_skips() {
        let plan = plan_idle_pool_refresh(true, &[], &BTreeMap::new(), 1000, 100);
        assert_eq!(
            plan,
            PoolRefreshPlan::Skip("no pooled repos configured".into())
        );
    }

    #[test]
    fn active_box_skips() {
        let repos = vec!["bn".to_string()];
        let plan = plan_idle_pool_refresh(false, &repos, &BTreeMap::new(), 1000, 100);
        assert_eq!(
            plan,
            PoolRefreshPlan::Skip("box is active — deferring pool refresh".into())
        );
    }

    #[test]
    fn recent_pool_skips() {
        let repos = vec!["bn".to_string()];
        let last = BTreeMap::from([("bn".to_string(), 950)]);
        let plan = plan_idle_pool_refresh(true, &repos, &last, 1000, 100);
        assert_eq!(
            plan,
            PoolRefreshPlan::Skip("all pools refreshed recently".into())
        );
    }

    #[test]
    fn stale_pool_warms() {
        let repos = vec!["bn".to_string()];
        let last = BTreeMap::from([("bn".to_string(), 800)]);
        let plan = plan_idle_pool_refresh(true, &repos, &last, 1000, 100);
        assert_eq!(plan, PoolRefreshPlan::Warm(vec!["bn".to_string()]));
    }

    #[test]
    fn duplicate_repos_are_deduped() {
        let repos = vec!["bn".to_string(), "bn".to_string(), "other".to_string()];
        let plan = plan_idle_pool_refresh(true, &repos, &BTreeMap::new(), 1000, 100);
        assert_eq!(
            plan,
            PoolRefreshPlan::Warm(vec!["bn".to_string(), "other".to_string()])
        );
    }
}
