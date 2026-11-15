use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const DEFAULT_KEEP_WARM_LOW_WATER: usize = 1;

/// Persisted policy for a resident supervisor that keeps its repo's backlog supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepWarmPolicy {
    pub low_water: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_titles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_labels: Vec<String>,
}

impl Default for KeepWarmPolicy {
    fn default() -> Self {
        Self {
            low_water: DEFAULT_KEEP_WARM_LOW_WATER,
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
    pub start_issues: Vec<String>,
}

/// Decide how to restore a repo to its low-water mark without touching disk or GitHub.
pub fn plan_keep_warm(
    policy: &KeepWarmPolicy,
    active_depth: usize,
    active_ids: &HashSet<String>,
    issues: &[KeepWarmIssue],
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
    let start_issues: Vec<String> = issues
        .iter()
        .filter(|issue| !issue.assigned)
        .filter(|issue| !excluded_title(&issue.title))
        .filter(|issue| !excluded_label(&issue.labels))
        .map(|issue| issue.number.to_string())
        .filter(|id| !active_ids.contains(id))
        .take(deficit)
        .collect();
    KeepWarmPlan { start_issues }
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
        );

        assert_eq!(plan.start_issues, ["1"]);
    }

    #[test]
    fn an_empty_issue_reservoir_produces_no_work() {
        let plan = plan_keep_warm(&KeepWarmPolicy::default(), 0, &HashSet::new(), &[]);

        assert_eq!(plan, KeepWarmPlan::default());
    }

    #[test]
    fn a_full_reservoir_does_not_pull_another_issue() {
        let plan = plan_keep_warm(
            &KeepWarmPolicy::default(),
            1,
            &HashSet::new(),
            &[issue(1, "new")],
        );

        assert!(plan.start_issues.is_empty());
    }
}
