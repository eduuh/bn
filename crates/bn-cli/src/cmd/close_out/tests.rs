use super::*;
use bn_core::test_support::forge::{agent_row, ForgeEnv};

const PR: &str = "https://github.com/eduuh/bn/pull/42";

fn ctx() -> Resolved {
    Resolved {
        repo: "bn".into(),
        branch: "feat/x".into(),
    }
}

fn forge(name: &str) -> ForgeEnv {
    let mut row = agent_row(
        "a1",
        "Wire the gate",
        "2026-09-01",
        &[
            ("id", "t1"),
            ("repo", "bn"),
            ("branch", "feat/x"),
            ("claimer", "bn@m1"),
        ],
    );
    row.started_at = Some("2026-09-02T08:00:00Z".into());
    ForgeEnv::new(name, vec![row]).unwrap()
}

#[test]
fn a_forge_task_with_a_pr_stays_open_until_the_merge() {
    let f = forge("close-out-pr");
    let closed = run(
        &f.cfg,
        &ctx(),
        "gate wired",
        &Some(PR.into()),
        false,
        false,
        true,
    );

    assert_eq!(closed.task, None, "not closed yet");
    assert_eq!(closed.waiting_on_pr.as_deref(), Some("t1"));
    let row = f.fake.row("a1");
    assert!(!row.is_completed);
    assert!(row.started_at.is_some(), "still wip");
    assert_eq!(row.agent_meta.get("pr").map(String::as_str), Some(PR));
    assert_eq!(
        f.fake.comments(),
        [(
            "a1".to_string(),
            format!("agent done: gate wired\nPR: {PR}\nThis task completes when the PR merges.")
        )]
    );
}

#[test]
fn a_forge_task_done_without_a_pr_completes_and_says_so() {
    let f = forge("close-out-no-pr");
    let closed = run(&f.cfg, &ctx(), "answered", &None, false, false, true);
    assert_eq!(closed.task.as_deref(), Some("t1"));
    assert!(f.fake.row("a1").is_completed);
    assert_eq!(
        f.fake.comments(),
        [("a1".to_string(), "agent done: answered".to_string())]
    );
}

#[test]
fn a_failed_forge_task_stays_open_and_reports_the_failure() {
    let f = forge("close-out-failed");
    let closed = run(
        &f.cfg,
        &ctx(),
        "tests red",
        &Some(PR.into()),
        true,
        false,
        true,
    );
    assert_eq!((closed.task, closed.waiting_on_pr), (None, None));
    assert!(!f.fake.row("a1").is_completed);
    let comments = f.fake.comments();
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0].1.starts_with("agent failed: tests red\nPR: "),
        "{comments:?}"
    );
}

#[test]
fn a_forge_outage_on_the_comment_still_closes_out() {
    let f = forge("close-out-outage");
    f.fake
        .fail_comments
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let closed = run(&f.cfg, &ctx(), "done", &Some(PR.into()), false, false, true);
    assert_eq!(closed.waiting_on_pr.as_deref(), Some("t1"));
    assert_eq!(
        f.fake.row("a1").agent_meta.get("pr").map(String::as_str),
        Some(PR)
    );
}

/// The file provider is unchanged: done closes, and nothing about the PR is written to tasks.txt.
#[test]
fn the_file_provider_closes_on_done_and_stamps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = bn_core::test_support::config(dir.path());
    let mut task = bn_core::todo::Task::new_todo("t1", "Wire the gate", None, "", "cli", "2026-09-01");
    task.set_meta("repo", "bn");
    task.set_meta("branch", "feat/x");
    let mut file = bn_core::todo::TodoFile::default();
    file.push_task(task);
    bn_core::todo::save(&cfg.tasks_file(), &file).unwrap();

    let closed = run(&cfg, &ctx(), "done", &Some(PR.into()), false, false, true);
    assert_eq!(closed.task.as_deref(), Some("t1"));
    assert_eq!(closed.waiting_on_pr, None);
    let back = bn_core::todo::load(&cfg.tasks_file()).unwrap();
    let t1 = back.tasks().next().unwrap();
    assert!(t1.done);
    assert_eq!(t1.meta("pr"), None);
}
