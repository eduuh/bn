//! Who may launch an agent with nobody watching (#1003, EPIC #993).
//!
//! bn has exactly one rail against prompt injection reaching an unattended agent, and it is the
//! `source` field: `super::query::open_auto_eligible` historically required `source == "manual"`,
//! meaning *self-authored*. Only text you wrote yourself could start a `--yolo` agent in a worktree
//! with repo write access.
//!
//! #1000 makes tasks out of **public GitHub issues**. Issue titles and bodies are writable by
//! anyone who can file an issue. Stamping those `manual` to make them auto-eligible would delete
//! the rail outright, turning "auto-pull, then improve" into a remote code execution path.
//!
//! # The third tier
//!
//! So `source: "task"` is a *narrower* tier, not a bypass. An item carrying it may run unattended
//! only when **all** of these hold:
//!
//! 1. It names a task, and that task passed the SMART gate (`smart:ok`, #998).
//! 2. The task targets a **known repo** — an attacker must not be able to steer work at an
//!    arbitrary path.
//! 3. Its prompt is **bn-authored**: it starts with a recognised template and never quotes the
//!    task's own words.
//!
//! Condition 3 is the load-bearing one. Untrusted text may reach an agent as *reference material*
//! it chooses to read — the task and its context doc, which the agent opens itself, following the
//! discipline [`super::ingest::promote_todo`] already uses — but never as instructions in a prompt.
//!
//! # If in doubt, fail closed
//!
//! Every check returns a reason rather than a bare `false`, and anything unrecognised is
//! ineligible. An agent that does not start is a nuisance; one that starts on attacker input is an
//! incident.

use super::model::AttnItem;
use crate::resolver::Config;

/// Self-authored: you typed it. The original and widest tier.
pub const SOURCE_SELF: &str = "manual";
/// Task-derived: minted by bn from a task, possibly one pulled from a public issue.
pub const SOURCE_TASK: &str = "task";
/// Staff-promoted: routed by a Markdown agent the person switched on, which deliberately marked
/// the item `mode: auto`. The prefix every routed staff item carries.
pub const SOURCE_STAFF_PREFIX: &str = "staff-";

/// Prompt openers bn itself writes for task-derived items. A prompt that does not begin with one
/// of these was not composed by us, so it cannot be trusted to be instruction-free.
pub const BN_TASK_PROMPT_PREFIXES: [&str; 3] = [
    "Run `bn task enhance ",
    // Legacy prompt text from before enhance became the canonical task agent name.
    "Run `bn task improve ",
    "Run `bn task ",
];

/// Why an item may not run unattended. Stable strings — callers log and test on these.
pub type Denial = &'static str;

/// The verdict on whether one item may launch an agent with nobody watching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoVerdict {
    Eligible,
    Denied(Denial),
}

impl AutoVerdict {
    pub fn eligible(&self) -> bool {
        matches!(self, AutoVerdict::Eligible)
    }
    pub fn reason(&self) -> Option<Denial> {
        match self {
            AutoVerdict::Denied(r) => Some(*r),
            AutoVerdict::Eligible => None,
        }
    }
}

/// The task id an attention item is about, from its `attn:<kind>:<task-id>` key.
///
/// Keys are bn-authored (a caller cannot set an arbitrary one through any ingest path that also
/// sets `source: "task"`), so this is a safe way to tie an item back to the record that governs it.
pub fn task_id_of(item: &AttnItem) -> Option<&str> {
    let key = item.key.as_deref()?;
    let rest = key.strip_prefix("attn:")?;
    let (_kind, id) = rest.split_once(':')?;
    let id = id.trim();
    // A task id is `t<digits>`; anything else is not ours to trust.
    (id.len() >= 2 && id.starts_with('t') && id[1..].chars().all(|c| c.is_ascii_digit()))
        .then_some(id)
}

/// Whether a prompt looks like something bn composed, rather than something an issue author did.
///
/// Two independent conditions, both required:
///
/// - it starts with one of [`BN_TASK_PROMPT_PREFIXES`], and
/// - it does not contain the task's own title.
///
/// The second is the one that actually stops injection. A pulled task's title *is* the issue
/// title — attacker-controlled — so a prompt quoting it is a prompt carrying attacker text into
/// the agent's instructions, however innocuous the wrapper looks.
pub fn is_bn_authored_prompt(prompt: &str, task_summary: &str) -> bool {
    if !BN_TASK_PROMPT_PREFIXES
        .iter()
        .any(|p| prompt.starts_with(*p))
    {
        return false;
    }
    let summary = task_summary.trim();
    if summary.is_empty() {
        return true;
    }
    !prompt
        .to_ascii_lowercase()
        .contains(&summary.to_ascii_lowercase())
}

/// The shape every tier needs regardless of trust: something to run, and somewhere to run it.
fn has_runnable_shape(i: &AttnItem) -> Result<(), Denial> {
    if i.mode() != "auto" {
        return Err("mode is not auto");
    }
    if i.repo.is_none() || i.branch.is_none() {
        return Err("no repo/branch to open a window in");
    }
    if i.prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
        return Err("no prompt");
    }
    Ok(())
}

