//! Progress notes back to Forge from the pipeline's own write points: a stage transition and a
//! SMART refusal. The file provider has nowhere to put them, so these run with the Forge provider
//! over the in-memory fake, reached through the ordinary `task_store(cfg)` path.

use std::sync::atomic::Ordering;

use super::gate::{self, GateDecision};
use super::stage::{self, Stage};
use crate::forge_app::client::BnTaskRow;
use crate::resolver::Resolved;
use crate::test_support::forge::{row, ForgeEnv};

fn forge(name: &str, rows: Vec<BnTaskRow>) -> ForgeEnv {
    ForgeEnv::new(name, rows).unwrap()
}

fn context(repo: &str, branch: &str) -> Resolved {
    Resolved {
        repo: repo.into(),
        branch: branch.into(),
    }
}

#[test]
fn a_stage_transition_comments_on_the_governing_forge_task() {
    let f = forge(
        "stage-note",
        vec![row(
            "a1",
            "Wire the gate",
            "2026-09-01",
            &[("id", "t1"), ("repo", "bn"), ("branch", "feat/x")],
        )],
    );
    let ctx = context("bn", "feat/x");

    let decision = stage::transition(&f.cfg, &ctx, Stage::Plan).unwrap();
    assert_eq!(decision, GateDecision::Admitted);
    assert_eq!(
        f.fake.comments(),
        [("a1".to_string(), "stage 3/9: plan (bn:feat/x)".to_string())]
    );
}

#[test]
fn a_forge_outage_never_fails_the_transition() {
    let f = forge(
        "stage-outage",
        vec![row(
            "a1",
            "Wire the gate",
            "2026-09-01",
            &[("id", "t1"), ("repo", "bn"), ("branch", "feat/x")],
        )],
    );
    f.fake.fail_comments.store(true, Ordering::Relaxed);
    let ctx = context("bn", "feat/x");

    stage::transition(&f.cfg, &ctx, Stage::Execute).expect("the stage still moves");
    assert_eq!(stage::current("bn", "feat/x"), Some(Stage::Execute));
    assert!(f.fake.comments().is_empty());
}

#[test]
fn a_worktree_no_task_governs_leaves_no_comment() {
    let f = forge(
        "stage-ungoverned",
        vec![row(
            "a1",
            "Other work",
            "2026-09-01",
            &[("id", "t1"), ("repo", "bn"), ("branch", "feat/other")],
        )],
    );
    let ctx = context("bn", "feat/x");
    stage::transition(&f.cfg, &ctx, Stage::Plan).unwrap();
    assert!(f.fake.comments().is_empty());
}

#[test]
fn a_smart_refusal_tells_forge_what_detail_is_missing() {
    let f = forge(
        "smart-refusal",
        vec![row(
            "a1",
            "look into it",
            "2026-09-01",
            &[("id", "t4"), ("repo", "bn")],
        )],
    );
    let task = crate::task_store::task_store(&f.cfg)
        .load()
        .unwrap()
        .tasks()
        .next()
        .cloned()
        .unwrap();
    f.fake.take_calls();

    let decision = gate::judge(&f.cfg, &task, "2026-09-14");
    assert!(
        matches!(decision, GateDecision::Refused { .. }),
        "{decision:?}"
    );
    let comments = f.fake.comments();
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0].0, "a1");
    assert!(
        comments[0].1.starts_with("needs more detail: "),
        "{comments:?}"
    );
    assert_eq!(
        f.fake.row("a1").agent_meta.get("smart").map(String::as_str),
        Some("fail"),
        "the stamp still lands"
    );
}
