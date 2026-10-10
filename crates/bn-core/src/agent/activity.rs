//! Session activity — fold a harness transcript into a readable timeline.
//!
//! The web dashboard used to supervise an agent by mirroring its tmux pane into xterm.js. That
//! never worked: the agent's real pane is ~200 columns of TUI chrome, the browser grid is half
//! that, so every line wrapped and the box rules doubled — and an agent whose window has already
//! closed had nothing to show at all, even though its whole session sits on disk.
//!
//! This reads the same transcripts [`super::supervise`] already parses for the fleet, but keeps
//! the *content* instead of the counters:
//!   - Claude: `~/.claude/projects/<encoded-cwd>/<session>.jsonl` — `user` / `assistant` lines,
//!     the latter carrying `text`, `thinking`, and `tool_use` content blocks.
//!   - Copilot: `~/.copilot/session-state/<id>/events.jsonl` — `user.message`,
//!     `assistant.message`, and `tool.execution_start` events.
//!
//! Both fold into one shape ([`ActivityEntry`]): what you said, what the agent said, and — because
//! a 240-turn session is mostly tool traffic — consecutive tool calls collapsed into a single
//! summary row ("14 tool calls · Read ×6, Edit ×5, Bash ×3") that the UI can expand.
//!
//! The folds are pure functions over parsed JSON lines, so they're unit-testable without a harness
//! ever having run on the machine.

use super::parse::{events_json, ts_of};
use super::switch::find_claude_transcript;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// How many tool groups a summary names before it elides the rest ("… +3 more").
const SUMMARY_GROUPS: usize = 4;
/// Cap on a rendered tool target, so one `Bash` line with a 900-char command can't dominate.
const TARGET_MAX: usize = 120;

/// One tool invocation, reduced to what's worth reading in a feed: the tool's name and the single
/// most identifying argument (a path, a command, a pattern).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCall {
    /// The harness's tool name, normalized to its display casing ("Read", "Bash", "Edit").
    pub name: String,
    /// The identifying argument — a file path, shell command, or search pattern. Empty when the
    /// tool takes nothing worth showing.
    pub target: String,
}

/// What an entry represents. Serialized lowercase so the web client can switch on it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A prompt you typed.
    User,
    /// Prose the agent wrote back.
    Agent,
    /// A run of consecutive tool calls, collapsed.
    Tools,
}

/// One row of the timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivityEntry {
    pub kind: EntryKind,
    /// Epoch seconds of the underlying event (0 when the line carried no timestamp).
    pub ts: i64,
    /// Markdown prose for [`EntryKind::User`] / [`EntryKind::Agent`]; empty for a tool run.
    pub text: String,
    /// The individual calls behind a [`EntryKind::Tools`] row; empty otherwise.
    pub tools: Vec<ToolCall>,
    /// A one-line description of a tool run ("14 tool calls · Read ×6, Bash ×3"); empty otherwise.
    pub summary: String,
}

/// A session's readable timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    pub session_id: String,
    /// "claude" | "copilot".
    pub harness: String,
    /// The trailing entries, oldest first — at most the requested limit.
    pub entries: Vec<ActivityEntry>,
    /// How many entries the session has in total, so the UI can say "showing the last 40 of 312".
    pub total: usize,
}

/// The timeline for `session_id`, looked up under the real harness roots. `limit` keeps the
/// trailing N entries (the tail is what you're supervising). `None` when no transcript exists for
/// that id under either harness.
pub fn activity_for(session_id: &str, limit: usize) -> Option<Activity> {
    let home = dirs::home_dir()?;
    activity_from(
        &home.join(".claude").join("projects"),
        &home.join(".copilot").join("session-state"),
        session_id,
        limit,
    )
}

/// [`activity_for`] with explicit harness roots — the testable core. Claude is checked first (a
/// session id is a uuid, so the two namespaces don't collide in practice).
pub fn activity_from(
    claude_root: &Path,
    copilot_root: &Path,
    session_id: &str,
    limit: usize,
) -> Option<Activity> {
    if let Some(path) = find_claude_transcript(claude_root, session_id) {
        let all = fold_claude(&events_json(&path));
        return Some(finish(session_id, "claude", all, limit));
    }
    let events = copilot_root.join(session_id).join("events.jsonl");
    if events.is_file() {
        let all = fold_copilot(&events_json(&events));
        return Some(finish(session_id, "copilot", all, limit));
    }
    None
}

/// Trim a folded timeline to its trailing `limit` entries, remembering the pre-trim total.
fn finish(session_id: &str, harness: &str, all: Vec<ActivityEntry>, limit: usize) -> Activity {
    let total = all.len();
    let start = total.saturating_sub(limit.max(1));
    Activity {
        session_id: session_id.to_string(),
        harness: harness.to_string(),
        entries: all[start..].to_vec(),
        total,
    }
}

