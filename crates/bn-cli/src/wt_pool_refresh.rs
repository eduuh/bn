use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// Don't re-warm a repo whose slots were refreshed within this window.
const POOL_REFRESH_MAX_AGE_SECS: i64 = 24 * 3600;

/// Minimum gap between mandatory warm attempts.
const POOL_REFRESH_ATTEMPT_MIN_AGE_SECS: i64 = 30 * 60;

/// What a pool refresh should do this tick.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PoolRefreshPlan {
    /// Nothing to warm (no pools, all fresh, or attempted recently) — logged, not an error.
    Skip(String),
    /// Warm these repos (de-duped, config order preserved).
    Warm(Vec<String>),
}

/// Why one repository needs its next pool refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoolFreshness {
    /// The latest successful warm satisfies the most recent daily deadline.
    Fresh,
    /// A daily deadline passed after the latest successful warm.
    DailyDue,
    /// No successful warm exists, or the latest one is at least 24 hours old.
    HardStale,
}

/// One pooled repository and its refresh state at a captured instant.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolRefreshAssessment {
    repo: String,
    freshness: PoolFreshness,
}

pub(crate) fn plan_pool_refresh(cfg: &Config, now: i64) -> PoolRefreshPlan {
    let events = bn_core::events::read_all();
    if mandatory_sweep_attempted_recently(&events, now) {
        return PoolRefreshPlan::Skip("pool-warm attempted recently".to_string());
    }
    let deadline = latest_pool_warm_deadline(&cfg.queue.windows, now);
    let assessments = classify_pool_refreshes(
        &crate::wt_pool::pooled_repos(),
        &last_successful_pool_warm_by_repo(&events),
        now,
        deadline,
        POOL_REFRESH_MAX_AGE_SECS,
    );
    plan_refreshes(&assessments)
}

fn mandatory_sweep_attempted_recently(events: &[bn_core::events::Event], now: i64) -> bool {
    events.iter().any(|event| {
        event.kind == "pool_refresh_attempt"
            && now.saturating_sub(event.ts) < POOL_REFRESH_ATTEMPT_MIN_AGE_SECS
    })
}

/// Most recent daily pool-warm deadline at or before `now`.
///
/// A custom queue config may define unrelated windows and omit the built-in pool windows. Falling
/// back here keeps a valid daily deadline authoritative instead of silently disabling freshness.
fn latest_pool_warm_deadline(
    windows: &[bn_core::queue::window::PriorityWindow],
    now: i64,
) -> Option<i64> {
    windows
        .iter()
        .filter(|w| {
            w.name == bn_core::queue::window::POOL_WARM_AFTER_WORK
                && w.mode == bn_core::queue::window::WindowMode::Deadline
        })
        .filter_map(|w| bn_core::staff_schedule::previous_occurrence(&w.at, now))
        .max()
        .or_else(|| {
            bn_core::queue::window::default_pool_warm_priority_windows()
                .iter()
                .find(|w| w.name == bn_core::queue::window::POOL_WARM_AFTER_WORK)
                .and_then(|w| bn_core::staff_schedule::previous_occurrence(&w.at, now))
        })
}

/// Freshest successful `pool_warm` timestamp per repo from the machine-local event log.
///
/// Legacy events predate the `complete` field and count as successful for migration. Once a warm
/// records `complete = false`, it is an attempt only and cannot move the freshness clock.
fn last_successful_pool_warm_by_repo(evs: &[bn_core::events::Event]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for e in evs.iter().filter(|e| {
        e.kind == "pool_warm"
            && e.fields
                .get("complete")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
    }) {
        if let Some(repo) = e.fields.get("repo").and_then(|v| v.as_str()) {
            let slot = m.entry(repo.to_string()).or_insert(e.ts);
            if e.ts > *slot {
                *slot = e.ts;
            }
        }
    }
    m
}

