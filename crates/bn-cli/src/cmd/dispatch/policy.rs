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
    if pinned.is_none()
        && dispatch_copilot_only_window_open(bn_core::util::now_epoch_secs())
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

/// What a dispatch asks of the plan-review gate.
///
/// Tri-state rather than a `bool`, because "I did not ask for the gate" and "I am asking for no
/// gate" are different requests and only the second one may override `--yolo`. A second bool
/// alongside `plan` would have made the pair (true, true) representable and left every reader to
/// guess which wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanGate {
    /// No opinion — `[dispatch].plan_review` and `--yolo` decide.
    Auto,
    /// `--plan`: raise the gate whatever the config says.
    Force,
    /// `--no-plan-review`: no gate, even under `--yolo`.
    Off,
}

/// Whether this dispatch runs behind the plan-review gate.
///
/// A scout still skips the plan-review gate: it investigates first, then either ships a bounded
/// fix directly or finishes report-only, so there is no separate plan artifact to review.
/// Otherwise the request wins where it states one, and `Auto` falls back to the trusted-autonomy
/// rule — `--yolo` raises the gate rather than bypassing it, so autonomy costs a plan review
/// instead of buying its way out of one.
pub(crate) fn plan_gate_enabled(request: PlanGate, configured: bool, yolo: bool, scout: bool) -> bool {
    if scout {
        return false;
    }
    match request {
        PlanGate::Force => true,
        PlanGate::Off => false,
        PlanGate::Auto => configured || yolo,
    }
}

/// The seed-routing predicate: a skill-less non-scout agent dispatch enters the multi-session task
/// pipeline at its Investigate stage; a scout seeds the scout envelope; an explicit `--skill X`
/// lane (e.g. the critic's execute re-dispatch) runs that skill under the ship
/// envelope — autonomy stance, verify gate, completion protocol, optional plan gate.
///
/// `configured` (`$BN_DISPATCH_PIPELINE=1`, default false) is the master
/// switch: it defaults **off**, so the ordinary ship lane runs a single session on the `ship-solo`
/// workflow (investigate → implement → review as bounded subagents, human-gated on ambiguity)
/// rather than fanning the work across a fresh session per pipeline stage. Set it true to restore
/// the nine-stage pipeline as the default `bn dispatch` entry; the `bn pipeline` state machine is
/// unchanged and always reachable directly.
///
/// Pure so the seed-assembly tests can pin the routing table: without the `skill` arm every
/// non-scout agent dispatch took the investigate envelope, leaving the whole ship-envelope path
/// unreachable.
pub(crate) fn routes_to_pipeline_investigate(
    configured: bool,
    has_agent: bool,
    scout: bool,
    skill: Option<&str>,
) -> bool {
    configured && has_agent && !scout && skill.is_none()
}

/// Which stage this dispatch runs as — **declared if the caller knows, inferred only if not**.
///
/// The inference is a three-way guess over `(scout, skill)` and cannot express `consensus`,
/// `validate`, `critique`, `ship` or `learn` at all: every one of them lands on `execute` and
/// silently borrows that stage's model, effort and harness. That is fine for a hand-typed
/// `bn dispatch`, where those stages are not what anyone means, and wrong for a pipeline that walks
/// a task through all nine — so a producer that knows its stage passes `--stage` and is believed.
pub(crate) fn dispatch_pipeline_stage(
    declared: Option<bn_core::pipeline::stage::Stage>,
    scout: bool,
    skill: Option<&str>,
) -> bn_core::pipeline::stage::Stage {
    use bn_core::pipeline::stage::Stage;

    if let Some(stage) = declared {
        return stage;
    }
    if scout || skill == Some("investigate") {
        Stage::Investigate
    } else if skill == Some("plan") {
        Stage::Plan
    } else {
        Stage::Execute
    }
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

    #[test]
    fn pinned_claude_overrides_the_copilot_only_window() {
        let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "0");
        let cfg = Config::default();
        let resolved = effective_launch_harness(&cfg, Some("claude"), None);
        assert_eq!(resolved.harness, "claude");
        assert!(resolved.fallback_from.is_none());
    }
}