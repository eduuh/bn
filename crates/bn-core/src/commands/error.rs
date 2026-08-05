//! Shared command error type (`CmdError`) and the best-effort metrics helper used by the
//! note/todo verbs.

use crate::resolver::{Config, Resolved};
use crate::store::StoreError;
use serde_json::{json, Map, Value};

/// Best-effort productivity event to the **synced** metrics log (`cfg.metrics_log_path()`).
/// Records the work signal behind close-out lag and flow time. Never fails the command —
/// metrics are observability, not correctness — so a write error is swallowed. Every event
/// carries repo/branch/machine; `extra` adds kind-specific fields.
pub(crate) fn emit_metric(cfg: &Config, ctx: &Resolved, kind: &str, extra: Vec<(&str, Value)>) {
    let mut fields = Map::new();
    fields.insert("repo".into(), json!(ctx.repo));
    fields.insert("branch".into(), json!(ctx.branch));
    fields.insert("machine".into(), json!(cfg.machine()));
    for (k, v) in extra {
        fields.insert(k.into(), v);
    }
    crate::util::best_effort(
        crate::events::append_metric_at(
            &cfg.metrics_log_path(),
            kind,
            crate::util::now_epoch_secs(),
            fields,
        ),
        "metrics: append event",
    );
}

#[derive(Debug)]
pub enum CmdError {
    Store(StoreError),
    UnknownSection(String),
    EmptyText,
    NoNote,
    NotFound(String),
    /// query, and the ids of the open todos it matched
    Ambiguous(String, Vec<String>),
    AlreadyDone(String),
    /// A date argument wasn't a valid `YYYY-MM-DD`.
    InvalidDate(String),
    /// A quarter-goal key (or other identifier) collided with an existing one.
    Duplicate(String),
}

impl CmdError {
    /// Stable error code for the JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            CmdError::Store(_) => "io",
            CmdError::UnknownSection(_) => "unknown_section",
            CmdError::EmptyText => "empty_text",
            CmdError::NoNote => "no_note",
            CmdError::NotFound(_) => "not_found",
            CmdError::Ambiguous(..) => "ambiguous",
            CmdError::AlreadyDone(_) => "already_done",
            CmdError::InvalidDate(_) => "invalid_date",
            CmdError::Duplicate(_) => "duplicate",
        }
    }

    pub fn message(&self) -> String {
        match self {
            CmdError::Store(e) => e.to_string(),
            CmdError::UnknownSection(s) => format!(
                "Unknown section: {s} (use: todo, blocker, decision, research, collab, ask)"
            ),
            CmdError::EmptyText => "Usage: bn add <section> <text>".into(),
            CmdError::NoNote => "No note for this branch".into(),
            CmdError::NotFound(q) => format!("No match: {q}"),
            CmdError::Ambiguous(q, ids) => {
                format!(
                    "Multiple matches for '{q}': {} (use the id)",
                    ids.join(", ")
                )
            }
            CmdError::AlreadyDone(id) => format!("Already done: {id}"),
            CmdError::InvalidDate(d) => format!("Invalid date '{d}' (use YYYY-MM-DD)"),
            CmdError::Duplicate(k) => format!("Already exists: {k}"),
        }
    }
}

impl From<StoreError> for CmdError {
    fn from(e: StoreError) -> Self {
        CmdError::Store(e)
    }
}
