//! `bn night` — the nightly fleet planner. The piece that strings the existing autonomy together
//! while you sleep: it picks the top promoted todos, isolates each in its own worktree, dispatches
//! an agent, and leaves you a morning review queue of draft PRs.
//!
//! This module is the **pure, deterministic core**: given a ranked list of eligible candidates
//! (the promoted-todo queue in `bn next` order, each tagged with its repo/branch, changed files,
//! and live status) it produces a [`NightPlan`] — the ordered picks and the skip reasons — with no
//! I/O. The CLI gathers the inputs (attention items, git dirt, the live fleet, `git diff`) and
//! executes the plan by composing the same dispatch primitives `bn attn start` uses; the agents
//! then close the loop overnight via `bn agent done` (the verify gate + draft PR live there).
//!
//! Safety rails are enforced here so they can't be bypassed: only repos on the explicit allowlist
//! run, a hard `max_dispatch` cap is respected, and two items that touch overlapping files never
//! dispatch concurrently (the same conflict-avoidance idea as `wt merge --all`). The kill switch,
//! the run window, and the audit log round out the picture.

use crate::clock::Clock;
use crate::events;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Why a candidate was not dispatched this shift. Adjacently tagged so both the plain skips and
/// [`SkipReason::FileOverlap`] (which carries the branch it collides with) round-trip as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", content = "overlaps", rename_all = "snake_case")]
pub enum SkipReason {
    /// The candidate's repo is not on the `[night] repos` allowlist — the opt-in boundary. An
    /// empty allowlist means nothing runs, so autonomy never touches a repo you didn't name.
    NotAllowlisted,
    /// The candidate's branch has uncommitted changes — dispatching would fight a work-in-progress
    /// worktree, so it's left alone.
    DirtyBranch,
    /// A live agent session already owns the branch (the fleet knows) — don't launch a second.
    LiveSession,
    /// The candidate touches a file an already-picked item touches; running both concurrently
    /// risks a merge conflict, so it's serialized out of tonight's batch (carries that branch).
    FileOverlap(String),
    /// A higher-ranked pick this shift already targets this repo/branch — they would share one
    /// worktree, so the lower-ranked one is serialized to a later night (carries that branch).
    BranchBusy(String),
    /// The `max_dispatch` cap was already filled by higher-ranked picks.
    OverCap,
}

impl SkipReason {
    /// A short human label for the digest / dry-run listing.
    pub fn label(&self) -> String {
        match self {
            SkipReason::NotAllowlisted => "repo not on the night allowlist".into(),
            SkipReason::DirtyBranch => "branch has uncommitted changes".into(),
            SkipReason::LiveSession => "a live agent session already owns the branch".into(),
            SkipReason::FileOverlap(branch) => format!("file overlap with {branch} — serialized"),
            SkipReason::BranchBusy(branch) => {
                format!("another pick already targets {branch} — serialized")
            }
            SkipReason::OverCap => "over the max_dispatch cap".into(),
        }
    }
}

/// One ranked, eligible candidate for the shift — the pure input to [`plan`]. Eligibility (auto
/// mode, self-authored, a prompt + repo/branch) is established by the caller; this carries only
/// what the scheduler decides on: identity, the files it would touch, and its live status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    /// Files the branch already touches vs its base (`git diff --name-only base...branch`), for
    /// concurrency conflict-avoidance. Empty is fine — a fresh branch overlaps nothing.
    pub files: Vec<String>,
    /// The branch's worktree has uncommitted changes.
    pub dirty: bool,
    /// A live agent session already owns the branch.
    pub live_session: bool,
}

/// A candidate the shift will dispatch, in rank order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightPick {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
}

/// A candidate the shift declined, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightSkip {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    #[serde(flatten)]
    pub reason: SkipReason,
}

/// The outcome of planning a shift: what to dispatch and what was skipped (with reasons). Pure —
/// no side effects — so `bn night plan` is a true dry-run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NightPlan {
    pub picks: Vec<NightPick>,
    pub skips: Vec<NightSkip>,
}

