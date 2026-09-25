use super::*;
use bn_core::forge_app::client::Assignee;
use bn_core::runner::CommandOutput;
use bn_core::test_support::forge::{agent_row, row, FakeForge, ForgeEnv};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const ME: &str = "bn@m1";

/// Local clones this machine has: `eduuh/bn` is cloned as `bn`, `eduuh/wira360` as `tracker`.
fn resolve(slug: &str) -> Option<String> {
    match slug {
        "eduuh/bn" | "bn" => Some("bn".into()),
        "eduuh/wira360" => Some("tracker".into()),
        _ => None,
    }
}

fn claims(fake: &FakeForge) -> Vec<String> {
    fake.take_calls()
        .into_iter()
        .filter(|c| !c.starts_with("list_bn_tasks"))
        .collect()
}

#[test]
fn only_open_unstarted_agent_tasks_in_an_allowed_clone_are_candidates() {
    let mut started = agent_row("started", "x", "2026-09-01", &[]);
    started.started_at = Some("2026-09-02T00:00:00Z".into());
    let mut done = agent_row("done", "x", "2026-09-01", &[]);
    done.is_completed = true;
    let mut blocked = agent_row("blocked", "x", "2026-09-01", &[]);
    blocked.status = "Blocked".into();
    let mut elsewhere = agent_row("elsewhere", "x", "2026-09-01", &[]);
    elsewhere.goal_repo_url = Some("https://github.com/someone/unknown".into());
    let mut urgent = agent_row("urgent", "x", "2026-09-05", &[]);
    urgent.priority = "Urgent".into();
    let mut tracker = agent_row("tracker", "x", "2026-09-03", &[]);
    tracker.goal_repo_url = Some("https://github.com/eduuh/wira360".into());
    let mut human = row("human", "x", "2026-09-01", &[]);
    human.assignees = vec![Assignee {
        user_id: "u-kid".into(),
        display_name: Some("Kid".into()),
        is_agent: false,
    }];
    let rows = vec![
        agent_row("plain", "x", "2026-09-01", &[]),
        started,
        done,
        blocked,
        elsewhere,
        human,
        tracker,
        urgent,
    ];

    let all = candidates(&rows, resolve, |_| true);
    let ids: Vec<(&str, &str)> = all
        .iter()
        .map(|c| (c.activity.as_str(), c.repo.as_str()))
        .collect();
    assert_eq!(
        ids,
        [("urgent", "bn"), ("plain", "bn"), ("tracker", "tracker")]
    );

    // A machine allowed only `bn` never sees the tracker task.
    let only_bn = candidates(&rows, resolve, |repo| repo.ends_with("bn"));
    assert!(only_bn.iter().all(|c| c.repo == "bn"), "{only_bn:?}");
    assert_eq!(only_bn.len(), 2);
}

#[test]
fn free_lanes_count_only_this_machines_unfinished_claims() {
    let rows = vec![
        agent_row("a", "x", "2026-09-01", &[("claimer", ME)]),
        agent_row("b", "x", "2026-09-01", &[("claimer", ME), ("pr", "https://x/1")]),
        agent_row("c", "x", "2026-09-01", &[("claimer", "bn@other")]),
    ];
    assert_eq!(free_lanes(&rows, ME, 3), 2);
    assert_eq!(free_lanes(&rows, ME, 1), 0);
}

#[test]
fn a_claim_stamps_the_local_repo_and_says_who_claimed_it() {
    let fake = FakeForge::with(vec![agent_row("a1", "Do it", "2026-09-01", &[])]);
    let rows = fake.list_bn_tasks(Some(0)).unwrap();
    let picked = candidates(&rows, resolve, |_| true);
    let attempts = claim(fake.as_ref(), &rows, picked, ME, 1);

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].outcome, "claimed");
    assert_eq!(
        claims(&fake),
        [
            "claim_activity a1 bn@m1".to_string(),
            r#"set_activity_meta a1 set={"repo": "bn"} remove=[] expected=None"#.to_string(),
            "create_comment a1 claimed by bn@m1".to_string(),
        ]
    );
    let a1 = fake.row("a1");
    assert!(a1.started_at.is_some());
    assert_eq!(a1.agent_meta.get("claimer").map(String::as_str), Some(ME));

    // The claimed task loads as an ordinary wip task in the bn repo.
    let task = bn_core::forge_app::store::ForgeTaskStore::new(fake);
    let file = bn_core::task_store::TaskStore::load(&task).unwrap();
    let t = file.tasks().next().unwrap();
    assert_eq!(t.meta("repo"), Some("bn"));
    assert!(t.contexts().contains(&"wip"));
}

