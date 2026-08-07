//! The dispatcher's history, read back.
//!
//! [`crate::queue::decision`] holds exactly one pass — the last. That answers "why is nothing
//! running *right now*" and nothing at all about "what happened while I was away", which is the
//! question you actually have when work you expected never ran and the current pass looks healthy.
//! The event log already had the answer; this is the reader.
//!
//! In bn-core rather than the CLI because two surfaces ask it — `bn queue log` and the dashboard —
//! and a terminal that disagreed with the page about what the dispatcher did would be worse than
//! either alone. Both render [`Row`]s from this one fold.
use crate::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// The queue's own event kinds, paired with the short names a filter accepts.
///
/// One table, so the filter, the renderer and the UI's legend cannot disagree about what belongs to
/// the queue.
pub const KINDS: [(&str, &str); 6] = [
    ("queue_dispatch", "dispatch"),
    ("queue_tick", "tick"),
    ("queue_tick_skipped", "skipped"),
    ("queue_finish", "finish"),
    ("queue_expired", "expired"),
    ("queue_lease_reclaimed", "reclaimed"),
];

/// The short name for a raw event kind, or `None` if it is not the queue's.
pub fn short_name(kind: &str) -> Option<&'static str> {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, s)| *s)
}

/// Whether `short` names a queue event kind — for validating a filter before using it.
pub fn is_kind(short: &str) -> bool {
    KINDS.iter().any(|(_, s)| *s == short)
}

/// Every short name, for help text and the UI's filter chips.
pub fn kind_names() -> Vec<&'static str> {
    KINDS.iter().map(|(_, s)| *s).collect()
}

/// How an event reads: fine, or something went wrong.
///
/// Carried separately from the summary so a surface can colour by it without parsing prose — the
/// failure of the first version of this, where the only signal a job had failed was the word
/// FAILED inside a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tone {
    /// Ordinary progress.
    Info,
    /// Work started.
    Start,
    /// Something did not work, or was given up on.
    Bad,
}

/// One line of dispatcher history, ready to render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub at: i64,
    /// The short kind (`dispatch`, `tick`, …).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The one-line human summary, identical in the terminal and the dashboard.
    pub summary: String,
    pub tone: Tone,
    /// The complete structured payload from the append-only event log.
    #[serde(default)]
    pub fields: Map<String, Value>,
}

/// One recorded state transition inside a paired queue run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transition {
    pub at: i64,
    pub kind: String,
    pub summary: String,
    pub tone: Tone,
    pub fields: Map<String, Value>,
}

/// A dispatch and its terminal event, or one standalone operational event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub key: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_secs: i64,
    pub repo: String,
    pub id: String,
    pub work: String,
    pub work_kind: String,
    pub lane: String,
    pub exec: String,
    pub node: String,
    pub harness: String,
    pub priority: Option<i64>,
    pub rationale: String,
    pub task: String,
    pub entry_status: String,
    pub outcome: String,
    pub summary: String,
    pub tone: Tone,
    pub transitions: Vec<Transition>,
}

/// What to read back.
pub struct Query<'a> {
    /// Oldest event to include, epoch secs.
    pub since: i64,
    /// Most recent N after filtering.
    pub limit: usize,
    /// Restrict to one short kind.
    pub kind: Option<&'a str>,
    /// Restrict to one repo.
    pub repo: Option<&'a str>,
}

/// Fold `events` into history rows, oldest first so the result reads as a narrative.
///
/// Pure over the event list: the caller supplies the log, so this is testable without a state dir
/// and identical whichever surface asked.
pub fn rows(events: &[Event], q: &Query) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for e in events {
        let Some(kind) = short_name(&e.kind) else {
            continue;
        };
        if e.ts < q.since || q.kind.is_some_and(|k| k != kind) {
            continue;
        }
        let row = row(e, kind);
        if q.repo.is_some_and(|r| !row.repo.eq_ignore_ascii_case(r)) {
            continue;
        }
        out.push(row);
    }
    // Newest N, then back into chronological order — a limit should trim the distant past, not the
    // part being looked at.
    if out.len() > q.limit {
        out.drain(..out.len() - q.limit);
    }
    out
}

