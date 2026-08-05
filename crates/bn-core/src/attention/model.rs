//! The attention-item model: severities, modes, and the outcome/input structs.

use crate::commands;
use crate::resolver::{self, Resolved};
use serde::{Deserialize, Serialize};

/// Severity ranks for thresholds and sorting: `info` < `warn` < `urgent`. Unknown strings rank
/// as `info` so a malformed field never silently outranks a real urgent item.
pub fn severity_rank(severity: &str) -> u8 {
    match severity {
        "urgent" => 2,
        "warn" => 1,
        _ => 0,
    }
}

/// The accepted severity values, in ascending order — for CLI validation and help text. Note
/// [`DONE_SEVERITY`] is deliberately absent: a completion is not something a person raises, so
/// `bn attn raise --severity done` stays rejected.
pub const SEVERITIES: [&str; 3] = ["info", "warn", "urgent"];

/// The severity a *completion push* (an agent's `bn agent done`) carries. A completion is an
/// outcome, not an actionable item — it never belongs in the attention list, only the review
/// inbox. Kept out of [`SEVERITIES`] so it's an internal marker, not a user-raisable level.
pub const DONE_SEVERITY: &str = "done";

/// Whether an open item is a *completion* (a finished-work report awaiting review) rather than an
/// actionable, needs-you item. The forward marker is [`DONE_SEVERITY`]; the `done:<repo>/<branch>`
/// key catches legacy completions raised as `info` before the severity existed, so the existing
/// backlog also moves to the review inbox instead of lingering as attention.
pub(crate) fn is_completion(item: &AttnItem) -> bool {
    item.severity == DONE_SEVERITY || item.key.as_deref().is_some_and(|k| k.starts_with("done:"))
}

/// How an item may be actioned — the label that tells the flow (and an agent) who can pick it up:
/// `manual` (human-only, the default), `assist` (an agent preps a worktree/window + stages the
/// prompt, you drive), `auto` (an agent may run it unattended). Absent → `manual`. `assist`/`auto`
/// require a `prompt`.
pub const MODES: [&str; 3] = ["manual", "assist", "auto"];

/// The accepted `task_type` values — the pipeline stage an attention item belongs to, for CLI
/// validation and help text. `coding` is the default (an absent field folds to it, see
/// [`AttnItem::task_type`]); `planning`/`critique`/`review` are the typed stages of the
/// Plan → Execute → Critique pipeline (#649).
pub const TASK_TYPES: [&str; 4] = ["coding", "planning", "critique", "review"];

/// One open attention item, as folded from the event log.
#[derive(Debug, Clone, Serialize)]
pub struct AttnItem {
    pub id: String,
    /// Who raised it: `manual` (a person/agent) or a job name (`pr-queue`, `cron`, …).
    pub source: String,
    pub title: String,
    /// `info` | `warn` | `urgent`.
    pub severity: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub link: Option<String>,
    /// Dedup key, if the raiser set one.
    pub key: Option<String>,
    /// Rich, multi-line context for the detail/second view (e.g. a review's full reasoning).
    /// `None` for the many one-line items; the list view shows only `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// How the item may be actioned — see [`MODES`]. `None` == `manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The agent prompt for an `assist`/`auto` item — what to do when the item is started. Multi-
    /// line. `None` for plain (human-driven) items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Which pipeline stage the item belongs to — see [`TASK_TYPES`]. `None` == `coding` (the
    /// execution default), so untyped legacy items fold to a `coding` item. Serialized only when
    /// set, like `mode`/`prompt`, keeping existing payloads byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// Absolute path of the worktree a `wt-stuck` item is about — lets a consumer act on it (cd,
    /// open, remove) without parsing the title. `None` for the many items that aren't worktree-
    /// scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// The uncommitted file paths blocking a merged worktree's reclaim (`git status --porcelain`
    /// entries), so a consumer can render the actionable list instead of re-parsing the change-
    /// count sentence. Empty for every item that isn't a `wt-stuck` notice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted_files: Vec<String>,
    /// Unix epoch seconds when the item was raised (the `attn` event's `ts`).
    pub created: i64,
}

impl AttnItem {
    /// The effective mode, defaulting an absent field to `manual`.
    pub fn mode(&self) -> &str {
        self.mode.as_deref().unwrap_or("manual")
    }

    /// The effective task type, defaulting an absent field to `coding` (the execution stage) —
    /// mirrors [`AttnItem::mode`] so untyped legacy items surface under `--type coding`.
    pub fn task_type(&self) -> &str {
        self.task_type.as_deref().unwrap_or("coding")
    }
}

/// Outcome of [`add`]: the item's id, and whether the add was skipped because an open item
/// already carried the same `key` (idempotent script pulls). Mirrors the `bn add` shape.
#[derive(Debug, Serialize)]
pub struct AddOutcome {
    pub id: String,
    pub skipped: bool,
}

/// One script-provided attention item. Parsed from JSON or JSONL by [`parse_ingest_items`].
/// `title` may be provided as `text` for parity with feed jobs; `link` may be provided as `url`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IngestItem {
    #[serde(alias = "text")]
    pub title: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default, alias = "url")]
    pub link: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Per-row result from [`ingest`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IngestedItem {
    pub id: String,
    pub title: String,
    pub source: String,
    pub key: Option<String>,
    pub skipped: bool,
}

/// Summary for a script batch ingested into the attention list.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub items: Vec<IngestedItem>,
    pub raised: usize,
    pub skipped: usize,
    pub resolved: usize,
}

/// Outcome of promoting an attention item into a branch-note todo.
#[derive(Debug)]
pub struct PlanOutcome {
    pub todo: commands::AddOutcome,
    pub ctx: Resolved,
}

/// Why promoting an attention item to a todo failed.
#[derive(Debug)]
pub enum PlanError {
    NoItem,
    NoTarget(resolver::ContextError),
    Add(commands::CmdError),
    Resolve(std::io::Error),
}
