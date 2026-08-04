//! Estimation calibration — the join between the *estimate* side (todos / daily goals carrying
//! an `--est`) and the *actuals* side (the [punch log](crate::track)). Nearly nobody measures
//! their own estimation calibration, yet it's the cheapest way to make future plans honest, and
//! bn already holds both halves; this module adds the join (#245).
//!
//! Two moments feed it, both deliberately approximate (the report's job is the *ratio trend*,
//! not per-item precision):
//!
//! - **at close** — [`todo_actual_secs`] / [`goal_actual_secs`] sum the punch-log intervals over
//!   the item's open window (`[created..=done]`), scoped to the item's branch (todos) or link
//!   (daily goals). `bn done` / `bn daily done` stamp the result as `actual:` when it's non-zero,
//!   and skip cleanly otherwise so an empty log records nothing rather than garbage.
//! - **on demand** — [`report`] reads every closed item that carries *both* `est:` and `actual:`
//!   across branch notes and daily history, and renders `median(actual/est)` per bucket
//!   (`overall`, `repo:<repo>`, `tag:<tag>`) with a sample count `n` so thin buckets are visibly
//!   untrustworthy.

use crate::model::Item;
use crate::resolver::Config;
use crate::track::{self, Interval};
use crate::{focus, rollups, util};
use serde::Serialize;
use std::collections::BTreeMap;

/// Guard against a malformed date range spinning the day loop; ~11 years is far past any real
/// item lifetime, so a legitimate window always terminates on the `>= to` break first.
const MAX_WINDOW_DAYS: usize = 4000;

/// Coarse t-shirt sizes for an `--est`, in seconds — so sizing (and `bn next --fit`) works before
/// the calibration habit forms and you have real duration estimates. `S|M|L` (case-insensitive)
/// map here; anything else is parsed as a normal duration. Overridable per machine via
/// `[calib] s|m|l` in config.toml (see [`crate::resolver`]); the defaults are the issue's
/// `s=30m m=90m l=3h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoarseSizes {
    pub s: i64,
    pub m: i64,
    pub l: i64,
}

impl Default for CoarseSizes {
    fn default() -> Self {
        Self {
            s: 30 * 60,
            m: 90 * 60,
            l: 3 * 3600,
        }
    }
}

impl CoarseSizes {
    /// Seconds for a coarse token `s`/`m`/`l` (case-insensitive, trimmed), or `None` for anything
    /// else — including a real duration like `30m`, which the caller resolves via the grammar.
    pub fn get(&self, token: &str) -> Option<i64> {
        match token.trim().to_ascii_lowercase().as_str() {
            "s" => Some(self.s),
            "m" => Some(self.m),
            "l" => Some(self.l),
            _ => None,
        }
    }
}

/// Resolve an `est:` stamp to seconds: a coarse t-shirt size (`S`/`M`/`L`, from `sizes`) first,
/// else the `bn focus start` duration grammar (`90m`, `2h`, `1h30m`). `None` for a missing,
/// malformed, or non-positive value. A bare `m`/`s`/`l` can't be a valid duration on its own
/// (the grammar needs a number), so the coarse and duration namespaces never collide.
pub fn est_secs(sizes: &CoarseSizes, est: &str) -> Option<i64> {
    sizes
        .get(est)
        .or_else(|| focus::parse_duration(est).filter(|&n| n > 0))
}

/// Parse an `actual:` stamp to seconds via the `bn focus start` duration grammar. `None` for a
/// missing, malformed, or non-positive value — so a hand-edited garbage stamp folds the item out
/// of the report rather than skewing it. Actuals are always real durations (bn stamps them), so
/// coarse sizes don't apply here.
fn dur_secs(s: Option<&str>) -> Option<i64> {
    focus::parse_duration(s?).filter(|&n| n > 0)
}

/// The `actual/est` ratio for one item, or `None` unless it carries *both* stamps and both parse
/// to a positive duration. `sizes` lets a coarse `est` (`S`/`M`/`L`) calibrate like any other.
/// This is the single observation the report medians over.
fn ratio(sizes: &CoarseSizes, item: &Item) -> Option<f64> {
    let est = est_secs(sizes, item.est.as_deref()?)?;
    let actual = dur_secs(item.actual.as_deref())?;
    Some(actual as f64 / est as f64)
}

