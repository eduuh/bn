//! The PR review queue: open PRs from followed authors, for review or learning. The
//! `pr-queue` job (GitHub via `gh`) writes the cache on a schedule; this just reads it, so
//! `bn reviews` is instant and offline. Network stays in the cron job, never here — the
//! north star: the script fetches, bn renders.

use crate::resolver::state_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One open PR in the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPr {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub author: String,
    pub url: String,
    #[serde(default)]
    pub updated_at: String,
    /// When the PR was created (RFC-3339 / `YYYY-MM-DD…`), if the queue job stamped it. The
    /// anchor for review-wait time; absent on older caches, so callers fall back to `updated_at`.
    #[serde(default)]
    pub created_at: String,
    /// Why it's here: `review` or `learning` (from the author's config tag).
    #[serde(default)]
    pub tag: String,
    /// Lifecycle state stamped by the `pr-queue` job: `open` | `closed` | `merged` | `abandoned`
    /// (GitHub OPEN/CLOSED/MERGED; ADO active/completed/abandoned map onto these). Empty on older
    /// caches → treated as open. A non-open PR is history: it drops off the attention list.
    #[serde(default)]
    pub state: String,
    /// The PR's review verdict as the queue job saw it on GitHub: for a PR awaiting *your* review,
    /// `APPROVED` / `CHANGES_REQUESTED` mean you've already acted (no longer awaiting you); for one
    /// of *your own* PRs (`mine`), `APPROVED` means it's ready to land. `COMMENTED` / `DISMISSED` /
    /// absent → no verdict. Absent on older caches that predate the field. The producer stamps this
    /// from GitHub's `reviewDecision` / latest-review state.
    #[serde(
        rename = "reviewDecision",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub review_decision: Option<String>,
}

/// The viewer's GitHub login, for telling your own PRs apart from ones awaiting your review.
/// Read from `BN_GITHUB_LOGIN`; `None` when unset (then no author filtering happens).
pub fn my_login() -> Option<String> {
    std::env::var("BN_GITHUB_LOGIN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl ReviewPr {
    /// Whether this PR has left the open set — closed, merged, or abandoned. Such PRs are kept in
    /// the cache as history but filtered out of the attention list. Empty/`open` → still live.
    pub fn is_dead(&self) -> bool {
        matches!(self.state.as_str(), "closed" | "merged" | "abandoned")
    }

    /// Whether the PR is approved and just waiting on a merge — for one of *your own* PRs this is
    /// the cue to land it: it outranks a plain ping and is exempt from the recency cutoff.
    pub fn is_approved(&self) -> bool {
        self.review_decision
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case("approved"))
    }
}

/// How a PR awaiting review sits against the first-review SLA, by elapsed wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlaState {
    /// Comfortably within the SLA (< 75% of the window elapsed).
    Ok,
    /// Approaching the SLA (≥ 75% of the window elapsed, not yet over).
    Near,
    /// Past the SLA window.
    Over,
}

impl ReviewPr {
    /// Epoch seconds the review wait started — the PR's `created_at`, falling back to
    /// `updated_at`. `None` when neither parses (an old cache with no timestamp).
    pub fn wait_anchor_ts(&self) -> Option<i64> {
        crate::util::parse_ts(&self.created_at).or_else(|| crate::util::parse_ts(&self.updated_at))
    }

    /// Hours this PR has waited on review as of `now` (epoch secs), clamped at 0. `None` when no
    /// timestamp is available to measure from.
    pub fn wait_hours(&self, now: i64) -> Option<f64> {
        self.wait_anchor_ts()
            .map(|a| ((now - a).max(0)) as f64 / 3600.0)
    }

    /// True once you've given this PR a definitive review (approved or requested changes) — so it
    /// is no longer waiting on you. `COMMENTED`/`DISMISSED`/absent don't count as acted.
    pub fn acted(&self) -> bool {
        matches!(
            self.review_decision.as_deref().map(str::trim),
            Some("APPROVED") | Some("CHANGES_REQUESTED")
        )
    }

    /// True when this PR genuinely awaits *your* review: it's tagged `review`, it isn't your own
    /// PR (when `me` is known), and you haven't already acted on it. This is the signal that turns
    /// a queued PR into a "review it" action — `learning` PRs, your own PRs, and ones you've
    /// already approved/requested-changes on are excluded.
    pub fn awaiting_review(&self, me: Option<&str>) -> bool {
        self.tag == "review"
            && !self.acted()
            && me
                .map(|m| !self.author.eq_ignore_ascii_case(m))
                .unwrap_or(true)
    }

    /// The PR's SLA state given the first-review window (`sla_hours`) and `now`. `None` when the
    /// wait can't be measured (no timestamp). `Near` once ≥75% of the window has elapsed.
    pub fn sla_state(&self, now: i64, sla_hours: f64) -> Option<SlaState> {
        let waited = self.wait_hours(now)?;
        Some(if waited >= sla_hours {
            SlaState::Over
        } else if waited >= sla_hours * 0.75 {
            SlaState::Near
        } else {
            SlaState::Ok
        })
    }
}

/// The cached queue plus when it was generated (so callers can show staleness).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ReviewCache {
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub prs: Vec<ReviewPr>,
}

pub fn cache_path() -> PathBuf {
    state_dir().join("reviews.json")
}

