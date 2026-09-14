use super::*;

#[test]
fn disabled_machine_has_no_desired_crons() {
    let cfg = Config {
        machine_profile: Some(bn_core::resolver::MachineProfile {
            cron: false,
            ..Default::default()
        }),
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
fn scheduler_runs_due_agents_inline() {
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
        agents.join("due-agent.md"),
        "---\nname: due-agent\nkind: command\nrequires_repo: test-repo\non_calendar: minutely\nrun: \"printf 'due-agent-work\\\\n'\"\n---\n",
    )
    .expect("due agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let mut cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };
    make_repo_available(&mut cfg, temp.path(), "test-repo");
    std::fs::create_dir_all(&state).expect("state dir");

    assert_eq!(run_job(&cfg, None, "agent-scheduler"), 0);
    assert_eq!(run_events("agent-scheduler").len(), 1);

    let runs = run_events("due-agent");
    assert_eq!(runs.len(), 1, "a due agent runs in the scheduler's own pass");
    assert_eq!(
        runs[0]
            .fields
            .get("output_tail")
            .and_then(serde_json::Value::as_str),
        Some("due-agent-work")
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
fn cron_health_creates_repair_task_and_starts_one_agent() {
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

    let launched = crate::cmd::dispatch::invocation::take_launched();
    assert_eq!(launched.len(), 1, "one repair agent starts for the new task");
    let args = &launched[0];
    assert_eq!(args[0], "dispatch");
    assert!(args[1].contains("Fix failing cron job 'broken-cron'"));
    assert!(args[1].contains("panic: missing config"));
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|at| args.get(at + 1))
            .map(String::as_str)
    };
    assert_eq!(flag("--repo"), Some("bn"));
    assert_eq!(flag("--task"), Some(task_id.as_str()));

    assert_eq!(
        crate::cmd::attention_workflow::job_cron_health(&cfg, None),
        0
    );
    assert!(
        crate::cmd::dispatch::invocation::take_launched().is_empty(),
        "an open repair task means the agent already started; the next pass starts no other"
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
/// work it minted. The attention row auto-acks via `replace=true`, but the repair task used to have
/// no such path — which is how `scout` ended up green while `Fix failing cron scout` stayed open.
#[test]
fn cron_health_closes_the_repair_task_once_the_cron_recovers() {
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
    assert_eq!(
        crate::cmd::dispatch::invocation::take_launched().len(),
        1,
        "a repair agent starts while the cron is failing"
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
        crate::cmd::dispatch::invocation::take_launched().is_empty(),
        "a green cron starts no repair agent"
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
    assert!(
        crate::cmd::dispatch::invocation::take_launched().is_empty(),
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

/// A typo or a renamed job must fail where a person can read it, not exit 0.
#[test]
fn running_an_unresolvable_job_fails_fast() {
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
    assert!(run_events("task-improve").is_empty());
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

