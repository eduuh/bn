//! The long-horizon impact packet (#141): `bn brag`. Where `bn standup`/`bn weekly` cover the
//! day/week, this compiles the multi-month accumulation a perf review needs — logged achievements
//! (`bn achieve`), your merged PRs (gh through the [`CommandRunner`] seam), metric highlights, and
//! completed daily goals over a configurable window — grouped by theme/project into review-ready
//! markdown. It also flags impactful merged PRs you never `bn achieve`d, so silent work still lands
//! in the packet. Pure aggregation: it reads the existing sources (`rollups`, `daily`, `metrics`,
//! gh via the runner) and joins them — no new collection, no re-fetch beyond the one gh read.

use crate::daily;
use crate::metrics;
use crate::resolver::Config;
use crate::rollups;
use crate::runner::CommandRunner;
use crate::util;
use serde::Serialize;
use std::collections::BTreeMap;

/// A merged PR pulled from gh — the raw material for the packet and for un-logged-win detection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BragPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// Owning repo basename (`bn` from `eduuh/bn`) — the theme grouping key.
    pub repo: String,
    /// Merge date as `YYYY-MM-DD`.
    pub merged_at: String,
}

/// A logged achievement carried into the packet (note identity + the line + its date).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BragAchievement {
    pub repo: String,
    pub branch: String,
    pub date: String,
    pub text: String,
}

/// A completed daily goal in the window (the "what I actually finished" half).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BragGoal {
    pub date: String,
    pub text: String,
    pub link: Option<String>,
}

/// One theme/project group: everything attributed to a repo, joined for the narrative.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThemeGroup {
    /// The repo basename the group is keyed on (the "project").
    pub theme: String,
    pub achievements: Vec<BragAchievement>,
    pub merged_prs: Vec<BragPr>,
}

/// The metric highlights folded into the packet — the quantitative half of the narrative
/// (throughput + the feature/maintenance mix). All best-effort; thin history reads as zeros.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BragMetrics {
    /// PRs merged in the window (from the synced event log across machines).
    pub merged: usize,
    /// PRs opened in the window.
    pub opened: usize,
    /// Median open→merge cycle time (hours), when any PR merged.
    pub cycle_median_hours: Option<f64>,
    /// Median change lead time (hours), when the observer stamped it.
    pub lead_median_hours: Option<f64>,
    /// feat / (feat + maintenance) commit ratio over the window — the building-vs-keeping mix.
    pub feature_ratio: Option<f64>,
    /// Feature commits classified in the window.
    pub feature_commits: usize,
    /// Maintenance commits classified in the window.
    pub maintenance_commits: usize,
}

/// The review-ready impact packet over `[start..=end]`: themed groups, metric highlights,
/// completed goals, and the un-logged wins to consider logging.
#[derive(Debug, Clone, Serialize)]
pub struct Brag {
    pub start: String,
    pub end: String,
    /// Theme/project groups, sorted by theme name.
    pub themes: Vec<ThemeGroup>,
    pub metrics: BragMetrics,
    /// Completed daily goals across the window, newest day first.
    pub completed_goals: Vec<BragGoal>,
    /// Merged PRs with no matching logged achievement — "you shipped this, log it?".
    pub unlogged_wins: Vec<BragPr>,
    /// Total logged achievements in the window (across all themes).
    pub achievement_total: usize,
}

impl Brag {
    /// True when there's nothing to compile — drives the "thin history" render.
    pub fn is_empty(&self) -> bool {
        self.themes.is_empty() && self.completed_goals.is_empty() && self.metrics.merged == 0
    }
}

