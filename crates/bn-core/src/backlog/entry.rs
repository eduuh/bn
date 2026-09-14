//! What a backlog entry *is*: the record, its status, and the spec that creates one.
//!
//! Split from [`super`], which owns the log: how lines are folded into a view, leased, resolved and
//! compacted.
use super::kind::BacklogKind;
use crate::orchestrate::derive_id;
use serde::{Deserialize, Serialize};

/// Where an entry should run — the substrate hint the selection brain (#348) later honors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Substrate {
    /// Let the scheduler decide — the default.
    #[default]
    Auto,
    /// Prefer a local worktree + agent.
    Local,
    /// Prefer a remote/cloud runner.
    Remote,
}

impl Substrate {
    pub fn label(self) -> &'static str {
        match self {
            Substrate::Auto => "auto",
            Substrate::Local => "local",
            Substrate::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Substrate> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Substrate::Auto),
            "local" => Some(Substrate::Local),
            "remote" => Some(Substrate::Remote),
            _ => None,
        }
    }
}

/// **Where — on which physical node — an item executes** (#717, M1 foundation). This is a
/// *separate axis* from [`Substrate`] and must not be conflated with it: `Substrate` chooses
/// *how* an item runs (a local worktree+agent vs. a GitHub-hosted fan-out that occupies no local
/// slot), whereas `ExecutionTarget` chooses *the node* a locally-substrated item lands on — the
/// implicit local box, or a chosen SSH-reachable worker. Making execution location first-class
/// lets dispatch target a picked node instead of always the box the daemon runs on.
///
/// Orthogonality: a `Substrate::Remote` item is GitHub fan-out and ignores this axis; a
/// `Substrate::Local`/`Auto` item is placed by the scheduler among `{Local, SshHost(..)}` (the
/// [`GitHubRunner`](ExecutionTarget::GitHubRunner) variant names the cloud runner for later
/// slices and is treated as always-eligible by the pure placement filter). Defaults to `Local`,
/// so a pre-#717 run's state — which carries no target — reads as the implicit local box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionTarget {
    /// The implicit local box the daemon runs on — the default and pre-#717 behavior.
    #[default]
    Local,
    /// A chosen SSH-reachable worker node, addressed by its `~/.ssh/config` alias / node id.
    SshHost(String),
    /// A GitHub-hosted runner (the cloud-execution target named for later M1 slices).
    GitHubRunner,
}

impl ExecutionTarget {
    /// The kebab-case label for status surfaces: `local`, `ssh:<alias>`, or `github-runner`.
    pub fn label(&self) -> String {
        match self {
            ExecutionTarget::Local => "local".to_string(),
            ExecutionTarget::SshHost(alias) => format!("ssh:{alias}"),
            ExecutionTarget::GitHubRunner => "github-runner".to_string(),
        }
    }

    /// Whether this is the default implicit-local-box target — the serde skip predicate, so a
    /// `Local` item omits the `target` key entirely (keeping pre-#717 state files byte-identical).
    pub fn is_local(&self) -> bool {
        matches!(self, ExecutionTarget::Local)
    }

    /// Parse a label back to a target (`None` for anything unrecognized). Accepts `local`,
    /// `ssh:<alias>` (alias must be non-empty), and `github-runner`.
    pub fn parse(s: &str) -> Option<ExecutionTarget> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "local" => Some(ExecutionTarget::Local),
            "github-runner" => Some(ExecutionTarget::GitHubRunner),
            _ => s
                .strip_prefix("ssh:")
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(|a| ExecutionTarget::SshHost(a.to_string())),
        }
    }
}

