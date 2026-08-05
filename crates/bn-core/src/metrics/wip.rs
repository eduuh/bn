//! WIP / focus guard over open branches, and the daily fragmentation signal.

use super::flow::{compute, FLOW_GAP_SECS};
use super::visibility::{is_excluded_repo, visible_events};
use crate::events::{self, Event};
use crate::resolver::Config;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// In-flight branches at or above this idle threshold are "stale" — old enough that they're
/// drifting rather than being actively finished. 4 days matches the issue's guidance and the
/// `bn doctor` staleness feel without firing on a branch you touched yesterday.
const WIP_STALE_DAYS: i64 = 4;

/// More than this many in-flight branches and the WIP guard speaks up; at or below it the guard
/// stays quiet (`healthy = true`). Worktree-per-branch makes 1-2 parallel branches normal; the
/// tax shows past that.
const WIP_HEALTHY_MAX: usize = 2;

/// One open branch, reduced to just what the WIP guard ranks on. The recommendation picks the
/// branch *closest to merge* — fewest remaining open todos, freshest activity — so it's the one
/// least effort lands.
#[derive(Debug, Clone)]
pub struct BranchWip {
    pub repo: String,
    pub branch: String,
    /// Open todos remaining — the primary closest-to-merge signal (fewer ⇒ nearer done).
    pub open_todos: usize,
    /// Whole days since the branch's last recorded activity.
    pub idle_days: i64,
    /// Whether the branch has a PR open — a strong "almost there" signal.
    pub has_pr: bool,
}

/// Forward-looking WIP / focus guard: how many branches are in flight, which are drifting, the one
/// to finish next, and when the desk clears at the current merge cadence. The antidote to a
/// backward-only scorecard — it answers *what to finish* and *when you'll be clear*, not just
/// what already happened. Quiet (`healthy`) when in-flight branches are within [`WIP_HEALTHY_MAX`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WipGuard {
    /// Open branches in flight.
    pub in_flight: usize,
    /// Of those, how many are stale (idle ≥ [`WIP_STALE_DAYS`]).
    pub stale: usize,
    /// The branch to finish next (`repo/branch`), closest-to-merge — `None` when nothing is open.
    pub recommend: Option<String>,
    /// Why it was picked (open-todo count / idle / PR), for the human line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommend_reason: Option<String>,
    /// Forecast clear-by date (`YYYY-MM-DD`) at the historical merge cadence — `None` when the
    /// cadence is zero (no merges in the window ⇒ no basis to forecast).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_by: Option<String>,
    /// True when WIP is within the healthy band — surfaces stay quiet.
    pub healthy: bool,
}

/// Compute the WIP guard from the open branches, the window's merge throughput, and `today`.
/// Pure: the caller supplies the branch summaries and `merged_in_window` (the cadence numerator),
/// so it's testable with a [`FixedClock`]-derived `today` and no disk. `window_days` turns the
/// merge count into a per-day cadence for the clear-by forecast.
pub fn wip_guard(
    branches: &[BranchWip],
    merged_in_window: usize,
    window_days: i64,
    today: &str,
) -> WipGuard {
    let in_flight = branches.len();
    let stale = branches
        .iter()
        .filter(|b| b.idle_days >= WIP_STALE_DAYS)
        .count();

    // Closest to merge: a PR open beats none, then fewer open todos, then fresher (less idle).
    let pick = branches.iter().min_by(|a, b| {
        b.has_pr
            .cmp(&a.has_pr)
            .then(a.open_todos.cmp(&b.open_todos))
            .then(a.idle_days.cmp(&b.idle_days))
    });
    let recommend = pick.map(|b| format!("{}/{}", b.repo, b.branch));
    let recommend_reason = pick.map(|b| {
        let pr = if b.has_pr { "PR open · " } else { "" };
        format!("{pr}{} open todo(s) · idle {}d", b.open_todos, b.idle_days)
    });

    // Clear-by forecast: at `merged / window_days` merges per day, `in_flight` branches take
    // `in_flight / cadence` days to clear. Zero cadence ⇒ no forecast (can't divide by zero).
    let clear_by = if in_flight > 0 && merged_in_window > 0 && window_days > 0 {
        let per_day = merged_in_window as f64 / window_days as f64;
        let days = (in_flight as f64 / per_day).ceil() as i64;
        crate::util::add_days(today, days)
    } else {
        None
    };

    WipGuard {
        in_flight,
        stale,
        recommend,
        recommend_reason,
        clear_by,
        healthy: in_flight <= WIP_HEALTHY_MAX,
    }
}

/// Build the WIP guard for the current machine: open branches (active notes, excluded repos
/// dropped) ranked closest-to-merge, with a clear-by forecast from the window's merge cadence.
/// The impure convenience over [`wip_guard`] — reads notes + the synced metrics log so both the
/// scorecard and `bn next` can call it. `now` ages the cadence window; `today` ages each branch.
pub fn wip_guard_for(cfg: &Config, now: i64, window_days: i64, today: &str) -> WipGuard {
    let evs = visible_events(
        cfg,
        events::read_events_across(&cfg.metrics_logs_all_machines()),
    );
    let merged = compute(&evs, now, window_days).merged;
    let branches = open_branch_wips(cfg, today);
    wip_guard(&branches, merged, window_days, today)
}

