use super::*;

#[test]
fn disabled_machine_has_no_desired_crons() {
    let cfg = Config {
        machine_profile: None,
        ..Config::default()
    };

    assert!(desired_crons(&cfg, None).is_empty());
}

fn run_events(job: &str) -> Vec<bn_core::events::Event> {
    bn_core::events::read_all()
        .into_iter()
        .filter(|event| {
            event.kind == "cron_run"
                && event.fields.get("job").and_then(serde_json::Value::as_str) == Some(job)
        })
        .collect()
}

fn make_repo_available(cfg: &mut Config, root: &std::path::Path, repo: &str) {
    cfg.worktree_dir = root.join("worktree");
    std::fs::create_dir_all(root.join("bare").join(format!("{repo}.git")))
        .expect("create repo marker");
}

#[test]
fn named_agents_use_one_recorded_execution_boundary() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let agents = temp.path().join("agents");
    let jobs = temp.path().join("jobs");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::create_dir_all(&jobs).expect("jobs dir");
    std::fs::write(
        agents.join("custom-agent.md"),
        "---\nname: custom-agent\nkind: command\nrun: \"printf 'custom-tail\\\\n'\"\n---\n",
    )
    .expect("custom agent");
    std::fs::write(
        agents.join("default-agent.md"),
        "---\nname: default-agent\nkind: command\n---\n",
    )
    .expect("default agent");
    std::fs::write(jobs.join("default-agent.sh"), "printf 'script-tail\\n'\n")
        .expect("default script");
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\nkind: specialist\nrepo: demo\n---\nReview demo.\n",
    )
    .expect("specialist");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", &jobs);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };

    assert_eq!(run_job(&cfg, None, "custom-agent"), 0);
    assert_eq!(run_job(&cfg, None, "default-agent"), 0);
    assert_eq!(run_job(&cfg, None, "reviewer"), 2);

    let custom = run_events("custom-agent");
    assert_eq!(custom.len(), 1);
    assert_eq!(
        custom[0]
            .fields
            .get("output_tail")
            .and_then(serde_json::Value::as_str),
        Some("custom-tail")
    );
    let default = run_events("default-agent");
    assert_eq!(default.len(), 1);
    assert_eq!(
        default[0]
            .fields
            .get("output_tail")
            .and_then(serde_json::Value::as_str),
        Some("script-tail")
    );
    assert!(run_events("reviewer").is_empty());
}

#[test]
fn custom_agents_preserve_the_job_host_gate() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let agents = temp.path().join("agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("pinned.md"),
        "---\nname: pinned\nkind: command\nrun: \"exit 99\"\n---\n",
    )
    .expect("agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let mut cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    cfg.job_only_on
        .insert("pinned".to_string(), vec!["other-host".to_string()]);

    assert_eq!(run_job(&cfg, None, "pinned"), 0);
    assert!(run_events("pinned").is_empty());
}

#[test]
fn scheduler_isolates_sibling_failures_and_backs_off_retries() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    // Isolate the shipped cron manifest. `crons::discover` appends `config_dir()/crons.toml` as a
    // Global tier, and on a machine where bn is actually installed that resolves to the REAL
    // manifest under $HOME — whose `agent-scheduler` entry is bound to repo `bn`, which this
    // fixture has not registered. The job was then skipped and the assertions saw zero events.
    // It passed on CI (fresh HOME, no manifest) and failed for every developer with bn installed.
    let _cfg_dir = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", temp.path().join("cfg"));
    let agents = temp.path().join("agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("a-fail.md"),
        "---\nname: a-fail\nkind: command\nrequires_repo: test-repo\non_calendar: minutely\nrun: \"exit 7\"\n---\n",
    )
    .expect("failing agent");
    std::fs::write(
        agents.join("z-good.md"),
        "---\nname: z-good\nkind: command\nrequires_repo: test-repo\non_calendar: minutely\nrun: \"printf 'good\\\\n'\"\n---\n",
    )
    .expect("successful agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let mut cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    make_repo_available(&mut cfg, temp.path(), "test-repo");

    assert_eq!(job_agent_scheduler(&cfg, None), 0);
    assert_eq!(run_events("a-fail").len(), 1);
    assert_eq!(run_events("z-good").len(), 1);

    assert_eq!(job_agent_scheduler(&cfg, None), 0);
    assert_eq!(run_events("a-fail").len(), 1);
    assert_eq!(run_events("z-good").len(), 1);
}

