use super::parse::events_json;
use super::session::{sessions_for, AgentSession};
use super::supervise::LiveAgent;
use crate::resolver::Config;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

// --- switch: capture the outgoing session for a cross-harness handoff -----------------------
//
// `bn switch <harness>` (the CLI orchestrator) leaves the current harness for another mid-task —
// e.g. Claude hit a usage limit and the work should continue in Copilot. The continuity artifact
// is the handoff packet (composed from the note); switch's job here is to summarize the session
// being abandoned so the incoming harness knows what it's picking up. This owns the mining (per
// the #96 boundary — handoff never re-mines): it reads the live fleet / recorded sessions and
// drafts the one-line narrative switch feeds into the handoff. It writes nothing.

/// The session `bn switch` is leaving: a still-running one (preferred — it's the session you're
/// abandoning) or, if none is live, the most recently finished one. Serializes into the switch
/// `--json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchOutgoing {
    /// "claude" | "copilot" — the harness that ran it.
    pub harness: String,
    pub session_id: String,
    pub turns: i64,
    pub tool_calls: i64,
    /// Seconds since its last recorded activity (0 when unknown).
    pub idle_secs: i64,
    /// True when the session is still running (read from the live fleet, not yet finished).
    pub live: bool,
}

impl SwitchOutgoing {
    /// From a live fleet entry — the session still running on the branch.
    pub fn from_live(a: &LiveAgent) -> SwitchOutgoing {
        SwitchOutgoing {
            harness: a.harness.clone(),
            session_id: a.session_id.clone(),
            turns: a.turns,
            tool_calls: a.tool_calls,
            idle_secs: a.idle_secs.max(0),
            live: true,
        }
    }

    /// From a recorded (finished) session; `now` dates its idle gap.
    pub fn from_session(s: &AgentSession, now: i64) -> SwitchOutgoing {
        SwitchOutgoing {
            harness: s.harness.clone(),
            session_id: s.session_id.clone(),
            turns: s.turns,
            tool_calls: s.tool_calls,
            idle_secs: (now - s.ended_ts).max(0),
            live: false,
        }
    }
}

/// The most recent recorded session on a branch (newest-first via [`sessions_for`]), or `None` —
/// the finished-session fallback when nothing is live at switch time.
pub fn latest_session_for(cfg: &Config, repo: &str, branch: &str) -> Option<AgentSession> {
    sessions_for(cfg, repo, branch).into_iter().next()
}

/// The handoff narrative `bn switch` writes when leaving for `to`, summarizing the outgoing
/// session so the incoming harness lands oriented. Pure: no I/O. `outgoing` is `None` when no
/// The working-tree change summary captured at switch time — the concrete WIP the outgoing
/// session leaves behind. Read from git in the worktree (accurate regardless of harness, and of
/// whether the session itself logged a diff), so the handoff records *what was done*, not just a
/// turn/tool count. Serializes into the switch `--json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkingChanges {
    pub files: usize,
    pub insertions: i64,
    pub deletions: i64,
    /// Up to a few changed file names, for a recognizable one-line summary.
    pub sample: Vec<String>,
}

/// The `## Progress` bullet folded into the note for the WIP at switch time — what the outgoing
/// session actually changed, so the handoff (composed from the note) carries it forward.
pub fn wip_progress_line(wc: &WorkingChanges) -> String {
    let sample = if wc.sample.is_empty() {
        String::new()
    } else {
        format!(" ({})", wc.sample.join(", "))
    };
    format!(
        "WIP at harness switch: +{}/-{} across {} file{}{sample}",
        wc.insertions,
        wc.deletions,
        wc.files,
        plural(wc.files as i64),
    )
}

