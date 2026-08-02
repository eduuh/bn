//! pr-observe — the producer for the PR metrics events. `metrics.rs` consumes `pr_opened` /
//! `pr_merged` / `pr_closed` / `pr_review` events but nothing ever emitted them, so PR
//! throughput, completion rate, cycle/lead time, and review latency all read zero (#213).
//! The `pr-observe` job polls GitHub for your own PRs (gh stays in the CLI job, like
//! `pr-queue`); this module is the pure half — deciding which events a PR's observed state
//! warrants that aren't in the metrics log yet. Events are stamped at the time the thing
//! *happened* on GitHub (createdAt/mergedAt/…), not when the job noticed, so windows and
//! cycle times are truthful even when the job backfills.
//!
//! Idempotency mirrors agent ingest: an event identity is `(kind, repo, number)`, checked
//! against the whole-person log (every machine's synced metrics log), so re-runs and
//! multi-machine setups don't double-emit.

use crate::events::Event;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// One of your PRs as observed on GitHub, normalized for event derivation. `repo` is the gh
/// `owner/repo` slug — the attribution key the metrics readers join on. Timestamps are epoch
/// secs; `None` means "didn't happen" (or wasn't fetched — a search row without enrichment
/// carries only `created_ts`).
#[derive(Debug, Clone, Default)]
pub struct ObservedPr {
    pub repo: String,
    pub number: i64,
    pub created_ts: Option<i64>,
    pub merged_ts: Option<i64>,
    /// Closed-unmerged only matters when `merged_ts` is absent (GitHub stamps both on merge).
    pub closed_ts: Option<i64>,
    /// Earliest commit on the PR — the DORA lead-time anchor stamped as `lead_secs`.
    pub first_commit_ts: Option<i64>,
    /// Earliest review by someone other than the author.
    pub first_review_ts: Option<i64>,
}

/// A metrics event the job should append: `kind` at `ts` with `fields`.
#[derive(Debug)]
pub struct PendingEvent {
    pub kind: &'static str,
    pub ts: i64,
    pub fields: Map<String, Value>,
}

/// The `(kind, repo, number)` identities already in the metrics log — the dedupe set. Reads
/// the same four kinds it emits; anything else in the log is ignored.
pub fn seen_keys(events: &[Event]) -> HashSet<(String, String, i64)> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "pr_opened" | "pr_merged" | "pr_closed" | "pr_review"
            )
        })
        .filter_map(|e| {
            let repo = e.fields.get("repo")?.as_str()?;
            let number = e.fields.get("number")?.as_i64()?;
            Some((e.kind.clone(), repo.to_string(), number))
        })
        .collect()
}

/// Whether a PR still needs the per-PR `gh pr view` enrichment (one gh call each — the
/// expensive step, so the job asks first). A closed search row needs it while its terminal
/// event is unknown; any row needs it while its first review is unknown. An open PR that
/// already has a `pr_review` logged needs nothing beyond the free search row.
pub fn needs_enrichment(
    repo: &str,
    number: i64,
    closed: bool,
    seen: &HashSet<(String, String, i64)>,
) -> bool {
    let missing = |k: &str| !seen.contains(&(k.to_string(), repo.to_string(), number));
    (closed && missing("pr_merged") && missing("pr_closed")) || missing("pr_review")
}

/// Derive the events `prs` warrant that aren't in `seen` yet, stamped at their GitHub
/// timestamps. Pure over the slice — the job gathers, this decides, `events::append_metric_at`
/// records. A merged PR emits `pr_merged` (never `pr_closed`, even though GitHub stamps both);
/// `lead_secs` rides along when the first commit is known and sane.
pub fn derive_events(
    prs: &[ObservedPr],
    seen: &HashSet<(String, String, i64)>,
) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for pr in prs {
        let missing = |k: &str| !seen.contains(&(k.to_string(), pr.repo.clone(), pr.number));
        let base = || {
            let mut f = Map::new();
            f.insert("repo".into(), json!(pr.repo));
            f.insert("number".into(), json!(pr.number));
            f
        };
        if let Some(ts) = pr.created_ts.filter(|_| missing("pr_opened")) {
            out.push(PendingEvent {
                kind: "pr_opened",
                ts,
                fields: base(),
            });
        }
        if let Some(ts) = pr.merged_ts.filter(|_| missing("pr_merged")) {
            let mut fields = base();
            if let Some(lead) = pr.first_commit_ts.map(|fc| ts - fc).filter(|&s| s >= 0) {
                fields.insert("lead_secs".into(), json!(lead));
            }
            out.push(PendingEvent {
                kind: "pr_merged",
                ts,
                fields,
            });
        } else if let Some(ts) = pr
            .closed_ts
            .filter(|_| pr.merged_ts.is_none() && missing("pr_closed"))
        {
            out.push(PendingEvent {
                kind: "pr_closed",
                ts,
                fields: base(),
            });
        }
        if let Some(ts) = pr.first_review_ts.filter(|_| missing("pr_review")) {
            out.push(PendingEvent {
                kind: "pr_review",
                ts,
                fields: base(),
            });
        }
    }
    out
}

