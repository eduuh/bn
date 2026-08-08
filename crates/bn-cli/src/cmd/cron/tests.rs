use super::*;

fn run_events(job: &str) -> Vec<bn_core::events::Event> {
    bn_core::events::read_all()
        .into_iter()
        .filter(|event| {
            event.kind == "cron_run"
                && event.fields.get("job").and_then(serde_json::Value::as_str) == Some(job)
        })
        .collect()
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
    let agents = temp.path().join("agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("a-fail.md"),
        "---\nname: a-fail\nkind: command\non_calendar: minutely\nrun: \"exit 7\"\n---\n",
    )
    .expect("failing agent");
    std::fs::write(
        agents.join("z-good.md"),
        "---\nname: z-good\nkind: command\non_calendar: minutely\nrun: \"printf 'good\\\\n'\"\n---\n",
    )
    .expect("successful agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };

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
    let agents = temp.path().join("agents");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("queued-agent.md"),
        "---\nname: queued-agent\nkind: command\non_calendar: minutely\nrun: \"printf 'queued-agent-work\\\\n'\"\n---\n",
    )
    .expect("queued agent");

    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let cfg = Config {
        personal_root: temp.path().join("personal"),
        work_root: temp.path().join("work"),
        hostname: "test-host".to_string(),
        ..Default::default()
    };

    // A real dispatcher pass is what makes queue capture active for producers.
    assert_eq!(crate::cmd::queue_tick::job_queue_tick(&cfg, None), 0);

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
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
    let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
    let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
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

    assert_eq!(crate::cmd::attn_agent::job_cron_health(&cfg, None), 0);

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

    assert_eq!(crate::cmd::attn_agent::job_cron_health(&cfg, None), 0);
    let tasks_again = bn_core::todo::load(&cfg.tasks_file()).expect("tasks again");
    assert_eq!(
        tasks_again
            .tasks()
            .filter(|task| task.meta("key") == Some("cron-repair:broken-cron"))
            .count(),
        1
    );
}

#[test]
fn scheduler_catalog_failure_is_recorded_on_the_scheduler_row() {
    let _lock = bn_core::test_support::env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
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
