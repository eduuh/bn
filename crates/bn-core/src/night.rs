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