/// The `(repo, number)` a GitHub PR URL points at — `https://github.com/eduuh/bn/pull/923` →
/// `("eduuh/bn", 923)`. Tolerates a trailing segment/query/fragment (`/files`, `?w=1`, `#issue`)
/// and the `/pulls/` spelling. `None` for anything that isn't a PR URL (an ADO link, a work-item
/// id, a bare note reference), so callers treat unknown links as "still live".
pub fn parse_pr_link(link: &str) -> Option<(String, i64)> {
    let (head, tail) = link
        .rsplit_once("/pull/")
        .or_else(|| link.rsplit_once("/pulls/"))?;
    let number: i64 = tail
        .split(['/', '?', '#'])
        .next()
        .filter(|n| !n.is_empty())
        .and_then(|n| n.parse().ok())?;
    // The two path segments before `/pull/` are the `owner/repo` slug the metrics events key on.
    let mut segs = head.rsplit('/');
    let repo = segs.next().filter(|s| !s.is_empty())?;
    let owner = segs.next().filter(|s| !s.is_empty())?;
    Some((format!("{owner}/{repo}"), number))
}

/// The PRs the local record knows have left the open set: every `(repo, number)` carrying a
/// `pr_merged` or `pr_closed` event in the metrics log. This is the offline source of truth for
/// "that PR is history" — the `pr-observe` job does the fetching, everything downstream reads
/// this set. Positive evidence only: a PR *absent* from the log is treated as still open (the
/// `pr-queue` cache drops non-open PRs entirely, so absence there means nothing).
pub fn terminal_prs(events: &[Event]) -> HashSet<(String, i64)> {
    events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "pr_merged" | "pr_closed"))
        .filter_map(|e| {
            let repo = e.fields.get("repo")?.as_str()?;
            let number = e.fields.get("number")?.as_i64()?;
            Some((repo.to_string(), number))
        })
        .collect()
}

