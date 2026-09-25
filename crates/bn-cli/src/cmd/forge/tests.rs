use super::mcp_client::{read_mcp_message, tool_result_json};
use super::*;
use bn_core::test_support::{env_lock, EnvGuard};
use std::fs;
use std::io::BufReader;

struct FakeClient {
    list_calls: usize,
    claim_calls: Vec<(String, String, u64)>,
    release_calls: Vec<String>,
    heartbeat_calls: Vec<String>,
    tasks: Vec<ForgeBnTask>,
    all_tasks: Vec<ForgeBnTask>,
    claim: ForgeClaimActivityResponse,
}

fn fake_task(id: &str) -> ForgeBnTask {
    ForgeBnTask {
        id: id.into(),
        goal_id: None,
        goal_name: Some("Goal".into()),
        goal_repo_url: None,
        title: "Fix thing".into(),
        description: None,
        priority: Some("High".into()),
        status: Some("Active".into()),
        version: Some(1),
        is_completed: false,
        agent_meta: bn_core::forge_local::ForgeAgentMeta::default(),
        assignees: Vec::new(),
    }
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

impl FakeClient {
    fn new() -> Self {
        Self {
            list_calls: 0,
            claim_calls: Vec::new(),
            release_calls: Vec::new(),
            heartbeat_calls: Vec::new(),
            tasks: vec![fake_task("task-1")],
            all_tasks: Vec::new(),
            claim: ForgeClaimActivityResponse {
                claimed: true,
                activity: Some(ForgeBnTask {
                    version: Some(2),
                    ..fake_task("task-1")
                }),
                lease_expires_at: Some("2026-09-17T16:00:00Z".into()),
                reason: None,
            },
        }
    }
}

impl ForgeMcpClient for FakeClient {
    fn list_claimable_tasks(
        &mut self,
        _profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError> {
        self.list_calls += 1;
        Ok(self.tasks.iter().take(limit).cloned().collect())
    }

    fn claim_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        lease_seconds: u64,
    ) -> Result<ForgeClaimActivityResponse, ForgeMcpError> {
        self.claim_calls
            .push((profile.claimer.id.clone(), task_id.to_string(), lease_seconds));
        Ok(self.claim.clone())
    }

    fn list_all_tasks(
        &mut self,
        _profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError> {
        Ok(self.all_tasks.iter().take(limit).cloned().collect())
    }

    fn release_activity(
        &mut self,
        _profile: &ForgeAgentProfile,
        task_id: &str,
        _reason: &str,
    ) -> Result<(), ForgeMcpError> {
        self.release_calls.push(task_id.to_string());
        Ok(())
    }

    fn heartbeat_activity(
        &mut self,
        _profile: &ForgeAgentProfile,
        task_id: &str,
        _lease_seconds: u64,
    ) -> Result<(), ForgeMcpError> {
        self.heartbeat_calls.push(task_id.to_string());
        Ok(())
    }
}

fn write_profile(home: &std::path::Path) {
    let dir = home.join(".config/wira360/agents");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("mac-agent.json"),
        r#"{
            "claimer": { "id": "bn@mac", "displayName": "Mac Agent" },
            "forgeMcp": { "server": "forge", "oauthProfile": "default" }
        }"#,
    )
    .unwrap();
}

fn tick_req(profile: ForgeAgentProfile, max_active: usize, dry_run: bool) -> TickRequest {
    TickRequest {
        profile,
        repo: Some("bn".into()),
        agent_harness: "copilot".into(),
        base: None,
        skill: "forge-task".into(),
        max_active,
        dry_run,
        lease_seconds: 900,
        json_output: false,
    }
}

fn held_task(id: &str, claimer: &str) -> ForgeBnTask {
    let mut task = fake_task(id);
    task.agent_meta.claimer = Some(claimer.into());
    task
}