/// Plan tonight's shift from ranked candidates. Walks them in rank order (highest-priority first,
/// as produced by `bn next`) applying the safety rails in fixed precedence so each skip reports
/// the most specific cause:
///
/// 1. allowlist (repo must be on `[night] repos`; an empty list admits nothing),
/// 2. dirty branch, 3. live agent session, 4. another pick already on the same repo/branch (they
///    would share one worktree — serialized), 5. file overlap with an already-picked item *in the
///    same repo* (serialized — the higher-ranked item wins the slot), 6. the `max_dispatch` cap.
///
/// Deterministic and I/O-free, so the whole selection is unit-testable.
pub fn plan(candidates: &[Candidate], allowlist: &[String], max_dispatch: usize) -> NightPlan {
    let mut out = NightPlan::default();
    // (repo_lc, branch) of each pick, to serialize a later same-branch candidate (one worktree).
    let mut picked_branches: HashSet<(String, String)> = HashSet::new();
    // (repo_lc, branch, files) of each pick, to serialize a later file-overlapping candidate in the
    // same repo. Overlap is repo-scoped: two repos touching `src/lib.rs` don't actually collide.
    let mut picked_files: Vec<(String, String, HashSet<String>)> = Vec::new();
    let allowed = |repo: &str| allowlist.iter().any(|r| r.eq_ignore_ascii_case(repo));

    for c in candidates {
        let skip = |reason: SkipReason| NightSkip {
            id: c.id.clone(),
            repo: c.repo.clone(),
            branch: c.branch.clone(),
            title: c.title.clone(),
            reason,
        };

        if !allowed(&c.repo) {
            out.skips.push(skip(SkipReason::NotAllowlisted));
            continue;
        }
        if c.dirty {
            out.skips.push(skip(SkipReason::DirtyBranch));
            continue;
        }
        if c.live_session {
            out.skips.push(skip(SkipReason::LiveSession));
            continue;
        }
        let repo_lc = c.repo.to_lowercase();
        // Same repo/branch as an earlier pick → they'd open the same worktree. Serialize.
        if picked_branches.contains(&(repo_lc.clone(), c.branch.clone())) {
            out.skips
                .push(skip(SkipReason::BranchBusy(c.branch.clone())));
            continue;
        }
        // File overlap with an earlier pick in the SAME repo → serialize.
        let my_files: HashSet<&str> = c.files.iter().map(String::as_str).collect();
        if let Some((_, branch, _)) = picked_files
            .iter()
            .find(|(r, _, fs)| *r == repo_lc && fs.iter().any(|f| my_files.contains(f.as_str())))
        {
            out.skips
                .push(skip(SkipReason::FileOverlap(branch.clone())));
            continue;
        }
        if out.picks.len() >= max_dispatch {
            out.skips.push(skip(SkipReason::OverCap));
            continue;
        }
        picked_branches.insert((repo_lc.clone(), c.branch.clone()));
        picked_files.push((repo_lc, c.branch.clone(), c.files.iter().cloned().collect()));
        out.picks.push(NightPick {
            id: c.id.clone(),
            repo: c.repo.clone(),
            branch: c.branch.clone(),
            title: c.title.clone(),
        });
    }
    out
}

// --- run window ------------------------------------------------------------------------------

/// Parse `HH:MM` (24h) to minutes-since-midnight. `None` on any malformed / out-of-range value.
pub fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h <= 23 && m <= 59).then_some(h * 60 + m)
}

/// Parse a `HH:MM-HH:MM` window (e.g. `23:00-06:00`) to `(start_min, end_min)`. `None` if either
/// endpoint is malformed.
pub fn parse_window(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('-')?;
    Some((parse_hm(a)?, parse_hm(b)?))
}

/// Whether `now_min` (minutes-since-midnight, local) falls inside `window`. Handles the common
/// overnight case where the window wraps midnight (`start > end`, e.g. 23:00–06:00). A degenerate
/// `start == end` window is treated as always-open.
pub fn in_window(now_min: u32, window: (u32, u32)) -> bool {
    let (start, end) = window;
    if start == end {
        return true;
    }
    if start < end {
        now_min >= start && now_min < end
    } else {
        now_min >= start || now_min < end
    }
}

/// Current local wall-clock as minutes-since-midnight, via the injectable [`Clock`] (its
/// `now_stamp` is `YYYY-MM-DD HH:MM`). `None` only if the stamp is unparseable.
pub fn local_minutes(clock: &dyn Clock) -> Option<u32> {
    let stamp = clock.now_stamp();
    let hm = stamp.split_whitespace().nth(1)?;
    parse_hm(hm)
}

// --- audit log -------------------------------------------------------------------------------

/// A dispatched-item record read back from the audit log, for the morning report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRecord {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    pub ts: i64,
}

