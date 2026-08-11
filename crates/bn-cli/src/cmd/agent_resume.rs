//! Resume-or-fresh: is there a harness session in this worktree worth resuming, and did resuming it
//! actually work?
//!
//! Split out of `cmd/agent.rs` for the 1000-line ratchet. One question, asked twice for two
//! harnesses that answer it in completely different places — Copilot keeps a session index this can
//! query, Claude leaves transcripts on disk under an encoded cwd — and then asked a third time
//! *after* the attempt, because Copilot reports a session that no longer exists as an ordinary
//! non-zero exit carrying a message rather than as a distinguishable status.
//!
//! Keeping the "is there one?" and the "did it take?" halves together is the point: they are the
//! two ends of a single fallback, and the launcher's only correct move when the second says no is
//! to start fresh, exactly once.
use crate::*;

const COPILOT_MISSING_SESSION_ERROR: &str = "No session, task, or name matched";

/// Run a Copilot resume attempt while teeing stderr to the pane and retaining enough output to
/// distinguish a missing session from unrelated failures. Only resume attempts use this wrapper;
/// fresh launches still `exec` directly so there can be at most one fallback.
pub(crate) fn run_copilot_resume(
    bin: &str,
    args: &[String],
) -> std::io::Result<(std::process::ExitStatus, String)> {
    use std::io::{Read, Write};
    use std::process::Stdio;

    const CAPTURE_LIMIT: usize = 64 * 1024;

    let mut child = crate::retry::io(&format!("bn agent launch: spawn {bin}"), || {
        std::process::Command::new(bin)
            .args(args)
            .stderr(Stdio::piped())
            .spawn()
    })?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("Copilot resume stderr pipe was not available"))?;
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stderr.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            std::io::stderr().write_all(&chunk[..read])?;
            let remaining = CAPTURE_LIMIT.saturating_sub(captured.len());
            captured.extend_from_slice(&chunk[..read.min(remaining)]);
        }
        Ok(captured)
    });

    let status = child.wait()?;
    let captured = reader
        .join()
        .map_err(|_| std::io::Error::other("Copilot resume stderr reader panicked"))??;
    Ok((status, String::from_utf8_lossy(&captured).into_owned()))
}

fn copilot_session_missing(stderr: &str) -> bool {
    stderr.contains(COPILOT_MISSING_SESSION_ERROR)
}

pub(crate) fn should_retry_copilot_resume(status: std::process::ExitStatus, stderr: &str) -> bool {
    !status.success() && copilot_session_missing(stderr)
}

/// The most-recent Copilot session id recorded for `cwd`, by scanning
/// `~/.copilot/session-state/*/workspace.yaml`. IO wrapper over [`pick_copilot_resume`].
pub(crate) fn copilot_resume_id(cwd: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home)
        .join(".copilot")
        .join("session-state");
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let wy = entry.path().join("workspace.yaml");
        let Ok(body) = std::fs::read_to_string(&wy) else {
            continue;
        };
        let field = |key: &str| {
            body.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        rows.push((field("cwd:"), field("updated_at:"), field("id:")));
    }
    let since = crate::wt_pool::slot_lease_started_at(std::path::Path::new(cwd))
        .and_then(bn_core::util::epoch_to_rfc3339_utc);
    pick_copilot_resume(
        rows.iter()
            .map(|(c, t, i)| (c.as_str(), t.as_str(), i.as_str())),
        cwd,
        since.as_deref(),
    )
}

/// Whether this worktree has a Claude transcript (`~/.claude/projects/<proj>/*.jsonl`) belonging to
/// its **current tenant** — the thing `--continue` would resume.
///
/// Transcripts are filed by directory, and a pool slot is one directory that many branches pass
/// through, so "this path has transcripts" is not the same question as "I have transcripts". In a
/// slot, only history from the current lease onward counts ([`wt_pool::slot_lease_started_at`]);
/// otherwise typing `claude` in a returned, idle slot silently resumes the previous tenant's
/// session. A per-branch worktree has no lease and keeps its whole history.
pub(crate) fn claude_has_transcripts(cwd: &str) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let proj = claude_project_dirname(cwd);
    let dir = std::path::Path::new(&home)
        .join(".claude")
        .join("projects")
        .join(proj);
    let since = crate::wt_pool::slot_lease_started_at(std::path::Path::new(cwd));
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .any(|e| transcript_is_current(&e, since))
        })
        .unwrap_or(false)
}

/// Whether a transcript file belongs to the tenancy that began at `since` — always true when there
/// is no lease boundary to respect. Modification time is the signal: a session touched since this
/// slot was leased is this tenant's, and one that stopped before it belongs to whoever had the
/// directory before.
fn transcript_is_current(entry: &std::fs::DirEntry, since: Option<i64>) -> bool {
    let Some(since) = since else {
        return true;
    };
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|age| age.as_secs() as i64 >= since)
}

pub(crate) fn claude_has_session(cwd: &str, session_id: &str) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    std::path::Path::new(&home)
        .join(".claude")
        .join("projects")
        .join(claude_project_dirname(cwd))
        .join(format!("{session_id}.jsonl"))
        .is_file()
}