#[test]
fn tick_releases_claims_with_no_live_agent() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    client.all_tasks = vec![held_task("abcdef12-0000-0000-0000-000000000000", "bn@mac")];
    client.tasks.clear();

    let outcome = run_tick(&mut client, tick_req(profile, 2, false), &[], &[]).unwrap();

    let ForgeOutcome::Tick { released, held, .. } = outcome else {
        panic!("expected tick outcome");
    };
    assert_eq!(released.len(), 1);
    assert!(held.is_empty());
    assert_eq!(client.release_calls.len(), 1);
    assert!(client.heartbeat_calls.is_empty());
}

#[test]
fn tick_heartbeats_claims_with_a_live_agent() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    client.all_tasks = vec![held_task("abcdef12-0000-0000-0000-000000000000", "bn@mac")];
    client.tasks.clear();
    let live = vec!["bn:dispatch-forge-task-abcdef12-0000".to_string()];

    let outcome = run_tick(&mut client, tick_req(profile, 2, false), &live, &[]).unwrap();

    let ForgeOutcome::Tick { released, held, .. } = outcome else {
        panic!("expected tick outcome");
    };
    assert!(released.is_empty());
    assert_eq!(held.len(), 1);
    assert_eq!(client.heartbeat_calls.len(), 1);
    assert!(client.release_calls.is_empty());
}

#[test]
fn tick_stops_dispatching_at_max_active() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    client.all_tasks = vec![
        held_task("abcdef12-0000-0000-0000-000000000000", "bn@mac"),
        held_task("fedcba21-0000-0000-0000-000000000000", "bn@mac"),
    ];
    let live = vec![
        "bn:dispatch-forge-task-abcdef12-0000".to_string(),
        "bn:dispatch-forge-task-fedcba21-0000".to_string(),
    ];

    let outcome = run_tick(&mut client, tick_req(profile, 2, true), &live, &[]).unwrap();

    let ForgeOutcome::Tick { dispatched, .. } = outcome else {
        panic!("expected tick outcome");
    };
    assert!(dispatched.is_empty(), "capacity was full");
    assert_eq!(client.list_calls, 0, "claimable queue not consulted");
}

#[test]
fn tick_dry_run_mutates_nothing() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    client.all_tasks = vec![held_task("abcdef12-0000-0000-0000-000000000000", "bn@mac")];

    let outcome = run_tick(&mut client, tick_req(profile, 2, true), &[], &[]).unwrap();

    let ForgeOutcome::Tick {
        released,
        dispatched,
        ..
    } = outcome
    else {
        panic!("expected tick outcome");
    };
    assert_eq!(released.len(), 1, "reports what it would release");
    assert_eq!(dispatched.len(), 1, "reports what it would dispatch");
    assert!(client.release_calls.is_empty());
    assert!(client.claim_calls.is_empty());
    assert!(client.heartbeat_calls.is_empty());
    assert!(crate::cmd::dispatch::invocation::take_launched().is_empty());
}

#[test]
fn list_is_side_effect_free() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let cfg = Config::load();
    let mut client = FakeClient::new();

    let outcome = run_with_client(
        &cfg,
        ForgeCmd::List {
            agent: "mac-agent".into(),
            task: None,
            limit: 50,
        },
        false,
        &mut client,
    )
    .unwrap();

    assert!(matches!(outcome, ForgeOutcome::List { .. }));
    assert_eq!(client.list_calls, 1);
    assert!(client.claim_calls.is_empty());
    assert!(crate::cmd::dispatch::invocation::take_launched().is_empty());
}

#[test]
fn claim_uses_configured_claimer() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let cfg = Config::load();
    let mut client = FakeClient::new();

    let outcome = run_with_client(
        &cfg,
        ForgeCmd::Claim {
            agent: "mac-agent".into(),
            task: "task-1".into(),
            lease_seconds: 1200,
        },
        false,
        &mut client,
    )
    .unwrap();

    assert!(matches!(outcome, ForgeOutcome::Claim { .. }));
    assert_eq!(client.claim_calls, vec![("bn@mac".into(), "task-1".into(), 1200)]);
    assert!(crate::cmd::dispatch::invocation::take_launched().is_empty());
}

