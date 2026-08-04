//! Weekly auto-retro (#244) — mine the telemetry bn already collects into one read-only,
//! network-free retrospective. Nothing here writes or fetches: it reads the same stores
//! `bn track report`, `bn weekly`, and `bn metrics` already parse and folds them into a small
//! table of personal-engineering metrics plus **one** heuristic suggestion.
//!
//! Sources, all reused (never re-implemented):
//! - context switches — the punch log via [`crate::track`] + [`crate::metrics::fragmentation_of`]
//! - deep-work ratio — focus blocks via [`crate::focus::secs_in_window`] ÷ tracked time
//! - PR cycle-time trend + close-out lag — the metrics event log via [`crate::metrics`]
//! - open-todo age — branch notes via [`crate::rollups`]
//! - ship-day histogram — `pr_merged` events bucketed by weekday
//! - goal completion — the daily files via [`crate::daily`]
//!
//! On top of the retro sits the **scoreboard** (#276): the 2–3 success numbers chosen before the
//! agent-staff ships (PR cycle time, deep-work ratio) positioned against a fixed baseline anchor
//! recorded once via `bn insights --set-baseline`, plus the permanent **bn-time guard** — how much
//! tracked time went to the bn repo itself vs everything else — the honest check against the tool
//! becoming the hobby. The anchor is the only thing here that writes, and only on `--set-baseline`.
//!
//! The window is anchored exactly like `bn weekly` ([`crate::reports::window_start`]); `--since N`
//! overrides it with a flat N-day window. Every metric degrades to a "not enough data" shape on a
//! sparse machine rather than erroring, so the run is always total.

use crate::events::{read_all, read_events_across, Event};
use crate::metrics::CloseOut;
use crate::resolver::Config;
use crate::{daily, focus, metrics, reports, rollups, track, util};
use serde::{Deserialize, Serialize};

const DAY: i64 = 86_400;
/// Open todos older than this (whole days) count toward the stale-backlog figure and rule.
const TODO_STALE_DAYS: i64 = 14;
const NA: &str = "not enough data";
/// Weekday labels, Monday-first — matches [`ShipDays::by_weekday`]'s index order.
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Context-switch fragmentation over the window, from the punch log — how often the day's active
/// activity changed. `mean_per_day`/`worst_day` are `None` when no day in the window has punches.
#[derive(Debug, Serialize)]
pub struct ContextSwitches {
    pub days_tracked: usize,
    pub total: usize,
    pub mean_per_day: Option<f64>,
    pub worst_day: Option<DaySwitches>,
}

/// The single most-fragmented day in the window.
#[derive(Debug, Serialize)]
pub struct DaySwitches {
    pub date: String,
    pub switches: usize,
}

/// Deep-work ratio: realized focus-block seconds ÷ total tracked seconds. `ratio` is `None`
/// when nothing was tracked (division would be undefined).
#[derive(Debug, Serialize)]
pub struct DeepWork {
    pub focus_secs: i64,
    pub tracked_secs: i64,
    pub ratio: Option<f64>,
}

/// PR open→merge cycle-time trend: this window vs the immediately-preceding one of equal length.
/// Medians are `None` when nothing merged in the respective window.
#[derive(Debug, Serialize)]
pub struct PrCycleTrend {
    pub merged: usize,
    pub prev_merged: usize,
    pub median_hours: Option<f64>,
    pub prev_median_hours: Option<f64>,
}

/// Ages (whole days, as of the report date) of every currently-open todo across branch notes.
/// Percentiles are `None` when nothing is open.
#[derive(Debug, Serialize)]
pub struct TodoAge {
    pub open_total: usize,
    pub p50_days: Option<i64>,
    pub p90_days: Option<i64>,
    pub over_stale_days: usize,
    pub stale_threshold_days: i64,
}

