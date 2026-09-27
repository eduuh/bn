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

mod activity;
mod approval;
mod channel;
mod digest;
mod lease;
mod live_line;
mod parse;
mod pending;
mod prompt;
mod registry;
mod session;
mod stuck;
mod supervise;
mod switch;
mod transport;

pub use activity::*;
pub use approval::*;
pub use channel::*;
pub use digest::*;
pub use lease::*;
pub use prompt::*;
pub use registry::*;
pub use session::*;
pub use stuck::*;
pub use supervise::*;
pub use switch::*;
pub use transport::*;