/// The lifecycle of a backlog entry: pending → leased → done|failed. Distinct from the run state's
/// [`crate::orchestrate::ItemStatus`]: `leased` means "claimed by its consumer", which reconciles
/// the terminal outcome back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BacklogStatus {
    /// Available to be pulled into a run — the default and the only pullable state.
    #[default]
    Pending,
    /// Leased by its consumer — not pullable again.
    Leased,
    /// Terminal success — the leased run item merged. Dropped by [`super::compact_if_needed`].
    Done,
    /// Terminal failure — the leased run item failed. Dropped by [`super::compact_if_needed`].
    Failed,
    /// Terminal, and asked for: someone took this back out of the store. Kept so older lines that
    /// carry it still parse.
    Cancelled,
}

impl BacklogStatus {
    pub fn label(self) -> &'static str {
        match self {
            BacklogStatus::Pending => "pending",
            BacklogStatus::Leased => "leased",
            BacklogStatus::Done => "done",
            BacklogStatus::Failed => "failed",
            BacklogStatus::Cancelled => "cancelled",
        }
    }

    /// Whether this entry has reached a terminal state (done or failed) and can be compacted away.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            BacklogStatus::Done | BacklogStatus::Failed | BacklogStatus::Cancelled
        )
    }

    /// Whether this entry still occupies the backlog as live work — pending or leased. Used for
    /// dedup: re-adding a `(repo, id)` that is already active is a no-op.
    pub fn is_active(self) -> bool {
        matches!(self, BacklogStatus::Pending | BacklogStatus::Leased)
    }
}

/// One durable backlog entry, keyed `(repo, id)`. `id` is [`derive_id`] of `item` (issue number,
/// issue/PR url, or a slug), so the same subject declared two ways dedups to one entry.
///
/// Lines written by the retired machine queue carry more fields than this; serde ignores them, so
/// those lines still fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklogEntry {
    /// Stable id within `repo` — [`derive_id`] of `item`.
    pub id: String,
    /// The store key this work belongs to (a repo, or `controller:<id>`). Half of the `(repo, id)`
    /// key (#343 multi-repo).
    pub repo: String,
    /// The raw subject as declared (`338`, a url, or free text).
    pub item: String,
    #[serde(default)]
    pub kind: BacklogKind,
    /// Higher is pulled first; ties break FIFO by `added_at` then `id`. Signed so a de-prioritized
    /// item can sit below the default-`0` floor.
    #[serde(default)]
    pub priority: i64,
    /// Path globs this work is expected to touch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
    #[serde(default)]
    pub substrate: Substrate,
    /// Epoch secs when first added — the FIFO tiebreak and the "added N ago" clock.
    #[serde(default)]
    pub added_at: i64,
    /// Free-form provenance (e.g. `"controller:c1"`). Empty when unset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// The task this work came from (#1001) — the id of a line in the global store. Empty when the
    /// producer names none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task: String,
    /// Controller-agent ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(default)]
    pub status: BacklogStatus,
    /// Epoch secs of the last status change (lease / terminal reconcile).
    #[serde(default)]
    pub updated_at: i64,
}

/// The mutable fields carried on a fresh `add` (everything not derived from `item`/clock). Lets a
/// caller declare one set of options and stamp it onto every item in a batch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddSpec {
    pub kind: BacklogKind,
    pub priority: i64,
    pub touches: Vec<String>,
    pub substrate: Substrate,
    pub source: String,
    pub controller_id: Option<String>,
    /// The task this work comes from (#1001).
    pub task: String,
}

impl BacklogEntry {
    /// Build a fresh pending entry for `item` in `repo`, stamping the [`AddSpec`] options and clock.
    pub fn new(repo: &str, item: &str, spec: &AddSpec, now: i64) -> BacklogEntry {
        BacklogEntry {
            id: derive_id(item),
            repo: repo.to_string(),
            item: item.to_string(),
            kind: spec.kind,
            priority: spec.priority,
            touches: spec.touches.clone(),
            substrate: spec.substrate,
            added_at: now,
            source: spec.source.clone(),
            controller_id: spec.controller_id.clone(),
            task: spec.task.clone(),
            status: BacklogStatus::Pending,
            updated_at: now,
        }
    }
}
