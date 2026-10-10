//! Fixture tests for the transcript folds behind the session activity feed.

use super::*;
use serde_json::json;

const T0: &str = "2026-01-01T00:00:00Z";
const T1: &str = "2026-01-01T00:00:10Z";
const T0_SECS: i64 = 1_767_225_600;

fn claude_assistant(ts: &str, blocks: Value) -> Value {
    json!({"type": "assistant", "timestamp": ts, "message": {"content": blocks}})
}

fn kinds(entries: &[ActivityEntry]) -> Vec<EntryKind> {
    entries.iter().map(|e| e.kind).collect()
}

#[test]
fn claude_folds_prompts_prose_and_collapses_tool_runs() {
    let lines = vec![
        json!({"type": "user", "timestamp": T0, "message": {"content": "Fix the flaky test"}}),
        claude_assistant(
            T1,
            json!([
                {"type": "thinking", "thinking": "scratch the user never sees"},
                {"type": "text", "text": "Looking at it now."},
                {"type": "tool_use", "name": "Read", "input": {"file_path": "/src/a.rs"}},
                {"type": "tool_use", "name": "Read", "input": {"file_path": "/src/b.rs"}},
            ]),
        ),
        // A tool result wears the `user` type but is machine traffic.
        json!({"type": "user", "timestamp": T1, "message": {"content": [
            {"type": "tool_result", "content": "fn a() {}"}
        ]}}),
        claude_assistant(
            T1,
            json!([
                {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test", "description": "Run tests"}},
                {"type": "text", "text": "Fixed."},
            ]),
        ),
    ];

    let entries = fold_claude(&lines);

    assert_eq!(
        kinds(&entries),
        [
            EntryKind::User,
            EntryKind::Agent,
            EntryKind::Tools,
            EntryKind::Agent
        ]
    );
    assert_eq!(entries[0].text, "Fix the flaky test");
    assert_eq!(entries[0].ts, T0_SECS);
    assert_eq!(entries[1].text, "Looking at it now.");
    let run = &entries[2];
    assert_eq!(run.summary, "3 tool calls · Read ×2, Bash");
    let targets: Vec<_> = run.tools.iter().map(|t| t.target.as_str()).collect();
    assert_eq!(targets, ["/src/a.rs", "/src/b.rs", "Run tests"]);
    assert_eq!(run.ts, T0_SECS + 10);
    assert_eq!(entries[3].text, "Fixed.");
}

#[test]
fn claude_prompts_keep_only_what_a_human_typed() {
    let lines = vec![
        // Tagged human, array content: the first text block counts.
        json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": [
            {"type": "image"}, {"type": "text", "text": "see screenshot"}
        ]}}),
        // Untagged array content (a spliced skill body) is machine traffic.
        json!({"type": "user", "message": {"content": [
            {"type": "text", "text": "# Skill instructions"}
        ]}}),
        // A non-human origin is never a prompt.
        json!({"type": "user", "origin": {"kind": "task-notification"}, "message": {"content": "done"}}),
        // System reminders are stripped; a prompt that was only a reminder disappears.
        json!({"type": "user", "message": {"content": "<system-reminder>ctx</system-reminder>  ship it"}}),
        json!({"type": "user", "message": {"content": "<system-reminder>only context</system-reminder>"}}),
        json!({"type": "user", "message": {"content": "keep <system-reminder>unterminated tail"}}),
    ];

    let texts: Vec<_> = fold_claude(&lines).into_iter().map(|e| e.text).collect();

    assert_eq!(texts, ["see screenshot", "ship it", "keep"]);
}

#[test]
fn claude_skips_malformed_and_unknown_lines() {
    let lines = vec![
        json!({"type": "assistant"}),
        json!({"type": "summary", "summary": "x"}),
        claude_assistant(T0, json!([{"type": "text", "text": "   "}])),
        json!({"type": "user", "message": {"content": "hi"}}),
    ];

    let entries = fold_claude(&lines);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].ts, 0, "a line without a timestamp reads as 0");
}

#[test]
fn claude_tool_names_and_targets_are_normalized() {
    let long = "x".repeat(300);
    let lines = vec![claude_assistant(
        T0,
        json!([
            {"type": "tool_use", "name": "mcp__claude-in-chrome__tabs_context_mcp", "input": {"tabId": "7"}},
            {"type": "tool_use", "name": "Grep", "input": {"pattern": "fn main"}},
            {"type": "tool_use", "name": "Bash", "input": {"command": "cargo\n  build"}},
            {"type": "tool_use", "name": "Bash", "input": {"command": long}},
        ]),
    )];

    let tools = &fold_claude(&lines)[0].tools;

    assert_eq!(tools[0].name, "Tabs context");
    assert_eq!(tools[0].target, "7");
    assert_eq!(tools[1].target, "fn main");
    assert_eq!(tools[2].target, "cargo build");
    assert_eq!(tools[3].target.chars().count(), TARGET_MAX + 1);
    assert!(tools[3].target.ends_with('…'));
}

#[test]
fn a_tool_run_names_four_groups_then_elides() {
    let blocks: Vec<Value> = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "alpha",
    ]
    .iter()
    .map(|n| json!({"type": "tool_use", "name": n, "input": {}}))
    .collect();

    let entries = fold_claude(&[claude_assistant(T0, Value::Array(blocks))]);

    assert_eq!(
        entries[0].summary,
        "7 tool calls · Alpha ×2, Beta, Gamma, Delta, +2 more"
    );
}

#[test]
fn copilot_folds_events_into_the_same_timeline() {
    let events = vec![
        json!({"type": "session.start", "timestamp": T0, "data": {}}),
        json!({"type": "user.message", "timestamp": T0, "data": {
            "content": "<system-reminder>ctx</system-reminder>Add a flag"
        }}),
        json!({"type": "tool.execution_start", "timestamp": T1, "data": {
            "toolName": "view", "arguments": {"path": "/src/main.rs"}
        }}),
        json!({"type": "tool.execution_start", "timestamp": T1, "data": {
            "toolName": "edit", "arguments": {"path": "/src/main.rs"}
        }}),
        json!({"type": "tool.execution_start", "timestamp": T1, "data": {
            "toolName": "bash", "arguments": {"command": "cargo check"}
        }}),
        json!({"type": "assistant.message", "timestamp": T1, "data": {"content": ""}}),
        json!({"type": "assistant.message", "timestamp": T1, "data": {"content": "Added --flag."}}),
        json!({"type": "tool.execution_start", "timestamp": T1, "data": {}}),
    ];

    let entries = fold_copilot(&events);

    assert_eq!(
        kinds(&entries),
        [
            EntryKind::User,
            EntryKind::Tools,
            EntryKind::Agent,
            EntryKind::Tools
        ]
    );
    assert_eq!(entries[0].text, "Add a flag");
    let names: Vec<_> = entries[1].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Read", "Edit", "Bash"]);
    assert_eq!(entries[1].tools[2].target, "cargo check");
    assert_eq!(entries[1].summary, "3 tool calls · Read, Edit, Bash");
    assert_eq!(entries[2].text, "Added --flag.");
    assert_eq!(entries[3].summary, "1 tool call · Tool");
}

#[test]
fn an_empty_transcript_folds_to_nothing() {
    assert!(fold_claude(&[]).is_empty());
    assert!(fold_copilot(&[]).is_empty());
}
