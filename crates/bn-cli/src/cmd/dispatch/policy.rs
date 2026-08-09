use super::{may_reuse_occupied_worktree, slugify};
use crate::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchBranchChoice {
    Use(String),
    AlreadyActive(String),
}

pub(crate) fn choose_implicit_dispatch_branch(
    title: &str,
    fresh: bool,
    launching_agent: bool,
    mut occupied: impl FnMut(&str) -> bool,
) -> DispatchBranchChoice {
    let slug = slugify(title);
    let desired = format!("dispatch/{}", if slug.is_empty() { "work" } else { &slug });
    if !occupied(&desired) || may_reuse_occupied_worktree(fresh, launching_agent) {
        DispatchBranchChoice::Use(desired)
    } else {
        DispatchBranchChoice::AlreadyActive(desired)
    }
}

const COPILOT_ONLY_UNTIL_HM: &str = "21:00";

pub(crate) fn dispatch_copilot_only_window_open(now: i64) -> bool {
    before_copilot_only_cutoff(&bn_core::util::clock_hm(now))
}

fn before_copilot_only_cutoff(hm: &str) -> bool {
    hm.len() == 5 && hm < COPILOT_ONLY_UNTIL_HM
}

pub(crate) fn normalize_agent(value: &str) -> Option<&'static str> {
    match value.trim().to_lowercase().as_str() {
        "" | "copilot" | "gh" | "github" => Some("copilot"),
        "claude" | "claudecode" | "claude-code" | "cc" => Some("claude"),
        _ => None,
    }
}

pub(crate) fn effective_launch_harness(
    cfg: &Config,
    pinned: Option<&str>,
    stage_harness: Option<&str>,
) -> bn_core::harness::DispatchHarness {
    let resolved = bn_core::harness::resolve_dispatch_harness(cfg, pinned.or(stage_harness));
    if dispatch_copilot_only_window_open(bn_core::util::now_epoch_secs())
        && resolved.harness != "copilot"
    {
        return bn_core::harness::DispatchHarness {
            harness: "copilot",
            fallback_from: Some((
                resolved.harness,
                "copilot-only dispatch window is open until 21:00".to_string(),
            )),
        };
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_dispatch_branch_uses_the_plain_slug_when_free() {
        assert_eq!(
            choose_implicit_dispatch_branch("Fix flaky cron", false, true, |_| false),
            DispatchBranchChoice::Use("dispatch/fix-flaky-cron".to_string())
        );
    }

    #[test]
    fn implicit_dispatch_branch_refuses_suffix_duplicates() {
        assert_eq!(
            choose_implicit_dispatch_branch("Fix flaky cron", false, true, |branch| {
                branch == "dispatch/fix-flaky-cron"
            }),
            DispatchBranchChoice::AlreadyActive("dispatch/fix-flaky-cron".to_string())
        );
    }

    #[test]
    fn fresh_agent_dispatch_reuses_the_existing_implicit_branch() {
        assert_eq!(
            choose_implicit_dispatch_branch("Fix flaky cron", true, true, |branch| {
                branch == "dispatch/fix-flaky-cron"
            }),
            DispatchBranchChoice::Use("dispatch/fix-flaky-cron".to_string())
        );
    }

    #[test]
    fn copilot_only_window_is_open_before_9pm() {
        assert!(before_copilot_only_cutoff("20:59"));
        assert!(!before_copilot_only_cutoff("21:00"));
        assert!(!before_copilot_only_cutoff("21:01"));
    }

    #[test]
    fn malformed_clock_time_does_not_force_copilot() {
        assert!(!before_copilot_only_cutoff(""));
        assert!(!before_copilot_only_cutoff("9:00"));
    }
}