/// The calibration multiplier that best applies to a plan item, most-specific first: its
/// `repo:<repo>` bucket, then any of its `tag:<tag>` buckets, then the whole-corpus `overall`.
/// `None` when nothing has been calibrated yet — the caller then sizes on the raw estimate.
/// Backs `bn next --fit` (see [`crate::next`]); daily goals carry no repo/tags, so they resolve
/// straight to `overall`.
pub fn multiplier_for(report: &Report, repo: Option<&str>, tags: &[String]) -> Option<f64> {
    let find = |label: &str| {
        report
            .buckets
            .iter()
            .find(|b| b.bucket == label)
            .map(|b| b.multiplier)
    };
    if let Some(r) = repo {
        if let Some(m) = find(&format!("repo:{r}")) {
            return Some(m);
        }
    }
    for t in tags {
        if let Some(m) = find(&format!("tag:{t}")) {
            return Some(m);
        }
    }
    report.overall.as_ref().map(|b| b.multiplier)
}

/// Cap for a day's derivation `now`: the day's own midnight boundary, or the real `now` when
/// that's earlier (i.e. today). Bounding a still-open interval to its calendar day means a
/// forgotten `punch_out` on a past day can't balloon the total to `now`.
fn day_cap(date: &str, now: i64) -> i64 {
    match track::parse_at(date, "00:00") {
        Some(midnight) => now.min(midnight + 86_400),
        None => now,
    }
}

/// Sum punch-log seconds over the inclusive day window `[from..=to]` whose intervals satisfy
/// `keep`. Reads one day file at a time (the punch log is per-day) and folds each day's intervals
/// with a per-day [`day_cap`]. An inverted range (`from > to`) sums nothing.
pub fn window_secs(
    cfg: &Config,
    from: &str,
    to: &str,
    now: i64,
    keep: impl Fn(&Interval) -> bool,
) -> i64 {
    if from > to {
        return 0;
    }
    let mut total = 0i64;
    let mut day = from.to_string();
    for _ in 0..MAX_WINDOW_DAYS {
        let cap = day_cap(&day, now);
        let punches = track::read_day(cfg, &day);
        for iv in track::intervals(&punches, cap) {
            if keep(&iv) {
                total += iv.secs;
            }
        }
        if day.as_str() >= to {
            break;
        }
        match util::add_days(&day, 1) {
            Some(next) => day = next,
            None => break,
        }
    }
    total
}

/// Actual seconds worked on a branch-note todo: punch intervals auto-stamped with this
/// `repo`/`branch` over the todo's open window `[created..=done]`.
pub fn todo_actual_secs(
    cfg: &Config,
    repo: &str,
    branch: &str,
    created: &str,
    done: &str,
    now: i64,
) -> i64 {
    window_secs(cfg, created, done, now, |iv| {
        iv.repo.as_deref() == Some(repo) && iv.branch.as_deref() == Some(branch)
    })
}

/// Actual seconds for a daily goal: punch intervals whose `link` matches the goal's, over its
/// window. A goal with no link can't be joined to the timeline, so this is `0` — the caller
/// then stamps nothing (skips cleanly), per the design.
pub fn goal_actual_secs(cfg: &Config, link: &str, created: &str, done: &str, now: i64) -> i64 {
    window_secs(cfg, created, done, now, |iv| {
        iv.link.as_deref() == Some(link)
    })
}

/// One calibration bucket: its label (`overall`, `repo:<repo>`, `tag:<tag>`), the median
/// `actual/est` multiplier, and the sample count `n`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Bucket {
    pub bucket: String,
    pub multiplier: f64,
    pub n: usize,
}

/// The calibration report: the whole-corpus `overall` bucket (if any calibrated item exists),
/// plus the per-repo / per-tag buckets, most-sampled first.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Report {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall: Option<Bucket>,
    pub buckets: Vec<Bucket>,
}

