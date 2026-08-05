//! `--json` **contract (golden) tests** for the dashboard-facing surfaces (#464).
//!
//! The bn-web dashboard (#386/#389/#391), `bn cost` (#381), and external `… | jq` tooling consume
//! the machine-readable `data` payloads of `bn cost`, `bn fleet --cost`, `bn orchestrate status`,
//! and `bn orchestrate answers`. There were no contract tests, so a silent field rename/removal
//! would break every consumer with no local signal. These pin the exact serialized **shape**
//! (field set + types) of each surface as checked-in `json!` goldens built from fixed in-memory
//! fixtures — any change to the wire shape now fails loudly and must be deliberately re-blessed.
//!
//! Additive by design: they build the view structs directly (or via the shared `payload` builders
//! the CLI itself calls) and assert their serialization; they do **not** change any output. The
//! goldens are pure over their in-memory inputs (no clock/tz/disk), so they're stable across
//! machines and time. `bn fleet --json` (the tmux/gh board) is pinned in bn-cli, where its
//! `LiveView`/`RemoteRunView` types live.

use bn_core::backlog::{ExecutionTarget, Substrate};
use bn_core::cost::{CostRow, CostTotals, CostView, DayCategory, LiveCost};
use bn_core::orchestrate::{
    Answer, DispatchPhase, ItemStatus, OrchestrateState, PaneState, QueueItem, RecoveryEvent,
    SupervisorAction, WorkClass,
};
use bn_core::payload;
use bn_core::plan_review::Review;
use serde_json::{json, Value};

// --- shared fixtures ------------------------------------------------------------------------

/// A folded cost row in both units — the shape every `by_*` breakdown of a [`CostView`] repeats.
fn row(
    key: &str,
    sessions: u64,
    input: i64,
    cache: i64,
    output: i64,
    aic: Option<f64>,
    usd: Option<f64>,
) -> CostRow {
    CostRow {
        key: key.into(),
        sessions,
        input_tokens: input,
        cache_read_tokens: cache,
        output_tokens: output,
        aic,
        cost_usd: usd,
    }
}

/// A queue item with every field defaulted — override only the ones a case exercises, so each
/// fixture reads as its delta from a fresh queued item.
fn queue_item(id: &str) -> QueueItem {
    QueueItem {
        id: id.into(),
        item: id.into(),
        repo: "bn".into(),
        status: ItemStatus::Queued,
        dispatch_phase: DispatchPhase::Pending,
        branch: None,
        pr: None,
        attempts: 0,
        dispatched_at: 0,
        last_relaunch_at: 0,
        updated_at: 1_760_000_100,
        last_state: None,
        pane_activity_fingerprint: None,
        pane_activity_changed_at: 0,
        stuck: None,
        stuck_notified_key: None,
        harness: None,
        last_switch_at: 0,
        touches: Vec::new(),
        controller_id: None,
        held_by: None,
        substrate: Substrate::Auto,
        work_class: WorkClass::Coding,
        target: ExecutionTarget::Local,
        required_capabilities: Vec::new(),
        excluded_capabilities: Vec::new(),
        command_approvals_trusted: false,
        answer: None,
        claimed_remote: false,
        node: None,
        request_id: None,
        attempt_id: None,
        worker_state: None,
    }
}

// --- (1) `bn cost --json` → the full `CostView` ---------------------------------------------

