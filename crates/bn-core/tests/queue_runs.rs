use bn_core::backlog::{AddSpec, BacklogEntry};
use bn_core::events::Event;
use bn_core::queue::log::{rows, runs, Query, Row, Tone};
use serde_json::{Map, Value};

fn row(
    at: i64,
    kind: &'static str,
    repo: &str,
    id: &str,
    tone: Tone,
    fields: &[(&str, Value)],
) -> Row {
    Row {
        at,
        kind,
        repo: repo.to_string(),
        id: id.to_string(),
        summary: kind.to_string(),
        tone,
        fields: Map::from_iter(
            fields
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone())),
        ),
    }
}

#[test]
fn dispatch_and_finish_pair_into_one_transparent_run() {
    let rows = vec![
        row(
            100,
            "dispatch",
            "bn",
            "work-1",
            Tone::Start,
            &[
                ("item", Value::String("ship queue events".to_string())),
                ("work_kind", Value::String("ship".to_string())),
                ("lane", Value::String("coding".to_string())),
                ("node", Value::String("mac.lan".to_string())),
                ("rationale", Value::String("highest priority".to_string())),
            ],
        ),
        row(
            125,
            "finish",
            "bn",
            "work-1",
            Tone::Info,
            &[("exit_code", Value::from(0))],
        ),
    ];

    let folded = runs(&rows, &[], 130);

    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].outcome, "completed");
    assert_eq!(folded[0].duration_secs, 25);
    assert_eq!(folded[0].node, "mac.lan");
    assert_eq!(folded[0].transitions.len(), 2);
    assert_eq!(
        folded[0].transitions[0].fields["rationale"],
        "highest priority"
    );
}

#[test]
fn launch_failure_closes_the_run_with_the_recorded_error() {
    let rows = vec![
        row(
            100,
            "dispatch",
            "bn",
            "work-1",
            Tone::Start,
            &[("item", Value::String("start an agent".to_string()))],
        ),
        row(
            104,
            "finish",
            "bn",
            "work-1",
            Tone::Bad,
            &[
                ("outcome", Value::String("failed".to_string())),
                ("phase", Value::String("launch".to_string())),
                ("error", Value::String("harness unavailable".to_string())),
            ],
        ),
    ];

    let folded = runs(&rows, &[], 110);

    assert_eq!(folded[0].outcome, "failed");
    assert_eq!(folded[0].duration_secs, 4);
    assert_eq!(
        folded[0].summary,
        "Executor did not start — harness unavailable"
    );
    assert_eq!(
        folded[0].transitions[1].fields["error"],
        "harness unavailable"
    );
}

#[test]
fn an_open_dispatch_remains_in_flight() {
    let rows = vec![row(
        100,
        "dispatch",
        "bn",
        "work-1",
        Tone::Start,
        &[("item", Value::String("long agent".to_string()))],
    )];

    let folded = runs(&rows, &[], 145);

    assert_eq!(folded[0].outcome, "in-flight");
    assert_eq!(folded[0].ended_at, None);
    assert_eq!(folded[0].duration_secs, 45);
}

#[test]
fn raw_rows_preserve_every_structured_event_field() {
    let fields = Map::from_iter([
        ("repo".to_string(), Value::String("bn".to_string())),
        ("id".to_string(), Value::String("work-1".to_string())),
        (
            "evidence".to_string(),
            serde_json::json!({ "attempt": 2, "labels": ["rust", "local"] }),
        ),
    ]);
    let events = vec![Event {
        kind: "queue_dispatch".to_string(),
        ts: 100,
        fields: fields.clone(),
    }];

    let folded = rows(
        &events,
        &Query {
            since: 0,
            limit: 10,
            kind: None,
            repo: None,
        },
    );

    assert_eq!(folded[0].fields, fields);
}

#[test]
fn durable_backlog_enriches_older_sparse_events() {
    let entry = BacklogEntry::new(
        "bn",
        "legacy work",
        &AddSpec {
            task: "t42".to_string(),
            ..AddSpec::default()
        },
        10,
    );
    let rows = vec![row(100, "expired", "bn", &entry.id, Tone::Bad, &[])];

    let folded = runs(&rows, &[entry], 100);

    assert_eq!(folded[0].work, "legacy work");
    assert_eq!(folded[0].task, "t42");
    assert_eq!(folded[0].outcome, "expired");
}

#[test]
fn successful_tick_duplicates_are_omitted_but_holds_remain() {
    let rows = vec![
        row(
            100,
            "tick",
            "",
            "",
            Tone::Info,
            &[("outcome", Value::String("started".to_string()))],
        ),
        row(
            110,
            "tick",
            "",
            "",
            Tone::Info,
            &[
                ("outcome", Value::String("held".to_string())),
                ("reason", Value::String("coding lane full".to_string())),
            ],
        ),
    ];

    let folded = runs(&rows, &[], 120);

    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].outcome, "held");
    assert_eq!(folded[0].summary, "coding lane full");
}
