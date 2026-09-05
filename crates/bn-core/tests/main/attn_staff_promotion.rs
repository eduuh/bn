//! A staff agent's `mode: auto` promotion has to be able to actually run.
//!
//! The deleted Rust stamped its promotions `manual` — the self-authored tier — with a comment
//! saying that was deliberate, so the night shift could pull them. Once routing stamped every card
//! item `staff-<agent>` instead, the source fell through to "not a trusted tier" and every
//! promotion was minted, queued, and never picked up. Silently: a denied item looks exactly like
//! one nothing has got to yet.

use bn_core::attention::{auto_verdict, AttnItem};
use bn_core::resolver::Config;

fn promotion(source: &str, repo: Option<&str>) -> AttnItem {
    AttnItem {
        id: "a1".to_string(),
        source: source.to_string(),
        title: "Clear an aging TODO".to_string(),
        severity: "warn".to_string(),
        repo: repo.map(str::to_string),
        branch: Some("sentinel/todo-parse-rs".to_string()),
        link: None,
        key: Some("attn:staff-sentinel:promote:todo:bn:src/parse.rs:42".to_string()),
        detail: None,
        mode: Some("auto".to_string()),
        prompt: Some("Clear the 90-day-old TODO in src/parse.rs.".to_string()),
        task_type: None,
        worktree_path: None,
        uncommitted_files: Vec::new(),
        created: 0,
    }
}

/// The guard that makes the tier safe at all, and the same one the task tier keeps: an attacker
/// who chooses the repo chooses where an unattended agent gets write access.
#[test]
fn a_staff_promotion_without_a_repo_is_refused() {
    let verdict = auto_verdict(
        &Config::default(),
        &promotion("staff-sentinel", None),
        "2026-09-03",
    );
    assert!(!verdict.eligible(), "got {verdict:?}");
}

#[test]
fn a_staff_promotion_into_an_unknown_repo_is_refused() {
    let verdict = auto_verdict(
        &Config::default(),
        &promotion("staff-sentinel", Some("some-repo-bn-does-not-manage")),
        "2026-09-03",
    );
    assert!(!verdict.eligible(), "got {verdict:?}");
}

/// A source outside every tier still fails closed — the tier is for `staff-` items, not for
/// anything that happens to carry a prompt.
#[test]
fn an_untrusted_source_is_still_refused() {
    let verdict = auto_verdict(
        &Config::default(),
        &promotion("script", Some("bn")),
        "2026-09-03",
    );
    assert!(!verdict.eligible(), "got {verdict:?}");
}