#[test]
fn cost_view_json_matches_golden() {
    // A window with usage across every breakdown, exercising both cost units: a priceable Copilot
    // row (`aic`+`cost_usd` set) and an unpriceable one (both `null`), so the Option shape is pinned.
    let view = CostView {
        window_days: 7,
        totals: CostTotals {
            sessions: 3,
            input_tokens: 1_000,
            cache_read_tokens: 19_000,
            output_tokens: 2_000,
            aic: Some(1.5),
            cost_usd: Some(0.42),
        },
        by_category: vec![
            row(
                "orchestration",
                2,
                800,
                15_000,
                1_500,
                Some(1.5),
                Some(0.42),
            ),
            row("coding", 1, 200, 4_000, 500, None, None),
        ],
        by_machine: vec![row(
            "my-laptop",
            3,
            1_000,
            19_000,
            2_000,
            Some(1.5),
            Some(0.42),
        )],
        by_repo: vec![row("bn", 3, 1_000, 19_000, 2_000, Some(1.5), Some(0.42))],
        by_model: vec![row(
            "claude-opus-4.8",
            3,
            1_000,
            19_000,
            2_000,
            Some(1.5),
            Some(0.42),
        )],
        by_day: vec![row(
            "2026-07-01",
            3,
            1_000,
            19_000,
            2_000,
            Some(1.5),
            Some(0.42),
        )],
        by_day_category: vec![DayCategory {
            day: "2026-07-01".into(),
            category: "orchestration".into(),
            sessions: 2,
            input_tokens: 800,
            cache_read_tokens: 15_000,
            output_tokens: 1_500,
            aic: Some(1.5),
            cost_usd: Some(0.42),
        }],
    };

    let got = serde_json::to_value(&view).unwrap();
    let golden = json!({
        "window_days": 7,
        "totals": {
            "sessions": 3,
            "input_tokens": 1_000,
            "cache_read_tokens": 19_000,
            "output_tokens": 2_000,
            "aic": 1.5,
            "cost_usd": 0.42
        },
        "by_category": [
            { "key": "orchestration", "sessions": 2, "input_tokens": 800, "cache_read_tokens": 15_000, "output_tokens": 1_500, "aic": 1.5, "cost_usd": 0.42 },
            { "key": "coding", "sessions": 1, "input_tokens": 200, "cache_read_tokens": 4_000, "output_tokens": 500, "aic": null, "cost_usd": null }
        ],
        "by_machine": [
            { "key": "my-laptop", "sessions": 3, "input_tokens": 1_000, "cache_read_tokens": 19_000, "output_tokens": 2_000, "aic": 1.5, "cost_usd": 0.42 }
        ],
        "by_repo": [
            { "key": "bn", "sessions": 3, "input_tokens": 1_000, "cache_read_tokens": 19_000, "output_tokens": 2_000, "aic": 1.5, "cost_usd": 0.42 }
        ],
        "by_model": [
            { "key": "claude-opus-4.8", "sessions": 3, "input_tokens": 1_000, "cache_read_tokens": 19_000, "output_tokens": 2_000, "aic": 1.5, "cost_usd": 0.42 }
        ],
        "by_day": [
            { "key": "2026-07-01", "sessions": 3, "input_tokens": 1_000, "cache_read_tokens": 19_000, "output_tokens": 2_000, "aic": 1.5, "cost_usd": 0.42 }
        ],
        "by_day_category": [
            { "day": "2026-07-01", "category": "orchestration", "sessions": 2, "input_tokens": 800, "cache_read_tokens": 15_000, "output_tokens": 1_500, "aic": 1.5, "cost_usd": 0.42 }
        ]
    });
    assert_eq!(
        got, golden,
        "`bn cost --json` (CostView) shape drifted from golden"
    );
}

// --- (2) `bn fleet --cost --json` → the live `LiveCost` snapshot ----------------------------

#[test]
fn live_cost_json_matches_golden() {
    // In-flight usage only: live logs surface output tokens but no model/AIC mid-session, so `aic`
    // and `cost_usd` are `null` on every live row — pin that so consumers don't expect a bill yet.
    let live = LiveCost {
        agents: 2,
        output_tokens: 1_234,
        by_category: vec![
            row("orchestration", 1, 0, 0, 1_000, None, None),
            row("coding", 1, 0, 0, 234, None, None),
        ],
    };

    let got = serde_json::to_value(&live).unwrap();
    let golden = json!({
        "agents": 2,
        "output_tokens": 1_234,
        "by_category": [
            { "key": "orchestration", "sessions": 1, "input_tokens": 0, "cache_read_tokens": 0, "output_tokens": 1_000, "aic": null, "cost_usd": null },
            { "key": "coding", "sessions": 1, "input_tokens": 0, "cache_read_tokens": 0, "output_tokens": 234, "aic": null, "cost_usd": null }
        ]
    });
    assert_eq!(
        got, golden,
        "`bn fleet --cost --json` (LiveCost) shape drifted from golden"
    );
}

// --- (3) `bn orchestrate status --json` → the run viewmodel ---------------------------------

