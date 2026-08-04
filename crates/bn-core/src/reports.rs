//! Standup and weekly-1-1 reports over the daily goals. Pure aggregation — reads the day
//! files (`crate::daily`), open blockers across branches (`crate::rollups`), and PR
//! throughput from the event log (`crate::metrics`); builds typed structs the CLI/MCP
//! render. No printing here beyond the text-render helpers.

use crate::daily::{self, ScheduledItem};
use crate::metrics;
use crate::model::Item;
use crate::resolver::Config;
use crate::rollups;
use crate::runner::CommandRunner;
use crate::util;
use serde::Serialize;

/// A goal as shown in a report: identity, optional link, and how many days it's been open.
#[derive(Debug, Clone, Serialize)]
pub struct GoalView {
    pub id: String,
    pub text: String,
    pub link: Option<String>,
    /// Whole days since the goal was first raised (`created`), as of the report date.
    pub pending_days: i64,
}

/// An open blocker on some branch note (the standup/1-1 "what's in my way").
#[derive(Debug, Clone, Serialize)]
pub struct BlockerRef {
    pub repo: String,
    pub branch: String,
    pub text: String,
}

/// One prior day's goals, split into what was finished vs. left open.
#[derive(Debug, Clone, Serialize)]
pub struct DaySummary {
    pub date: String,
    pub completed: Vec<GoalView>,
    pub incomplete: Vec<GoalView>,
}

/// The standup view: yesterday's done/undone, today's plan, blockers, upcoming follow-ups.
#[derive(Debug, Serialize)]
pub struct Standup {
    pub date: String,
    pub yesterday: Option<DaySummary>,
    pub today: Vec<GoalView>,
    pub blockers: Vec<BlockerRef>,
    pub upcoming: Vec<ScheduledItem>,
    /// The top at-risk / off-track quarter goal (#252), as `<status>: <text>`. `None` when no
    /// quarter goal is flagged. Omitted from JSON while absent so existing standup shapes are
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarter_at_risk: Option<String>,
}

/// PR throughput folded into the weekly view (from the event log via [`metrics`]).
#[derive(Debug, Serialize)]
pub struct PrSummary {
    pub opened: usize,
    pub merged: usize,
    pub cycle_median_hours: Option<f64>,
}

/// Goals completed on one day in the weekly window.
#[derive(Debug, Serialize)]
pub struct DayGroup {
    pub date: String,
    pub completed: Vec<GoalView>,
}

/// The subjective DXI trend folded into the weekly view (#110): the in-window self-ratings plus
/// their averages, so the perceptual axis sits next to the objective metrics. `None` when DXI is
/// disabled or no rating fell in the window.
#[derive(Debug, Serialize)]
pub struct DxiTrend {
    /// Ratings recorded inside the weekly window, oldest first.
    pub ratings: Vec<daily::DxiRating>,
    /// Mean focus score (1–5) over the window.
    pub avg_focus: f64,
    /// Mean friction score (1–5) over the window.
    pub avg_friction: f64,
}

/// The weekly 1-1 view: completed-by-day, totals, PR throughput, still-open, blockers, and the
/// optional subjective DXI trend.
#[derive(Debug, Serialize)]
pub struct Weekly {
    pub start: String,
    pub end: String,
    pub by_day: Vec<DayGroup>,
    pub completed_total: usize,
    pub prs: PrSummary,
    pub still_open: Vec<GoalView>,
    pub blockers: Vec<BlockerRef>,
    /// Things learned across the window (the weekly "learn something new" habit), newest day last.
    pub learnings: Vec<LoggedNote>,
    /// Reflections across the window (one thing to improve), newest day last.
    pub reflections: Vec<LoggedNote>,
    /// The subjective DXI trend over the window, when DXI is enabled and rated.
    pub dxi: Option<DxiTrend>,
}

/// A dated day-log entry (a learning or a reflection) surfaced in the weekly review.
#[derive(Debug, Serialize)]
pub struct LoggedNote {
    pub date: String,
    pub text: String,
}

