use super::*;

fn parse(text: &str) -> Result<AgentDef, AgentCatalogError> {
    parse_agent(
        text,
        "demo",
        JobScope::Global,
        PathBuf::from("config/agents/demo.md"),
    )
}

#[test]
fn command_defaults_run_and_retry() {
    let agent =
        parse("---\nname: demo\nkind: command\nrequires_repo: bn\non_calendar: \"*:0/15\"\n---\n")
            .expect("agent parses");

    assert_eq!(agent.kind, AgentKind::Command);
    assert_eq!(agent.run, None);
    assert_eq!(agent.retry_after_secs, Some(DEFAULT_RETRY_AFTER_SECS));
    assert!(!agent.catch_up);
}

#[test]
fn catch_up_frontmatter_parses() {
    let agent = parse(
        "---\nname: demo\nkind: command\nrequires_repo: bn\non_calendar: \"*:0/15\"\ncatch_up: true\n---\n",
    )
    .expect("agent parses");
    assert!(agent.catch_up);
}

#[test]
fn a_retired_frontmatter_key_still_parses() {
    // `queue:` was removed with the machine queue; manifests that still carry it must load.
    let agent = parse(
        "---\nname: demo\nkind: command\nrequires_repo: bn\non_calendar: \"*:0/15\"\nqueue: false\n---\n",
    )
    .expect("agent parses");
    assert_eq!(agent.kind, AgentKind::Command);
}

#[test]
fn a_retired_startup_trigger_still_parses() {
    // `trigger: startup` was removed with `bn startup`; manifests that still carry it must load
    // as plain on-demand agents.
    for trigger in ["startup", "[cron, startup]"] {
        let agent = parse(&format!(
            "---\nname: demo\nkind: command\nrequires_repo: bn\ntrigger: {trigger}\n---\n"
        ))
        .expect("agent parses");
        assert_eq!(agent.on_calendar, None);
    }
}

fn run_detail(ts: i64, exit_code: i32) -> CronRunDetail {
    CronRunDetail {
        job: "demo".to_string(),
        repo: None,
        branch: None,
        ts,
        exit_code,
        duration_ms: 0,
        output_tail: String::new(),
    }
}

#[test]
fn due_catches_up_and_waits_for_the_next_successful_slot() {
    let schedule = "*:0/15";
    let now = 1_800_000_000;
    let slot = crate::schedule::previous_occurrence(schedule, now).expect("previous slot");
    let next = crate::schedule::next_occurrence(schedule, slot).expect("next slot");

    assert!(due(schedule, None, now, DEFAULT_RETRY_AFTER_SECS));
    assert!(!due(
        schedule,
        Some(&run_detail(slot, 0)),
        now,
        DEFAULT_RETRY_AFTER_SECS
    ));
    assert!(due(
        schedule,
        Some(&run_detail(slot, 0)),
        next,
        DEFAULT_RETRY_AFTER_SECS
    ));
}

#[test]
fn failed_runs_respect_retry_backoff() {
    let schedule = "*:0/15";
    let now = 1_800_000_000;

    assert!(!due(schedule, Some(&run_detail(now - 899, 1)), now, 900));
    assert!(due(schedule, Some(&run_detail(now - 900, 1)), now, 900));
}

#[test]
fn kind_schedule_and_recursion_errors_are_typed_and_path_qualified() {
    let missing_kind = parse("---\nname: demo\n---\n").expect_err("kind is required");
    assert!(matches!(
        missing_kind,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::MissingKind,
            ..
        }
    ));
    assert!(missing_kind.to_string().contains("config/agents/demo.md"));

    let bad_schedule = parse(
        "---\nname: demo\nkind: command\nrequires_repo: bn\non_calendar: \"sometimes\"\n---\n",
    )
    .expect_err("schedule is invalid");
    assert!(matches!(
        bad_schedule,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::InvalidSchedule { .. },
            ..
        }
    ));

    let recursive = parse("---\nname: demo\nkind: command\nrun: \"bn run demo\"\n---\n")
        .expect_err("explicit self-run recurses");
    assert!(matches!(
        recursive,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::RecursiveRun { .. },
            ..
        }
    ));

    let zero_retry = parse(
        "---\nname: demo\nkind: command\nrequires_repo: bn\non_calendar: daily\nretry_after: 0m\n---\n",
    )
    .expect_err("retry must be positive");
    assert!(matches!(
        zero_retry,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::InvalidRetryAfter { .. },
            ..
        }
    ));

    let missing_repo = parse("---\nname: demo\nkind: command\non_calendar: daily\n---\n")
        .expect_err("scheduled agents require a repo");
    assert!(matches!(
        missing_repo,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::RequiredField {
                field: "requires_repo",
                ..
            },
            ..
        }
    ));
}

#[test]
fn staff_kind_requires_prompt_gather_and_sink() {
    let error = parse("---\nname: demo\nkind: staff\ngather: \"bn gather\"\n---\n")
        .expect_err("empty body is invalid");
    assert!(matches!(
        error,
        AgentCatalogError::InvalidAgent {
            source: AgentParseError::RequiredField {
                kind: AgentKind::Staff,
                field: "body"
            },
            ..
        }
    ));
}

#[test]
fn specialist_is_not_runnable() {
    let agent = parse("---\nname: demo\nkind: specialist\nrepo: demo\n---\nReview demo.\n")
        .expect("specialist parses");

    assert_eq!(
        agent.ensure_runnable(),
        Err(AgentRunError::Specialist {
            name: "demo".to_string()
        })
    );
}

