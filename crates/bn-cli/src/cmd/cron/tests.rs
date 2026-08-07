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