/// Append the per-item audit entry for a dispatched pick (`night_dispatch`). This is the "audit
/// log entry per item" — the morning report reads these back, and `bn insights` can later mine
/// them for cost/outcome.
pub fn log_dispatch(
    clock: &dyn Clock,
    id: &str,
    repo: &str,
    branch: &str,
    title: &str,
    launched: bool,
) -> std::io::Result<()> {
    let mut f = Map::new();
    f.insert("id".into(), json!(id));
    f.insert("repo".into(), json!(repo));
    f.insert("branch".into(), json!(branch));
    f.insert("title".into(), json!(title));
    f.insert("launched".into(), json!(launched));
    events::append_at("night_dispatch", clock.now_epoch_secs(), f)
}

/// Append the shift-summary audit entry (`night_run`) — what was picked and why each skip was
/// skipped. The record of the run as a whole, complementing the per-item [`log_dispatch`] entries.
pub fn log_run(clock: &dyn Clock, plan: &NightPlan) -> std::io::Result<()> {
    let mut f = Map::new();
    f.insert("picked".into(), json!(plan.picks.len()));
    f.insert("skipped".into(), json!(plan.skips.len()));
    f.insert(
        "picks".into(),
        serde_json::to_value(&plan.picks).unwrap_or(Value::Null),
    );
    f.insert(
        "skips".into(),
        serde_json::to_value(&plan.skips).unwrap_or(Value::Null),
    );
    events::append_at("night_run", clock.now_epoch_secs(), f)
}

/// Dispatched-item audit records at or after `cutoff` (epoch secs), newest first. Backs the
/// morning report's "what the shift dispatched" list.
pub fn read_dispatches_since(cutoff: i64) -> Vec<DispatchRecord> {
    let mut out: Vec<DispatchRecord> = events::read_all()
        .into_iter()
        .filter(|e| e.kind == "night_dispatch" && e.ts >= cutoff)
        .map(|e| {
            let s = |k: &str| {
                e.fields
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            DispatchRecord {
                id: s("id"),
                repo: s("repo"),
                branch: s("branch"),
                title: s("title"),
                ts: e.ts,
            }
        })
        .collect();
    out.sort_by_key(|d| std::cmp::Reverse(d.ts));
    out
}

// --- morning report --------------------------------------------------------------------------

/// The current fate of a dispatched item, derived from its attention-item lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NightOutcome {
    /// Done and shipped a PR (the agent reported `bn attn done --link <pr>`) — a review is waiting.
    PrReady,
    /// Done but no PR (nothing to change, or the verify gate failed) — needs a look.
    Reported,
    /// Still in flight — the agent didn't finish overnight.
    Running,
    /// No trace in the attention lifecycle (acked/removed, or never started).
    Unknown,
}

impl NightOutcome {
    /// A short label for the digest.
    pub fn label(&self) -> &'static str {
        match self {
            NightOutcome::PrReady => "PR ready",
            NightOutcome::Reported => "reported (no PR)",
            NightOutcome::Running => "still running",
            NightOutcome::Unknown => "unknown",
        }
    }
}

/// Classify a dispatched item from its attention lifecycle: `started`/`done` flags and whether it
/// carries a PR link. Pure so the morning-digest branching is unit-testable.
pub fn classify_outcome(started: bool, done: bool, has_link: bool) -> NightOutcome {
    match (started, done, has_link) {
        (_, true, true) => NightOutcome::PrReady,
        (_, true, false) => NightOutcome::Reported,
        (true, false, _) => NightOutcome::Running,
        _ => NightOutcome::Unknown,
    }
}

/// One line of the morning report — a dispatched item and where it landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightReportItem {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    pub outcome: NightOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// The morning report for every autonomous dispatch at or after `cutoff` (epoch secs): each
/// dispatched item joined to its attention lifecycle to classify where it landed. A pure
/// composition of [`read_dispatches_since`], [`crate::attention::started_items`], and
/// [`classify_outcome`] — the single source of truth reused by both the AM chief card
/// (`bn staff chief`) and the `night-shift` findings gatherer, so the two can never drift.
pub fn report_since(cutoff: i64) -> Vec<NightReportItem> {
    let started: std::collections::HashMap<String, crate::attention::StartedItem> =
        crate::attention::started_items()
            .into_iter()
            .map(|s| (s.item.id.clone(), s))
            .collect();
    read_dispatches_since(cutoff)
        .into_iter()
        .map(|d| {
            let (is_started, done, link) = match started.get(&d.id) {
                Some(s) => (true, s.done, s.item.link.clone()),
                None => (false, false, None),
            };
            let outcome = classify_outcome(is_started, done, link.is_some());
            NightReportItem {
                id: d.id,
                repo: d.repo,
                branch: d.branch,
                title: d.title,
                outcome,
                link,
            }
        })
        .collect()
}