/// Merges bucketed by local weekday over the window. `by_weekday[0]` is Monday … `[6]` Sunday.
#[derive(Debug, Serialize)]
pub struct ShipDays {
    pub total: usize,
    pub by_weekday: [usize; 7],
}

/// Daily-goal completion over the window: goals finished vs still carried on the latest day.
/// `rate` is `None` when the window has no goals at all.
#[derive(Debug, Serialize)]
pub struct GoalCompletion {
    pub completed: usize,
    pub carried: usize,
    pub rate: Option<f64>,
}

/// The bn-time meta-trap guard (#276): how much tracked time went to the bn repo itself vs
/// everything else, over the window. The failure mode of a powerful personal system is it
/// quietly becoming the hobby; this number keeps that honest and stays on the scoreboard
/// permanently. `ratio` is `bn_secs / tracked_secs`, `None` when nothing was tracked.
#[derive(Debug, Serialize)]
pub struct BnTimeGuard {
    pub bn_secs: i64,
    pub other_secs: i64,
    pub tracked_secs: i64,
    pub ratio: Option<f64>,
}

/// One success metric on the scoreboard, positioned against the fixed pre-staff baseline anchor
/// (#276). `baseline` is the value recorded when `bn insights --set-baseline` last ran; it is
/// `None` until an anchor exists. `lower_is_better` drives whether a move reads as an improvement.
#[derive(Debug, Serialize)]
pub struct ScoreLine {
    /// Stable machine-facing key (matches [`ScoreboardBaseline`]'s fields).
    pub key: String,
    pub label: String,
    pub current: Option<f64>,
    pub baseline: Option<f64>,
    pub lower_is_better: bool,
}

impl ScoreLine {
    /// Relative percent change from baseline to current, `None` without both (or a zero baseline).
    pub fn pct_change(&self) -> Option<f64> {
        match (self.current, self.baseline) {
            (Some(c), Some(b)) if b.abs() > f64::EPSILON => Some((c - b) / b * 100.0),
            _ => None,
        }
    }

    /// Whether the current reading is an improvement over the baseline (direction-aware). `None`
    /// until both a current value and a baseline exist.
    pub fn improved(&self) -> Option<bool> {
        match (self.current, self.baseline) {
            (Some(c), Some(b)) => Some(if self.lower_is_better { c < b } else { c > b }),
            _ => None,
        }
    }
}

/// The scoreboard (#276): the success numbers picked before the agent-staff ships, each positioned
/// against a fixed baseline anchor, plus the permanent bn-time meta-trap guard. Rendered at the
/// head of `bn insights` so "are we better than the pre-staff baseline?" has one honest answer
/// instead of vibes.
#[derive(Debug, Serialize)]
pub struct Scoreboard {
    pub bn_time: BnTimeGuard,
    /// The baseline bn-time ratio, if an anchor exists — so the guard can show whether the hobby
    /// has grown since the baseline was set.
    pub bn_time_baseline: Option<f64>,
    /// Success metrics vs baseline, in display order.
    pub metrics: Vec<ScoreLine>,
    /// The date the baseline anchor was captured, if one exists.
    pub baseline_date: Option<String>,
}

/// The fixed baseline anchor persisted at [`Config::scoreboard_baseline_path`] (#276): a
/// point-in-time snapshot of the scoreboard numbers, recorded once (before the agent-staff ships)
/// via `bn insights --set-baseline`, then compared against on every run. Distinct from the rolling
/// weekly [`crate::metrics::Snapshot`] history — this is one durable comparison anchor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreboardBaseline {
    /// The date the anchor was captured (`YYYY-MM-DD`).
    pub date: String,
    /// Median PR cycle time (hours) at baseline.
    #[serde(default)]
    pub cycle_median_hours: Option<f64>,
    /// Deep-work ratio (focus ÷ tracked) at baseline.
    #[serde(default)]
    pub deep_work_ratio: Option<f64>,
    /// bn-time guard ratio at baseline.
    #[serde(default)]
    pub bn_time_ratio: Option<f64>,
}