/// A logged achievement as shown in the stand view (note identity + the line).
#[derive(Debug, Serialize)]
pub struct AchievementLine {
    pub repo: String,
    pub branch: String,
    pub text: String,
}

/// One day's "what got done": daily goals checked off that day, plus achievements logged to a
/// note on that date.
#[derive(Debug, Serialize)]
pub struct StandDay {
    pub date: String,
    pub goals: Vec<GoalView>,
    pub achievements: Vec<AchievementLine>,
}

/// `bn stand`: what was achieved yesterday and today. The "what did I get done" counterpart to
/// standup's "what's the plan" — completed goals + logged achievements across both days.
#[derive(Debug, Serialize)]
pub struct Stand {
    pub yesterday: StandDay,
    pub today: StandDay,
}

/// Build the stand report as of `today`. Reads the two daily files (yesterday + today) for
/// checked-off goals and the achievements logs for dated entries; pure aggregation otherwise.
pub fn stand(cfg: &Config, today: &str) -> Stand {
    let yday = util::add_days(today, -1).unwrap_or_else(|| today.to_string());
    // Materialize today's file so scheduled follow-ups drain; goals themselves come from the
    // task store (#1004), so this is no longer what makes today's list appear.
    util::best_effort(
        daily::ensure_today(cfg, today),
        "stand: ensure today's daily",
    );
    let mut dailies = daily::read_window(cfg, &yday, today);
    // `read_window` is file-only; swap today's entry for the projection so `bn stand` counts the
    // same goals `bn daily` shows.
    if let Some(slot) = dailies.iter_mut().find(|d| d.front.date == today) {
        *slot = daily::view(cfg, today);
    }
    let achievements = rollups::achievements(cfg);

    let day = |date: &str| StandDay {
        date: date.to_string(),
        goals: dailies
            .iter()
            .find(|d| d.front.date == date)
            .map(|d| d.completed().map(|g| goal_view(g, today)).collect())
            .unwrap_or_default(),
        achievements: achievements
            .iter()
            .filter(|a| a.date == date)
            .map(|a| AchievementLine {
                repo: a.repo.clone(),
                branch: a.branch.clone(),
                text: a.text.clone(),
            })
            .collect(),
    };

    Stand {
        yesterday: day(&yday),
        today: day(today),
    }
}

/// Build a [`GoalView`] from a goal item, dating pending-days against `today`.
fn goal_view(g: &Item, today: &str) -> GoalView {
    GoalView {
        id: g.id.clone(),
        text: g.text.clone(),
        link: g.link.clone(),
        pending_days: util::idle_days_since(&g.created, today),
    }
}

/// Open blockers across all active local branch notes.
fn open_blockers(cfg: &Config) -> Vec<BlockerRef> {
    rollups::summary(cfg)
        .into_iter()
        .flat_map(|e| {
            let (repo, branch) = (e.repo, e.branch);
            e.blockers.into_iter().map(move |b| BlockerRef {
                repo: repo.clone(),
                branch: branch.clone(),
                text: b.text,
            })
        })
        .collect()
}

/// Build the standup report as of `today`.
pub fn standup(cfg: &Config, today: &str) -> Standup {
    let yesterday = daily::previous_day(cfg, today).map(|d| DaySummary {
        date: d.front.date.clone(),
        completed: d.completed().map(|g| goal_view(g, today)).collect(),
        incomplete: d.open().map(|g| goal_view(g, today)).collect(),
    });

    // `standup` is a report you *run*, so materializing today (which is what carries yesterday's
    // unfinished day-file goals forward) is fine here. `view` then folds the task store in on top.
    // The read-only surfaces — `/api/goals`, the rail badge — deliberately skip this and rely on
    // the projection alone, which is what makes carried work visible there without a write.
    let _ = daily::ensure_today(cfg, today);
    let today_daily = daily::view(cfg, today);
    let today_goals = today_daily.open().map(|g| goal_view(g, today)).collect();

    Standup {
        date: today.to_string(),
        yesterday,
        today: today_goals,
        blockers: open_blockers(cfg),
        upcoming: daily::scheduled_pending(cfg, today),
        quarter_at_risk: quarter_at_risk(cfg, today),
    }
}

