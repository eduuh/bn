//! Your own recently-merged pull requests, gathered with the material needed to talk about them.
//!
//! A collector, not a reviewer. It answers "what did I ship, and what did the change actually look
//! like" — the merge events, each PR's diff, and the branch note's `goal` and `decisions`, which
//! are the *why* a post-hoc reader never otherwise has. It forms no opinion about any of it.
//!
//! Discovery is **offline**, from the `pr_merged` events the `pr-observe` job already folds into
//! the metrics log; only the diff fetch touches the network. Diffs are capped and the day's PRs
//! ordered largest-first, because a burst day must degrade by dropping the smallest change rather
//! than by silently truncating everything.
//!
//! Read today by the `staff-mentor` agent (`config/agents/staff-mentor.md`), and by the standup
//! and completed-work rollups, none of which this module knows about.

use serde::{Deserialize, Serialize};

/// PRs gathered per run: a day rarely merges more of *your own* work than this, and each one
/// carries a full diff — the cap keeps a burst day from ballooning a packet.
pub const DEFAULT_PR_CAP: usize = 3;

/// The bare repo name (path segment) for a `owner/repo` slug, lowercased — the key notes and the
/// dojo deck are stored under.
pub fn bare_repo(slug: &str) -> String {
    slug.rsplit('/').next().unwrap_or(slug).to_ascii_lowercase()
}

// --- offline discovery ---------------------------------------------------------------------------

/// A candidate merged PR discovered offline from the metrics log — the material before any network
/// fetch. `repo` is the gh `owner/repo` slug the `pr_merged` event carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedRef {
    pub repo: String,
    pub number: i64,
    /// The PR title as stamped on the `pr_merged` event. Empty for events written before the
    /// title was carried (#213 follow-up), so every reader must tolerate a blank.
    pub title: String,
    /// GitHub merge time (epoch secs), as stamped on the `pr_merged` event.
    pub merged_ts: i64,
}

/// Discover *your* PRs merged on `reviewed_date` (local `YYYY-MM-DD`) from the metrics log's
/// `pr_merged` events. Offline and deterministic: the `pr-observe` job only ever logs your own
/// PRs, so no author filter is needed here; a PR is kept when the local date of its merge matches
/// `reviewed_date`. De-duplicated by `(repo, number)` (a re-observed merge is the same PR) and
/// returned newest-merge-first, then by repo/number, for a stable order independent of log order.
pub fn discover_merged(events: &[crate::events::Event], reviewed_date: &str) -> Vec<MergedRef> {
    merged_refs(events, |ts| crate::util::local_date_of(ts) == reviewed_date)
}

/// Every merged PR in the log, newest-first — [`discover_merged`] without the date narrowing, for
/// readers that want the whole history (the Completed feed) rather than one day (the Mentor).
pub fn all_merged(events: &[crate::events::Event]) -> Vec<MergedRef> {
    merged_refs(events, |_| true)
}

/// Shared fold behind [`discover_merged`] and [`all_merged`]: pick `pr_merged` events whose
/// timestamp passes `keep`, dedupe by `(repo, number)`, sort newest-merge-first.
fn merged_refs(events: &[crate::events::Event], keep: impl Fn(i64) -> bool) -> Vec<MergedRef> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<MergedRef> = Vec::new();
    for e in events {
        if e.kind != "pr_merged" {
            continue;
        }
        let (Some(repo), Some(number)) = (
            e.fields.get("repo").and_then(|v| v.as_str()),
            e.fields.get("number").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        if !keep(e.ts) {
            continue;
        }
        if !seen.insert((repo.to_string(), number)) {
            continue;
        }
        out.push(MergedRef {
            repo: repo.to_string(),
            number,
            title: e
                .fields
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            merged_ts: e.ts,
        });
    }
    out.sort_by(|a, b| {
        b.merged_ts
            .cmp(&a.merged_ts)
            .then(a.repo.cmp(&b.repo))
            .then(a.number.cmp(&b.number))
    });
    out
}

// --- the gathered packet -------------------------------------------------------------------------

/// One PR's material — the harness's entire world for that review. The `description` and `diff`
/// are untrusted external content (framed as data at dispatch, #272); `goal`/`decisions` are the
/// branch-note "why" a post-hoc reviewer never has.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrPacket {
    /// gh `owner/repo` slug (the trusted routing key — the harness echo is never used for it).
    pub repo: String,
    pub number: i64,
    /// The PR's head branch (`headRefName`) — where the craft note lands.
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub diff: String,
    /// Diff size in bytes — the largest-first ordering key (see [`select_and_cap`]). Not presented
    /// to the model as guidance; just the deterministic cap order.
    #[serde(default)]
    pub diff_bytes: usize,
    /// The branch-note one-line goal, when the note is on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The branch-note decisions (the "why"), when present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    /// Verify-gate history for the branch (did it flake, was it re-run) — best-effort; empty when
    /// no such record exists on this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify_history: Vec<String>,
}

/// One gather's whole result — the day's merged PRs with their material, plus what was dropped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergedPrPacket {
    /// The run date (`today`, `YYYY-MM-DD`).
    pub date: String,
    /// The merge day being reviewed (`yesterday`).
    pub reviewed_date: String,
    /// The PRs, capped and largest-diff-first.
    pub prs: Vec<PrPacket>,
    /// PRs merged that day beyond the cap (dropped, but *noted* — never silently truncated).
    pub overflow: usize,
    /// Total PRs gathered before the cap.
    pub total_merged: usize,
}

impl MergedPrPacket {
    /// The `(repo, number)` of every PR in the packet — the set a reader is expected to cover.
    pub fn expected(&self) -> Vec<(String, i64)> {
        self.prs
            .iter()
            .map(|p| (p.repo.clone(), p.number))
            .collect()
    }

    /// Serialize to the JSON the harness receives.
    pub fn to_json(&self) -> std::io::Result<String> {
        crate::store::to_json_pretty(self)
    }
}

/// Order PRs largest-diff-first and cap the count, reporting the overflow. Deterministic: ties
/// break by `(repo, number)` so the same day always yields the same three. Returns `(kept,
/// overflow)` where `overflow = len - cap` when the day merged more than the cap.
pub fn select_and_cap(mut prs: Vec<PrPacket>, cap: usize) -> (Vec<PrPacket>, usize) {
    prs.sort_by(|a, b| {
        b.diff_bytes
            .cmp(&a.diff_bytes)
            .then(a.repo.cmp(&b.repo))
            .then(a.number.cmp(&b.number))
    });
    let overflow = prs.len().saturating_sub(cap);
    prs.truncate(cap);
    (prs, overflow)
}

/// Assemble the packet from the fetched PRs: capture the pre-cap total, then order + cap.
pub fn build_packet(
    date: &str,
    reviewed_date: &str,
    prs: Vec<PrPacket>,
    cap: usize,
) -> MergedPrPacket {
    let total_merged = prs.len();
    let (prs, overflow) = select_and_cap(prs, cap);
    MergedPrPacket {
        date: date.to_string(),
        reviewed_date: reviewed_date.to_string(),
        prs,
        overflow,
        total_merged,
    }
}