/// The full weekly retro. Serializes verbatim into the `--json` envelope payload.
#[derive(Debug, Serialize)]
pub struct Insights {
    pub start: String,
    pub end: String,
    pub window_days: i64,
    /// The scoreboard headline: success metrics vs the fixed baseline anchor + the bn-time guard.
    pub scoreboard: Scoreboard,
    pub context_switches: ContextSwitches,
    pub deep_work: DeepWork,
    pub pr_cycle: PrCycleTrend,
    pub close_out: CloseOut,
    pub todo_age: TodoAge,
    pub ship_days: ShipDays,
    pub goal_completion: GoalCompletion,
    /// Per-agent staff engagement (#268): cards read %, items acted %, ratings, and the fire-me
    /// flag — over the fixed [`crate::staff::REVIEW_WINDOW_DAYS`] window, independent of `--since`.
    /// Empty when no staff agent produced a card or rating in that window.
    pub staff_review: Vec<crate::staff::AgentReview>,
    /// The single highest-severity heuristic suggestion, or `None` when nothing fired.
    pub suggestion: Option<String>,
}

/// Build the weekly retro as of `today`. `since` overrides the anchored window with a rolling
/// N-day window (mirrors `bn weekly`). Pure read: touches no network and writes nothing.
pub fn insights(cfg: &Config, today: &str, since: Option<i64>) -> Insights {
    let start = reports::window_start(cfg, today, since);
    let window_days = util::days_since(&start, today).unwrap_or(7) + 1;
    let now = util::now_epoch_secs();

    let context_switches = compute_context_switches(cfg, &start, today);
    let deep_work = compute_deep_work(cfg, &start, today, now);

    // PR metrics read the whole-person, repo-visible event view — the same source `bn weekly`
    // and `bn metrics` use, so the numbers agree across surfaces.
    let evs = metrics::visible_events(cfg, read_events_across(&cfg.metrics_logs_all_machines()));
    let cur = metrics::compute(&evs, now, window_days);
    // The previous window is [boundary - window, boundary). `boundary` is exactly `compute`'s own
    // current-window cutoff, so re-running `compute` over the events strictly before it — with
    // `boundary` as the synthetic "now" — yields the prior window using the identical method, for
    // a fair trend comparison without any new date→epoch plumbing.
    let boundary = now - window_days.max(0) * DAY;
    let prev_evs: Vec<Event> = evs.iter().filter(|e| e.ts < boundary).cloned().collect();
    let prev = metrics::compute(&prev_evs, boundary, window_days);
    let pr_cycle = PrCycleTrend {
        merged: cur.merged,
        prev_merged: prev.merged,
        median_hours: cur.cycle_median_hours,
        prev_median_hours: prev.cycle_median_hours,
    };
    let close_out = metrics::close_out_lag(&evs, now, window_days);
    let ship_days = compute_ship_days(&evs, boundary);

    let todo_age = compute_todo_age(cfg, today);
    let goal_completion = compute_goal_completion(cfg, &start, today);

    // The scoreboard folds two already-computed success metrics (PR cycle time, deep-work ratio)
    // and the fresh bn-time guard against the fixed baseline anchor. Read the Option<f64> figures
    // before their owners move into the struct below.
    let scoreboard = build_scoreboard(
        cfg,
        &start,
        today,
        now,
        pr_cycle.median_hours,
        deep_work.ratio,
    );

    let mut ins = Insights {
        start,
        end: today.to_string(),
        window_days,
        scoreboard,
        context_switches,
        deep_work,
        pr_cycle,
        close_out,
        todo_age,
        ship_days,
        goal_completion,
        staff_review: crate::staff::performance_review(cfg, today),
        suggestion: None,
    };
    ins.suggestion = suggest(&ins);
    ins
}