/// Resolve the window start date from the `--since` value. Accepts a literal `YYYY-MM-DD` (used
/// verbatim), `review` / `last-review` (the default review horizon, 6 months), or an `N` / `Nmo`
/// / `Nm` / `Nmonth(s)` count of months back. Anything unparsable falls back to the 6-month
/// default. Returns the start date as `YYYY-MM-DD`, dated back from `today`.
pub fn resolve_since(today: &str, since: Option<&str>) -> String {
    const DEFAULT_MONTHS: i64 = 6;
    let back_months =
        |m: i64| util::add_days(today, -m.max(0) * 30).unwrap_or_else(|| today.to_string());
    let Some(raw) = since.map(str::trim).filter(|s| !s.is_empty()) else {
        return back_months(DEFAULT_MONTHS);
    };
    if util::epoch_day(raw).is_some() {
        // A literal ISO date — use it as the window start.
        return raw.to_string();
    }
    let lower = raw.to_ascii_lowercase();
    if lower == "review" || lower == "last-review" || lower == "since-last-review" {
        return back_months(DEFAULT_MONTHS);
    }
    // Strip a trailing month unit and parse the leading number; default on failure.
    let digits: String = lower.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse::<i64>() {
        Ok(n) if n > 0 => back_months(n),
        _ => back_months(DEFAULT_MONTHS),
    }
}

/// Build the impact packet as of `today` over the window starting at `since` (see
/// [`resolve_since`]). gh/git reads go through `runner` so it's deterministic under `FakeRunner`;
/// achievements/goals/metrics come from the existing local sources.
pub fn brag(cfg: &Config, today: &str, since: Option<&str>, runner: &dyn CommandRunner) -> Brag {
    let start = resolve_since(today, since);

    let achievements = window_achievements(cfg, &start, today);
    let merged_prs = my_merged_prs(&start, runner);
    let themes = group_by_theme(&achievements, &merged_prs);
    let unlogged_wins = unlogged_wins(&achievements, &merged_prs);
    let completed_goals = window_goals(cfg, &start, today);
    let metrics = window_metrics(cfg, &start, today, runner);

    Brag {
        achievement_total: achievements.len(),
        start,
        end: today.to_string(),
        themes,
        metrics,
        completed_goals,
        unlogged_wins,
    }
}

