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