#[test]
fn a_lost_race_is_skipped_silently_and_the_next_candidate_takes_the_lane() {
    let fake = FakeForge::with(vec![
        agent_row("taken", "x", "2026-09-01", &[]),
        agent_row("next", "x", "2026-09-02", &[]),
        agent_row("later", "x", "2026-09-03", &[]),
    ]);
    // Listed as unclaimed, but another machine claims it before this one asks.
    let rows = fake.list_bn_tasks(Some(0)).unwrap();
    fake.edit("taken", |r| {
        r.started_at = Some("2026-09-14T00:00:00Z".into());
    })
    .unwrap();
    let picked = candidates(&rows, resolve, |_| true);
    let attempts = claim(fake.as_ref(), &rows, picked, ME, 1);

    let outcomes: Vec<(&str, &str)> = attempts
        .iter()
        .map(|a| (a.activity.as_str(), a.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [("taken", "already-claimed"), ("next", "claimed")],
        "capacity 1: the lane goes to the next task, and `later` is not asked"
    );
    let comments = fake.comments();
    assert_eq!(
        comments,
        [("next".to_string(), "claimed by bn@m1".to_string())],
        "no comment on the task another machine won"
    );
}

/// A claim that landed starts through `bn dispatch --task <id>`, in the local clone, with the bn id
/// the task list gave the activity. Races lost and refusals start nothing.
#[test]
fn a_landed_claim_starts_through_dispatch_with_its_bn_id() {
    let f = ForgeEnv::new(
        "claim-start",
        vec![
            agent_row("a1", "Wire the gate", "2026-09-01", &[("id", "t7")]),
            agent_row("a2", "Taken elsewhere", "2026-09-02", &[("id", "t8")]),
        ],
    )
    .unwrap();
    let _ = crate::cmd::dispatch::invocation::take_launched();
    let rows = f.fake.list_bn_tasks(Some(0)).unwrap();
    f.fake
        .edit("a2", |r| r.started_at = Some("2026-09-14T00:00:00Z".into()))
        .unwrap();
    let picked = candidates(&rows, resolve, |_| true);
    let mut attempts = claim(f.fake.as_ref(), &rows, picked, ME, 2);
    start_claimed(&f.cfg, &mut attempts);

    let started: Vec<(&str, Option<&str>)> = attempts
        .iter()
        .map(|a| (a.activity.as_str(), a.started.as_deref()))
        .collect();
    assert_eq!(started, [("a1", Some("t7")), ("a2", None)]);
    let launched = crate::cmd::dispatch::invocation::take_launched();
    assert_eq!(launched.len(), 1, "{launched:?}");
    let args = &launched[0];
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    assert!(args[1].starts_with("Wire the gate"), "{args:?}");
    assert_eq!(flag("--repo"), Some("bn"));
    assert_eq!(flag("--task"), Some("t7"));
}

#[test]
fn the_file_provider_runs_neither_forge_step() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = bn_core::test_support::config(dir.path());
    assert!(sweep_forge_claims(&cfg, 1).is_empty());
    let runner = GhRunner::new("MERGED");
    assert!(poll_forge_merges(&cfg, &runner, 1_000).is_empty());
    assert_eq!(runner.calls(), 0);
}

/// A `gh` that answers one PR state and counts how often it was asked.
struct GhRunner {
    state: Mutex<String>,
    calls: AtomicUsize,
}