/// May this item launch an agent unattended?
///
/// `cfg` is needed only by the task tier (to read the store and the known-repo set); the
/// self-authored tier is a pure field check, exactly as before.
pub fn auto_verdict(cfg: &Config, item: &AttnItem, today: &str) -> AutoVerdict {
    let _ = today;
    if let Err(why) = has_runnable_shape(item) {
        return AutoVerdict::Denied(why);
    }
    let source = effective_source(item);
    match source {
        // Self-authored: unchanged, and still the widest tier.
        SOURCE_SELF => AutoVerdict::Eligible,
        SOURCE_TASK => task_tier(cfg, item),
        _ if source.starts_with(SOURCE_STAFF_PREFIX) => staff_tier(cfg, item),
        // Anything else — a script, a PR webhook, an unrecognised label — stays out. Fail closed.
        _ => AutoVerdict::Denied("source is not a trusted tier"),
    }
}

/// The tier for an item a staff agent elevated with `mode: auto`.
///
/// Three deliberate acts stand behind such an item: the person switched the agent on, they
/// allowlisted the repo it may patrol, and the agent chose to promote this finding. That is a
/// weaker warrant than "you typed it" — the prompt was written by a model — so it carries the same
/// guard the task tier does and for the same reason: an attacker who chooses the repo chooses
/// where the agent gets write access. Everything downstream (the night allowlist, its enable, its
/// window) still applies.
///
/// Before this tier existed the promotion path was dead: the deleted Rust stamped its promotions
/// `manual` to clear the gate, and once routing stamped them `staff-<agent>` instead, every
/// promotion was minted, queued, and never picked up — silently, since a denied item looks exactly
/// like one nothing has got to yet.
fn staff_tier(cfg: &Config, item: &AttnItem) -> AutoVerdict {
    let Some(repo) = item.repo.as_deref().map(str::to_ascii_lowercase) else {
        return AutoVerdict::Denied("staff item names no repo");
    };
    if !crate::issues::known_repos(cfg).contains(&repo) {
        return AutoVerdict::Denied("staff item targets an unknown repo");
    }
    AutoVerdict::Eligible
}

/// The tier an item *actually* belongs to, which is not always the tier its `source` records.
///
/// This change is not retroactive on its own: `add_new`'s keyed refresh rewrites an item's
/// title/severity/detail but never its `source`, so a task item raised before #1003 — stamped
/// `manual` by the older gate — would keep full self-authored trust forever, even as the gate
/// re-fires on it. Task items are recognisable by their bn-authored `attn:smart:` key, so classify
/// on that rather than trusting a label written by an earlier version of ourselves.
fn effective_source(item: &AttnItem) -> &str {
    let looks_like_task = item
        .key
        .as_deref()
        .is_some_and(|k| k.starts_with("attn:smart:"));
    if looks_like_task {
        return SOURCE_TASK;
    }
    item.source.as_str()
}

/// The narrow tier for task-derived items. Every condition must hold.
fn task_tier(cfg: &Config, item: &AttnItem) -> AutoVerdict {
    let Some(id) = task_id_of(item) else {
        return AutoVerdict::Denied("item names no task");
    };
    let Ok(file) = crate::task_store::task_store(cfg).load() else {
        return AutoVerdict::Denied("task store unreadable");
    };
    let Some(task) = file.tasks().find(|t| t.id() == Some(id)).cloned() else {
        return AutoVerdict::Denied("task no longer exists");
    };

    // 1. It passed the gate. A `fail`/`stuck`/absent verdict means nothing has judged this
    //    scoped enough to execute — which is the whole point of #998.
    if task.meta("smart") != Some("ok") {
        return AutoVerdict::Denied("task has not passed the SMART gate");
    }

    // 2. It targets a repo bn manages. Otherwise an attacker choosing the repo chooses where the
    //    agent gets write access.
    let Some(repo) = task.meta("repo").map(str::to_ascii_lowercase) else {
        return AutoVerdict::Denied("task names no repo");
    };
    let known = crate::issues::known_repos(cfg);
    if !known.contains(&repo) {
        return AutoVerdict::Denied("task targets an unknown repo");
    }

    // The launcher opens a worktree at `item.repo`/`item.branch`, not at the task's — they agree
    // today only because one call seeds both, and a keyed refresh never rewrites them. Check the
    // field that is actually *used*, so the guarantee is structural rather than incidental.
    let item_repo = item
        .repo
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if item_repo != repo || !known.contains(&item_repo) {
        return AutoVerdict::Denied("item and task disagree about the repo");
    }

    // 3. The prompt is ours, and quotes nothing the issue author wrote.
    let prompt = item.prompt.as_deref().unwrap_or_default();
    if !is_bn_authored_prompt(prompt, &task.summary()) {
        return AutoVerdict::Denied("prompt is not bn-authored");
    }

    AutoVerdict::Eligible
}