#[test]
fn scheduler_enqueues_due_agents_and_queue_tick_runs_them() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    // Isolate the shipped cron manifest. `crons::discover` appends `config_dir()/crons.toml` as a
    // Global tier, and on a machine where bn is actually installed that resolves to the REAL
    // manifest under $HOME — whose `agent-scheduler` entry is bound to repo `bn`, which this
    // fixture has not registered. The job was then skipped and the assertions saw zero events.
    // It passed on CI (fresh HOME, no manifest) and failed for every developer with bn installed.
    let _cfg_dir = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", temp.path().join("cfg"));
    let agents = temp.path().join("agents");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("queued-agent.md"),
        "---\nname: queued-agent\nkind: command\nrequires_repo: test-repo\non_calendar: minutely\nrun: \"printf 'queued-agent-work\\\\n'\"\n---\n",
    )
    .expect("queued agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let pools = temp.path().join("pools");
    std::fs::write(&pools, "").expect("empty pools config");
    let _pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
    // This test asserts the agent is *queued* rather than run, which is exactly what an inherited
    // dispatch extent turns off — see `outside_a_dispatch` for why that leaks into agent sessions.
    let _outside = bn_core::test_support::outside_a_dispatch();
    let mut cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    make_repo_available(&mut cfg, temp.path(), "test-repo");

    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::write(
        state.join("queue-daemon.heartbeat"),
        bn_core::util::now_epoch_secs().to_string(),
    )
    .expect("queue heartbeat");

    assert_eq!(run_job(&cfg, None, "agent-scheduler"), 0);
    assert_eq!(run_events("agent-scheduler").len(), 1);
    assert!(run_events("queued-agent").is_empty());

    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let entries = backlog.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "run-queued-agent");
    assert_eq!(entries[0].kind, bn_core::backlog::BacklogKind::Cron);
    assert_eq!(
        entries[0].exec,
        bn_core::backlog::ExecSpec::Job {
            job: "queued-agent".to_string(),
            repo: None,
            branch: None,
        }
    );

    assert_eq!(crate::cmd::queue_tick::job_queue_tick(&cfg, None), 0);

    let runs = run_events("queued-agent");
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0]
            .fields
            .get("output_tail")
            .and_then(serde_json::Value::as_str),
        Some("queued-agent-work")
    );
    assert_eq!(
        bn_core::backlog::load(&bn_core::backlog::backlog_path())
            .entries()
            .iter()
            .find(|entry| entry.id == "run-queued-agent")
            .map(|entry| entry.status),
        Some(bn_core::backlog::BacklogStatus::Done)
    );
}

#[test]
fn cron_job_path_prefers_current_executable_directory() {
    let exe_dir = std::env::current_exe()
        .expect("current exe")
        .parent()
        .expect("exe dir")
        .to_string_lossy()
        .into_owned();
    let path = super::job_path();
    assert_eq!(path.split(':').next(), Some(exe_dir.as_str()));
}

#[test]
fn cron_health_creates_repair_task_and_queues_agent() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let state = temp.path().join("state");
    let personal = temp.path().join("personal");
    let jobs = temp.path().join("jobs");
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", &jobs);
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
    // The cron must actually resolve, or it reads as retired rather than broken and the watchdog
    // deliberately withholds the repair agent — see `cron_health_withholds_repair_agent_...`.
    std::fs::create_dir_all(&jobs).expect("jobs dir");
    std::fs::write(jobs.join("broken-cron.sh"), "#!/bin/sh\nexit 7\n").expect("job script");
    let cfg = Config {
        personal_root: personal,
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    let now = bn_core::util::now_epoch_secs();
    for i in 0..3 {
        bn_core::events::log_cron_run(
            "broken-cron",
            "global",
            None,
            None,
            now - (3 - i) * 60,
            10,
            7,
            "panic: missing config\nstack tail",
        )
        .expect("cron event");
    }

    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );

    let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("tasks");
    let repair = tasks
        .tasks()
        .find(|task| task.meta("key") == Some("cron-repair:broken-cron"))
        .expect("repair task");
    assert_eq!(repair.meta("repo"), Some("bn"));
    assert_eq!(repair.meta("src"), Some("cron-health"));
    assert_eq!(repair.meta("cron"), Some("broken-cron"));
    let task_id = repair.id().expect("task id").to_string();

    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let entry = backlog
        .entries()
        .iter()
        .find(|entry| entry.task == task_id)
        .expect("repair queue entry");
    assert_eq!(entry.repo, "bn");
    assert_eq!(entry.kind, bn_core::backlog::BacklogKind::Ship);
    assert_eq!(entry.priority, 4);
    assert!(entry.item.contains("Fix failing cron job 'broken-cron'"));
    assert!(entry.item.contains("panic: missing config"));

    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );
    let tasks_again = bn_core::todo::load(&cfg.tasks_file()).expect("tasks again");
    assert_eq!(
        tasks_again
            .tasks()
            .filter(|task| task.meta("key") == Some("cron-repair:broken-cron"))
            .count(),
        1
    );
}