/// A morning-plan goal the report wants added to `bn daily` — one per PR waiting for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightGoal {
    pub text: String,
    pub link: Option<String>,
}

/// A compact PR reference like `bn#245` from the repo name + PR URL, falling back to the raw link
/// when the URL has no `/pull/<n>` (or `/pulls/<n>`) segment.
pub fn pr_slug(repo: &str, link: &str) -> String {
    let num = link
        .rsplit_once("/pull/")
        .or_else(|| link.rsplit_once("/pulls/"))
        .map(|(_, tail)| tail.trim_end_matches('/'))
        .and_then(|tail| tail.split(['/', '?', '#']).next())
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    match num {
        Some(n) => format!("{repo}#{n}"),
        None => link.to_string(),
    }
}

/// The morning-plan goal text for a PR waiting on review (`review night PR bn#245`).
pub fn goal_text(repo: &str, link: &str) -> String {
    format!("review night PR {}", pr_slug(repo, link))
}

/// Fold the report items into the daily goals to add — one per [`NightOutcome::PrReady`] item,
/// deduped by link so a re-run of the report never double-adds. Pure.
pub fn report_goals(items: &[NightReportItem]) -> Vec<NightGoal> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut goals = Vec::new();
    for it in items {
        if it.outcome != NightOutcome::PrReady {
            continue;
        }
        let Some(link) = it.link.as_deref() else {
            continue;
        };
        if !seen.insert(link) {
            continue;
        }
        goals.push(NightGoal {
            text: goal_text(&it.repo, link),
            link: Some(link.to_string()),
        });
    }
    goals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::test_support::with_state_dir_path as with_state_dir;

    fn cand(id: &str, repo: &str, branch: &str, files: &[&str]) -> Candidate {
        Candidate {
            id: id.into(),
            repo: repo.into(),
            branch: branch.into(),
            title: format!("todo {id}"),
            files: files.iter().map(|s| s.to_string()).collect(),
            dirty: false,
            live_session: false,
        }
    }

    fn ids(picks: &[NightPick]) -> Vec<&str> {
        picks.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn empty_allowlist_admits_nothing() {
        let plan = plan(&[cand("a", "bn", "feat-a", &[])], &[], 3);
        assert!(plan.picks.is_empty());
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].reason, SkipReason::NotAllowlisted);
    }

    #[test]
    fn allowlist_is_case_insensitive_and_gates_repo() {
        let cands = [
            cand("a", "BN", "feat-a", &[]),
            cand("b", "other", "feat-b", &[]),
        ];
        let plan = plan(&cands, &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["a"]);
        assert_eq!(plan.skips[0].id, "b");
        assert_eq!(plan.skips[0].reason, SkipReason::NotAllowlisted);
    }

    #[test]
    fn dirty_and_live_session_are_skipped_with_reason() {
        let mut dirty = cand("a", "bn", "feat-a", &[]);
        dirty.dirty = true;
        let mut live = cand("b", "bn", "feat-b", &[]);
        live.live_session = true;
        let ok = cand("c", "bn", "feat-c", &[]);
        let plan = plan(&[dirty, live, ok], &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["c"]);
        let by_id = |id: &str| {
            plan.skips
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.reason.clone())
                .unwrap()
        };
        assert_eq!(by_id("a"), SkipReason::DirtyBranch);
        assert_eq!(by_id("b"), SkipReason::LiveSession);
    }

    #[test]
    fn max_dispatch_caps_picks_in_rank_order() {
        let cands = [
            cand("a", "bn", "feat-a", &[]),
            cand("b", "bn", "feat-b", &[]),
            cand("c", "bn", "feat-c", &[]),
        ];
        let plan = plan(&cands, &["bn".into()], 2);
        assert_eq!(ids(&plan.picks), ["a", "b"]);
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].id, "c");
        assert_eq!(plan.skips[0].reason, SkipReason::OverCap);
    }

    #[test]
    fn max_dispatch_zero_picks_nothing() {
        let plan = plan(&[cand("a", "bn", "feat-a", &[])], &["bn".into()], 0);
        assert!(plan.picks.is_empty());
        assert_eq!(plan.skips[0].reason, SkipReason::OverCap);
    }

    #[test]
    fn file_overlap_serializes_lower_ranked_item() {
        // a and c share src/lib.rs; a is higher-ranked so it wins the slot, c is serialized.
        let cands = [
            cand("a", "bn", "feat-a", &["src/lib.rs", "src/a.rs"]),
            cand("b", "bn", "feat-b", &["src/b.rs"]),
            cand("c", "bn", "feat-c", &["src/lib.rs"]),
        ];
        let plan = plan(&cands, &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["a", "b"], "disjoint b still runs");
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].id, "c");
        assert_eq!(
            plan.skips[0].reason,
            SkipReason::FileOverlap("feat-a".into())
        );
    }

    #[test]
    fn disjoint_files_all_dispatch() {
        let cands = [
            cand("a", "bn", "feat-a", &["src/a.rs"]),
            cand("b", "bn", "feat-b", &["src/b.rs"]),
        ];
        let plan = plan(&cands, &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["a", "b"]);
        assert!(plan.skips.is_empty());
    }

    #[test]
    fn same_repo_branch_is_serialized_even_with_disjoint_files() {
        // Two todos promoted off the same branch note: same repo/branch, disjoint (or empty) files.
        // They'd open the same worktree, so the lower-ranked one must wait — like next_auto_batch.
        let cands = [
            cand("a", "bn", "feat-x", &["src/a.rs"]),
            cand("b", "bn", "feat-x", &["src/b.rs"]),
        ];
        let plan = plan(&cands, &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["a"]);
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].id, "b");
        assert_eq!(
            plan.skips[0].reason,
            SkipReason::BranchBusy("feat-x".into())
        );
    }

    #[test]
    fn fresh_branches_no_files_still_serialize_on_same_branch() {
        // Empty file sets (fresh branches) don't overlap by file, so only the branch check catches
        // the collision.
        let cands = [
            cand("a", "bn", "feat-x", &[]),
            cand("b", "bn", "feat-x", &[]),
        ];
        let plan = plan(&cands, &["bn".into()], 5);
        assert_eq!(ids(&plan.picks), ["a"]);
        assert_eq!(
            plan.skips[0].reason,
            SkipReason::BranchBusy("feat-x".into())
        );
    }

    #[test]
    fn file_overlap_is_repo_scoped() {
        // Two different repos both touch src/lib.rs — no real conflict, both run.
        let cands = [
            cand("a", "bn", "feat-a", &["src/lib.rs"]),
            cand("b", "other", "feat-b", &["src/lib.rs"]),
        ];
        let plan = plan(&cands, &["bn".into(), "other".into()], 5);
        assert_eq!(ids(&plan.picks), ["a", "b"]);
        assert!(plan.skips.is_empty());
    }

    #[test]
    fn window_no_wrap() {
        let w = parse_window("09:00-17:00").unwrap();
        assert_eq!(w, (540, 1020));
        assert!(!in_window(480, w)); // 08:00
        assert!(in_window(540, w)); // 09:00 inclusive
        assert!(in_window(1019, w)); // 16:59
        assert!(!in_window(1020, w)); // 17:00 exclusive
    }

    #[test]
    fn window_wraps_midnight() {
        let w = parse_window("23:00-06:00").unwrap();
        assert_eq!(w, (1380, 360));
        assert!(in_window(1380, w)); // 23:00
        assert!(in_window(0, w)); // midnight
        assert!(in_window(359, w)); // 05:59
        assert!(!in_window(360, w)); // 06:00 exclusive
        assert!(!in_window(720, w)); // noon
    }

    #[test]
    fn parse_hm_rejects_garbage() {
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("12:60"), None);
        assert_eq!(parse_hm("noon"), None);
        assert_eq!(parse_hm("7:5"), Some(7 * 60 + 5));
        assert_eq!(parse_window("23:00"), None);
    }

    #[test]
    fn local_minutes_from_clock() {
        // 1_700_000_000 = 2023-11-14T22:13:20Z; local minute depends on TZ but is well-formed.
        let m = local_minutes(&FixedClock::at(1_700_000_000)).unwrap();
        assert!(m < 24 * 60);
    }

    #[test]
    fn pr_slug_extracts_repo_hash_number() {
        assert_eq!(
            pr_slug("bn", "https://github.com/eduuh/bn/pull/245"),
            "bn#245"
        );
        assert_eq!(
            pr_slug("bn", "https://github.com/eduuh/bn/pull/245/files"),
            "bn#245"
        );
        // No /pull/ segment → fall back to the raw link.
        assert_eq!(
            pr_slug("bn", "https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn goal_text_is_review_night_pr() {
        assert_eq!(
            goal_text("bn", "https://github.com/eduuh/bn/pull/9"),
            "review night PR bn#9"
        );
    }

    #[test]
    fn classify_outcome_covers_lifecycle() {
        assert_eq!(classify_outcome(true, true, true), NightOutcome::PrReady);
        assert_eq!(classify_outcome(true, true, false), NightOutcome::Reported);
        assert_eq!(classify_outcome(true, false, false), NightOutcome::Running);
        assert_eq!(classify_outcome(false, false, false), NightOutcome::Unknown);
    }

    #[test]
    fn report_goals_one_per_pr_deduped() {
        let item = |id: &str, outcome: NightOutcome, link: Option<&str>| NightReportItem {
            id: id.into(),
            repo: "bn".into(),
            branch: format!("feat-{id}"),
            title: id.into(),
            outcome,
            link: link.map(str::to_string),
        };
        let items = [
            item(
                "a",
                NightOutcome::PrReady,
                Some("https://github.com/eduuh/bn/pull/1"),
            ),
            item("b", NightOutcome::Reported, None),
            item("c", NightOutcome::Running, None),
            // Duplicate link (report re-run) → not double-added.
            item(
                "d",
                NightOutcome::PrReady,
                Some("https://github.com/eduuh/bn/pull/1"),
            ),
            item(
                "e",
                NightOutcome::PrReady,
                Some("https://github.com/eduuh/bn/pull/2"),
            ),
        ];
        let goals = report_goals(&items);
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].text, "review night PR bn#1");
        assert_eq!(goals[1].text, "review night PR bn#2");
    }

    #[test]
    fn audit_log_round_trips() {
        with_state_dir(|_| {
            let clock = FixedClock::at(1000);
            let plan = NightPlan {
                picks: vec![NightPick {
                    id: "a".into(),
                    repo: "bn".into(),
                    branch: "feat-a".into(),
                    title: "t".into(),
                }],
                skips: vec![],
            };
            log_run(&clock, &plan).unwrap();
            log_dispatch(&clock, "a", "bn", "feat-a", "t", true).unwrap();
            let disp = read_dispatches_since(500);
            assert_eq!(disp.len(), 1);
            assert_eq!(disp[0].id, "a");
            assert_eq!(disp[0].branch, "feat-a");
            // Records before the cutoff are excluded.
            assert!(read_dispatches_since(2000).is_empty());
        });
    }

    #[test]
    fn report_since_joins_dispatches_to_lifecycle_and_orders_newest_first() {
        with_state_dir(|_| {
            // Two bare dispatches with no attention lifecycle → Unknown outcome.
            log_dispatch(
                &FixedClock::at(1000),
                "u1",
                "bn",
                "feat-1",
                "todo one",
                true,
            )
            .unwrap();
            log_dispatch(
                &FixedClock::at(1100),
                "u2",
                "bn",
                "feat-2",
                "todo two",
                true,
            )
            .unwrap();

            // A dispatch whose attention item started, finished, and shipped a PR → PrReady + link.
            let link = "https://github.com/eduuh/bn/pull/9";
            let id = crate::attention::add(
                "night",
                "ship it",
                "info",
                Some("bn"),
                Some("feat-x"),
                None,
                Some("k1"),
                "2026-07-23",
            )
            .unwrap()
            .id;
            crate::attention::mark_started(&id).unwrap();
            crate::attention::mark_done(&id, None, Some(link)).unwrap();
            log_dispatch(&FixedClock::at(1500), &id, "bn", "feat-x", "ship it", true).unwrap();

            let report = report_since(500);
            assert_eq!(report.len(), 3, "every dispatch after the cutoff");
            // Newest first, and the lifecycle join classifies the shipped one as PR-ready.
            assert_eq!(report[0].id, id);
            assert_eq!(report[0].outcome, NightOutcome::PrReady);
            assert_eq!(report[0].link.as_deref(), Some(link));
            assert_eq!(report[1].id, "u2");
            assert_eq!(report[1].outcome, NightOutcome::Unknown);
            assert_eq!(report[2].id, "u1");

            // The cutoff drops older dispatches, matching read_dispatches_since.
            let recent = report_since(1200);
            assert_eq!(recent.len(), 1);
            assert_eq!(recent[0].id, id);
        });
    }
}