fn row(e: &Event, kind: &'static str) -> Row {
    let s = |k: &str| {
        e.fields
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let n = |k: &str| e.fields.get(k).and_then(serde_json::Value::as_i64);
    let (summary, tone) = match e.kind.as_str() {
        "queue_dispatch" => {
            let target = s("target");
            let where_ = if target.is_empty() {
                String::new()
            } else {
                format!(" → {target}")
            };
            (
                format!(
                    "{} · {}{where_} · p{}",
                    s("work_kind"),
                    s("exec"),
                    n("priority").unwrap_or_default()
                ),
                Tone::Start,
            )
        }
        "queue_tick" => (tick_summary(&s("outcome"), &s("bind"), &n), Tone::Info),
        "queue_tick_skipped" => (format!("skipped — {}", s("reason")), Tone::Info),
        "queue_finish" => {
            let failed = s("outcome") == "failed" || n("exit_code").is_some_and(|code| code != 0);
            if !failed {
                ("ok".to_string(), Tone::Info)
            } else if let Some(code) = n("exit_code") {
                (format!("FAILED (exit {code})"), Tone::Bad)
            } else if !s("error").is_empty() {
                (format!("FAILED — {}", s("error")), Tone::Bad)
            } else {
                ("FAILED".to_string(), Tone::Bad)
            }
        }
        "queue_expired" => ("dropped — its window passed".to_string(), Tone::Bad),
        "queue_lease_reclaimed" => (
            "lease reclaimed — its executor was gone".to_string(),
            Tone::Bad,
        ),
        _ => (String::new(), Tone::Info),
    };
    Row {
        at: e.ts,
        kind,
        repo: s("repo"),
        id: s("id"),
        summary,
        tone,
        fields: e.fields.clone(),
    }
}

/// Fold raw rows into work-shaped records while preserving every transition.
///
/// `queue_tick` events that merely report a successful pass are omitted because their dispatches
/// already become rows of their own. Held/skipped passes remain: they are outcomes with no executor,
/// and dropping them would erase the evidence for why nothing moved.
pub fn runs(rows: &[Row], entries: &[crate::backlog::BacklogEntry], now: i64) -> Vec<Run> {
    let entries: HashMap<(&str, &str), &crate::backlog::BacklogEntry> = entries
        .iter()
        .map(|entry| ((entry.repo.as_str(), entry.id.as_str()), entry))
        .collect();
    let mut open: HashMap<(String, String), usize> = HashMap::new();
    let mut out: Vec<Run> = Vec::new();

    for (sequence, row) in rows.iter().enumerate() {
        if row.kind == "tick" && string_field(row, "outcome") == Some("started") {
            continue;
        }
        let entry = entries.get(&(row.repo.as_str(), row.id.as_str())).copied();
        let identity = (row.repo.clone(), row.id.clone());

        if row.kind == "dispatch" && !row.id.is_empty() {
            let mut run = base_run(row, entry, sequence, now);
            push_transition(&mut run, row);
            open.insert(identity, out.len());
            out.push(run);
            continue;
        }

        if matches!(row.kind, "finish" | "expired") && !row.id.is_empty() {
            if let Some(index) = open.remove(&identity) {
                push_transition(&mut out[index], row);
                finish_run(&mut out[index], row);
            } else {
                let mut run = base_run(row, entry, sequence, now);
                push_transition(&mut run, row);
                finish_run(&mut run, row);
                out.push(run);
            }
            continue;
        }

        let mut run = base_run(row, entry, sequence, now);
        push_transition(&mut run, row);
        finish_run(&mut run, row);
        out.push(run);
    }

    out
}

fn base_run(
    row: &Row,
    entry: Option<&crate::backlog::BacklogEntry>,
    sequence: usize,
    now: i64,
) -> Run {
    let work_kind = string_field(row, "work_kind")
        .map(str::to_string)
        .or_else(|| entry.map(|value| value.kind.label().to_string()))
        .unwrap_or_else(|| "system".to_string());
    let lane = string_field(row, "lane")
        .map(str::to_string)
        .or_else(|| {
            entry.map(|value| {
                crate::queue::kind::WorkClass::from_kind(value.kind)
                    .label()
                    .to_string()
            })
        })
        .unwrap_or_default();
    let started_at = row.at;
    let outcome = outcome(row);
    Run {
        key: format!(
            "{}/{}/{started_at}/{sequence}",
            if row.repo.is_empty() {
                "machine"
            } else {
                &row.repo
            },
            if row.id.is_empty() { row.kind } else { &row.id }
        ),
        started_at,
        ended_at: (row.kind != "dispatch").then_some(row.at),
        duration_secs: if row.kind == "dispatch" {
            now.saturating_sub(row.at)
        } else {
            0
        },
        repo: row.repo.clone(),
        id: row.id.clone(),
        work: string_field(row, "item")
            .map(str::to_string)
            .or_else(|| entry.map(|value| value.item.clone()))
            .unwrap_or_else(|| standalone_work(row)),
        work_kind,
        lane,
        exec: string_field(row, "exec")
            .map(str::to_string)
            .or_else(|| entry.map(|value| value.exec.label().to_string()))
            .unwrap_or_default(),
        node: string_field(row, "node")
            .or_else(|| string_field(row, "target"))
            .map(str::to_string)
            .unwrap_or_default(),
        harness: string_field(row, "harness")
            .map(str::to_string)
            .unwrap_or_default(),
        priority: integer_field(row, "priority").or_else(|| entry.map(|value| value.priority)),
        rationale: string_field(row, "rationale")
            .map(str::to_string)
            .unwrap_or_default(),
        task: entry.map(|value| value.task.clone()).unwrap_or_default(),
        entry_status: entry
            .map(|value| value.status.label().to_string())
            .unwrap_or_default(),
        summary: outcome_summary(row, &outcome),
        tone: row.tone,
        outcome,
        transitions: Vec::new(),
    }
}

fn push_transition(run: &mut Run, row: &Row) {
    run.transitions.push(Transition {
        at: row.at,
        kind: row.kind.to_string(),
        summary: row.summary.clone(),
        tone: row.tone,
        fields: row.fields.clone(),
    });
}

fn finish_run(run: &mut Run, row: &Row) {
    run.ended_at = Some(row.at);
    run.duration_secs = row.at.saturating_sub(run.started_at);
    run.outcome = outcome(row);
    run.summary = outcome_summary(row, &run.outcome);
    run.tone = row.tone;
}

fn outcome(row: &Row) -> String {
    match row.kind {
        "dispatch" => "in-flight".to_string(),
        "finish" => string_field(row, "outcome")
            .map(str::to_string)
            .unwrap_or_else(|| {
                if integer_field(row, "exit_code").unwrap_or_default() == 0 {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                }
            }),
        "expired" => "expired".to_string(),
        "reclaimed" => "reclaimed".to_string(),
        "skipped" => "skipped".to_string(),
        "tick" => string_field(row, "outcome")
            .unwrap_or("decision")
            .to_string(),
        other => other.to_string(),
    }
}

fn outcome_summary(row: &Row, outcome: &str) -> String {
    match outcome {
        "in-flight" => {
            if let Some(rationale) = string_field(row, "rationale") {
                format!("Dispatched and still running — {rationale}")
            } else {
                "Dispatched and still waiting for a terminal event.".to_string()
            }
        }
        "completed" => "Completed successfully.".to_string(),
        "failed" => {
            if let Some(error) = string_field(row, "error") {
                format!("Executor did not start — {error}")
            } else {
                format!(
                    "Executor exited with code {}.",
                    integer_field(row, "exit_code").unwrap_or_default()
                )
            }
        }
        "expired" => string_field(row, "detail")
            .or_else(|| string_field(row, "reason"))
            .unwrap_or("Its execution window passed before admission.")
            .to_string(),
        "held" | "skipped" => string_field(row, "reason")
            .unwrap_or(row.summary.as_str())
            .to_string(),
        "reclaimed" => "The executor disappeared and its lease was reclaimed.".to_string(),
        _ => row.summary.clone(),
    }
}

fn standalone_work(row: &Row) -> String {
    match row.kind {
        "tick" => "Dispatcher decision".to_string(),
        "skipped" => "Dispatcher tick".to_string(),
        "reclaimed" => "Lease reconciliation".to_string(),
        _ if !row.id.is_empty() => row.id.clone(),
        _ => row.summary.clone(),
    }
}

fn string_field<'a>(row: &'a Row, key: &str) -> Option<&'a str> {
    row.fields.get(key).and_then(Value::as_str)
}