/// The current quarter's top at-risk / off-track goal, as `<status>: <text>` (#252). Reads the
/// quarter file only if it already exists — the standup view never *creates* a quarter. `None`
/// when there's no quarter, no flagged goal, or the read fails.
fn quarter_at_risk(cfg: &Config, today: &str) -> Option<String> {
    let qid = crate::quarter::quarter_id(today)?;
    let q = crate::quarter::read_quarter(cfg, &qid).ok().flatten()?;
    q.top_at_risk()
        .map(|g| format!("{}: {}", q.status_of(&g.key), g.text))
}

/// The start date of the weekly window: `today - since` days when given, else back to the
/// most recent configured anchor weekday strictly before today (so on the anchor day you
/// see the full prior week). Shared with [`crate::insights`] so `bn insights` anchors its
/// window exactly like `bn weekly`.
pub fn window_start(cfg: &Config, today: &str, since: Option<i64>) -> String {
    if let Some(days) = since {
        return util::add_days(today, -days.max(0)).unwrap_or_else(|| today.to_string());
    }
    let Some(wt) = util::weekday_index(today) else {
        return today.to_string();
    };
    let mut delta = (wt as i64 - cfg.weekly_anchor as i64).rem_euclid(7);
    if delta == 0 {
        delta = 7;
    }
    util::add_days(today, -delta).unwrap_or_else(|| today.to_string())
}

/// Build the weekly 1-1 report as of `today`. `since` overrides the anchored window with a
/// rolling N-day window. `dxi` opts in the subjective DXI trend (#110); when false the trend is
/// omitted regardless of any recorded ratings.
pub fn weekly(cfg: &Config, today: &str, since: Option<i64>, dxi: bool) -> Weekly {
    let start = window_start(cfg, today, since);
    let days = daily::read_window(cfg, &start, today);
    let goal_aggregation = daily::aggregate_goals(&days);
    let mut by_day = Vec::new();
    let mut learnings = Vec::new();
    let mut reflections = Vec::new();
    for d in days {
        // Learnings/reflections are collected even on days with no completed goals.
        for l in &d.front.learnings {
            learnings.push(LoggedNote {
                date: d.front.date.clone(),
                text: l.text.clone(),
            });
        }
        for r in &d.front.reflections {
            reflections.push(LoggedNote {
                date: d.front.date.clone(),
                text: r.text.clone(),
            });
        }
        let completed: Vec<GoalView> = d.completed().map(|g| goal_view(g, today)).collect();
        if completed.is_empty() {
            continue;
        }
        by_day.push(DayGroup {
            date: d.front.date.clone(),
            completed,
        });
    }

    let window_days = util::days_since(&start, today).unwrap_or(7) + 1;
    let evs = metrics::visible_events(
        cfg,
        crate::events::read_events_across(&cfg.metrics_logs_all_machines()),
    );
    let m = metrics::compute(&evs, util::now_epoch_secs(), window_days);

    let still_open = daily::view(cfg, today)
        .open()
        .map(|g| goal_view(g, today))
        .collect();

    let dxi = if dxi {
        dxi_trend(cfg, &start, today)
    } else {
        None
    };

    Weekly {
        start,
        end: today.to_string(),
        by_day,
        completed_total: goal_aggregation.completed,
        prs: PrSummary {
            opened: m.opened,
            merged: m.merged,
            cycle_median_hours: m.cycle_median_hours,
        },
        still_open,
        blockers: open_blockers(cfg),
        learnings,
        reflections,
        dxi,
    }
}

/// The DXI self-ratings inside `[start..=end]` plus their focus/friction averages, or `None`
/// when no rating fell in the window. Reuses [`daily::read_ratings`].
fn dxi_trend(cfg: &Config, start: &str, end: &str) -> Option<DxiTrend> {
    let ratings: Vec<daily::DxiRating> = daily::read_ratings(cfg)
        .into_iter()
        .filter(|r| r.date.as_str() >= start && r.date.as_str() <= end)
        .collect();
    if ratings.is_empty() {
        return None;
    }
    let n = ratings.len() as f64;
    let avg_focus = ratings.iter().map(|r| r.focus as f64).sum::<f64>() / n;
    let avg_friction = ratings.iter().map(|r| r.friction as f64).sum::<f64>() / n;
    Some(DxiTrend {
        ratings,
        avg_focus,
        avg_friction,
    })
}

