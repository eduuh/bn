//! The repo-visibility filter every metrics surface reads through: tooling repos are
//! always dropped, and `work_only` machines keep only the work allowlist.

use crate::events::Event;
use crate::resolver::Config;
use serde_json::Value;

/// Repos excluded from the scorecard — the tooling/dotfiles repos whose churn would otherwise
/// dwarf real project work and flatter the numbers. Matched on the repo basename so both the gh
/// `owner/repo` PR field (`eduuh/bn`) and the bare bn repo name (`bn`) resolve the same.
pub const EXCLUDED_REPOS: &[&str] = &["bn", "dotfiles"];

/// True if `repo` names an excluded repo (see [`EXCLUDED_REPOS`]), matched on its trailing path
/// segment: `eduuh/bn` → `bn`, `dotfiles` → `dotfiles`.
pub fn is_excluded_repo(repo: &str) -> bool {
    let base = repo.rsplit('/').next().unwrap_or(repo);
    EXCLUDED_REPOS.contains(&base)
}

/// Drop events whose `repo` field names a repo the metrics scorecard shouldn't count on this
/// machine: always the tooling/dotfiles repos ([`EXCLUDED_REPOS`]), and — on a `work_only`
/// machine — anything outside the work allowlist, so personal throughput never lands on a work
/// screen. Events with no `repo` field are kept (no attribution to exclude). The single
/// chokepoint every metrics surface filters through, so the scorecard and weekly review agree.
pub fn visible_events(cfg: &Config, events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .filter(|e| {
            e.fields
                .get("repo")
                .and_then(Value::as_str)
                .is_none_or(|r| !is_excluded_repo(r) && cfg.is_visible_repo(r))
        })
        .collect()
}


