#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! `bn on` end to end: a rule fires a real process, exactly once, and only for events that arrive
//! after the rule exists.
//!
//! The unit tests in `bn_core::reactor` cover the matching decision. What they cannot cover is the
//! part that actually bites in use: whether the cursor is durable across separate `bn` processes,
//! and whether the event really reaches the command. Both are checked here by having the reaction
//! write a file and counting the lines.
//!
//! Run with: `cargo test -p bn-e2e --test reactor -- --ignored --test-threads=1`

use bn_e2e::{Sandbox, FROZEN_NOW};
use serde_json::Value;

/// A rule that appends `$BN_EVENT_KIND:$BN_EVENT_NUMBER` to `receipts` on each firing.
fn add_appender(sb: &Sandbox, at: i64, kind: &str, extra: &[&str]) -> anyhow::Result<()> {
    let receipts = sb.path("receipts");
    let script = format!(
        "printf '%s:%s\\n' \"$BN_EVENT_KIND\" \"$BN_EVENT_NUMBER\" >> {}",
        receipts.display()
    );
    let mut args = vec!["on", "add", kind];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "sh", "-c", &script]);
    sb.bn_at(at, &args)?.expect_success()?;
    Ok(())
}

fn receipts(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(sb.path("receipts"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_rule_fires_once_per_event_and_receives_its_fields() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    add_appender(&sb, FROZEN_NOW, "pr_merged", &[])?;
    sb.bn_at(
        FROZEN_NOW + 10,
        ["event", "pr_merged", "--field", "number=52"],
    )?
    .expect_success()?;

    sb.bn(["on", "tick"])?.expect_success()?;
    assert_eq!(
        receipts(&sb),
        ["pr_merged:52"],
        "the rule should have fired once with its field"
    );

    // The cursor is durable across processes: a second tick must not re-run the same event.
    sb.bn(["on", "tick"])?.expect_success()?;
    assert_eq!(
        receipts(&sb),
        ["pr_merged:52"],
        "a second tick re-fired a handled event"
    );
    Ok(())
}

/// Adding a rule says what should happen next; it is not a request to replay the log.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_new_rule_does_not_fire_on_events_that_predate_it() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    sb.bn_at(FROZEN_NOW, ["event", "pr_merged", "--field", "number=1"])?
        .expect_success()?;
    add_appender(&sb, FROZEN_NOW + 10, "pr_merged", &[])?;
    sb.bn(["on", "tick"])?.expect_success()?;
    assert!(
        receipts(&sb).is_empty(),
        "a new rule replayed history: {:?}",
        receipts(&sb)
    );
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_field_filter_keeps_the_wrong_events_out() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    add_appender(&sb, FROZEN_NOW, "pr_merged", &["--field", "repo=bn"])?;
    sb.bn_at(
        FROZEN_NOW + 10,
        [
            "event",
            "pr_merged",
            "--field",
            "repo=other",
            "--field",
            "number=1",
        ],
    )?
    .expect_success()?;
    sb.bn_at(
        FROZEN_NOW + 20,
        [
            "event",
            "pr_merged",
            "--field",
            "repo=bn",
            "--field",
            "number=2",
        ],
    )?
    .expect_success()?;
    sb.bn(["on", "tick"])?.expect_success()?;
    assert_eq!(
        receipts(&sb),
        ["pr_merged:2"],
        "the filter let the wrong repo through"
    );
    Ok(())
}

/// A dry run must change nothing — not the receipts, and not the cursor the next pass reads.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_dry_run_neither_runs_nor_consumes_the_event() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    add_appender(&sb, FROZEN_NOW, "pr_merged", &[])?;
    sb.bn_at(
        FROZEN_NOW + 10,
        ["event", "pr_merged", "--field", "number=7"],
    )?
    .expect_success()?;

    let run = sb.bn(["on", "tick", "--dry-run", "--json"])?;
    run.expect_success()?;
    let v: Value = serde_json::from_str(&run.stdout)?;
    assert_eq!(
        v["data"]["fired"].as_array().map(Vec::len),
        Some(1),
        "dry run saw no firing"
    );
    assert!(receipts(&sb).is_empty(), "a dry run ran the command");

    // The event is still pending, so the real tick fires it.
    sb.bn(["on", "tick"])?.expect_success()?;
    assert_eq!(
        receipts(&sb),
        ["pr_merged:7"],
        "a dry run consumed the event"
    );
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn test_reports_matches_without_running_or_consuming_anything() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    add_appender(&sb, FROZEN_NOW, "pr_merged", &["--field", "repo=bn"])?;

    let hit = sb.bn(["on", "test", "pr_merged", "--field", "repo=bn", "--json"])?;
    hit.expect_success()?;
    let v: Value = serde_json::from_str(&hit.stdout)?;
    assert_eq!(v["data"]["matched"].as_array().map(Vec::len), Some(1));

    let miss = sb.bn([
        "on",
        "test",
        "pr_merged",
        "--field",
        "repo=elsewhere",
        "--json",
    ])?;
    miss.expect_success()?;
    let v: Value = serde_json::from_str(&miss.stdout)?;
    assert_eq!(v["data"]["matched"].as_array().map(Vec::len), Some(0));

    assert!(receipts(&sb).is_empty(), "`on test` ran the command");
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_disabled_rule_stops_firing_and_resumes_when_enabled() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    add_appender(&sb, FROZEN_NOW, "pr_merged", &[])?;
    let listed = sb.bn(["on", "--json"])?;
    listed.expect_success()?;
    let v: Value = serde_json::from_str(&listed.stdout)?;
    let id = v["data"]["rules"][0]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!id.is_empty(), "no rule id in {}", listed.stdout);

    sb.bn(["on", "disable", &id])?.expect_success()?;
    sb.bn_at(
        FROZEN_NOW + 10,
        ["event", "pr_merged", "--field", "number=1"],
    )?
    .expect_success()?;
    sb.bn(["on", "tick"])?.expect_success()?;
    assert!(receipts(&sb).is_empty(), "a disabled rule fired");

    sb.bn(["on", "enable", &id])?.expect_success()?;
    sb.bn_at(
        FROZEN_NOW + 20,
        ["event", "pr_merged", "--field", "number=2"],
    )?
    .expect_success()?;
    sb.bn(["on", "tick"])?.expect_success()?;
    assert_eq!(
        receipts(&sb),
        ["pr_merged:2"],
        "an enabled rule did not resume"
    );
    Ok(())
}