fn integer_field(row: &Row, key: &str) -> Option<i64> {
    row.fields.get(key).and_then(Value::as_i64)
}

/// A pass's one-liner: what it decided, and what bound it if that says something the outcome did
/// not (`held — local-cap` earns its words; `empty — empty` does not).
fn tick_summary(outcome: &str, bind: &str, n: &impl Fn(&str) -> Option<i64>) -> String {
    if outcome == "empty" {
        return "nothing queued".to_string();
    }
    let held = if bind.is_empty() || bind == outcome {
        String::new()
    } else {
        format!(" — {bind}")
    };
    let started = n("started").unwrap_or_default();
    let pending = n("pending").unwrap_or_default();
    let deferred = n("deferred").unwrap_or_default();
    // `started 3 · 3 pending` reads as a contradiction, so say what the number is *of*; and
    // `started 3 of 3` already names the outcome, so do not repeat it.
    let lead = if outcome == "started" {
        String::new()
    } else {
        format!("{outcome}{held} · ")
    };
    let mut line = format!("{lead}started {started} of {pending} pending");
    if deferred > 0 {
        line.push_str(&format!(" · {deferred} held back"));
    }
    line
}

/// Read the machine's event log and fold it — the IO wrapper over [`rows`].
pub fn read(q: &Query) -> Vec<Row> {
    rows(
        &crate::events::read_events_at(&crate::events::log_path()),
        q,
    )
}