/// Text render for `bn standup`, tuned to read aloud.
pub fn render_standup(s: &Standup) -> String {
    let mut out = format!("Standup — {}\n", s.date);

    out.push_str("\nYesterday");
    match &s.yesterday {
        Some(y) => {
            out.push_str(&format!(" ({})\n", y.date));
            if y.completed.is_empty() {
                out.push_str("  (nothing marked done)\n");
            }
            for g in &y.completed {
                out.push_str(&format!("  ✓ {}{}\n", g.text, link_suffix(&g.link)));
            }
            for g in &y.incomplete {
                out.push_str(&format!("  ↻ {} (carried)\n", g.text));
            }
        }
        None => out.push_str("\n  (no prior day)\n"),
    }

    out.push_str("\nToday\n");
    if s.today.is_empty() {
        out.push_str("  (no goals yet — `bn daily add <text>`)\n");
    }
    for g in &s.today {
        out.push_str(&format!(
            "  - {}{}{}\n",
            g.text,
            pending_suffix(g.pending_days),
            link_suffix(&g.link)
        ));
    }

    if !s.blockers.is_empty() {
        out.push_str("\nBlockers\n");
        for b in &s.blockers {
            out.push_str(&format!("  ! {} ({}/{})\n", b.text, b.repo, b.branch));
        }
    }

    if !s.upcoming.is_empty() {
        out.push_str("\nUpcoming follow-ups\n");
        for u in &s.upcoming {
            out.push_str(&format!("  → {} [{}]\n", u.text, u.scheduled));
        }
    }

    if let Some(q) = &s.quarter_at_risk {
        out.push_str(&format!("\nQuarter\n  ⚠ {q}\n"));
    }
    out
}

/// Text render for `bn stand` — what got done yesterday and today, read-aloud friendly.
pub fn render_stand(s: &Stand) -> String {
    let mut out = format!("Achievements — {}\n", s.today.date);
    out.push_str(&render_stand_day("Yesterday", &s.yesterday));
    out.push_str(&render_stand_day("Today", &s.today));
    out
}

fn render_stand_day(label: &str, d: &StandDay) -> String {
    let mut out = format!("\n{} ({})\n", label, d.date);
    if d.goals.is_empty() && d.achievements.is_empty() {
        out.push_str("  (nothing recorded)\n");
        return out;
    }
    for g in &d.goals {
        out.push_str(&format!("  ✓ {}{}\n", g.text, link_suffix(&g.link)));
    }
    for a in &d.achievements {
        out.push_str(&format!("  ★ {} ({}/{})\n", a.text, a.repo, a.branch));
    }
    out
}

