//! Agent session telemetry — fold the local event streams that Claude Code and the GitHub
//! Copilot CLI each write per session into bn's own event log, as `agent_session` events.
//!
//! Both harnesses already record everything we want, on disk, per machine:
//!   - Copilot: `~/.copilot/session-state/<id>/events.jsonl` (+ `workspace.yaml`). A
//!     `session.shutdown` event carries the whole aggregate — cost (`totalNanoAiu`), per-model
//!     token usage, API duration, and `codeChanges` (lines/files). One clean record per session.
//!   - Claude: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. No shutdown record, so we
//!     sum `.message.usage` across `assistant` lines; each line carries `.gitBranch` + `.cwd`.
//!
//! The north star is the same as the rest of bn: read what's already there, don't re-fetch. The
//! poller ([`ingest`]) is idempotent by `session_id` — a session already in the metrics log is
//! never re-emitted, so re-runs (and cross-machine sync) are no-ops. Emitted events land in the
//! **synced** metrics log (like `pr_opened`), feeding both the whole-person scorecard and the
//! per-branch recall view ([`sessions_for`]).
//!
//! Split into focused submodules; the public types and functions are re-exported
//! so callers keep using `agent::ingest`, `agent::supervise`, … as before.

mod digest;
mod lease;
mod parse;
mod registry;
mod session;
mod supervise;
mod switch;

pub use digest::*;
pub use lease::*;
pub use registry::*;
pub use session::*;
pub use supervise::*;
pub use switch::*;

#[cfg(test)]
use crate::resolver::Config;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
/// A Config whose metrics log + worktree root live under `base` (an isolated temp dir). Built
/// directly — no env mutation — so it never races other tests (mirrors `doctor.rs`).
fn test_cfg(base: &Path) -> Config {
    Config {
        personal_root: base.join("notes"),
        work_root: base.join("notes"),
        worktree_dir: base.join("worktree"),
        hostname: "testhost".into(),
        ..Default::default()
    }
}

#[cfg(test)]
fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[cfg(test)]
/// A session ending at `ended` with the given diff shape (everything else nominal).
fn session(
    id: &str,
    harness: &str,
    started: i64,
    ended: i64,
    added: i64,
    files: i64,
) -> AgentSession {
    AgentSession {
        session_id: id.into(),
        harness: harness.into(),
        repo: "bn".into(),
        branch: "feat/x".into(),
        started_ts: started,
        ended_ts: ended,
        turns: 5,
        tool_calls: 12,
        permission_prompts: 0,
        models: vec!["claude-opus-4-8".into()],
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cost_nanoaiu: None,
        lines_added: added,
        lines_removed: 0,
        files_modified: files,
        outcome: "completed".into(),
        work_class: crate::cost::WorkClass::Unattributed,
    }
}