#[test]
fn precedence_is_first_tier_wins() {
    let temp = tempfile::tempdir().expect("tempdir");
    let branch = temp.path().join("branch");
    let global = temp.path().join("global");
    std::fs::create_dir_all(&branch).expect("branch dir");
    std::fs::create_dir_all(&global).expect("global dir");
    std::fs::write(
        branch.join("demo.md"),
        "---\nname: demo\nkind: hosted\n---\nbranch\n",
    )
    .expect("branch agent");
    std::fs::write(
        global.join("demo.md"),
        "---\nname: demo\nkind: hosted\n---\nglobal\n",
    )
    .expect("global agent");

    let catalog =
        discover_in(&[(JobScope::Branch, branch), (JobScope::Global, global)]).expect("catalog");

    assert_eq!(
        catalog.find("demo").map(|agent| agent.body.trim()),
        Some("branch")
    );
}

#[test]
fn agent_directories_reject_non_markdown_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("demo.toml"),
        "name = \"demo\"\nkind = \"command\"\n",
    )
    .expect("legacy agent fixture");

    let error =
        discover_in(&[(JobScope::Global, temp.path().to_path_buf())]).expect_err("not markdown");

    assert!(matches!(error, AgentCatalogError::NonMarkdownAgent { .. }));
    assert!(error.to_string().contains("not a Markdown .md manifest"));
}

#[test]
fn shipped_agent_catalog_is_valid() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config")
        .join("agents");
    for entry in std::fs::read_dir(&dir).expect("shipped agents dir") {
        let entry = entry.expect("shipped agent entry");
        if entry.file_type().expect("shipped agent type").is_file() {
            assert_eq!(
                entry.path().extension().and_then(|ext| ext.to_str()),
                Some("md"),
                "{} is not a Markdown manifest",
                entry.path().display()
            );
        }
    }
    let catalog = discover_in(&[(JobScope::Global, dir)]).expect("shipped catalog");

    assert!(!catalog.is_empty());
    assert!(catalog.find("controller").is_some());
    assert!(catalog.find("task-triage").is_some());

    for agent in catalog.iter() {
        let args: Map<String, Value> = agent
            .arguments
            .iter()
            .map(|argument| {
                (
                    argument.name.clone(),
                    Value::String(format!("test-{}", argument.name)),
                )
            })
            .collect();
        let rendered = agent
            .render(Some(&args))
            .unwrap_or_else(|error| panic!("render {}: {error}", agent.name));
        assert!(
            !rendered.contains("{{"),
            "{} left an unresolved placeholder",
            agent.name
        );
    }

    // Contract markers for the agents the catalog still ships. The staff roster's entries
    // (quarter-review, today-agent, staff-mentor, sentinel, staff-archivist) went with that roster.
    // If the Mentor is ever brought back, restore its "Never post to the pull request" pin too: it
    // is the only agent whose subject is a PR thread other people read, it runs unattended on a box
    // logged in to GitHub, and that rule was already deleted once by an unrelated refactor.
    for (name, contract) in [
        ("attention-agent", "the single ranked list"),
        ("task-enhance", "Do not implement"),
        ("task-triage", "Do not implement"),
        ("controller", "Keep the user in the loop"),
    ] {
        assert!(
            catalog
                .find(name)
                .is_some_and(|agent| agent.body.contains(contract)),
            "{name} lost contract marker {contract:?}"
        );
    }

    let task_enhance = catalog.find("task-enhance").expect("task-enhance agent");
    assert_eq!(task_enhance.harness.as_deref(), Some("copilot"));
    assert!(
        task_enhance.model.is_none(),
        "task-enhance should not pin a model after the stalled-cron recovery"
    );
}

#[test]
fn the_scheduler_alone_fires_manifest_schedules() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = discover_in(&[(JobScope::Global, root.join("config/agents"))])
        .expect("shipped agent catalog");
    let cron_text =
        std::fs::read_to_string(root.join("config/crons.toml")).expect("shipped cron manifest");
    let cron_jobs: Vec<&str> = cron_text
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("job = \"")
                .and_then(|job| job.strip_suffix('"'))
        })
        .collect();

    // One timer fires every manifest schedule; no agent also has a timer of its own.
    assert!(cron_jobs.contains(&"agent-scheduler"));
    for agent in catalog.iter().filter(|agent| agent.is_cron()) {
        assert!(
            !cron_jobs.iter().any(|job| *job == agent.name),
            "{} has both a manifest schedule and a crons.toml entry",
            agent.name
        );
    }
}

#[test]
fn a_specialist_always_publishes_natively() {
    let agent =
        parse("---\nname: demo\nkind: specialist\nrepo: bn\n---\nBody.\n").expect("agent parses");
    assert!(agent.publishes_natively());
}

#[test]
fn a_plain_agent_stays_out_of_the_harness_agent_picker() {
    // The default has to be "not published": most of the catalog is packet-fed, and a staff agent
    // offered in a harness's own `/agent` list with no `gather:` behind it reads as broken.
    let agent = parse("---\nname: demo\nkind: command\nrequires_repo: bn\n---\nBody.\n")
        .expect("agent parses");
    assert!(!agent.publishes_natively());
}

#[test]
fn declaring_a_world_is_how_a_non_specialist_opts_in() {
    let agent =
        parse("---\nname: demo\nkind: command\nrequires_repo: bn\nworlds: [claude]\n---\nBody.\n")
            .expect("agent parses");
    assert!(agent.publishes_natively());
}