/// The recovery half of the repair loop: once the cron goes green, the watchdog must fold out the
/// work it minted. The attention row auto-acks via `replace=true`, but the repair task and the
/// queued repair agent used to have no such path — which is how `scout` ended up green while
/// `Fix failing cron scout` stayed open and a repair agent was still queued to reproduce it.
#[test]
fn cron_health_closes_the_repair_task_and_withdraws_the_agent_once_the_cron_recovers() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let jobs = temp.path().join("jobs");
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", &jobs);
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
    std::fs::create_dir_all(&jobs).expect("jobs dir");
    std::fs::write(jobs.join("flaky-cron.sh"), "#!/bin/sh\nexit 7\n").expect("job script");
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    let now = bn_core::util::now_epoch_secs();
    for i in 0..3 {
        bn_core::events::log_cron_run(
            "flaky-cron",
            "global",
            None,
            None,
            now - (4 - i) * 60,
            10,
            7,
            "/bin/sh: bn: command not found",
        )
        .expect("cron event");
    }
    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );

    let today = bn_core::clock::Clock::today(&bn_core::clock::RealClock);
    let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("tasks");
    let repair = tasks
        .tasks()
        .find(|task| task.meta("key") == Some("cron-repair:flaky-cron"))
        .expect("repair task");
    assert!(repair.is_open(&today), "the repair task starts open");
    let task_id = repair.id().expect("task id").to_string();
    assert!(
        bn_core::backlog::load(&bn_core::backlog::backlog_path())
            .entries()
            .iter()
            .any(|entry| entry.task == task_id
                && entry.status == bn_core::backlog::BacklogStatus::Pending),
        "a repair agent is queued while the cron is failing"
    );

    // The cron recovers — exactly what the PATH-prelude fix did to `scout`.
    bn_core::events::log_cron_run("flaky-cron", "global", None, None, now, 10, 0, "")
        .expect("green run");
    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );

    let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("tasks after recovery");
    let repair = tasks
        .tasks()
        .find(|task| task.meta("key") == Some("cron-repair:flaky-cron"))
        .expect("repair task still recorded");
    assert!(
        !repair.is_open(&today),
        "a recovered cron closes its repair task"
    );
    assert_eq!(
        repair.meta("reason"),
        Some("cron-recovered"),
        "closed as cancelled-because-recovered, not as work someone completed"
    );
    assert!(
        !bn_core::backlog::load(&bn_core::backlog::backlog_path())
            .entries()
            .iter()
            .any(|entry| entry.task == task_id
                && entry.status == bn_core::backlog::BacklogStatus::Pending),
        "and withdraws the queued agent instead of letting it burn a worktree on a green job"
    );
}

/// A cron that no longer resolves still earns its attention row, but must not keep dispatching
/// autonomous repair agents: the repair prompt tells them to reproduce with `bn run <job>`, which
/// cannot resolve, so each one can only burn a worktree to report the same nothing. This is the
/// `task-improve` loop that ran every 15 minutes after the job was renamed to `task-enhance`.
#[test]
fn cron_health_withholds_repair_agent_for_a_cron_that_no_longer_resolves() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    let now = bn_core::util::now_epoch_secs();
    for i in 0..3 {
        bn_core::events::log_cron_run(
            "renamed-away",
            "global",
            None,
            None,
            now - (3 - i) * 60,
            3,
            127,
            "/bin/sh: bn: command not found",
        )
        .expect("cron event");
    }

    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );

    let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("tasks");
    assert!(
        tasks
            .tasks()
            .any(|task| task.meta("key") == Some("cron-repair:renamed-away")),
        "an unresolvable cron is still worth a person's attention"
    );
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    assert!(
        !backlog
            .entries()
            .iter()
            .any(|entry| entry.item.contains("renamed-away")),
        "but no autonomous agent should be dispatched at a job that cannot be run"
    );
}

/// End-to-end for `bn cron retire`: the tombstone reaches the watchdog through the real event log,
/// so a retired name stops being raised on the very next run rather than in a fortnight.
#[test]
fn retiring_a_cron_silences_the_watchdog_immediately() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    let now = bn_core::util::now_epoch_secs();
    for i in 0..3 {
        bn_core::events::log_cron_run(
            "gone-for-good",
            "global",
            None,
            None,
            now - (3 - i) * 60,
            3,
            127,
            "/bin/sh: bn: command not found",
        )
        .expect("cron event");
    }

    // Failing minutes ago, so only the tombstone can retire it this soon.
    let runs = bn_core::events::cron_status();
    let before = bn_core::crons::LiveJobs::discover(&cfg, None);
    assert_eq!(
        bn_core::crons::alerting_crons(&runs, bn_core::crons::ALERT_MIN_FAILURES, &before, now)
            .len(),
        1
    );

    bn_core::events::retire_cron("gone-for-good", Some("renamed")).expect("tombstone");

    let after = bn_core::crons::LiveJobs::discover(&cfg, None);
    assert!(
        bn_core::crons::alerting_crons(&runs, bn_core::crons::ALERT_MIN_FAILURES, &after, now)
            .is_empty(),
        "a retired cron must stop being raised at once"
    );
}

