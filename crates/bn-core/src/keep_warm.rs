use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const DEFAULT_KEEP_WARM_LOW_WATER: usize = 1;
pub const DEFAULT_KEEP_WARM_BRAINSTORM_COUNT: usize = 1;
pub const DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS: i64 = 60 * 60;

/// Persisted policy for a resident supervisor that keeps its repo's backlog supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepWarmPolicy {
    pub low_water: usize,
    pub brainstorm_count: usize,
    pub brainstorm_interval_secs: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_titles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_labels: Vec<String>,
}

impl Default for KeepWarmPolicy {
    fn default() -> Self {
        Self {
            low_water: DEFAULT_KEEP_WARM_LOW_WATER,
            brainstorm_count: DEFAULT_KEEP_WARM_BRAINSTORM_COUNT,
            brainstorm_interval_secs: DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS,
            exclude_titles: vec!["gap(".to_string()],
            exclude_labels: vec!["blocked".to_string()],
        }
    }
}

/// Minimal open-issue view consumed by the pure keep-warm planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepWarmIssue {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
    pub assigned: bool,
}

/// Side effects the CLI executor should realize for one keep-warm pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepWarmPlan {
    pub enqueue_issues: Vec<String>,
    pub brainstorm: Vec<String>,
}

/// Decide how to restore a repo to its low-water mark without touching disk or GitHub.
pub fn plan_keep_warm(
    repo: &str,
    policy: &KeepWarmPolicy,
    active_depth: usize,
    active_ids: &HashSet<String>,
    issues: &[KeepWarmIssue],
    last_brainstorm_at: Option<i64>,
    now: i64,
) -> KeepWarmPlan {
    let deficit = policy.low_water.saturating_sub(active_depth);
    if deficit == 0 {
        return KeepWarmPlan::default();
    }

    let excluded_title = |title: &str| {
        let title = title.to_ascii_lowercase();
        policy
            .exclude_titles
            .iter()
            .any(|needle| title.contains(&needle.to_ascii_lowercase()))
    };
    let excluded_label = |labels: &[String]| {
        labels.iter().any(|label| {
            policy
                .exclude_labels
                .iter()
                .any(|excluded| label.eq_ignore_ascii_case(excluded))
        })
    };
    let enqueue_issues: Vec<String> = issues
        .iter()
        .filter(|issue| !issue.assigned)
        .filter(|issue| !excluded_title(&issue.title))
        .filter(|issue| !excluded_label(&issue.labels))
        .map(|issue| issue.number.to_string())
        .filter(|id| !active_ids.contains(id))
        .take(deficit)
        .collect();
    if !enqueue_issues.is_empty() || active_depth > 0 || policy.brainstorm_count == 0 {
        return KeepWarmPlan {
            enqueue_issues,
            brainstorm: Vec::new(),
        };
    }

    let rate_limited = last_brainstorm_at
        .is_some_and(|last| now.saturating_sub(last) < policy.brainstorm_interval_secs);
    if rate_limited {
        return KeepWarmPlan::default();
    }
    let bucket = now / policy.brainstorm_interval_secs.max(1);
    let brainstorm = (1..=policy.brainstorm_count)
        .map(|index| {
            format!(
                "keep-warm brainstorm {bucket}-{index} for {repo}: inspect the repository and file one deduplicated enhancement issue with `bn issue file --kind enhancement`; do not write code or open a PR"
            )
        })
        .collect();
    KeepWarmPlan {
        enqueue_issues,
        brainstorm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, title: &str) -> KeepWarmIssue {
        KeepWarmIssue {
            number,
            title: title.to_string(),
            labels: Vec::new(),
            assigned: false,
        }
    }

    #[test]
    fn selects_only_unassigned_non_excluded_issues_up_to_the_deficit() {
        let mut blocked = issue(2, "blocked");
        blocked.labels = vec!["blocked".to_string()];
        let mut assigned = issue(3, "owned");
        assigned.assigned = true;
        let mut gap = issue(4, "gap(todo)");
        gap.labels = vec!["enhancement".to_string()];
        let plan = plan_keep_warm(
            "bn",
            &KeepWarmPolicy {
                low_water: 2,
                ..Default::default()
            },
            0,
            &HashSet::from(["5".to_string()]),
            &[
                issue(1, "good"),
                blocked,
                assigned,
                gap,
                issue(5, "already active"),
            ],
            None,
            7_200,
        );

        assert_eq!(plan.enqueue_issues, ["1"]);
        assert!(plan.brainstorm.is_empty());
    }

    #[test]
    fn brainstorms_when_empty_and_not_rate_limited() {
        let plan = plan_keep_warm(
            "bn",
            &KeepWarmPolicy {
                brainstorm_count: 2,
                brainstorm_interval_secs: 3_600,
                ..Default::default()
            },
            0,
            &HashSet::new(),
            &[],
            None,
            7_200,
        );

        assert!(plan.enqueue_issues.is_empty());
        assert_eq!(plan.brainstorm.len(), 2);
        assert!(plan.brainstorm[0].contains("keep-warm brainstorm 2-1 for bn"));
    }

    #[test]
    fn recent_brainstorms_suppress_new_ones() {
        let plan = plan_keep_warm(
            "bn",
            &KeepWarmPolicy {
                brainstorm_count: 1,
                brainstorm_interval_secs: 3_600,
                ..Default::default()
            },
            0,
            &HashSet::new(),
            &[],
            Some(7_000),
            7_200,
        );

        assert!(plan.enqueue_issues.is_empty());
        assert!(plan.brainstorm.is_empty());
    }
}