/// True when `repo` names the bn repo itself, matched on the trailing path segment so both the
/// bare `bn` name and an `owner/bn` form resolve the same (mirrors [`metrics::is_excluded_repo`]'s
/// basename match). Backs the bn-time meta-trap guard.
fn is_bn_repo(repo: &str) -> bool {
    repo.rsplit('/')
        .next()
        .unwrap_or(repo)
        .eq_ignore_ascii_case("bn")
}

/// The bn-time meta-trap guard over the window: tracked seconds attributed to the bn repo vs
/// everything else. Reads the punch log one day at a time, capping each day's open interval at
/// [`day_cap`] exactly like [`tracked_secs_in`], so `tracked_secs` here agrees with the deep-work
/// block's denominator.
fn compute_bn_time(cfg: &Config, start: &str, end: &str, now: i64) -> BnTimeGuard {
    let mut bn_secs = 0i64;
    let mut tracked_secs = 0i64;
    let mut cur = start.to_string();
    while cur.as_str() <= end {
        let punches = track::read_day(cfg, &cur);
        for iv in track::intervals(&punches, day_cap(&cur, now)) {
            tracked_secs += iv.secs;
            if iv.repo.as_deref().is_some_and(is_bn_repo) {
                bn_secs += iv.secs;
            }
        }
        match util::add_days(&cur, 1) {
            Some(n) => cur = n,
            None => break,
        }
    }
    let other_secs = (tracked_secs - bn_secs).max(0);
    let ratio = (tracked_secs > 0).then(|| bn_secs as f64 / tracked_secs as f64);
    BnTimeGuard {
        bn_secs,
        other_secs,
        tracked_secs,
        ratio,
    }
}

/// Assemble the scoreboard: the bn-time guard plus the success metrics positioned against the
/// fixed baseline anchor (absent until `bn insights --set-baseline` has run).
fn build_scoreboard(
    cfg: &Config,
    start: &str,
    end: &str,
    now: i64,
    cycle_median_hours: Option<f64>,
    deep_work_ratio: Option<f64>,
) -> Scoreboard {
    let bn_time = compute_bn_time(cfg, start, end, now);
    let baseline = read_baseline(cfg);
    let metrics = vec![
        ScoreLine {
            key: "cycle_median_hours".into(),
            label: "PR cycle time".into(),
            current: cycle_median_hours,
            baseline: baseline.as_ref().and_then(|b| b.cycle_median_hours),
            lower_is_better: true,
        },
        ScoreLine {
            key: "deep_work_ratio".into(),
            label: "Deep-work ratio".into(),
            current: deep_work_ratio,
            baseline: baseline.as_ref().and_then(|b| b.deep_work_ratio),
            lower_is_better: false,
        },
    ];
    Scoreboard {
        bn_time,
        bn_time_baseline: baseline.as_ref().and_then(|b| b.bn_time_ratio),
        metrics,
        baseline_date: baseline.map(|b| b.date),
    }
}