/// The open branches on this machine as [`BranchWip`] rows (excluded repos dropped). The shared
/// note→rank mapping behind both the scorecard's WIP guard and `bn next`'s top row.
pub(crate) fn open_branch_wips(cfg: &Config, today: &str) -> Vec<BranchWip> {
    crate::rollups::all_notes(cfg)
        .into_iter()
        .filter(|n| !is_excluded_repo(&n.repo))
        .map(|n| {
            let last = crate::doctor::last_activity(&n.note);
            BranchWip {
                repo: n.repo.clone(),
                branch: n.branch.clone(),
                open_todos: n
                    .note
                    .front
                    .todos
                    .iter()
                    .filter(|t| t.is_open_todo())
                    .count(),
                idle_days: crate::util::idle_days_since(&last, today),
                has_pr: n.note.front.pr.is_some(),
            }
        })
        .collect()
}

/// Today's fragmentation picture — the DevEx *cognitive-load* axis the delivery metrics miss.
/// Frictionless worktree-switching is great for parallelism and terrible for focus; this puts a
/// number on that cost from telemetry `bn` already captures (the `activity`/`note_closed` stream,
/// each event stamped with its `repo`/`branch`). Neutral framing — a signal, not a scold.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Fragmentation {
    /// The day this picture is for (`YYYY-MM-DD`), in local time.
    pub date: String,
    /// Context-switches today: transitions between distinct `repo/branch` across consecutive
    /// activity events. Zero when the day stayed on one branch (or had ≤1 event).
    pub switches: usize,
    /// Distinct branches touched today.
    pub branches: usize,
    /// Longest uninterrupted stretch on a single branch (minutes) — the focus-block length.
    /// A stretch ends at a branch switch or a gap longer than [`FLOW_GAP_SECS`].
    pub longest_block_minutes: i64,
}

/// One day's branch-touch timeline → its [`Fragmentation`]. Pure over `(ts, branch_key)` pairs so
/// the sessionizing is trivially testable; [`fragmentation`] gathers them from the event log. The
/// pairs needn't be pre-sorted. A "switch" is a change in branch key between time-adjacent events;
/// a focus block runs while the key holds *and* the gap stays within [`FLOW_GAP_SECS`].
pub fn fragmentation_of(date: &str, mut events: Vec<(i64, String)>) -> Fragmentation {
    events.sort_by_key(|(ts, _)| *ts);
    let mut branches: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut switches = 0usize;
    let mut longest = 0i64;
    let mut block_start: Option<i64> = None;
    let mut prev: Option<(i64, &str)> = None;
    for (ts, key) in &events {
        branches.insert(key.as_str());
        match prev {
            None => block_start = Some(*ts),
            Some((pt, pk)) => {
                let broke = pk != key.as_str() || ts - pt > FLOW_GAP_SECS;
                if pk != key.as_str() {
                    switches += 1;
                }
                if broke {
                    longest = longest.max(pt - block_start.unwrap_or(pt));
                    block_start = Some(*ts);
                }
            }
        }
        prev = Some((*ts, key.as_str()));
    }
    if let (Some(bs), Some((pt, _))) = (block_start, prev) {
        longest = longest.max(pt - bs);
    }
    Fragmentation {
        date: date.to_string(),
        switches,
        branches: branches.len(),
        longest_block_minutes: longest / 60,
    }
}