/// Accumulates entries, folding consecutive tool calls into one row. The whole point of the
/// timeline: a session is 90% tool traffic, and reading it call-by-call buries the prose.
#[derive(Default)]
struct Timeline {
    entries: Vec<ActivityEntry>,
    pending: Vec<ToolCall>,
    pending_ts: i64,
}

impl Timeline {
    /// Queue a tool call into the current run (starting one if this is the first).
    fn tool(&mut self, ts: i64, call: ToolCall) {
        if self.pending.is_empty() {
            self.pending_ts = ts;
        }
        self.pending.push(call);
    }

    /// Close the open tool run, if any, into a single collapsed entry.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let tools = std::mem::take(&mut self.pending);
        self.entries.push(ActivityEntry {
            kind: EntryKind::Tools,
            ts: self.pending_ts,
            text: String::new(),
            summary: summarize(&tools),
            tools,
        });
    }

    /// Append a prose entry, closing any open tool run first so ordering holds. Blank prose is
    /// dropped — an assistant line that carried only `thinking` isn't a message.
    fn prose(&mut self, kind: EntryKind, ts: i64, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.flush();
        self.entries.push(ActivityEntry {
            kind,
            ts,
            text,
            tools: Vec::new(),
            summary: String::new(),
        });
    }

    fn done(mut self) -> Vec<ActivityEntry> {
        self.flush();
        self.entries
    }
}