/// Text render for `bn weekly`, the 1-1 prep artifact.
pub fn render_weekly(w: &Weekly) -> String {
    let mut out = format!(
        "Weekly review — {} … {}  ({} completed)\n",
        w.start, w.end, w.completed_total
    );
    if w.by_day.is_empty() {
        out.push_str("  (no goals completed this window)\n");
    }
    for day in &w.by_day {
        out.push_str(&format!("\n{}\n", day.date));
        for g in &day.completed {
            out.push_str(&format!("  ✓ {}{}\n", g.text, link_suffix(&g.link)));
        }
    }

    out.push_str(&format!(
        "\nPRs — {} merged, {} opened",
        w.prs.merged, w.prs.opened
    ));
    if let Some(h) = w.prs.cycle_median_hours {
        out.push_str(&format!(" · cycle median {}\n", dur(h)));
    } else {
        out.push('\n');
    }

    if !w.still_open.is_empty() {
        out.push_str("\nStill open\n");
        for g in &w.still_open {
            out.push_str(&format!(
                "  - {}{}\n",
                g.text,
                pending_suffix(g.pending_days)
            ));
        }
    }

    if !w.blockers.is_empty() {
        out.push_str("\nBlockers\n");
        for b in &w.blockers {
            out.push_str(&format!("  ! {} ({}/{})\n", b.text, b.repo, b.branch));
        }
    }

    if !w.learnings.is_empty() {
        out.push_str("\nLearned\n");
        for l in &w.learnings {
            out.push_str(&format!("  • {}  ({})\n", l.text, l.date));
        }
    }

    if !w.reflections.is_empty() {
        out.push_str("\nReflections\n");
        for r in &w.reflections {
            out.push_str(&format!("  ↳ {}  ({})\n", r.text, r.date));
        }
    }

    if let Some(d) = &w.dxi {
        out.push_str(&format!(
            "\nDXI — focus {:.1}/5 · friction {:.1}/5  ({} rated)\n",
            d.avg_focus,
            d.avg_friction,
            d.ratings.len()
        ));
        for r in &d.ratings {
            let mood = r
                .mood
                .as_ref()
                .map(|m| format!("  {m}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {}  focus {} · friction {}{}\n",
                r.date, r.focus, r.friction, mood
            ));
        }
    }
    out
}

fn link_suffix(link: &Option<String>) -> String {
    link.as_ref()
        .map(|l| format!("  → {l}"))
        .unwrap_or_default()
}

fn pending_suffix(days: i64) -> String {
    if days > 0 {
        format!("  (pending {days}d)")
    } else {
        String::new()
    }
}

/// Hours as a compact human duration (mirrors the CLI's `dur`).
fn dur(hours: f64) -> String {
    if hours < 48.0 {
        format!("{hours:.0}h")
    } else {
        format!("{:.1}d", hours / 24.0)
    }
}

// ---- Wrap digest (#98) ---------------------------------------------------------------

/// A PR that merged today (the `bn close` prompt list in the wrap digest).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
}

/// A commit on the current branch today with no corresponding note update — the `bn sync`
/// hand-off. Carries the short hash + subject.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnnotedCommit {
    pub hash: String,
    pub subject: String,
}

/// One still-open todo, for the end-of-day "what's left" list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenTodo {
    pub repo: String,
    pub branch: String,
    pub text: String,
}

/// The end-of-day wrap digest (#98): where the day landed. A single batched, quiet snapshot —
/// branches touched today, commits with no note update, PRs merged today, still-open todos, and
/// tomorrow's carry-forward. [`is_empty`](WrapDigest::is_empty) drives the **silent** exit when
/// the day is clean — the property the deleted all-day poller lacked.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WrapDigest {
    pub date: String,
    /// Local branches whose tip commit landed today.
    pub branches_touched: Vec<String>,
    /// Today's commits on the current branch that the current note didn't capture.
    pub unnoted_commits: Vec<UnnotedCommit>,
    /// PRs that merged today → `bn close` prompts.
    pub merged_prs: Vec<MergedPr>,
    /// Open todos across active branch notes.
    pub open_todos: Vec<OpenTodo>,
    /// Today's open daily goals — what carries forward to tomorrow.
    pub carry_forward: Vec<GoalView>,
}

impl WrapDigest {
    /// True when there's nothing to report — drives the digest's silent, no-push exit.
    pub fn is_empty(&self) -> bool {
        self.branches_touched.is_empty()
            && self.unnoted_commits.is_empty()
            && self.merged_prs.is_empty()
            && self.open_todos.is_empty()
            && self.carry_forward.is_empty()
    }
}

/// The current branch note's last-activity date — the marker the wrap digest dates "un-noted
/// commits" against. Thin public wrapper over [`crate::doctor::last_activity`] so the CLI can
/// supply it to [`wrap`] without reaching into `doctor`.
pub fn note_last_activity(note: &crate::model::Note) -> String {
    crate::doctor::last_activity(note)
}

