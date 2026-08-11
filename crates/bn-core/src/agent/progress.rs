//! The #374 liveness probe: how much has the agent in a given worktree actually *done*.
//!
//! Split out of `supervise.rs` for the 1000-line ratchet, along a seam that file already marked
//! with a banner. It is a different question from the one supervision answers: [`super::supervise`]
//! enumerates the *fleet* and judges each agent's health, and stops reading a transcript once it
//! goes quiet. This reads one worktree's newest session **regardless** of quiet or finished —
//! because a *stalled* agent's log is untouched for far longer than [`super::parse::QUIET_SECS`],
//! and that is precisely the agent the orchestrator must measure to tell it apart from a
//! just-launched one that has produced no turn yet.
//!
//! Cumulative output tokens (AIC) and a last-activity stamp are the whole answer; what to do with
//! it lives in the caller.
use super::parse::{events_json, read_yaml, ts_of, ws_str};
use super::supervise::file_mtime;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// The cumulative progress of the newest harness session whose worktree is a given cwd — the #374
/// liveness probe the orchestrator uses to distinguish a *stalled* agent (worked, then died at an
/// idle prompt) from a *just-launched* one (no turn produced yet).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentProgress {
    pub harness: String,
    pub session_id: String,
    /// Cumulative output tokens burned (AIC, #309) — `0` when the agent produced no assistant turn.
    pub aic: i64,
    /// Epoch secs of the newest event (last sign of life), falling back to the log's mtime.
    pub last_activity_ts: i64,
}

/// [`agent_progress_from`] against the real `~/.claude/projects` + `~/.copilot/session-state`.
pub fn agent_progress_for_cwd(cwd: &Path) -> Option<AgentProgress> {
    let home = dirs::home_dir().unwrap_or_default();
    agent_progress_from(
        &home.join(".claude").join("projects"),
        &home.join(".copilot").join("session-state"),
        cwd,
    )
}

/// The newest Claude or Copilot session whose recorded cwd is `cwd`, with its cumulative output
/// tokens + last-activity ts — read regardless of quiet/finished (unlike [`supervise`]), so a
/// *stalled* agent (log untouched for > [`QUIET_SECS`]) is still measured. `None` when no session
/// targets `cwd`. The testable core: explicit roots, no `now`/`Config` needed.
pub fn agent_progress_from(
    claude_root: &Path,
    copilot_root: &Path,
    cwd: &Path,
) -> Option<AgentProgress> {
    let mut best: Option<AgentProgress> = None;
    let mut consider = |p: AgentProgress| {
        if best
            .as_ref()
            .map(|b| p.last_activity_ts > b.last_activity_ts)
            .unwrap_or(true)
        {
            best = Some(p);
        }
    };
    // Copilot: one directory per session (events.jsonl + workspace.yaml).
    if let Ok(dirs) = std::fs::read_dir(copilot_root) {
        for d in dirs.flatten() {
            let dir = d.path();
            if dir.is_dir() {
                if let Some(p) = copilot_progress(&dir, cwd) {
                    consider(p);
                }
            }
        }
    }
    // Claude: one transcript per session under <encoded-cwd>/<session>.jsonl.
    if let Ok(projects) = std::fs::read_dir(claude_root) {
        for proj in projects.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Some(p) = claude_progress(&path, cwd) {
                    consider(p);
                }
            }
        }
    }
    best
}

/// Whether two paths point at the same directory — literal match first (the common case: the
/// harness records the exact worktree path it was launched in), then canonicalized (tolerates a
/// `/private` symlink or trailing slash on macOS).
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Progress for a Copilot session dir, iff its recorded cwd is `cwd`. Sums each `assistant.message`'s
/// `outputTokens` (the live AIC signal) and takes the newest event ts as last-activity.
fn copilot_progress(dir: &Path, cwd: &Path) -> Option<AgentProgress> {
    let events = dir.join("events.jsonl");
    let evs = events_json(&events);
    if evs.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ws = read_yaml(&dir.join("workspace.yaml"));
    let ctx = evs
        .iter()
        .find(|v| typ(v) == "session.start")
        .and_then(|s| s.get("data"))
        .and_then(|d| d.get("context"))
        .cloned()
        .unwrap_or(Value::Null);
    let session_cwd = ws_str(&ws, "cwd")
        .or_else(|| ctx.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    if session_cwd.is_empty() || !same_dir(Path::new(&session_cwd), cwd) {
        return None;
    }
    let session_id =
        ws_str(&ws, "id").or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let last_activity_ts = evs
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(&events))
        .unwrap_or(0);
    let aic: i64 = evs
        .iter()
        .filter(|v| typ(v) == "assistant.message")
        .filter_map(|v| v.get("data").and_then(|d| d.get("outputTokens")))
        .filter_map(|v| v.as_i64())
        .sum();
    Some(AgentProgress {
        harness: "copilot".into(),
        session_id,
        aic,
        last_activity_ts,
    })
}

/// Progress for a Claude transcript, iff its recorded cwd is `cwd`. Sums each assistant turn's
/// `usage.output_tokens` and takes the newest event ts as last-activity.
fn claude_progress(path: &Path, cwd: &Path) -> Option<AgentProgress> {
    let lines = events_json(path);
    if lines.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let session_cwd = lines
        .iter()
        .find_map(|v| v.get("cwd").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default();
    if session_cwd.is_empty() || !same_dir(Path::new(&session_cwd), cwd) {
        return None;
    }
    let session_id = lines
        .iter()
        .find_map(|v| {
            v.get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let last_activity_ts = lines
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(path))
        .unwrap_or(0);
    let aic: i64 = lines
        .iter()
        .filter(|v| typ(v) == "assistant")
        .filter_map(|v| {
            v.get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_i64())
        })
        .sum();
    Some(AgentProgress {
        harness: "claude".into(),
        session_id,
        aic,
        last_activity_ts,
    })
}