/// Median of a sample. `0.0` for an empty slice; averages the two middle values for an even
/// count. Sorts a local copy so the caller's data (and its length) is untouched.
fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Round a multiplier to two decimals for a stable JSON/text surface.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Build the calibration report from every closed estimated item across branch notes (closed
/// notes included — a note usually closes right after its work ships) and daily history. Each
/// calibrated todo contributes to `overall`, `repo:<repo>`, and `tag:<tag>` for each of its
/// note's tags; each calibrated daily goal contributes to `overall`.
pub fn report(cfg: &Config) -> Report {
    let mut overall: Vec<f64> = Vec::new();
    let mut by_bucket: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let sizes = &cfg.coarse_sizes;

    for bn in rollups::machine_notes(cfg, false) {
        for todo in &bn.note.front.todos {
            let Some(r) = ratio(sizes, todo) else {
                continue;
            };
            overall.push(r);
            by_bucket
                .entry(format!("repo:{}", bn.repo))
                .or_default()
                .push(r);
            for tag in &bn.note.front.tags {
                by_bucket.entry(format!("tag:{tag}")).or_default().push(r);
            }
        }
    }

    for daily in read_all_daily(cfg) {
        for goal in &daily.front.goals {
            if let Some(r) = ratio(sizes, goal) {
                overall.push(r);
            }
        }
    }

    let overall_bucket = (!overall.is_empty()).then(|| Bucket {
        bucket: "overall".into(),
        multiplier: round2(median(&overall)),
        n: overall.len(),
    });
    let mut buckets: Vec<Bucket> = by_bucket
        .into_iter()
        .map(|(bucket, rs)| Bucket {
            bucket,
            multiplier: round2(median(&rs)),
            n: rs.len(),
        })
        .collect();
    // Most-sampled first so the trustworthy buckets lead; stable tie-break by label.
    buckets.sort_by(|a, b| b.n.cmp(&a.n).then_with(|| a.bucket.cmp(&b.bucket)));

    Report {
        overall: overall_bucket,
        buckets,
    }
}

/// Read every `YYYY-MM-DD.md` day file under the daily subtree (skipping `scheduled.md` and any
/// non-date stem), oldest first. Unreadable files are skipped — a hand-edit never poisons calib.
fn read_all_daily(cfg: &Config) -> Vec<crate::daily::Daily> {
    let dir = cfg.daily_dir();
    let mut dates: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if util::is_iso_date(stem) {
                dates.push(stem.to_string());
            }
        }
    }
    dates.sort();
    dates
        .iter()
        .filter_map(|d| crate::daily::read(&cfg.daily_file(d)).ok())
        .collect()
}

/// A one-line human hint for the overall multiplier: how far off, and in which direction.
fn calibration_hint(m: f64) -> String {
    let pct = ((m - 1.0).abs() * 100.0).round() as i64;
    if pct == 0 {
        "well-calibrated".into()
    } else if m > 1.0 {
        format!("under-estimate by {pct}%")
    } else {
        format!("over-estimate by {pct}%")
    }
}

/// Human render for `bn calib`: a header, one aligned row per bucket (`label ×mult (n=…)`), the
/// overall row carrying a plain-language hint. Empty corpus prints an actionable nudge.
pub fn render(report: &Report) -> String {
    let mut out = String::from("Calibration — median actual÷est\n");
    if report.overall.is_none() {
        out.push_str("  no estimated items closed yet\n");
        out.push_str("  add `--est` to a todo/goal, track time, then close it\n");
        return out;
    }
    let width = report
        .overall
        .iter()
        .chain(report.buckets.iter())
        .map(|b| b.bucket.len())
        .max()
        .unwrap_or(0);
    if let Some(o) = &report.overall {
        out.push_str(&format!(
            "  {:<width$}  ×{:.1}  (n={})   {}\n",
            o.bucket,
            o.multiplier,
            o.n,
            calibration_hint(o.multiplier),
            width = width,
        ));
    }
    for b in &report.buckets {
        out.push_str(&format!(
            "  {:<width$}  ×{:.1}  (n={})\n",
            b.bucket,
            b.multiplier,
            b.n,
            width = width,
        ));
    }
    out
}