/// The handoff narrative `bn switch` writes when leaving for `to`. The first sentence summarizes
/// the outgoing session; `wip` (the uncommitted diff) and `last_request` (the session's final user
/// prompt) append the "what was done / what was asked" context so the incoming harness lands
/// oriented. Pure: no I/O. `outgoing` is `None` on a cold start into `to`.
pub fn switch_narrative(
    to: &str,
    outgoing: Option<&SwitchOutgoing>,
    wip: Option<&WorkingChanges>,
    last_request: Option<&str>,
) -> String {
    let mut out = match outgoing {
        Some(o) => {
            let sid = short_id(&o.session_id);
            let when = humanize_secs(o.idle_secs);
            if o.live {
                format!(
                    "Switched to {to} mid-task, leaving a live {} session ({sid}, {} turn{}, {} tool call{}, idle {when}).",
                    o.harness,
                    o.turns,
                    plural(o.turns),
                    o.tool_calls,
                    plural(o.tool_calls),
                )
            } else {
                format!(
                    "Switched to {to} from a prior {} session ({sid}, {} turn{}, {} tool call{}, last active {when} ago).",
                    o.harness,
                    o.turns,
                    plural(o.turns),
                    o.tool_calls,
                    plural(o.tool_calls),
                )
            }
        }
        None => format!(
            "Switched to {to}. No prior agent session was found on this branch — continue from the note's open work."
        ),
    };
    if let Some(wc) = wip {
        out.push_str(&format!(
            " Uncommitted WIP: +{}/-{} across {} file{}.",
            wc.insertions,
            wc.deletions,
            wc.files,
            plural(wc.files as i64),
        ));
    }
    if let Some(req) = last_request.map(str::trim).filter(|r| !r.is_empty()) {
        out.push_str(&format!(" Last request: \"{req}\"."));
    }
    out.push_str(" Continue the priority queue below.");
    out
}

/// The last user prompt from the outgoing session's transcript — the "what was I asking for"
/// context, folded into the switch narrative. Best-effort: `None` when the transcript is missing
/// or carries no readable user turn. Single-line and truncated for the one-line narrative.
pub fn last_request_for(session_id: &str, harness: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let raw = match harness {
        // Copilot: one dir per session; the last `user.message` event carries the raw prompt.
        "copilot" => {
            let path = home
                .join(".copilot")
                .join("session-state")
                .join(session_id)
                .join("events.jsonl");
            events_json(&path)
                .iter()
                .rev()
                .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("user.message"))
                .and_then(|v| {
                    v.get("data")
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                        .map(String::from)
                })
        }
        // Claude: one transcript file per session under an encoded-cwd dir; the last `user` line.
        "claude" => {
            let path = find_claude_transcript(&home.join(".claude").join("projects"), session_id)?;
            events_json(&path)
                .iter()
                .rev()
                .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("user"))
                .find_map(|v| {
                    v.get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(user_text)
                })
        }
        _ => None,
    }?;
    Some(truncate_prompt(&raw, 200))
}

/// Locate a Claude transcript `<session>.jsonl` under `~/.claude/projects/<encoded-cwd>/`.
pub(super) fn find_claude_transcript(
    projects: &Path,
    session_id: &str,
) -> Option<std::path::PathBuf> {
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&file))
        .find(|p| p.exists())
}

/// The user-typed text of a Claude `message.content`: the string form, or the first `text` block
/// of the array form (skipping tool_result blocks). `None` when it carries no plain text.
fn user_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    content
        .as_array()?
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .and_then(|b| b.get("text").and_then(|t| t.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Collapse to a single trimmed line and cap at `max` chars ("…" when clipped) — for the narrative.
fn truncate_prompt(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > max {
        let cut: String = one_line.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    } else {
        one_line
    }
}

/// First 8 chars of a session UUID — enough to recognize it in `bn agent` / harness logs.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The plural suffix for a count: "" for 1, "s" otherwise.
fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// A coarse human duration for the narrative ("<1m" / "3m" / "2h" / "5d"). Bucketed, not exact —
/// orientation, not a stopwatch.
fn humanize_secs(s: i64) -> String {
    let s = s.max(0);
    if s < 60 {
        "<1m".to_string()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}