/// Build the end-of-day wrap digest as of `today`, reading git/gh through `runner` (so it's
/// deterministic under `FakeRunner`). `note_since` is the current branch note's last-activity
/// date ([`crate::doctor::last_activity`]); commits today are flagged "un-noted" only when the
/// note hasn't been touched today. Pure aggregation otherwise — reuses `rollups` + `daily`.
///
/// **Materializes** today's day file (via [`daily::ensure_today`], carrying yesterday's open goals
/// forward) as a side effect — the CLI/cron behaviour. The read-only web dashboard uses
/// [`wrap_readonly`] instead so a mere GET never creates or rolls the day.
pub fn wrap(cfg: &Config, today: &str, note_since: &str, runner: &dyn CommandRunner) -> WrapDigest {
    // Goals come from the task store (#1004), so project rather than reading the day file. The
    // materialize is still worth doing for scheduled follow-ups, but it is no longer what makes
    // today's goals appear.
    let _ = daily::ensure_today(cfg, today);
    let carry_forward = daily::view(cfg, today)
        .open()
        .map(|g| goal_view(g, today))
        .collect();
    wrap_with_carry(cfg, today, note_since, runner, carry_forward)
}

/// Read-only sibling of [`wrap`] for the web dashboard (#490): the identical digest, but today's
/// daily is *read* rather than `ensure_today`-materialized, so a dashboard GET never creates or
/// rolls the day file (mirroring the read-only `/api/goals`). An un-materialized day yields an
/// empty `carry_forward`.
pub fn wrap_readonly(
    cfg: &Config,
    today: &str,
    note_since: &str,
    runner: &dyn CommandRunner,
) -> WrapDigest {
    let carry_forward = Some(daily::view(cfg, today))
        .map(|d| d.open().map(|g| goal_view(g, today)).collect())
        .unwrap_or_default();
    wrap_with_carry(cfg, today, note_since, runner, carry_forward)
}

/// The git/gh/rollups aggregation shared by [`wrap`] and [`wrap_readonly`]; the two differ only in
/// how `carry_forward` (today's still-open daily goals) is sourced.
fn wrap_with_carry(
    cfg: &Config,
    today: &str,
    note_since: &str,
    runner: &dyn CommandRunner,
    carry_forward: Vec<GoalView>,
) -> WrapDigest {
    let branches_touched = branches_touched_today(today, runner);
    let unnoted_commits = if note_since >= today {
        // The note already moved today → nothing to hand off.
        Vec::new()
    } else {
        commits_today(today, runner)
    };
    let merged_prs = merged_today(today, runner);

    let open_todos = rollups::todos(cfg)
        .into_iter()
        .map(|t| OpenTodo {
            repo: t.repo,
            branch: t.branch,
            text: t.text,
        })
        .collect();

    WrapDigest {
        date: today.to_string(),
        branches_touched,
        unnoted_commits,
        merged_prs,
        open_todos,
        carry_forward,
    }
}