/// Read the fixed baseline anchor from [`Config::scoreboard_baseline_path`]. Missing or
/// unparseable → `None` (the scoreboard simply renders without a comparison).
pub fn read_baseline(cfg: &Config) -> Option<ScoreboardBaseline> {
    let text = std::fs::read_to_string(cfg.scoreboard_baseline_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Capture the current scoreboard numbers as the fixed baseline anchor (#276), overwriting any
/// prior anchor. Snapshots the pre-staff success metrics + bn-time guard so later runs can render
/// "vs baseline". Returns the path written. Backs `bn insights --set-baseline`.
pub fn set_baseline(cfg: &Config, ins: &Insights) -> std::io::Result<std::path::PathBuf> {
    let baseline = ScoreboardBaseline {
        date: ins.end.clone(),
        cycle_median_hours: ins.pr_cycle.median_hours,
        deep_work_ratio: ins.deep_work.ratio,
        bn_time_ratio: ins.scoreboard.bn_time.ratio,
    };
    let path = cfg.scoreboard_baseline_path();
    crate::store::save_json_atomic(&path, &baseline)?;
    Ok(path)
}

/// Count activity transitions per day across the punch log, reusing the pure switch counter the
/// metrics scorecard already uses. Each `track_in` contributes `(ts, activity)`; a "switch" is a
/// change in activity between time-adjacent punches (re-punching the same activity is not one).
fn compute_context_switches(cfg: &Config, start: &str, end: &str) -> ContextSwitches {
    let mut total = 0usize;
    let mut days_tracked = 0usize;
    let mut worst: Option<DaySwitches> = None;
    let mut cur = start.to_string();
    while cur.as_str() <= end {
        let pairs: Vec<(i64, String)> = track::read_day(cfg, &cur)
            .into_iter()
            .filter(|p| p.is_in && !p.activity.is_empty())
            .map(|p| (p.ts, p.activity))
            .collect();
        if !pairs.is_empty() {
            days_tracked += 1;
            let switches = metrics::fragmentation_of(&cur, pairs).switches;
            total += switches;
            if worst.as_ref().is_none_or(|w| switches > w.switches) {
                worst = Some(DaySwitches {
                    date: cur.clone(),
                    switches,
                });
            }
        }
        match util::add_days(&cur, 1) {
            Some(n) => cur = n,
            None => break,
        }
    }
    let mean_per_day = (days_tracked > 0).then(|| total as f64 / days_tracked as f64);
    ContextSwitches {
        days_tracked,
        total,
        mean_per_day,
        worst_day: worst,
    }
}

/// Focus-block seconds (machine-local focus events) over total tracked seconds (the punch log).
fn compute_deep_work(cfg: &Config, start: &str, end: &str, now: i64) -> DeepWork {
    let tracked_secs = tracked_secs_in(cfg, start, end, now);
    let start_epoch = util::parse_ts(start).unwrap_or(0);
    let end_epoch = day_cap(end, now);
    let focus_secs = focus::secs_in_window(&read_all(), start_epoch, end_epoch);
    let ratio = (tracked_secs > 0).then(|| focus_secs as f64 / tracked_secs as f64);
    DeepWork {
        focus_secs,
        tracked_secs,
        ratio,
    }
}

/// Total tracked seconds across the window, one day at a time. Each day's still-open interval is
/// capped at [`day_cap`] so a dangling punch on a past day can't inflate the total to "now".
fn tracked_secs_in(cfg: &Config, start: &str, end: &str, now: i64) -> i64 {
    let mut total = 0i64;
    let mut cur = start.to_string();
    while cur.as_str() <= end {
        let punches = track::read_day(cfg, &cur);
        let ivs = track::intervals(&punches, day_cap(&cur, now));
        total += track::total_secs(&ivs);
        match util::add_days(&cur, 1) {
            Some(n) => cur = n,
            None => break,
        }
    }
    total
}

/// The instant to close a day's still-open interval / bound the focus window: the local midnight
/// that ends `date`, but never past `now` (so today caps at the present instant, a past day at
/// its own midnight). Falls back to `now` if the date won't parse.
fn day_cap(date: &str, now: i64) -> i64 {
    util::add_days(date, 1)
        .and_then(|next| util::parse_ts(&next))
        .map(|midnight| midnight.min(now))
        .unwrap_or(now)
        .max(0)
}

/// Bucket `pr_merged` events in the current window (`ts >= boundary`) by local weekday.
fn compute_ship_days(evs: &[Event], boundary: i64) -> ShipDays {
    let mut by_weekday = [0usize; 7];
    let mut total = 0usize;
    for e in evs
        .iter()
        .filter(|e| e.kind == "pr_merged" && e.ts >= boundary)
    {
        // weekday_index is Sunday=0..Saturday=6; rotate to Monday=0..Sunday=6.
        if let Some(sun_idx) = util::weekday_index(&util::date_of(e.ts)) {
            by_weekday[((sun_idx + 6) % 7) as usize] += 1;
            total += 1;
        }
    }
    ShipDays { total, by_weekday }
}

/// Age distribution of every open todo across all active local branch notes, as of `today`.
fn compute_todo_age(cfg: &Config, today: &str) -> TodoAge {
    let mut ages: Vec<i64> = Vec::new();
    for bn in rollups::all_notes(cfg) {
        for t in bn.note.front.todos.iter().filter(|t| t.is_open_todo()) {
            ages.push(util::days_since(&t.created, today).unwrap_or(0).max(0));
        }
    }
    ages.sort_unstable();
    let over_stale_days = ages.iter().filter(|&&a| a > TODO_STALE_DAYS).count();
    TodoAge {
        open_total: ages.len(),
        p50_days: percentile(&ages, 0.5),
        p90_days: percentile(&ages, 0.9),
        over_stale_days,
        stale_threshold_days: TODO_STALE_DAYS,
    }
}

/// Daily-goal completion: goals finished across the window vs those still carried on its last day
/// (carried-forward opens collapse into that day's set, so each unfinished goal counts once).
fn compute_goal_completion(cfg: &Config, start: &str, end: &str) -> GoalCompletion {
    let days = daily::read_window(cfg, start, end);
    let completed: usize = days.iter().map(|d| d.completed().count()).sum();
    let carried = days.last().map(|d| d.open().count()).unwrap_or(0);
    let denom = completed + carried;
    let rate = (denom > 0).then(|| completed as f64 / denom as f64);
    GoalCompletion {
        completed,
        carried,
        rate,
    }
}

/// Nearest-rank percentile of a pre-sorted slice (same method as [`crate::metrics`]). `None` when
/// empty.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    Some(sorted[idx])
}

/// The single heuristic suggestion for this retro. Rules are an ordered table by severity: the
/// first one that fires wins, so a run ends with one pointed nudge rather than a wall of stats
/// (#244). Pure over the computed [`Insights`], so each rule is unit-testable in isolation.
pub fn suggest(ins: &Insights) -> Option<String> {
    // 1) PR cycle time regressed sharply versus the previous window.
    if let (Some(cur), Some(prev)) = (ins.pr_cycle.median_hours, ins.pr_cycle.prev_median_hours) {
        if ins.pr_cycle.merged >= 2 && prev > 0.0 && cur >= prev * 1.5 {
            return Some(format!(
                "PR cycle time rose from {} to {} versus last week — smaller PRs merge faster.",
                fmt_hours(prev),
                fmt_hours(cur)
            ));
        }
    }

    // 2) Fragmented days with little protected focus.
    if let (Some(mean), Some(ratio)) = (ins.context_switches.mean_per_day, ins.deep_work.ratio) {
        if mean >= 5.0 && ratio < 0.3 {
            return Some(format!(
                "You averaged {mean:.1} context switches/day with only {:.0}% deep-work time — try longer focus blocks.",
                ratio * 100.0
            ));
        }
    }

    // 3) Merges pile up late in the week and then stall over the weekend.
    if ins.ship_days.total >= 3 {
        let late = ins.ship_days.by_weekday[3] + ins.ship_days.by_weekday[4]; // Thu + Fri
        if late * 2 > ins.ship_days.total {
            return Some(
                "Most of your PRs land Thu–Fri; open them mid-week so they don't stall into the weekend."
                    .to_string(),
            );
        }
    }

    // 4) A growing backlog of stale todos.
    if ins.todo_age.over_stale_days >= 5 {
        return Some(format!(
            "{} open todos are older than {} days — close or drop them to clear the backlog.",
            ins.todo_age.over_stale_days, ins.todo_age.stale_threshold_days
        ));
    }

    // 5) Low daily-goal completion.
    if let Some(rate) = ins.goal_completion.rate {
        if ins.goal_completion.completed + ins.goal_completion.carried >= 4 && rate < 0.5 {
            return Some(format!(
                "You finished {:.0}% of your daily goals — set fewer, sharper goals you can close.",
                rate * 100.0
            ));
        }
    }

    None
}

/// Render the retro as the human table + one suggestion. Each metric line degrades to
/// "not enough data" when its underlying figure is absent.
pub fn render(ins: &Insights) -> String {
    let mut out = format!(
        "Weekly retro — {} … {}  ({}d)\n\n",
        ins.start, ins.end, ins.window_days
    );

    out.push_str(&render_scoreboard(&ins.scoreboard));
    out.push('\n');

    let cs = match (
        &ins.context_switches.mean_per_day,
        &ins.context_switches.worst_day,
    ) {
        (Some(mean), Some(worst)) => {
            format!(
                "{mean:.1}/day  (worst {} on {})",
                worst.switches, worst.date
            )
        }
        _ => NA.to_string(),
    };
    out.push_str(&row("Context switches", &cs));

    let dw = match ins.deep_work.ratio {
        Some(r) => format!(
            "{:.0}%  ({} focus / {} tracked)",
            r * 100.0,
            fmt_secs(ins.deep_work.focus_secs),
            fmt_secs(ins.deep_work.tracked_secs)
        ),
        None => NA.to_string(),
    };
    out.push_str(&row("Deep-work ratio", &dw));

    let pc = match ins.pr_cycle.median_hours {
        Some(cur) => {
            let trend = match ins.pr_cycle.prev_median_hours {
                Some(prev) => format!(" (prev {})", fmt_hours(prev)),
                None => String::new(),
            };
            format!(
                "{} median{trend}  · {} merged",
                fmt_hours(cur),
                ins.pr_cycle.merged
            )
        }
        None => NA.to_string(),
    };
    out.push_str(&row("PR cycle time", &pc));

    let co = match ins.close_out.lag_median_hours {
        Some(med) => {
            let p90 = ins
                .close_out
                .lag_p90_hours
                .map(|h| format!(" · p90 {}", fmt_hours(h)))
                .unwrap_or_default();
            format!(
                "median {}{p90}  ({} closed)",
                fmt_hours(med),
                ins.close_out.closed_in_window
            )
        }
        None => NA.to_string(),
    };
    out.push_str(&row("PR close-out lag", &co));

    let ta = match (ins.todo_age.p50_days, ins.todo_age.p90_days) {
        (Some(p50), Some(p90)) => format!(
            "p50 {p50}d · p90 {p90}d · {} over {}d  ({} open)",
            ins.todo_age.over_stale_days,
            ins.todo_age.stale_threshold_days,
            ins.todo_age.open_total
        ),
        _ => NA.to_string(),
    };
    out.push_str(&row("Open todo age", &ta));

    let sd = if ins.ship_days.total > 0 {
        WEEKDAYS
            .iter()
            .enumerate()
            .map(|(i, d)| format!("{d} {}", ins.ship_days.by_weekday[i]))
            .collect::<Vec<_>>()
            .join("  ")
    } else {
        NA.to_string()
    };
    out.push_str(&row("Ship days", &sd));

    let gc = match ins.goal_completion.rate {
        Some(r) => format!(
            "{:.0}%  ({} done / {} carried)",
            r * 100.0,
            ins.goal_completion.completed,
            ins.goal_completion.carried
        ),
        None => NA.to_string(),
    };
    out.push_str(&row("Goal completion", &gc));

    if !ins.staff_review.is_empty() {
        out.push('\n');
        out.push_str(&render_staff_review(&ins.staff_review));
    }

    match &ins.suggestion {
        Some(s) => out.push_str(&format!("\n  → {s}\n")),
        None => out.push_str("\n  → No standout signal this week — steady as she goes.\n"),
    }
    out
}

/// Render the staff performance review (#268): one line per agent — cards read %, items acted %,
/// and up/down ratings — with a fire-me flag when sustained ignore/down-rating proposes removal.
/// Only rendered when at least one staff agent was active in the window.
fn render_staff_review(review: &[crate::staff::AgentReview]) -> String {
    let mut out = format!(
        "Staff performance — last {}d\n",
        crate::staff::REVIEW_WINDOW_DAYS
    );
    for r in review {
        let read = match r.read_pct {
            Some(p) => format!(
                "read {:.0}% ({}/{})",
                p * 100.0,
                r.cards_read,
                r.cards_total
            ),
            None => "read —".to_string(),
        };
        let acted = match r.acted_pct {
            Some(p) => format!(
                "acted {:.0}% ({}/{})",
                p * 100.0,
                r.items_acted,
                r.items_routed
            ),
            None => "acted —".to_string(),
        };
        let rating = if r.up > 0 || r.down > 0 {
            format!(" · ▲{} ▼{}", r.up, r.down)
        } else {
            String::new()
        };
        let flag = if r.fire_me { "  ⚑ fire-me" } else { "" };
        out.push_str(&format!(
            "  {:<16}{read} · {acted}{rating}{flag}\n",
            r.agent
        ));
    }
    out
}

fn row(label: &str, value: &str) -> String {
    format!("  {label:<18}{value}\n")
}

/// Render the scoreboard block: a title line naming the baseline anchor (or nudging you to set
/// one), the success metrics vs baseline, and the permanent bn-time guard. Each line degrades to
/// "not enough data" when its figure is absent, so the block is always total.
fn render_scoreboard(sb: &Scoreboard) -> String {
    let title = match &sb.baseline_date {
        Some(d) => format!("Scoreboard — vs baseline {d}\n"),
        None => "Scoreboard — no baseline yet (`bn insights --set-baseline`)\n".to_string(),
    };
    let mut out = title;

    for m in &sb.metrics {
        let value = match m.current {
            Some(v) => fmt_score_value(&m.key, v),
            None => NA.to_string(),
        };
        let cmp = match (m.baseline, m.pct_change(), m.improved()) {
            (Some(b), Some(pct), Some(improved)) => {
                let arrow = if pct > 0.0 {
                    "↑"
                } else if pct < 0.0 {
                    "↓"
                } else {
                    "→"
                };
                let mark = if improved { "✓" } else { "✗" };
                format!(
                    "  (baseline {}, {arrow}{:.0}% {mark})",
                    fmt_score_value(&m.key, b),
                    pct.abs()
                )
            }
            (Some(b), _, _) => format!("  (baseline {})", fmt_score_value(&m.key, b)),
            _ => String::new(),
        };
        out.push_str(&row(&m.label, &format!("{value}{cmp}")));
    }

    let guard = match sb.bn_time.ratio {
        Some(r) => {
            let split = format!(
                "{}%  ({} of {} on bn)",
                (r * 100.0).round(),
                fmt_secs(sb.bn_time.bn_secs),
                fmt_secs(sb.bn_time.tracked_secs)
            );
            match sb.bn_time_baseline {
                Some(b) => format!("{split}  (baseline {}%)", (b * 100.0).round()),
                None => split,
            }
        }
        None => NA.to_string(),
    };
    out.push_str(&row("bn-time guard", &guard));
    out
}

/// Format a scoreboard metric value for its key: cycle time as hours, ratios as a percentage.
fn fmt_score_value(key: &str, v: f64) -> String {
    match key {
        "deep_work_ratio" => format!("{}%", (v * 100.0).round()),
        _ => fmt_hours(v),
    }
}

/// `Xh Ym` (or `Ym` under an hour) from seconds — the deep-work focus/tracked totals.
fn fmt_secs(secs: i64) -> String {
    let mins = secs / 60;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// Hours as `Nh` under two days, else `N.Nd` — matching the CLI's metrics duration style.
fn fmt_hours(h: f64) -> String {
    if h < 48.0 {
        format!("{h:.0}h")
    } else {
        format!("{:.1}d", h / 24.0)
    }
}