/// Read the review-queue cache. Missing or unparseable → an empty cache (the job just hasn't
/// run yet), never an error.
pub fn read() -> ReviewCache {
    match std::fs::read_to_string(cache_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => ReviewCache::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir_path as with_state_dir;

    #[test]
    fn reads_cache_written_by_the_job() {
        with_state_dir(|dir| {
            std::fs::write(
                dir.join("reviews.json"),
                r#"{"generated_at":"2026-06-20T10:00:00Z","prs":[
                    {"repo":"eduuh/bn","number":2,"title":"mcp server","author":"eduuh","url":"https://x/2","updated_at":"2026-06-20T09:00:00Z","tag":"review"},
                    {"repo":"facebook/react","number":99,"title":"hooks","author":"gaearon","url":"https://x/99","updated_at":"2026-06-19T00:00:00Z","tag":"learning"}
                ]}"#,
            )
            .unwrap();
            let c = read();
            assert_eq!(c.generated_at, "2026-06-20T10:00:00Z");
            assert_eq!(c.prs.len(), 2);
            assert_eq!(c.prs[0].author, "eduuh");
            assert_eq!(c.prs[0].tag, "review");
            assert_eq!(c.prs[1].repo, "facebook/react");
        });
    }

    #[test]
    fn missing_cache_is_empty_not_an_error() {
        with_state_dir(|_| {
            let c = read();
            assert!(c.prs.is_empty());
            assert_eq!(c.generated_at, "");
        });
    }

    fn pr_at(created: &str, updated: &str) -> ReviewPr {
        ReviewPr {
            repo: "o/r".into(),
            number: 1,
            title: "t".into(),
            author: "a".into(),
            url: "u".into(),
            updated_at: updated.into(),
            created_at: created.into(),
            tag: "review".into(),
            state: "open".into(),
            review_decision: None,
        }
    }

    #[test]
    fn awaiting_review_excludes_learning_own_and_acted() {
        let mut pr = pr_at("2026-06-20T00:00:00Z", "2026-06-20T00:00:00Z");
        pr.author = "alice".into();
        // A review-tagged PR by someone else, not yet acted → awaiting me.
        assert!(pr.awaiting_review(Some("eduuh")));
        assert!(pr.awaiting_review(None));
        // Learning PRs never await review.
        pr.tag = "learning".into();
        assert!(!pr.awaiting_review(Some("eduuh")));
        pr.tag = "review".into();
        // Your own PR isn't yours to review.
        pr.author = "eduuh".into();
        assert!(!pr.awaiting_review(Some("eduuh")));
        assert!(pr.awaiting_review(Some("someone-else")));
        // Once you've approved / requested changes, it's no longer awaiting you.
        pr.author = "alice".into();
        pr.review_decision = Some("APPROVED".into());
        assert!(pr.acted());
        assert!(!pr.awaiting_review(Some("eduuh")));
        pr.review_decision = Some("CHANGES_REQUESTED".into());
        assert!(!pr.awaiting_review(Some("eduuh")));
        // Commenting isn't a verdict — still awaiting.
        pr.review_decision = Some("COMMENTED".into());
        assert!(!pr.acted());
        assert!(pr.awaiting_review(Some("eduuh")));
    }

    #[test]
    fn review_decision_deserializes_from_camelcase_and_defaults() {
        let pr: ReviewPr = serde_json::from_str(
            r#"{"repo":"o/r","number":1,"title":"t","author":"a","url":"u","tag":"review","reviewDecision":"APPROVED"}"#,
        )
        .unwrap();
        assert_eq!(pr.review_decision.as_deref(), Some("APPROVED"));
        // Older caches without the field → None (treated as still-awaiting).
        let old: ReviewPr = serde_json::from_str(
            r#"{"repo":"o/r","number":1,"title":"t","author":"a","url":"u","tag":"review"}"#,
        )
        .unwrap();
        assert_eq!(old.review_decision, None);
    }

    #[test]
    fn sla_state_crosses_near_then_over() {
        use crate::clock::{Clock, FixedClock};
        // PR created at a fixed instant; "now" pinned through the clock so the wait is exact.
        let created = "2026-06-20T00:00:00Z";
        let anchor = crate::util::parse_ts(created).unwrap();
        let pr = pr_at(created, "2026-06-20T00:00:00Z");
        let sla = 4.0; // 4h first-review window

        // 1h in → comfortably OK.
        let now = FixedClock::at(anchor + 3600).now_epoch_secs();
        assert_eq!(pr.wait_hours(now), Some(1.0));
        assert_eq!(pr.sla_state(now, sla), Some(SlaState::Ok));
        // 3h in → ≥75% of 4h → Near.
        let now = anchor + 3 * 3600;
        assert_eq!(pr.sla_state(now, sla), Some(SlaState::Near));
        // 5h in → past the window → Over.
        let now = anchor + 5 * 3600;
        assert_eq!(pr.sla_state(now, sla), Some(SlaState::Over));
    }

    #[test]
    fn wait_falls_back_to_updated_at_then_none() {
        let anchor = crate::util::parse_ts("2026-06-20T00:00:00Z").unwrap();
        // No created_at → anchor on updated_at.
        let pr = pr_at("", "2026-06-20T00:00:00Z");
        assert_eq!(pr.wait_hours(anchor + 7200), Some(2.0));
        // Neither timestamp → can't measure.
        let pr = pr_at("", "");
        assert_eq!(pr.wait_hours(anchor), None);
        assert_eq!(pr.sla_state(anchor, 4.0), None);
    }
}