/// Classify every pooled repository at one captured instant.
///
/// Config order is retained and duplicate repo names collapse onto their first occurrence. The
/// 24-hour boundary is inclusive: exactly 24 hours old is already hard-stale.
fn classify_pool_refreshes(
    repos: &[String],
    last_success: &BTreeMap<String, i64>,
    now: i64,
    latest_deadline: Option<i64>,
    max_age: i64,
) -> Vec<PoolRefreshAssessment> {
    let max_age = max_age.max(0);
    let mut assessments = Vec::new();
    let mut seen = BTreeSet::new();
    for r in repos {
        if !seen.insert(r.as_str()) {
            continue;
        }
        let freshness = match last_success.get(r).copied() {
            None => PoolFreshness::HardStale,
            Some(last) if now.saturating_sub(last) >= max_age => PoolFreshness::HardStale,
            Some(last) if latest_deadline.is_some_and(|deadline| last < deadline) => {
                PoolFreshness::DailyDue
            }
            Some(_) => PoolFreshness::Fresh,
        };
        assessments.push(PoolRefreshAssessment {
            repo: r.clone(),
            freshness,
        });
    }
    assessments
}

/// Select every due repository for one mandatory sweep.
fn plan_refreshes(assessments: &[PoolRefreshAssessment]) -> PoolRefreshPlan {
    if assessments.is_empty() {
        return PoolRefreshPlan::Skip("no pooled repos configured".into());
    }
    let targets = assessments
        .iter()
        .filter(|assessment| assessment.freshness != PoolFreshness::Fresh)
        .map(|assessment| assessment.repo.clone())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return PoolRefreshPlan::Skip("all pools refreshed recently".into());
    }
    PoolRefreshPlan::Warm(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assessment(repo: &str, freshness: PoolFreshness) -> PoolRefreshAssessment {
        PoolRefreshAssessment {
            repo: repo.to_string(),
            freshness,
        }
    }

    #[test]
    fn no_pools_skips() {
        let plan = plan_refreshes(&[]);
        assert_eq!(
            plan,
            PoolRefreshPlan::Skip("no pooled repos configured".into())
        );
    }

    #[test]
    fn recent_pool_skips() {
        let assessments = vec![assessment("bn", PoolFreshness::Fresh)];
        let plan = plan_refreshes(&assessments);
        assert_eq!(
            plan,
            PoolRefreshPlan::Skip("all pools refreshed recently".into())
        );
    }

    #[test]
    fn stale_pool_warms() {
        let assessments = vec![assessment("bn", PoolFreshness::HardStale)];
        let plan = plan_refreshes(&assessments);
        assert_eq!(plan, PoolRefreshPlan::Warm(vec!["bn".to_string()]));
    }

    #[test]
    fn mandatory_sweep_includes_every_due_pool() {
        let assessments = vec![
            assessment("fresh", PoolFreshness::Fresh),
            assessment("daily", PoolFreshness::DailyDue),
            assessment("stale", PoolFreshness::HardStale),
        ];
        assert_eq!(
            plan_refreshes(&assessments),
            PoolRefreshPlan::Warm(vec!["daily".to_string(), "stale".to_string()])
        );
    }

    #[test]
    fn classifications_distinguish_fresh_daily_due_and_hard_stale() {
        let now = 100_000;
        let deadline = now - 1_000;
        let repos = vec!["fresh".into(), "due".into(), "stale".into(), "new".into()];
        let last = BTreeMap::from([
            ("fresh".into(), deadline + 1),
            ("due".into(), deadline - 1),
            ("stale".into(), now - 86_400),
        ]);

        assert_eq!(
            classify_pool_refreshes(&repos, &last, now, Some(deadline), 86_400),
            vec![
                assessment("fresh", PoolFreshness::Fresh),
                assessment("due", PoolFreshness::DailyDue),
                assessment("stale", PoolFreshness::HardStale),
                assessment("new", PoolFreshness::HardStale),
            ]
        );
    }

    #[test]
    fn deadline_boundary_uses_the_latest_success() {
        let repos = vec!["before".into(), "at".into(), "after".into()];
        let deadline = 50_000;
        let last = BTreeMap::from([
            ("before".into(), deadline - 1),
            ("at".into(), deadline),
            ("after".into(), deadline + 1),
        ]);
        assert_eq!(
            classify_pool_refreshes(&repos, &last, 60_000, Some(deadline), 86_400),
            vec![
                assessment("before", PoolFreshness::DailyDue),
                assessment("at", PoolFreshness::Fresh),
                assessment("after", PoolFreshness::Fresh),
            ]
        );
    }

    #[test]
    fn successful_retry_exits_due_state() {
        let repos = vec!["bn".to_string()];
        let deadline = 50_000;
        assert_eq!(
            classify_pool_refreshes(
                &repos,
                &BTreeMap::from([("bn".to_string(), deadline - 1)]),
                60_000,
                Some(deadline),
                86_400,
            )[0]
            .freshness,
            PoolFreshness::DailyDue
        );
        assert_eq!(
            classify_pool_refreshes(
                &repos,
                &BTreeMap::from([("bn".to_string(), deadline + 1)]),
                60_000,
                Some(deadline),
                86_400,
            )[0]
            .freshness,
            PoolFreshness::Fresh
        );
    }

    #[test]
    fn machine_deadline_uses_its_configured_time_on_weekends() {
        let now = bn_core::track::parse_at("2026-08-29", "12:00")
            .expect("Saturday noon exists in local time");
        let deadline = |hour: u32| bn_core::queue::window::PriorityWindow {
            name: bn_core::queue::window::POOL_WARM_AFTER_WORK.to_string(),
            at: format!("*-*-* {hour:02}:00:00"),
            secs: 3600,
            boost: 100,
            matches: vec!["source:cron:pool-warm".to_string()],
            mode: bn_core::queue::window::WindowMode::Deadline,
        };

        let eleven = latest_pool_warm_deadline(&[deadline(11)], now).unwrap();
        let ten = latest_pool_warm_deadline(&[deadline(10)], now).unwrap();
        assert_eq!(eleven - ten, 3600);
        assert_eq!(now - eleven, 3600);
    }

    #[test]
    fn exactly_24_hours_old_is_hard_stale() {
        let repos = vec!["bn".to_string()];
        let last = BTreeMap::from([("bn".to_string(), 13_600)]);
        assert_eq!(
            classify_pool_refreshes(&repos, &last, 100_000, Some(10_000), 86_400),
            vec![assessment("bn", PoolFreshness::HardStale)]
        );
    }

    #[test]
    fn duplicate_repos_are_classified_once_in_config_order() {
        let repos = vec!["bn".to_string(), "bn".to_string(), "other".to_string()];
        assert_eq!(
            classify_pool_refreshes(&repos, &BTreeMap::new(), 1_000, None, 100),
            vec![
                assessment("bn", PoolFreshness::HardStale),
                assessment("other", PoolFreshness::HardStale),
            ]
        );
    }

    #[test]
    fn incomplete_events_do_not_advance_successful_freshness() {
        let events = vec![
            bn_core::events::Event {
                kind: "pool_warm".into(),
                ts: 100,
                fields: serde_json::Map::from_iter([
                    ("repo".into(), "bn".into()),
                    ("complete".into(), true.into()),
                ]),
            },
            bn_core::events::Event {
                kind: "pool_warm".into(),
                ts: 200,
                fields: serde_json::Map::from_iter([
                    ("repo".into(), "bn".into()),
                    ("complete".into(), false.into()),
                ]),
            },
        ];
        assert_eq!(
            last_successful_pool_warm_by_repo(&events),
            BTreeMap::from([("bn".to_string(), 100)])
        );
    }

    #[test]
    fn only_machine_wide_attempts_throttle_mandatory_refresh() {
        let now = 10_000;
        let targeted = bn_core::events::Event {
            kind: "cron_run".into(),
            ts: now - 1,
            fields: serde_json::Map::from_iter([("job".into(), "pool-warm".into())]),
        };
        let mandatory = bn_core::events::Event {
            kind: "pool_refresh_attempt".into(),
            ts: now - 1,
            fields: serde_json::Map::new(),
        };

        assert!(!mandatory_sweep_attempted_recently(&[targeted], now));
        assert!(mandatory_sweep_attempted_recently(&[mandatory], now));
    }
}