/// The fragmentation picture for `today` (`YYYY-MM-DD`), from the activity stream. Each
/// `activity`/`note_closed` event landing on `today` (local date) contributes its `repo/branch`;
/// events with no branch attribution are skipped (they can't be a context-switch). Pure over the
/// event slice — the caller picks the source.
pub fn fragmentation(events: &[Event], today: &str) -> Fragmentation {
    let pairs: Vec<(i64, String)> = events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "activity" | "note_closed"))
        .filter(|e| crate::util::date_of(e.ts) == today)
        .filter_map(|e| {
            let repo = e.fields.get("repo").and_then(Value::as_str)?;
            let branch = e.fields.get("branch").and_then(Value::as_str)?;
            Some((e.ts, format!("{repo}/{branch}")))
        })
        .collect();
    fragmentation_of(today, pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ev;
    use serde_json::json;

    // ---- #108 focus / fragmentation signal ----

    #[test]
    fn fragmentation_counts_switches_and_longest_block() {
        let min = 60;
        let base = 50 * 86_400;
        // a,a (5m on bn/x) → switch to bn/y (10m later, 0m block) → back to bn/x after a 40m gap
        // (> FLOW_GAP, so a fresh block) for 5m. Switches: x→y, y→x = 2. Branches: 2. Longest
        // single-branch uninterrupted block = the first 5m on bn/x.
        let pairs = vec![
            (base, "bn/x".to_string()),
            (base + 5 * min, "bn/x".to_string()),
            (base + 15 * min, "bn/y".to_string()),
            (base + 55 * min, "bn/x".to_string()),
            (base + 60 * min, "bn/x".to_string()),
        ];
        let f = fragmentation_of("2026-06-26", pairs);
        assert_eq!(f.switches, 2, "x→y and y→x");
        assert_eq!(f.branches, 2);
        assert_eq!(f.longest_block_minutes, 5, "two 5m blocks; longest is 5m");
        assert_eq!(f.date, "2026-06-26");
    }

    #[test]
    fn fragmentation_single_branch_day_has_no_switches() {
        let min = 60;
        let pairs = vec![
            (1000, "bn/x".to_string()),
            (1000 + 10 * min, "bn/x".to_string()),
            (1000 + 20 * min, "bn/x".to_string()),
        ];
        let f = fragmentation_of("2026-06-26", pairs);
        assert_eq!(f.switches, 0, "stayed on one branch");
        assert_eq!(f.branches, 1);
        assert_eq!(f.longest_block_minutes, 20, "one unbroken 20m block");
    }

    #[test]
    fn fragmentation_filters_to_today_and_needs_branch() {
        use crate::clock::{Clock, FixedClock};
        // Pin "today" through the clock so the day-bucketing is deterministic.
        let today = FixedClock::at(1_750_000_000).today();
        let t0 = 1_750_000_000;
        let evs = vec![
            ev(
                "activity",
                t0,
                &[("repo", json!("o/r")), ("branch", json!("a"))],
            ),
            ev(
                "note_closed",
                t0 + 600,
                &[("repo", json!("o/r")), ("branch", json!("b"))],
            ),
            // no branch attribution → skipped (can't be a switch).
            ev("activity", t0 + 1200, &[("repo", json!("o/r"))]),
            // a different day → excluded.
            ev(
                "activity",
                t0 - 5 * 86_400,
                &[("repo", json!("o/r")), ("branch", json!("c"))],
            ),
        ];
        let f = fragmentation(&evs, &today);
        assert_eq!(f.branches, 2, "a and b today; c is another day");
        assert_eq!(f.switches, 1, "a→b");
    }

    // ---- #97 WIP / focus guard + clear-by forecast ----

    fn wip(repo: &str, branch: &str, open_todos: usize, idle_days: i64, has_pr: bool) -> BranchWip {
        BranchWip {
            repo: repo.into(),
            branch: branch.into(),
            open_todos,
            idle_days,
            has_pr,
        }
    }

    #[test]
    fn wip_guard_quiet_when_healthy() {
        // Two in-flight branches is within the healthy band → the guard stays quiet.
        let g = wip_guard(
            &[wip("o/r", "a", 1, 0, false), wip("o/r", "b", 2, 1, false)],
            5,
            30,
            "2026-06-26",
        );
        assert!(g.healthy);
        assert_eq!(g.in_flight, 2);
    }

    #[test]
    fn wip_guard_recommends_closest_to_merge_and_forecasts_clear_by() {
        use crate::clock::{Clock, FixedClock};
        // today derived from the clock so the clear-by forecast is deterministic.
        let today = FixedClock::at(1_750_000_000).today();
        let branches = vec![
            wip("o/r", "fresh-no-pr", 0, 0, false),
            wip("o/r", "has-pr", 2, 1, true), // PR open ⇒ closest to merge despite more todos
            wip("o/r", "stale", 1, 9, false), // idle ≥4d ⇒ stale
            wip("o/r", "busy", 5, 0, false),
        ];
        // Cadence: 15 merges over 30 days = 0.5/day ⇒ 4 branches take ceil(4/0.5)=8 days.
        let g = wip_guard(&branches, 15, 30, &today);
        assert!(!g.healthy, "4 in flight is above the band");
        assert_eq!(g.in_flight, 4);
        assert_eq!(g.stale, 1, "only the 9-day-idle branch is stale");
        assert_eq!(
            g.recommend.as_deref(),
            Some("o/r/has-pr"),
            "PR beats todo count"
        );
        assert_eq!(
            g.clear_by.as_deref(),
            crate::util::add_days(&today, 8).as_deref()
        );
    }

    #[test]
    fn wip_guard_no_forecast_without_merge_cadence() {
        let g = wip_guard(
            &[
                wip("o/r", "a", 1, 0, false),
                wip("o/r", "b", 1, 0, false),
                wip("o/r", "c", 1, 0, false),
            ],
            0, // no merges in the window → nothing to forecast from
            30,
            "2026-06-26",
        );
        assert!(!g.healthy);
        assert_eq!(g.clear_by, None, "zero cadence ⇒ no clear-by date");
        assert!(g.recommend.is_some(), "still recommends one to finish");
    }
}
