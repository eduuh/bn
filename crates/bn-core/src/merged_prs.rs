//! Your own merged pull requests, discovered offline from the metrics log.
//!
//! Discovery reads the `pr_merged` events the `pr-observe` job already folds into the metrics log —
//! no network, no GitHub call. [`MergedRef`] is what a merged PR looks like to the rest of bn:
//! repo, number, title and merge timestamp.
//!
//! Feeds the completed-work fold ([`crate::completed`]) and the dashboard's activity feed. The
//! diff-gathering half that fed the retired `staff-mentor` agent went with that agent.

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