#[test]
fn orchestrate_status_json_matches_golden() {
    // A resident supervisor run with one item in each lifecycle: a queued item, a dispatched
    // (in-flight) coding lane with a live pane + nav coordinates, and a merged scout carrying its
    // routed-back answer. Reviews are empty, so the dispatched item reads `waiting_for_review:false`.
    let dispatched = QueueItem {
        item: "340".into(),
        status: ItemStatus::Dispatched,
        dispatch_phase: DispatchPhase::Dispatched,
        branch: Some("dispatch/340".into()),
        pr: Some("https://github.com/eduuh/bn/pull/12".into()),
        attempts: 1,
        dispatched_at: 1_760_000_050,
        updated_at: 1_760_000_090,
        last_state: Some(PaneState::Working),
        harness: Some("copilot".into()),
        ..queue_item("340")
    };
    let scout = QueueItem {
        item: "why does the resolver 404".into(),
        status: ItemStatus::Merged,
        dispatch_phase: DispatchPhase::Dispatched,
        branch: Some("dispatch/scout-9".into()),
        work_class: WorkClass::Research,
        answer: Some(Answer {
            question: "why does the resolver 404".into(),
            path: "/notes/bn/scout-9/findings.md".into(),
            harvested_at: 1_760_000_070,
        }),
        ..queue_item("scout-9")
    };
    let mut st = OrchestrateState {
        repo: "bn".into(),
        queue: vec![queue_item("338"), dispatched, scout],
        concurrency: 3,
        research: Some(1),
        solo: false,
        recovery: vec![RecoveryEvent {
            ts: 1_760_000_080,
            id: "340".into(),
            state: PaneState::IdleWithWip,
            action: SupervisorAction::RelaunchToResume,
            note: "relaunch #1 after idle-with-wip".into(),
        }],
        started_at: 1_760_000_000,
        updated_at: 1_760_000_100,
        stopped: false,
        supervise: true,
        keep_warm: None,
        keep_warm_last_brainstorm_at: None,
        self_feed: false,
        harness: Some("copilot".into()),
        boot_id: Some("boot-abc".into()),
        dispatch_bind: Some("tokens".into()),
        dispatch_reason: Some("paused — hourly token budget reached".into()),
        dispatch_decision: None,
        nodes: Vec::new(),
    };
    st.dispatch_decision = Some(bn_core::orchestrate::DispatchDecision {
        at: 1_760_000_100,
        outcome: "blocked".into(),
        gate: Some("tokens".into()),
        reason: Some("paused — hourly token budget reached".into()),
        attempted: 0,
        launched: 0,
        lanes: Vec::new(),
        skipped: Vec::new(),
    });

    let reviews: &[Review] = &[];
    let got = payload::orchestrate_status_data(&st, reviews);
    let golden = json!({
        "repo": "bn",
        "concurrency": 3,
        "research": 1,
        "solo": false,
        "stopped": false,
        "supervise": true,
        "self_feed": false,
        "drained": false,
        "resident": true,
        "started_at": 1_760_000_000,
        "updated_at": 1_760_000_100,
        "counts": {
            "queued": 1,
            "in_flight": 1,
            "merged": 1,
            "failed": 0,
            "total": 3,
            "remote": 0
        },
        "dispatch_bind": "tokens",
        "dispatch_reason": "paused — hourly token budget reached",
        "queue": [
            {
                "id": "338",
                "item": "338",
                "repo": "bn",
                "status": "queued",
                "attempts": 0,
                "dispatched_at": 0,
                "last_relaunch_at": 0,
                "updated_at": 1_760_000_100,
                "last_switch_at": 0,
                "substrate": "auto",
                "work_class": "coding"
            },
            {
                "id": "340",
                "item": "340",
                "repo": "bn",
                "status": "dispatched",
                "dispatch_phase": "dispatched",
                "branch": "dispatch/340",
                "pr": "https://github.com/eduuh/bn/pull/12",
                "attempts": 1,
                "dispatched_at": 1_760_000_050,
                "last_relaunch_at": 0,
                "updated_at": 1_760_000_090,
                "last_state": "working",
                "harness": "copilot",
                "last_switch_at": 0,
                "substrate": "auto",
                "work_class": "coding",
                "tmux_session": "bn",
                "tmux_window": "dispatch-340",
                "waiting_for_review": false
            },
            {
                "id": "scout-9",
                "item": "why does the resolver 404",
                "repo": "bn",
                "status": "merged",
                "dispatch_phase": "dispatched",
                "branch": "dispatch/scout-9",
                "attempts": 0,
                "dispatched_at": 0,
                "last_relaunch_at": 0,
                "updated_at": 1_760_000_100,
                "last_switch_at": 0,
                "substrate": "auto",
                "work_class": "research",
                "answer": {
                    "question": "why does the resolver 404",
                    "path": "/notes/bn/scout-9/findings.md",
                    "harvested_at": 1_760_000_070
                },
                "tmux_session": "bn",
                "tmux_window": "dispatch-scout-9"
            }
        ],
        "recovery": [
            {
                "ts": 1_760_000_080,
                "id": "340",
                "state": "idle-with-wip",
                "action": "relaunch-to-resume",
                "note": "relaunch #1 after idle-with-wip"
            }
        ]
    });
    assert_eq!(
        got, golden,
        "`bn orchestrate status --json` shape drifted from golden"
    );
}

// --- (4) `bn orchestrate answers --json` → the scout/ask deliverable surface ----------------

#[test]
fn orchestrate_answers_json_matches_golden() {
    let a1 = QueueItem {
        answer: Some(Answer {
            question: "why does the resolver 404".into(),
            path: "/notes/bn/scout-9/findings.md".into(),
            harvested_at: 1_760_000_070,
        }),
        ..queue_item("scout-9")
    };
    let a2 = QueueItem {
        answer: Some(Answer {
            question: "which crate owns dispatch".into(),
            path: "/notes/bn/scout-10/findings.md".into(),
            harvested_at: 1_760_000_120,
        }),
        ..queue_item("scout-10")
    };

    let got = payload::orchestrate_answers_data("bn", &[&a1, &a2]);
    let golden = json!({
        "repo": "bn",
        "answers": [
            { "id": "scout-9", "question": "why does the resolver 404", "path": "/notes/bn/scout-9/findings.md", "harvested_at": 1_760_000_070 },
            { "id": "scout-10", "question": "which crate owns dispatch", "path": "/notes/bn/scout-10/findings.md", "harvested_at": 1_760_000_120 }
        ]
    });
    assert_eq!(
        got, golden,
        "`bn orchestrate answers --json` shape drifted from golden"
    );

    // The no-run / nothing-routed-back case is a stable empty array (not `null`) under the same repo.
    let empty: Value = payload::orchestrate_answers_data("bn", &[]);
    assert_eq!(empty, json!({ "repo": "bn", "answers": [] }));
}
