//! `bn fleet --json` contract golden for the worktree-centric snapshot (#464 / #871).

use crate::agent_live::{FleetSnapshot, FleetSnapshotRow, FLEET_SNAPSHOT_SCHEMA};
use crate::cmd::remote_track::RemoteRunView;
use bn_core::agent::AgentHealth;
use serde_json::json;

fn base_row(lane: &str) -> FleetSnapshotRow {
    let (repo, branch) = lane.split_once(':').unwrap_or((lane, ""));
    FleetSnapshotRow {
        id: "Fleet task".into(),
        lane: lane.into(),
        target: lane.into(),
        worktree: format!("/worktree/{repo}/{branch}"),
        repo: repo.into(),
        branch: branch.into(),
        state: "working".into(),
        status: "working".into(),
        state_source: "tmux".into(),
        health: None,
        idle_secs: None,
        agent_alive: true,
        has_window: true,
        backend: "local".into(),
        harness: "copilot".into(),
        pipeline_stage: None,
        tokens: None,
        turns: None,
        over_budget: false,
        artifact: None,
        return_channel: Some(lane.into()),
        session_id: "session-1".into(),
        updated_at: 100,
    }
}

fn remote_run() -> RemoteRunView {
    RemoteRunView {
        run_id: Some(42),
        slug: "bn-remote-1".into(),
        backend: "gh",
        state: "running".into(),
        run_status: None,
        run_url: Some("https://github.com/eduuh/bn/actions/runs/42".into()),
        pr_url: Some("https://github.com/eduuh/bn/pull/13".into()),
        pr_state: Some("OPEN".into()),
        seed: "issue-500".into(),
    }
}

#[test]
fn fleet_board_json_matches_golden() {
    let snapshot = FleetSnapshot {
        schema: FLEET_SNAPSHOT_SCHEMA,
        generated_at: 123,
        agents: vec![
            FleetSnapshotRow {
                id: "Implement fleet view".into(),
                state: "waiting".into(),
                status: "needs-you".into(),
                health: Some(AgentHealth::Active),
                idle_secs: Some(42),
                harness: "claude".into(),
                pipeline_stage: Some("implement".into()),
                tokens: Some(20),
                turns: Some(7),
                over_budget: true,
                artifact: Some("https://github.com/eduuh/bn/pull/13".into()),
                session_id: "sess-a".into(),
                worktree: "/worktree/bn/feat-a".into(),
                lane: "bn:feat-a".into(),
                branch: "feat-a".into(),
                ..base_row("bn:feat-a")
            },
            FleetSnapshotRow {
                id: "Recover fleet lane".into(),
                state: "stuck".into(),
                status: "stuck".into(),
                state_source: "session".into(),
                health: Some(AgentHealth::Stuck),
                idle_secs: Some(1_800),
                agent_alive: false,
                has_window: false,
                tokens: Some(5),
                turns: Some(3),
                return_channel: Some("controller:controller-1".into()),
                session_id: "sess-b".into(),
                worktree: "/worktree/bn/feat-b".into(),
                lane: "bn:feat-b".into(),
                branch: "feat-b".into(),
                ..base_row("bn:feat-b")
            },
        ],
        count: 2,
        total_tokens: 25,
        over_budget: 1,
        remote_runs: vec![remote_run()],
    };

    let got = serde_json::to_value(snapshot).unwrap();
    let golden = json!({
        "schema": "bn.fleet-snapshot.v1",
        "generated_at": 123,
        "agents": [
            {
                "id": "Implement fleet view",
                "lane": "bn:feat-a",
                "target": "bn:feat-a",
                "worktree": "/worktree/bn/feat-a",
                "repo": "bn",
                "branch": "feat-a",
                "state": "waiting",
                "status": "needs-you",
                "state_source": "tmux",
                "health": "active",
                "idle_secs": 42,
                "agent_alive": true,
                "has_window": true,
                "backend": "local",
                "harness": "claude",
                "pipeline_stage": "implement",
                "tokens": 20,
                "turns": 7,
                "over_budget": true,
                "artifact": "https://github.com/eduuh/bn/pull/13",
                "return_channel": "bn:feat-a",
                "session_id": "sess-a",
                "updated_at": 100
            },
            {
                "id": "Recover fleet lane",
                "lane": "bn:feat-b",
                "target": "bn:feat-b",
                "worktree": "/worktree/bn/feat-b",
                "repo": "bn",
                "branch": "feat-b",
                "state": "stuck",
                "status": "stuck",
                "state_source": "session",
                "health": "stuck",
                "idle_secs": 1800,
                "agent_alive": false,
                "has_window": false,
                "backend": "local",
                "harness": "copilot",
                "tokens": 5,
                "turns": 3,
                "over_budget": false,
                "return_channel": "controller:controller-1",
                "session_id": "sess-b",
                "updated_at": 100
            }
        ],
        "count": 2,
        "total_tokens": 25,
        "over_budget": 1,
        "remote_runs": [
            {
                "run_id": 42,
                "slug": "bn-remote-1",
                "backend": "gh",
                "state": "running",
                "run_url": "https://github.com/eduuh/bn/actions/runs/42",
                "pr_url": "https://github.com/eduuh/bn/pull/13",
                "pr_state": "OPEN",
                "seed": "issue-500"
            }
        ]
    });
    assert_eq!(
        got, golden,
        "`bn fleet --json` snapshot shape drifted from golden"
    );
}

#[test]
fn fleet_board_json_without_budget_flags_nothing() {
    let snapshot = FleetSnapshot {
        schema: FLEET_SNAPSHOT_SCHEMA,
        generated_at: 123,
        agents: vec![FleetSnapshotRow {
            tokens: Some(9_999),
            ..base_row("bn:feat-a")
        }],
        count: 1,
        total_tokens: 9_999,
        over_budget: 0,
        remote_runs: vec![],
    };
    let got = serde_json::to_value(snapshot).unwrap();
    assert_eq!(got["over_budget"], json!(0), "no budget -> 0 over budget");
    assert_eq!(got["total_tokens"], json!(9_999));
    assert_eq!(got["agents"][0]["over_budget"], json!(false));
    assert_eq!(got["agents"][0]["state"], json!("working"));
    assert_eq!(got["remote_runs"], json!([]));
}