/// Fold Claude transcript lines into a timeline. `user` lines carrying a plain prompt become user
/// entries (tool results and task notifications are skipped — they're machine traffic, not turns);
/// `assistant` lines contribute their `text` blocks as prose and their `tool_use` blocks as calls.
/// `thinking` is deliberately dropped: it's the agent's scratch, not what it told you.
pub fn fold_claude(lines: &[Value]) -> Vec<ActivityEntry> {
    let mut tl = Timeline::default();
    for line in lines {
        let ts = ts_of(line).unwrap_or(0);
        match line.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(text) = claude_prompt(line) {
                    tl.prose(EntryKind::User, ts, text);
                }
            }
            Some("assistant") => {
                let Some(blocks) = line
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                tl.prose(EntryKind::Agent, ts, t.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let input = b.get("input").unwrap_or(&Value::Null);
                            tl.tool(
                                ts,
                                ToolCall {
                                    name: display_name(name),
                                    target: tool_target(name, input),
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    tl.done()
}

/// The prompt text of a Claude `user` line, or `None` when the line isn't something a person
/// actually typed.
///
/// Far more wears the `user` type than turns you took: tool results, task notifications, and — the
/// one that matters most here — skill and slash-command bodies, which the harness splices in as a
/// `text` block so the agent reads them as instructions. Rendering those as "you said" put a wall
/// of tooling documentation in the feed above the prompt that triggered it.
///
/// So the bar is provenance, not shape. A turn counts when the harness tagged it `origin.kind:
/// "human"`, or — for transcripts written before that tag existed, including seeded dispatch
/// launches — when it's bare string content with no `origin` at all. An array-content line without
/// a human origin is always machine traffic.
fn claude_prompt(line: &Value) -> Option<String> {
    let origin = line
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    let content = line.get("message")?.get("content")?;

    let raw = match (origin, content.as_str()) {
        // Tagged human: take whatever it carries, string or first text block (an attachment turn).
        (Some("human"), Some(s)) => s.to_string(),
        (Some("human"), None) => content
            .as_array()?
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|b| b.get("text").and_then(Value::as_str))
            .map(str::to_string)?,
        // Untagged plain string — a pre-`origin` transcript's typed or seeded prompt.
        (None, Some(s)) => s.to_string(),
        _ => return None,
    };

    let cleaned = strip_system_reminders(&raw);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Drop `<system-reminder>…</system-reminder>` blocks the harness splices into a prompt. They're
/// context for the agent, noise in a feed the human is reading. Unterminated tags drop the tail.
fn strip_system_reminders(s: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Fold Copilot session events into a timeline. Copilot is already event-shaped, so the mapping is
/// direct: `user.message` / `assistant.message` carry `data.content`, and each
/// `tool.execution_start` is one call.
pub fn fold_copilot(events: &[Value]) -> Vec<ActivityEntry> {
    let mut tl = Timeline::default();
    for ev in events {
        let ts = ts_of(ev).unwrap_or(0);
        let data = ev.get("data").unwrap_or(&Value::Null);
        match ev.get("type").and_then(Value::as_str) {
            Some("user.message") => {
                if let Some(c) = data.get("content").and_then(Value::as_str) {
                    tl.prose(EntryKind::User, ts, strip_system_reminders(c));
                }
            }
            Some("assistant.message") => {
                if let Some(c) = data.get("content").and_then(Value::as_str) {
                    tl.prose(EntryKind::Agent, ts, c.to_string());
                }
            }
            Some("tool.execution_start") => {
                let name = data
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let args = data.get("arguments").unwrap_or(&Value::Null);
                tl.tool(
                    ts,
                    ToolCall {
                        name: display_name(name),
                        target: tool_target(name, args),
                    },
                );
            }
            _ => {}
        }
    }
    tl.done()
}

/// Normalize a harness tool name for display: Copilot writes `bash` / `edit` where Claude writes
/// `Bash` / `Edit`, and MCP tools arrive as `mcp__server__tool`. Reduce both so the same tool reads
/// the same in the feed regardless of which harness ran it.
fn display_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__") {
        // `mcp__claude-in-chrome__tabs_context_mcp` → `tabs context` — the server prefix is noise
        // in a summary, and snake_case reads as a sentence once the underscores go. The trailing
        // `_mcp` some servers append is dropped too; every tool here is an MCP tool.
        let leaf = rest.rsplit("__").next().unwrap_or(rest);
        let leaf = leaf.strip_suffix("_mcp").unwrap_or(leaf);
        return capitalize(&leaf.replace('_', " "));
    }
    match name {
        "bash" => "Bash".to_string(),
        "edit" => "Edit".to_string(),
        "read" | "view" => "Read".to_string(),
        "create" | "write" => "Write".to_string(),
        "ask_user" => "AskUser".to_string(),
        other => capitalize(other),
    }
}

/// Upper-case the first character, leaving the rest alone (so `navigate` → `Navigate` but `Bash`
/// and `WebFetch` survive untouched).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The single most identifying argument of a tool call. Tools are keyed by the field that tells you
/// *what* it acted on; anything unrecognized falls back to the first short string argument, so a new
/// tool still shows something useful instead of a bare name.
fn tool_target(name: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let picked = match name {
        "Bash" | "bash" => field("description").or_else(|| field("command")),
        "Read" | "read" | "view" | "Write" | "write" | "create" | "Edit" | "edit"
        | "NotebookEdit" => field("file_path").or_else(|| field("path")),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" | "navigate" => field("url"),
        "WebSearch" => field("query"),
        "Task" | "Agent" | "TaskCreate" => field("description").or_else(|| field("subject")),
        "TaskUpdate" => field("subject").or_else(|| field("status")),
        "Skill" => field("skill"),
        "ToolSearch" => field("query"),
        "AskUserQuestion" | "ask_user" => field("question"),
        _ => None,
    }
    .map(str::to_string)
    .or_else(|| first_string(input));
    picked
        .map(|s| clip(&one_line(&s), TARGET_MAX))
        .unwrap_or_default()
}

/// The first short string value in an argument object — the generic fallback for a tool we don't
/// know. Long values are skipped: a file body or a diff isn't an identifier.
fn first_string(input: &Value) -> Option<String> {
    input
        .as_object()?
        .values()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty() && s.len() <= 200)
        .map(str::to_string)
}

/// Collapse whitespace (including newlines) to single spaces — a multi-line shell command has to
/// fit on one feed row.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` characters with an ellipsis, counting chars (not bytes) so a multibyte target
/// never splits mid-codepoint.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// A one-line description of a collapsed tool run: the total, then the tool names by frequency
/// ("14 tool calls · Read ×6, Edit ×5, Bash ×3"). Ties break on first appearance, so the summary is
/// stable for a given run.
fn summarize(tools: &[ToolCall]) -> String {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for t in tools {
        match groups.iter_mut().find(|(n, _)| *n == t.name) {
            Some((_, count)) => *count += 1,
            None => groups.push((t.name.clone(), 1)),
        }
    }
    // `sort_by_key` is stable, so equal counts keep first-appearance order.
    groups.sort_by_key(|(_, c)| std::cmp::Reverse(*c));

    let shown: Vec<String> = groups
        .iter()
        .take(SUMMARY_GROUPS)
        .map(|(n, c)| {
            if *c == 1 {
                n.clone()
            } else {
                format!("{n} ×{c}")
            }
        })
        .collect();
    let mut detail = shown.join(", ");
    if groups.len() > SUMMARY_GROUPS {
        detail.push_str(&format!(", +{} more", groups.len() - SUMMARY_GROUPS));
    }

    let n = tools.len();
    let noun = if n == 1 { "tool call" } else { "tool calls" };
    format!("{n} {noun} · {detail}")
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
