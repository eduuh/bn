//! Sizing a plan against the time actually left — the one place bn already respects duration.
//!
//! `bn next --fit` hides today's goals that cannot finish before the next meeting. It is the only
//! fit-to-window logic in the product. These tests pin the two properties that make it safe to
//! reuse — non-plan work is never filtered, and work that cannot be sized is surfaced rather than
//! hidden.
use bn_core::calib::{CoarseSizes, Report};
use bn_core::meetings::Gap;
use bn_core::next::{expected_secs, fit_plan, Action, ActionKind};

fn action(kind: ActionKind, title: &str, est: Option<&str>) -> Action {
    Action {
        kind,
        title: title.to_string(),
        detail: String::new(),
        link: None,
        repo: Some("bn".to_string()),
        branch: None,
        momentum: None,
        attn_id: None,
        est: est.map(str::to_string),
        priority: 1,
        age: 0,
        age_hours: None,
    }
}

fn gap(seconds: i64) -> Gap {
    Gap {
        seconds,
        minutes: seconds / 60,
        title: "standup".to_string(),
        start: "10:30".to_string(),
    }
}

fn kept(plan: &bn_core::next::FitPlan) -> Vec<&str> {
    plan.kept.iter().map(|a| a.title.as_str()).collect()
}

/// An uncalibrated estimate is taken at face value; a multiplier scales it.
#[test]
fn the_calibration_multiplier_scales_an_estimate() {
    assert_eq!(expected_secs(3600, None), 3600);
    assert_eq!(expected_secs(3600, Some(1.5)), 5400);
    assert_eq!(expected_secs(3600, Some(0.5)), 1800);
    assert_eq!(
        expected_secs(3600, Some(0.0)),
        3600,
        "a nonsense multiplier must not erase the estimate"
    );
}

/// With no meeting ahead, `--fit` is a no-op — the whole list survives untouched.
#[test]
fn no_gap_keeps_everything() {
    let actions = vec![
        action(ActionKind::Goal, "a long goal", Some("3h")),
        action(ActionKind::Goal, "a short goal", Some("15m")),
    ];
    let plan = fit_plan(actions, None, &Report::default(), &CoarseSizes::default());
    assert_eq!(kept(&plan), ["a long goal", "a short goal"]);
    assert_eq!(plan.dropped, 0);
    assert!(plan.unsized_tail.is_empty());
}

/// The point of the feature: a goal that cannot finish before the meeting is not offered.
#[test]
fn a_goal_that_does_not_fit_the_gap_is_dropped() {
    let actions = vec![
        action(ActionKind::Goal, "fits", Some("15m")),
        action(ActionKind::Goal, "does not fit", Some("3h")),
    ];
    let plan = fit_plan(
        actions,
        Some(&gap(30 * 60)),
        &Report::default(),
        &CoarseSizes::default(),
    );
    assert_eq!(kept(&plan), ["fits"]);
    assert_eq!(plan.dropped, 1);
}

/// The wrap-up buffer is real: a goal that exactly fills the gap does not fit it.
#[test]
fn the_buffer_leaves_a_moment_to_wrap_up() {
    let exactly = 30 * 60;
    let snug = action(ActionKind::Goal, "snug", Some("30m"));
    let plan = fit_plan(
        vec![snug],
        Some(&gap(exactly)),
        &Report::default(),
        &CoarseSizes::default(),
    );
    assert_eq!(plan.dropped, 1, "a goal filling the gap leaves no buffer");

    let trimmed = action(ActionKind::Goal, "trimmed", Some("25m"));
    let plan = fit_plan(
        vec![trimmed],
        Some(&gap(exactly)),
        &Report::default(),
        &CoarseSizes::default(),
    );
    assert_eq!(kept(&plan), ["trimmed"]);
}

/// Work that cannot be sized is surfaced separately rather than silently hidden — fit cannot judge
/// it, and quietly dropping it would make the estimate mandatory by stealth.
#[test]
fn unestimated_goals_are_surfaced_not_hidden() {
    let actions = vec![
        action(ActionKind::Goal, "sized", Some("10m")),
        action(ActionKind::Goal, "unsized", None),
    ];
    let plan = fit_plan(
        actions,
        Some(&gap(30 * 60)),
        &Report::default(),
        &CoarseSizes::default(),
    );
    assert_eq!(kept(&plan), ["sized"]);
    assert_eq!(plan.dropped, 0);
    assert_eq!(
        plan.unsized_tail
            .iter()
            .map(|a| a.title.as_str())
            .collect::<Vec<_>>(),
        ["unsized"]
    );
}

/// Urgency does not care how much time is left. A failing cron or a review somebody is blocked on
/// is never filtered by fit — only the plan band is.
#[test]
fn only_the_plan_band_is_ever_filtered() {
    let actions = vec![
        action(ActionKind::Cron, "a failing cron", Some("8h")),
        action(ActionKind::Review, "somebody is blocked", Some("8h")),
        action(ActionKind::Blocked, "a blocked note", None),
        action(ActionKind::Goal, "a long goal", Some("8h")),
    ];
    let plan = fit_plan(
        actions,
        Some(&gap(10 * 60)),
        &Report::default(),
        &CoarseSizes::default(),
    );
    assert_eq!(
        kept(&plan),
        ["a failing cron", "somebody is blocked", "a blocked note"]
    );
    assert_eq!(plan.dropped, 1);
}

/// T-shirt sizes resolve through the configured coarse table, so `S` means whatever this machine
/// says it means.
#[test]
fn coarse_sizes_resolve_through_the_configured_table() {
    let sizes = CoarseSizes {
        s: 10 * 60,
        m: 90 * 60,
        l: 3 * 3600,
    };
    let plan = fit_plan(
        vec![action(ActionKind::Goal, "small", Some("S"))],
        Some(&gap(30 * 60)),
        &Report::default(),
        &sizes,
    );
    assert_eq!(kept(&plan), ["small"]);

    let plan = fit_plan(
        vec![action(ActionKind::Goal, "large", Some("L"))],
        Some(&gap(30 * 60)),
        &Report::default(),
        &sizes,
    );
    assert_eq!(plan.dropped, 1);
}