#[test]
fn dispatch_happens_only_after_claim() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let cfg = Config::load();
    let mut client = FakeClient::new();

    let outcome = run_with_client(
        &cfg,
        ForgeCmd::Dispatch {
            agent: "mac-agent".into(),
            task: "task-1".into(),
            repo: Some("bn".into()),
            agent_harness: "copilot".into(),
            base: Some("agent/forge-agent-cli".into()),
            skill: "forge-task".into(),
            name: None,
            verify: true,
            lease_seconds: 900,
        },
        true,
        &mut client,
    )
    .unwrap();

    assert!(matches!(outcome, ForgeOutcome::Dispatch { .. }));
    assert_eq!(client.claim_calls.len(), 1);
    let launched = crate::cmd::dispatch::invocation::take_launched();
    assert_eq!(launched.len(), 1);
    assert!(launched[0].contains(&"dispatch".to_string()));
    assert!(launched[0].contains(&"--task".to_string()));
    assert!(launched[0].contains(&"task-1".to_string()));
    assert!(launched[0].contains(&"--verify".to_string()));
    assert!(!launched[0].contains(&"--yolo".to_string()), "a hand-run dispatch stays interactive");
    assert!(launched[0].contains(&"--json".to_string()));
    assert_eq!(
        flag_value(&launched[0], "--base"),
        Some("agent/forge-agent-cli")
    );
    assert_eq!(flag_value(&launched[0], "--skill"), Some("forge-task"));
    let item = launched[0]
        .get(1)
        .expect("dispatch item should be the first positional arg");
    assert!(item.contains("Forge task task-1: Fix thing"));
    assert!(item.contains("Use the `forge-task` skill as the lifecycle wrapper"));
    assert!(item.contains("When done, update Forge task task-1"));
    assert!(item.contains("rich Markdown report"));
    assert!(item.contains("finish_activity using claimer bn@mac"));
}

#[test]
fn reads_line_framed_mcp_messages() {
    let mut input = BufReader::new(
        br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}
"#
        .as_slice(),
    );
    let value = read_mcp_message(&mut input).unwrap();
    assert_eq!(value["id"], 1);
    assert_eq!(value["result"]["ok"], true);
}

#[test]
fn extracts_json_from_tool_text_content() {
    let value = tool_result_json(json!({
        "content": [
            {
                "type": "text",
                "text": "{\"items\":[{\"id\":\"t1\",\"title\":\"Task\"}]}"
            }
        ]
    }))
    .unwrap();
    let parsed: ForgeListBnTasksResponse = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.items[0].id, "t1");
}

#[test]
fn tick_never_claims_a_task_it_cannot_route() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    let mut req = tick_req(profile, 2, false);
    req.repo = None;

    let outcome = run_tick(&mut client, req, &[], &[]).unwrap();

    let ForgeOutcome::Tick {
        dispatched,
        unrouted,
        ..
    } = outcome
    else {
        panic!("expected tick outcome");
    };
    assert!(dispatched.is_empty());
    assert_eq!(unrouted.len(), 1, "{unrouted:?}");
    assert!(client.claim_calls.is_empty(), "an unroutable task is left for another agent");
}

#[test]
fn the_poller_dispatches_unattended() {
    let _lock = env_lock();
    let temp = tempfile::tempdir().unwrap();
    write_profile(temp.path());
    let _home = EnvGuard::set("HOME", temp.path());
    let profile = load_profile("mac-agent").unwrap();
    let mut client = FakeClient::new();
    let _ = crate::cmd::dispatch::invocation::take_launched();

    let outcome = run_tick(&mut client, tick_req(profile, 2, false), &[], &[]).unwrap();

    let ForgeOutcome::Tick { dispatched, .. } = outcome else {
        panic!("expected tick outcome");
    };
    assert_eq!(dispatched.len(), 1);
    let launched = crate::cmd::dispatch::invocation::take_launched();
    assert!(
        launched[0].contains(&"--yolo".to_string()),
        "no one watches a poller dispatch, so it must not wait for plan confirmation: {:?}",
        launched[0]
    );
}