/// Local branches whose tip commit's date is `today`, via
/// `git for-each-ref --format='%(committerdate:short)\t%(refname:short)' refs/heads`.
fn branches_touched_today(today: &str, runner: &dyn CommandRunner) -> Vec<String> {
    let out = runner.git(&[
        "for-each-ref",
        "--format=%(committerdate:short)%09%(refname:short)",
        "refs/heads",
    ]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter_map(|line| {
            let (date, name) = line.split_once('\t')?;
            (date.trim() == today).then(|| name.trim().to_string())
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// Today's commits on the current branch, via `git log --since=<today> --pretty=…`. Same
/// `%h\x1f%s` shape `bn sync` uses, so a subject with any printable delimiter is safe.
fn commits_today(today: &str, runner: &dyn CommandRunner) -> Vec<UnnotedCommit> {
    let since = format!("--since={today}");
    let out = runner.git(&["log", &since, "--pretty=format:%h\x1f%s"]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('\x1f')?;
            let subject = subject.trim();
            (!hash.is_empty() && !subject.is_empty()).then(|| UnnotedCommit {
                hash: hash.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect()
}

/// PRs that merged today, via `gh pr list --state merged --search merged:>=<today> --json …`.
/// Best-effort: gh absent / unparsable → empty.
fn merged_today(today: &str, runner: &dyn CommandRunner) -> Vec<MergedPr> {
    let search = format!("merged:>={today}");
    let out = runner.gh(&[
        "pr",
        "list",
        "--state",
        "merged",
        "--search",
        &search,
        "--json",
        "number,title,url,mergedAt",
        "--limit",
        "50",
    ]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter(|pr| {
            // `gh`'s search rounds to the day; re-check the date prefix so an open-ended
            // `>=today` window can't leak a stale merge from a future-dated clock skew.
            pr.get("mergedAt")
                .and_then(|m| m.as_str())
                .map(|m| m.starts_with(today))
                .unwrap_or(false)
        })
        .filter_map(|pr| {
            Some(MergedPr {
                number: pr.get("number")?.as_u64()?,
                title: pr.get("title")?.as_str()?.to_string(),
                url: pr.get("url")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// Text render for `bn wrap` — the end-of-day digest, read-aloud friendly. Sections with no
/// content are omitted; an all-empty digest renders a single "clean day" line (callers should
/// stay silent instead of pushing it).
pub fn render_wrap(w: &WrapDigest) -> String {
    let mut out = format!("Wrap — {}\n", w.date);
    if w.is_empty() {
        out.push_str("  (clean day — nothing to wrap up)\n");
        return out;
    }

    if !w.branches_touched.is_empty() {
        out.push_str("\nBranches touched\n");
        for b in &w.branches_touched {
            out.push_str(&format!("  • {b}\n"));
        }
    }

    if !w.unnoted_commits.is_empty() {
        out.push_str("\nUn-noted commits (→ bn sync)\n");
        for c in &w.unnoted_commits {
            out.push_str(&format!("  {} {}\n", c.hash, c.subject));
        }
    }

    if !w.merged_prs.is_empty() {
        out.push_str("\nMerged today (→ bn close)\n");
        for p in &w.merged_prs {
            out.push_str(&format!("  ✓ #{} {}  → {}\n", p.number, p.title, p.url));
        }
    }

    if !w.open_todos.is_empty() {
        out.push_str("\nStill open\n");
        for t in &w.open_todos {
            out.push_str(&format!("  - {} ({}/{})\n", t.text, t.repo, t.branch));
        }
    }

    if !w.carry_forward.is_empty() {
        out.push_str("\nCarrying into tomorrow\n");
        for g in &w.carry_forward {
            out.push_str(&format!("  → {}{}\n", g.text, link_suffix(&g.link)));
        }
    }
    out
}

/// The copy-paste-ready one-liner for `bn standup --copy` (#99): yesterday's done count, today's
/// plan count, and the first blocker if any — the line you paste into the standup channel.
pub fn standup_line(s: &Standup) -> String {
    let done = s.yesterday.as_ref().map(|y| y.completed.len()).unwrap_or(0);
    let mut line = format!(
        "Standup: yesterday shipped {done}, today {} planned",
        s.today.len()
    );
    if let Some(b) = s.blockers.first() {
        line.push_str(&format!("; blocked on {}", b.text));
    }
    if let Some(q) = &s.quarter_at_risk {
        line.push_str(&format!("; quarter {q}"));
    }
    line
}

/// The optional one-shot PR delta for the standup autopilot (#99): a single `gh pr status`
/// read through the seam, summarised as "PRs: N open, M to review". `None` when gh is absent /
/// returns nothing — this is the *disciplined* delta the old poller did continuously, fetched
/// once at standup time only. Gated by the caller behind `[standup].fetch_delta`.
pub fn pr_delta(runner: &dyn CommandRunner) -> Option<String> {
    let out = runner.gh(&[
        "pr",
        "status",
        "--json",
        "number,title,state,reviewDecision",
    ]);
    let stdout = out.ok_stdout()?;
    // `gh pr status --json` returns an object with createdBy / needsReview / currentBranch arrays.
    let parsed: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let count = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };
    let mine = count("createdBy");
    let to_review = count("needsReview");
    if mine == 0 && to_review == 0 {
        return None;
    }
    Some(format!("PRs: {mine} open, {to_review} to review"))
}