/// Whether `link` points at a PR in `terminal` — the predicate that retires a PR-linked goal or
/// card item. A link that isn't a PR URL, or a PR with no terminal event recorded, is `false`.
pub fn link_is_terminal(terminal: &HashSet<(String, i64)>, link: &str) -> bool {
    parse_pr_link(link).is_some_and(|key| terminal.contains(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, repo: &str, number: i64) -> Event {
        let mut fields = Map::new();
        fields.insert("repo".into(), json!(repo));
        fields.insert("number".into(), json!(number));
        Event {
            kind: kind.into(),
            ts: 0,
            fields,
        }
    }

    #[test]
    fn merged_pr_emits_opened_and_merged_with_lead() {
        let pr = ObservedPr {
            repo: "o/r".into(),
            number: 7,
            created_ts: Some(1_000),
            merged_ts: Some(5_000),
            closed_ts: Some(5_000), // GitHub stamps closedAt on merge too — must not emit pr_closed
            first_commit_ts: Some(500),
            first_review_ts: Some(2_000),
        };
        let evs = derive_events(&[pr], &HashSet::new());
        let kinds: Vec<&str> = evs.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["pr_opened", "pr_merged", "pr_review"]);
        let merged = &evs[1];
        assert_eq!(merged.ts, 5_000);
        assert_eq!(merged.fields.get("lead_secs"), Some(&json!(4_500)));
    }

    #[test]
    fn closed_unmerged_pr_emits_closed() {
        let pr = ObservedPr {
            repo: "o/r".into(),
            number: 8,
            created_ts: Some(1_000),
            closed_ts: Some(3_000),
            ..Default::default()
        };
        let evs = derive_events(&[pr], &HashSet::new());
        let kinds: Vec<&str> = evs.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["pr_opened", "pr_closed"]);
    }

    #[test]
    fn already_logged_events_are_not_re_emitted() {
        let logged = vec![ev("pr_opened", "o/r", 7), ev("pr_review", "o/r", 7)];
        let seen = seen_keys(&logged);
        let pr = ObservedPr {
            repo: "o/r".into(),
            number: 7,
            created_ts: Some(1_000),
            merged_ts: Some(5_000),
            first_review_ts: Some(2_000),
            ..Default::default()
        };
        let evs = derive_events(std::slice::from_ref(&pr), &seen);
        let kinds: Vec<&str> = evs.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["pr_merged"], "opened + review already logged");
        // Same PR number in a different repo is a different identity.
        let other = ObservedPr {
            repo: "o/other".into(),
            ..pr
        };
        assert_eq!(derive_events(&[other], &seen).len(), 3);
    }

    #[test]
    fn negative_lead_is_dropped_not_stamped() {
        let pr = ObservedPr {
            repo: "o/r".into(),
            number: 9,
            merged_ts: Some(1_000),
            first_commit_ts: Some(2_000), // clock skew / rebase — later than the merge
            ..Default::default()
        };
        let evs = derive_events(&[pr], &HashSet::new());
        assert_eq!(evs.len(), 1);
        assert!(!evs[0].fields.contains_key("lead_secs"));
    }

    #[test]
    fn enrichment_needed_only_while_events_unknown() {
        let seen = seen_keys(&[ev("pr_merged", "o/r", 1), ev("pr_review", "o/r", 1)]);
        // Fully-known closed PR → no gh call.
        assert!(!needs_enrichment("o/r", 1, true, &seen));
        // Closed PR with no terminal event yet → enrich.
        assert!(needs_enrichment("o/r", 2, true, &seen));
        // Open PR with no review yet → enrich (to find the first review).
        assert!(needs_enrichment("o/r", 3, false, &seen));
        // Open PR whose review is already logged → the search row is enough.
        let seen_open = seen_keys(&[ev("pr_review", "o/r", 4)]);
        assert!(!needs_enrichment("o/r", 4, false, &seen_open));
    }

    #[test]
    fn pr_links_parse_to_the_metrics_identity() {
        assert_eq!(
            parse_pr_link("https://github.com/eduuh/wira360/pull/923"),
            Some(("eduuh/wira360".into(), 923))
        );
        // Trailing segment / query / fragment, and the `/pulls/` spelling.
        assert_eq!(
            parse_pr_link("https://github.com/eduuh/bn/pull/245/files?w=1"),
            Some(("eduuh/bn".into(), 245))
        );
        assert_eq!(
            parse_pr_link("https://github.com/eduuh/bn/pulls/7#discussion"),
            Some(("eduuh/bn".into(), 7))
        );
        // Not a PR URL → no identity, so callers leave the goal alone.
        assert_eq!(
            parse_pr_link("https://dev.azure.com/org/_workitems/12"),
            None
        );
        assert_eq!(parse_pr_link("AB#4312"), None);
        assert_eq!(parse_pr_link("https://github.com/eduuh/bn/pull/abc"), None);
    }

    #[test]
    fn terminal_set_covers_merged_and_closed_only() {
        let terminal = terminal_prs(&[
            ev("pr_opened", "eduuh/bn", 1),
            ev("pr_merged", "eduuh/bn", 2),
            ev("pr_closed", "eduuh/wira360", 923),
            ev("pr_review", "eduuh/bn", 3),
        ]);
        assert_eq!(terminal.len(), 2);
        assert!(link_is_terminal(
            &terminal,
            "https://github.com/eduuh/wira360/pull/923"
        ));
        assert!(link_is_terminal(
            &terminal,
            "https://github.com/eduuh/bn/pull/2/files"
        ));
        // Only opened/reviewed → still live.
        assert!(!link_is_terminal(
            &terminal,
            "https://github.com/eduuh/bn/pull/1"
        ));
        // Same number, different repo → different identity.
        assert!(!link_is_terminal(
            &terminal,
            "https://github.com/other/wira360/pull/923"
        ));
        // A non-PR link is never terminal.
        assert!(!link_is_terminal(&terminal, "AB#4312"));
    }
}