/// `bn run <name>` used to enqueue any name at all, so a typo or a renamed job printed
/// "queued …" and exited 0, then died exit 2 inside the queue executor where nobody saw it.
#[test]
fn running_an_unresolvable_job_fails_fast_instead_of_queueing_it() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };

    assert_eq!(super::run_job(&cfg, None, "task-improve"), 2);

    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    assert!(
        !backlog
            .entries()
            .iter()
            .any(|entry| entry.item.contains("task-improve")),
        "an unresolvable job must not reach the queue at all"
    );
}

#[test]
fn scheduler_catalog_failure_is_recorded_on_the_scheduler_row() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    // Isolate the shipped cron manifest. `crons::discover` appends `config_dir()/crons.toml` as a
    // Global tier, and on a machine where bn is actually installed that resolves to the REAL
    // manifest under $HOME — whose `agent-scheduler` entry is bound to repo `bn`, which this
    // fixture has not registered. The job was then skipped and the assertions saw zero events.
    // It passed on CI (fresh HOME, no manifest) and failed for every developer with bn installed.
    let _cfg_dir = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", temp.path().join("cfg"));
    let agents = temp.path().join("agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("broken.md"),
        "---\nname: broken\n---\nmissing kind\n",
    )
    .expect("broken agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };

    assert_eq!(run_job(&cfg, None, "agent-scheduler"), 2);
    let runs = run_events("agent-scheduler");
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0]
            .fields
            .get("exit_code")
            .and_then(serde_json::Value::as_i64),
        Some(2)
    );
    assert!(runs[0]
        .fields
        .get("output_tail")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|tail| tail.contains("missing required agent kind")));
}

/// The guard has to actually neutralize an inherited extent, not merely be present.
///
/// This is the property that matters for autonomous agents: they run with `BN_IN_QUEUE_TICK` set
/// for their whole session, so a suite that only passes in a clean shell is a suite that always
/// fails for them. Setting the variable here reproduces the agent's environment exactly.
#[test]
fn capture_tests_are_hermetic_against_an_inherited_dispatch_extent() {
    let _lock = bn_core::test_support::env_lock();
    let _inherited = bn_core::test_support::EnvGuard::set("BN_IN_QUEUE_TICK", "1");
    assert!(std::env::var_os("BN_IN_QUEUE_TICK").is_some());

    let outside = bn_core::test_support::outside_a_dispatch();
    assert!(
        std::env::var_os("BN_IN_QUEUE_TICK").is_none(),
        "the guard must clear the ambient extent, or capture assertions test the shell"
    );

    drop(outside);
    assert!(
        std::env::var_os("BN_IN_QUEUE_TICK").is_some(),
        "and must restore it, so one test cannot disarm the next"
    );
}

/// `--now` is the escape hatch `bn run` never had.
///
/// Capture is on and the dispatcher is alive, so this job would normally be queued — the same
/// state in which `bn run self-update` prints "queued …" and exits 0 while you wait for a result
/// that is not coming. `--now` must run it here and leave nothing in the queue for a dispatcher to
/// reconcile against a job that already ran.
#[test]
fn run_now_executes_in_place_instead_of_queueing() {
    let _lock = bn_core::test_support::env_lock();
    let _outside = bn_core::test_support::outside_a_dispatch();
    let temp = tempfile::tempdir().expect("tempdir");
    let agents = temp.path().join("agents");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::write(
        agents.join("now-agent.md"),
        "---\nname: now-agent\nkind: command\nrequires_repo: test-repo\non_calendar: minutely\nrun: \"printf 'ran-inline\\\\n'\"\n---\n",
    )
    .expect("agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    // A live dispatcher is what makes capture active; without it the producer declines anyway and
    // the test would pass for the wrong reason.
    std::fs::write(
        state.join("queue-daemon.heartbeat"),
        bn_core::util::now_epoch_secs().to_string(),
    )
    .expect("heartbeat");
    let mut cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    make_repo_available(&mut cfg, temp.path(), "test-repo");

    // Baseline: without --now this exact job is captured, not run.
    assert_eq!(super::run_job(&cfg, None, "now-agent"), 0);
    assert!(
        run_events("now-agent").is_empty(),
        "precondition: capture is active here, so the plain path must queue"
    );

    assert_eq!(super::run_job_now(&cfg, None, "now-agent", true), 0);
    let runs = run_events("now-agent");
    assert_eq!(runs.len(), 1, "--now must actually run the job");
    assert_eq!(
        runs[0]
            .fields
            .get("output_tail")
            .and_then(serde_json::Value::as_str),
        Some("ran-inline")
    );
}