/// Logged achievements dated within `[start..=end]`, via [`rollups::achievements`].
fn window_achievements(cfg: &Config, start: &str, end: &str) -> Vec<BragAchievement> {
    let mut out: Vec<BragAchievement> = rollups::achievements(cfg)
        .into_iter()
        .filter(|a| a.date.as_str() >= start && a.date.as_str() <= end)
        .map(|a| BragAchievement {
            repo: a.repo,
            branch: a.branch,
            date: a.date,
            text: a.text,
        })
        .collect();
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

/// Completed daily goals across the window, newest day first (the finished-work half).
fn window_goals(cfg: &Config, start: &str, end: &str) -> Vec<BragGoal> {
    let mut out = Vec::new();
    for d in daily::read_window(cfg, start, end) {
        for g in d.completed() {
            out.push(BragGoal {
                date: d.front.date.clone(),
                text: g.text.clone(),
                link: g.link.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// My merged PRs since `start`, via `gh search prs --author @me --merged --merged-at >=start`.
/// Best-effort: gh absent / unparsable → empty. Excluded repos (tooling/dotfiles) are dropped so
/// the packet reflects real project work, matching the metrics scope.
fn my_merged_prs(start: &str, runner: &dyn CommandRunner) -> Vec<BragPr> {
    let merged = format!(">={start}");
    let out = runner.gh(&[
        "search",
        "prs",
        "--author",
        "@me",
        "--merged",
        "--merged-at",
        &merged,
        "--json",
        "number,title,url,repository,closedAt",
        "--limit",
        "200",
    ]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) else {
        return Vec::new();
    };
    let mut prs: Vec<BragPr> = arr
        .into_iter()
        .filter_map(|pr| {
            let repo = pr
                .get("repository")
                .and_then(|r| r.get("nameWithOwner").or_else(|| r.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            if metrics::is_excluded_repo(&repo) {
                return None;
            }
            let merged_at = pr
                .get("closedAt")
                .and_then(|m| m.as_str())
                .map(|m| m.chars().take(10).collect::<String>())
                .unwrap_or_default();
            Some(BragPr {
                number: pr.get("number")?.as_u64()?,
                title: pr.get("title")?.as_str()?.to_string(),
                url: pr.get("url")?.as_str()?.to_string(),
                repo,
                merged_at,
            })
        })
        .collect();
    // Stable, deterministic order: newest merge first, then by number.
    prs.sort_by(|a, b| b.merged_at.cmp(&a.merged_at).then(b.number.cmp(&a.number)));
    prs
}

/// Join achievements + merged PRs into per-repo theme groups, sorted by theme name. A repo with
/// only one of the two sources still gets a group; the empty side is just empty.
fn group_by_theme(achievements: &[BragAchievement], merged_prs: &[BragPr]) -> Vec<ThemeGroup> {
    let mut groups: BTreeMap<String, ThemeGroup> = BTreeMap::new();
    for a in achievements {
        groups
            .entry(a.repo.clone())
            .or_insert_with(|| ThemeGroup {
                theme: a.repo.clone(),
                achievements: Vec::new(),
                merged_prs: Vec::new(),
            })
            .achievements
            .push(a.clone());
    }
    for p in merged_prs {
        groups
            .entry(p.repo.clone())
            .or_insert_with(|| ThemeGroup {
                theme: p.repo.clone(),
                achievements: Vec::new(),
                merged_prs: Vec::new(),
            })
            .merged_prs
            .push(p.clone());
    }
    groups.into_values().collect()
}

/// Merged PRs no logged achievement references — the "silent work" to consider logging. A PR is
/// considered logged when any achievement text mentions its `#number` (or bare number) or contains
/// its URL, so a `bn achieve` that cited the PR counts even if it didn't repeat the title.
fn unlogged_wins(achievements: &[BragAchievement], merged_prs: &[BragPr]) -> Vec<BragPr> {
    merged_prs
        .iter()
        .filter(|pr| !achievements.iter().any(|a| references_pr(&a.text, pr)))
        .cloned()
        .collect()
}

/// True if achievement text `a` references PR `pr` — by its full URL, or by a `#<number>` /
/// whitespace-bounded bare `<number>` token (so `#94` and `94` match, but `940` or `1294` don't).
fn references_pr(a: &str, pr: &BragPr) -> bool {
    if !pr.url.is_empty() && a.contains(&pr.url) {
        return true;
    }
    let hash = format!("#{}", pr.number);
    for (idx, _) in a.match_indices(&hash) {
        let next = a[idx + hash.len()..].chars().next();
        if !matches!(next, Some(c) if c.is_ascii_digit()) {
            return true;
        }
    }
    let num = pr.number.to_string();
    a.split(|c: char| !c.is_ascii_digit()).any(|tok| tok == num)
}

/// Throughput + impact-mix highlights over the window. Throughput reads the synced event log
/// across machines (you-as-an-engineer); the feature/maintenance mix reads this repo's commit log
/// through the runner. Both best-effort.
fn window_metrics(
    cfg: &Config,
    start: &str,
    today: &str,
    runner: &dyn CommandRunner,
) -> BragMetrics {
    let window_days = util::days_since(start, today).unwrap_or(180).max(0) + 1;
    let evs = metrics::visible_events(
        cfg,
        crate::events::read_events_across(&cfg.metrics_logs_all_machines()),
    );
    // Anchor the throughput window to `today` (end of day), not wall-clock `now`. `window_days` is
    // derived from the `[start..=today]` *date* window, so the epoch cutoff `compute` measures back
    // from must share that anchor. `now_epoch_secs()` drifts the cutoff by the intra-day time (up
    // to a full day of boundary leak at the `start` edge) and reads an entirely wrong window for a
    // backdated `today`; the end-of-`today` epoch keeps the two windows aligned (issue #162).
    let anchor = util::epoch_day(today)
        .map(|d| (d + 1) * 86_400)
        .unwrap_or_else(util::now_epoch_secs);
    let m = metrics::compute(&evs, anchor, window_days);
    let mix = metrics::impact_mix(runner, start);
    BragMetrics {
        merged: m.merged,
        opened: m.opened,
        cycle_median_hours: m.cycle_median_hours,
        lead_median_hours: m.lead_median_hours,
        feature_ratio: mix.feature_ratio,
        feature_commits: mix.feature,
        maintenance_commits: mix.maintenance,
    }
}

/// Render the impact packet as review-ready markdown — paste-ready into a self-review tool.
pub fn render_markdown(b: &Brag) -> String {
    let mut out = format!("# Impact packet — {} … {}\n", b.start, b.end);
    if b.is_empty() {
        out.push_str("\n_Thin history over this window — nothing logged yet. Keep logging `bn achieve` and shipping PRs._\n");
        return out;
    }

    // Quantitative summary up top — the numbers a review wants first. Count merged PRs from the
    // gh-sourced theme groups (the PRs this packet actually lists below), *not* the event-log
    // metric `metrics.merged`: that metric is scoped to the synced event log and reads 0 on a
    // machine whose log is unsynced, which would disagree with the PRs rendered in the body.
    let gh_merged: usize = b.themes.iter().map(|t| t.merged_prs.len()).sum();
    out.push_str(&format!(
        "\n{} merged PR{} · {} logged achievement{} across {} theme{}.\n",
        gh_merged,
        plural(gh_merged),
        b.achievement_total,
        plural(b.achievement_total),
        b.themes.len(),
        plural(b.themes.len()),
    ));

    out.push_str("\n## Highlights\n");
    out.push_str(&format!(
        "- PRs: {} merged, {} opened\n",
        b.metrics.merged, b.metrics.opened
    ));
    if let Some(h) = b.metrics.cycle_median_hours {
        out.push_str(&format!("- Cycle time (open→merge): {} median\n", dur(h)));
    }
    if let Some(h) = b.metrics.lead_median_hours {
        out.push_str(&format!("- Lead time (commit→merge): {} median\n", dur(h)));
    }
    if let Some(r) = b.metrics.feature_ratio {
        out.push_str(&format!(
            "- Build-vs-keep: {:.0}% feature ({} feat / {} maintenance commits)\n",
            r * 100.0,
            b.metrics.feature_commits,
            b.metrics.maintenance_commits
        ));
    }

    for g in &b.themes {
        out.push_str(&format!("\n## {}\n", g.theme));
        for a in &g.achievements {
            out.push_str(&format!("- ★ {} _( {})_\n", a.text, a.date));
        }
        for p in &g.merged_prs {
            out.push_str(&format!("- #{} {} — {}\n", p.number, p.title, p.url));
        }
    }

    if !b.completed_goals.is_empty() {
        out.push_str("\n## Goals completed\n");
        for g in &b.completed_goals {
            let link = g
                .link
                .as_ref()
                .map(|l| format!(" → {l}"))
                .unwrap_or_default();
            out.push_str(&format!("- {} _( {})_{}\n", g.text, g.date, link));
        }
    }

    if !b.unlogged_wins.is_empty() {
        out.push_str("\n## Un-logged wins (consider `bn achieve`)\n");
        for p in &b.unlogged_wins {
            out.push_str(&format!(
                "- #{} {} — {}  → `bn achieve`?\n",
                p.number, p.title, p.url
            ));
        }
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Hours as a compact human duration (mirrors `reports::dur`).
fn dur(hours: f64) -> String {
    if hours < 48.0 {
        format!("{hours:.0}h")
    } else {
        format!("{:.1}d", hours / 24.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::add;
    use crate::resolver::Resolved;
    use crate::runner::FakeRunner;
    use std::path::{Path, PathBuf};

    fn cfg(root: &Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            work_root: root.to_path_buf(),
            worktree_dir: PathBuf::from("/nonexistent"),
            hostname: "testhost".into(),
            ..Default::default()
        }
    }

    /// gh fixture for `my_merged_prs` over a given start date.
    fn gh_prs(start: &str, json: &str) -> FakeRunner {
        let merged = format!(">={start}");
        FakeRunner::new().on(
            "gh",
            &[
                "search",
                "prs",
                "--author",
                "@me",
                "--merged",
                "--merged-at",
                &merged,
                "--json",
                "number,title,url,repository,closedAt",
                "--limit",
                "200",
            ],
            json,
        )
    }

    #[test]
    fn resolve_since_handles_dates_months_and_review() {
        // literal ISO date used verbatim
        assert_eq!(
            resolve_since("2026-06-26", Some("2026-01-01")),
            "2026-01-01"
        );
        // months back (30-day months)
        assert_eq!(resolve_since("2026-06-26", Some("2mo")), "2026-04-27");
        assert_eq!(resolve_since("2026-06-26", Some("3")), "2026-03-28");
        // review / default → 6 months
        assert_eq!(resolve_since("2026-06-26", Some("review")), "2025-12-28");
        assert_eq!(resolve_since("2026-06-26", None), "2025-12-28");
        // garbage → default
        assert_eq!(resolve_since("2026-06-26", Some("soon")), "2025-12-28");
    }

    #[test]
    fn brag_groups_by_theme_and_folds_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = Resolved {
            repo: "alpha".into(),
            branch: "main".into(),
        };
        // an in-window achievement + an out-of-window one (excluded)
        add(&c, &ctx, "todo", "x", None, "2026-01-01").unwrap();
        std::fs::write(
            c.note_dir(&ctx).join("achievements.md"),
            "# Achievements\n\n- 2026-03-01 — shipped the auth rewrite\n- 2025-01-01 — ancient win\n",
        )
        .unwrap();
        // a completed daily goal in-window
        daily::add(&c, "2026-03-02", "wrote the design doc", None, None).unwrap();
        daily::done(&c, "2026-03-02", "design doc", None).unwrap();

        let runner = gh_prs(
            "2025-12-28",
            r#"[{"number":12,"title":"auth rewrite","url":"https://x/pr/12","repository":{"nameWithOwner":"o/alpha"},"closedAt":"2026-03-01T10:00:00Z"},
                {"number":99,"title":"dotfiles tidy","url":"https://x/pr/99","repository":{"nameWithOwner":"o/dotfiles"},"closedAt":"2026-03-05T10:00:00Z"}]"#,
        );

        let b = brag(&c, "2026-06-26", None, &runner);
        assert_eq!(b.start, "2025-12-28");
        // out-of-window achievement dropped → 1 in-window
        assert_eq!(b.achievement_total, 1);
        // excluded repo PR (dotfiles) dropped → one theme: alpha
        assert_eq!(b.themes.len(), 1);
        let alpha = &b.themes[0];
        assert_eq!(alpha.theme, "alpha");
        assert_eq!(alpha.achievements.len(), 1);
        assert_eq!(alpha.merged_prs.len(), 1);
        assert_eq!(alpha.merged_prs[0].number, 12);
        // completed goal folded in
        assert_eq!(b.completed_goals.len(), 1);
        assert_eq!(b.completed_goals[0].text, "wrote the design doc");
        assert!(!b.is_empty());
    }

    #[test]
    fn unlogged_wins_excludes_referenced_prs() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = Resolved {
            repo: "alpha".into(),
            branch: "main".into(),
        };
        add(&c, &ctx, "todo", "x", None, "2026-01-01").unwrap();
        // an achievement that cites PR #12 by number → that PR is "logged"
        std::fs::write(
            c.note_dir(&ctx).join("achievements.md"),
            "# Achievements\n\n- 2026-03-01 — shipped auth rewrite (#12)\n",
        )
        .unwrap();

        let runner = gh_prs(
            "2025-12-28",
            r#"[{"number":12,"title":"auth rewrite","url":"https://x/pr/12","repository":{"nameWithOwner":"o/alpha"},"closedAt":"2026-03-01T10:00:00Z"},
                {"number":40,"title":"silent refactor","url":"https://x/pr/40","repository":{"nameWithOwner":"o/alpha"},"closedAt":"2026-03-10T10:00:00Z"}]"#,
        );

        let b = brag(&c, "2026-06-26", None, &runner);
        // #12 referenced → only #40 surfaces as an un-logged win
        assert_eq!(b.unlogged_wins.len(), 1);
        assert_eq!(b.unlogged_wins[0].number, 40);
        let md = render_markdown(&b);
        assert!(md.contains("Un-logged wins"));
        assert!(md.contains("#40 silent refactor"));
    }

    #[test]
    fn references_pr_matches_hash_url_and_bare_but_not_substring() {
        let pr = BragPr {
            number: 94,
            title: "t".into(),
            url: "https://x/pr/94".into(),
            repo: "bn".into(),
            merged_at: "2026-06-01".into(),
        };
        assert!(references_pr("did the thing (#94)", &pr));
        assert!(references_pr("see https://x/pr/94 for detail", &pr));
        assert!(references_pr("closes 94 today", &pr));
        // a longer number that merely contains 94 must not match
        assert!(!references_pr("ticket 1940 done", &pr));
        assert!(!references_pr("no number here", &pr));
    }

    #[test]
    fn references_pr_hash_requires_digit_boundary() {
        let pr12 = BragPr {
            number: 12,
            title: "t".into(),
            url: String::new(),
            repo: "bn".into(),
            merged_at: "2026-06-01".into(),
        };
        let pr123 = BragPr {
            number: 123,
            title: "t".into(),
            url: String::new(),
            repo: "bn".into(),
            merged_at: "2026-06-01".into(),
        };
        // "#12" is a substring of "#123" — PR 12 must not be marked logged by a #123 mention
        assert!(!references_pr("shipped PR #123 today", &pr12));
        // the actual PR referenced still matches
        assert!(references_pr("shipped PR #123 today", &pr123));
        // a genuine bounded #12 still matches
        assert!(references_pr("shipped PR #12 today", &pr12));
        // a real #12 alongside a longer ref must still match (all occurrences checked)
        assert!(references_pr("closed #123 and #12", &pr12));
    }

    #[test]
    fn brag_is_empty_and_renders_thin_history() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // empty runner: gh + git both fail → no PRs, no metrics, no achievements
        let b = brag(&c, "2026-06-26", None, &FakeRunner::new());
        assert!(b.is_empty());
        let md = render_markdown(&b);
        assert!(md.contains("Thin history"));
    }

    #[test]
    fn summary_line_uses_gh_merged_count_not_unsynced_event_log() {
        // Machine with an unsynced event log (no events on disk) but gh reports a merged PR:
        // `metrics.merged` reads 0 while the themes body lists the PR. The summary top-line must
        // reconcile with the gh-sourced body (1), not print the event-log 0 (issue #162, item #2).
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let runner = gh_prs(
            "2025-12-28",
            r#"[{"number":7,"title":"ship it","url":"https://x/pr/7","repository":{"nameWithOwner":"o/alpha"},"closedAt":"2026-03-01T10:00:00Z"}]"#,
        );

        let b = brag(&c, "2026-06-26", None, &runner);
        // Event log is empty → the event-log-scoped throughput metric reads 0 …
        assert_eq!(b.metrics.merged, 0);
        // … but the gh-sourced body has the merged PR.
        assert_eq!(
            b.themes.iter().map(|t| t.merged_prs.len()).sum::<usize>(),
            1
        );

        let md = render_markdown(&b);
        // Summary top-line reflects the gh count (agrees with the themes body) …
        assert!(md.contains("1 merged PR ·"), "{md}");
        assert!(!md.contains("0 merged PR ·"), "{md}");
        // … while the event-log throughput highlight stays scoped to the (empty) event log.
        assert!(md.contains("- PRs: 0 merged, 0 opened"), "{md}");
    }

    #[test]
    fn metrics_window_anchors_to_today_not_wallclock() {
        // The throughput window is anchored to `today` (the packet's "as of" date), not wall-clock
        // now. A backdated `today` with a merged-PR event inside its date window proves it: the old
        // `now_epoch_secs()` anchor pushed the cutoff to ~real-now, dropping the event (merged=0);
        // anchoring to `today` keeps the window aligned to `[start..=today]` and counts it (#162).
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // A merged-PR event dated 2020-01-07 12:00, inside the window [2020-01-05 .. 2020-01-10].
        let ts = util::epoch_day("2020-01-07").unwrap() * 86_400 + 12 * 3_600;
        let mut fields = serde_json::Map::new();
        fields.insert("repo".into(), serde_json::json!("alpha"));
        fields.insert("number".into(), serde_json::json!(7));
        crate::events::append_event_at(&c.metrics_log_path(), "pr_merged", ts, fields).unwrap();

        // `today` backdated to 2020-01-10; window start pinned to 2020-01-05 via a literal `since`.
        let b = brag(&c, "2020-01-10", Some("2020-01-05"), &FakeRunner::new());
        assert_eq!(
            b.metrics.merged, 1,
            "throughput window must include a merged PR dated within [start..=today]"
        );
    }
}