impl GhRunner {
    fn new(state: &str) -> GhRunner {
        GhRunner {
            state: Mutex::new(state.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn set(&self, state: &str) {
        *self.state.lock().unwrap() = state.into();
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl CommandRunner for GhRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandOutput {
        assert_eq!(program, "gh");
        assert_eq!(&args[..2], ["pr", "view"]);
        assert_eq!(&args[3..], ["--json", "state"]);
        self.calls.fetch_add(1, Ordering::Relaxed);
        CommandOutput {
            success: true,
            stdout: format!("{{\"state\":\"{}\"}}", self.state.lock().unwrap()),
            stderr: String::new(),
        }
    }
}

const PR: &str = "https://github.com/eduuh/bn/pull/42";

fn waiting_on_pr(name: &str, claimer: &str) -> ForgeEnv {
    let mut r = agent_row(
        "a1",
        "Wire the gate",
        "2026-09-01",
        &[
            ("id", "t1"),
            ("repo", "bn"),
            ("branch", "feat/x"),
            ("claimer", claimer),
            ("pr", PR),
        ],
    );
    r.started_at = Some("2026-09-02T08:00:00Z".into());
    ForgeEnv::new(name, vec![r]).unwrap()
}

#[test]
fn a_merged_pr_completes_the_forge_task() {
    let f = waiting_on_pr("merge-poll-merged", ME);
    let gh = GhRunner::new("MERGED");
    let polls = poll_merges(&f.cfg, &gh, ME, 10_000);

    assert_eq!(polls.len(), 1);
    assert_eq!(polls[0].outcome, "merged");
    assert!(f.fake.row("a1").is_completed, "completed in Forge");
    assert_eq!(
        f.fake.comments(),
        [("a1".to_string(), "PR merged, completing".to_string())]
    );
    assert!(
        poll_merges(&f.cfg, &gh, ME, 20_000).is_empty(),
        "a done task is not polled again"
    );
    assert_eq!(gh.calls(), 1);
}

#[test]
fn a_closed_pr_is_reported_once_and_the_task_left_open() {
    let f = waiting_on_pr("merge-poll-closed", ME);
    let gh = GhRunner::new("CLOSED");
    assert_eq!(poll_merges(&f.cfg, &gh, ME, 10_000)[0].outcome, "closed");
    assert_eq!(
        poll_merges(&f.cfg, &gh, ME, 10_000 + PR_POLL_SECS)[0].outcome,
        "closed"
    );
    assert_eq!(gh.calls(), 2);
    assert!(!f.fake.row("a1").is_completed);
    assert_eq!(
        f.fake.comments(),
        [(
            "a1".to_string(),
            format!("PR closed without merge; task left open ({PR})")
        )],
        "said once, not on every poll"
    );
}

#[test]
fn each_pr_is_asked_about_at_most_once_per_interval() {
    let f = waiting_on_pr("merge-poll-throttle", ME);
    let gh = GhRunner::new("OPEN");
    assert_eq!(poll_merges(&f.cfg, &gh, ME, 10_000)[0].outcome, "open");
    assert_eq!(
        poll_merges(&f.cfg, &gh, ME, 10_000 + PR_POLL_SECS - 1)[0].outcome,
        "waiting"
    );
    assert_eq!(gh.calls(), 1, "no gh call inside the interval");

    gh.set("MERGED");
    assert_eq!(
        poll_merges(&f.cfg, &gh, ME, 10_000 + PR_POLL_SECS)[0].outcome,
        "merged"
    );
    assert_eq!(gh.calls(), 2);

    // The run-level throttle: `bn forge sweep` does nothing within a minute of the last poll.
    drop(f);
    let again = waiting_on_pr("merge-poll-pass", ME);
    let gh = GhRunner::new("OPEN");
    poll_merges(&again.cfg, &gh, ME, 50_000);
    assert!(poll_forge_merges(&again.cfg, &gh, 50_000 + SWEEP_EVERY_SECS - 1).is_empty());
    assert_eq!(gh.calls(), 1);
}

#[test]
fn another_machines_task_is_left_to_that_machine() {
    let f = waiting_on_pr("merge-poll-other", "bn@other");
    let gh = GhRunner::new("MERGED");
    assert!(poll_merges(&f.cfg, &gh, ME, 10_000).is_empty());
    assert_eq!(gh.calls(), 0);
    assert!(!f.fake.row("a1").is_completed);
}
