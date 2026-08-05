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

#[cfg(test)]
mod tests {
    use super::super::flow::compute;
    use super::*;
    use crate::metrics::ev;
    use serde_json::json;

    #[test]
    fn excluded_repos_drop_from_metrics_by_basename() {
        assert!(is_excluded_repo("bn"));
        assert!(is_excluded_repo("eduuh/bn"));
        assert!(is_excluded_repo("eduuh/dotfiles"));
        assert!(!is_excluded_repo("eduuh/bits-and-atoms"));
        // `bn-core` is a distinct repo — only an exact basename match excludes.
        assert!(!is_excluded_repo("eduuh/bn-core"));
    }

    /// A throwaway `Config` for the visibility filter: `work_only` + `work_repos` are the only
    /// fields it reads. Built without touching the env so it composes with the pure helpers.
    fn vis_cfg(work_only: bool, work_repos: &[&str]) -> Config {
        let _g = crate::test_support::env_lock();
        let _notes = crate::test_support::EnvGuard::set("BN_NOTES_DIR", "/tmp/bn-metrics-test");
        let mut c = Config::from_env();
        c.work_only = work_only;
        c.work_repos = work_repos.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn visible_events_drops_excluded_and_keeps_unattributed() {
        let evs = vec![
            // bn PR (owner/repo form) and a bn activity event — both excluded.
            ev(
                "pr_merged",
                10 * 86_400,
                &[("repo", json!("eduuh/bn")), ("number", json!(1))],
            ),
            ev("activity", 10 * 86_400, &[("repo", json!("bn"))]),
            // a real project's merge survives.
            ev(
                "pr_merged",
                10 * 86_400,
                &[("repo", json!("eduuh/atlas")), ("number", json!(2))],
            ),
            // no repo field → kept.
            ev("activity", 10 * 86_400, &[]),
        ];
        let kept = visible_events(&vis_cfg(false, &[]), evs);
        assert_eq!(
            kept.len(),
            2,
            "both bn events dropped, atlas + unattributed kept"
        );
        let m = compute(&kept, 100 * 86_400, 365);
        assert_eq!(m.merged, 1, "only the atlas merge counts");
    }

    #[test]
    fn work_only_keeps_only_work_repo_events() {
        let evs = vec![
            // a work PR (gh owner/repo, basename in the allowlist) — kept.
            ev(
                "pr_merged",
                10 * 86_400,
                &[("repo", json!("m365-core/arena")), ("number", json!(1))],
            ),
            // a personal PR — dropped on a work machine.
            ev(
                "pr_merged",
                10 * 86_400,
                &[
                    ("repo", json!("eduuh/bits-and-atoms")),
                    ("number", json!(2)),
                ],
            ),
            // unattributed event still kept.
            ev("activity", 10 * 86_400, &[]),
        ];
        let kept = visible_events(&vis_cfg(true, &["arena"]), evs);
        assert_eq!(
            kept.len(),
            2,
            "personal PR dropped, work PR + unattributed kept"
        );
        let m = compute(&kept, 100 * 86_400, 365);
        assert_eq!(m.merged, 1, "only the work merge counts");
    }
}
