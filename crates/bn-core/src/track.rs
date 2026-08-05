//! Time tracking — a low-friction punch log answering "what was I doing at any time today?".
//!
//! Unlike daily goals (a checklist) this axis is a **timeline**. One append-only JSONL file per
//! day (`<personal_root>/<machine>/track/<YYYY-MM-DD>.jsonl`), one punch per line, reusing the
//! [`crate::events`] substrate (tolerant reads, conflict-free per-machine writes).
//!
//! The model is a single open activity at a time: `punch_in` starts/switches the current
//! activity (the previous interval ends the instant the next punch lands); `punch_out` closes
//! the day. Intervals are *derived* from consecutive punch timestamps — never stored — so the
//! log stays a flat, append-only history. Every punch auto-stamps the current repo/branch, with
//! an optional `--tag` and a `--link` (an ADO work item or any URL, resolved like daily goals).

use crate::events::{append_event_at, read_events_at};
use crate::resolver::Config;
use serde_json::{json, Map, Value};

/// Event kind for an activity start/switch punch.
pub const KIND_IN: &str = "track_in";
/// Event kind for an end-of-day (or end-of-activity) punch.
pub const KIND_OUT: &str = "track_out";

/// One recorded punch (a row of the day's log). An `Out` punch carries no activity — it only
/// closes the currently-open interval.
#[derive(Debug, Clone, PartialEq)]
pub struct Punch {
    pub ts: i64,
    pub is_in: bool,
    pub activity: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub link: Option<String>,
}

/// A derived span of one activity: from its `In` punch to the next punch (or `now` for the
/// still-running one). Never persisted — recomputed from the punch log on read.
#[derive(Debug, Clone, PartialEq)]
pub struct Interval {
    pub activity: String,
    pub start: i64,
    pub end: i64,
    pub secs: i64,
    /// True for the activity still running now (closed at `now`, not by a later punch).
    pub open: bool,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub link: Option<String>,
}

/// How `report` rolls intervals up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportBy {
    Activity,
    Tag,
    Branch,
    Link,
}

impl ReportBy {
    /// Parse the `--by` flag; defaults to `Activity` for an unknown value.
    pub fn parse(s: &str) -> ReportBy {
        match s.trim().to_lowercase().as_str() {
            "tag" => ReportBy::Tag,
            "branch" => ReportBy::Branch,
            "link" | "task" => ReportBy::Link,
            _ => ReportBy::Activity,
        }
    }
}

/// One rolled-up row of a report: a bucket label and its summed seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportRow {
    pub label: String,
    pub secs: i64,
}

/// Result of a punch: the recorded punch plus the activity it closed (if any was running).
#[derive(Debug)]
pub struct PunchOutcome {
    pub punch: Punch,
    pub closed: Option<String>,
}

fn opt_str(fields: &Map<String, Value>, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Read a day's punches, oldest first. Missing file → empty.
pub fn read_day(cfg: &Config, date: &str) -> Vec<Punch> {
    let mut punches: Vec<Punch> = read_events_at(&cfg.track_file(date))
        .into_iter()
        .filter_map(|ev| {
            let is_in = match ev.kind.as_str() {
                KIND_IN => true,
                KIND_OUT => false,
                _ => return None,
            };
            Some(Punch {
                ts: ev.ts,
                is_in,
                activity: opt_str(&ev.fields, "activity").unwrap_or_default(),
                repo: opt_str(&ev.fields, "repo"),
                branch: opt_str(&ev.fields, "branch"),
                tag: opt_str(&ev.fields, "tag"),
                link: opt_str(&ev.fields, "link"),
            })
        })
        .collect();
    punches.sort_by_key(|p| p.ts);
    punches
}

/// Derive the activity intervals for a day's punches. Each `In` runs until the next punch of
/// any kind; the last open `In` runs to `now` (flagged `open`). `Out` punches only close.
/// Zero/negative spans (two punches in the same second) are dropped.
pub fn intervals(punches: &[Punch], now: i64) -> Vec<Interval> {
    let mut out = Vec::new();
    for (i, p) in punches.iter().enumerate() {
        if !p.is_in {
            continue;
        }
        let next_ts = punches.get(i + 1).map(|n| n.ts);
        let (end, open) = match next_ts {
            Some(ts) => (ts, false),
            None => (now.max(p.ts), true),
        };
        let secs = end - p.ts;
        if secs <= 0 && !open {
            continue;
        }
        out.push(Interval {
            activity: p.activity.clone(),
            start: p.ts,
            end,
            secs: secs.max(0),
            open,
            repo: p.repo.clone(),
            branch: p.branch.clone(),
            tag: p.tag.clone(),
            link: p.link.clone(),
        });
    }
    out
}

/// The activity in progress at `at_ts` (the interval covering it), if any. The open interval
/// extends to its `end` (which is `now`), so a query between the last punch and now still hits.
pub fn at(intervals: &[Interval], at_ts: i64) -> Option<&Interval> {
    intervals
        .iter()
        .find(|iv| at_ts >= iv.start && (at_ts < iv.end || iv.open))
}

/// Roll intervals up into summed buckets, largest first.
pub fn report(intervals: &[Interval], by: ReportBy) -> Vec<ReportRow> {
    use std::collections::BTreeMap;
    let mut sums: BTreeMap<String, i64> = BTreeMap::new();
    for iv in intervals {
        let label = match by {
            ReportBy::Activity => iv.activity.clone(),
            ReportBy::Tag => iv.tag.clone().unwrap_or_else(|| "untagged".into()),
            ReportBy::Branch => match (&iv.repo, &iv.branch) {
                (Some(r), Some(b)) => format!("{r}/{b}"),
                _ => "no branch".into(),
            },
            ReportBy::Link => iv.link.clone().unwrap_or_else(|| "no link".into()),
        };
        *sums.entry(label).or_insert(0) += iv.secs;
    }
    let mut rows: Vec<ReportRow> = sums
        .into_iter()
        .map(|(label, secs)| ReportRow { label, secs })
        .collect();
    rows.sort_by(|a, b| b.secs.cmp(&a.secs).then(a.label.cmp(&b.label)));
    rows
}

/// Total tracked seconds across a day's intervals.
pub fn total_secs(intervals: &[Interval]) -> i64 {
    intervals.iter().map(|iv| iv.secs).sum()
}

/// Append a `track_in` punch for `date` at `ts`, auto-stamping `repo`/`branch` and any
/// `tag`/`link`. Reports the activity it closed (the previously-open one), if any.
#[allow(clippy::too_many_arguments)]
pub fn punch_in(
    cfg: &Config,
    date: &str,
    ts: i64,
    activity: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    tag: Option<&str>,
    link: Option<&str>,
) -> std::io::Result<PunchOutcome> {
    let closed = currently_open(cfg, date, ts);
    let mut fields = Map::new();
    fields.insert("activity".into(), json!(activity.trim()));
    if let Some(r) = repo {
        fields.insert("repo".into(), json!(r));
    }
    if let Some(b) = branch {
        fields.insert("branch".into(), json!(b));
    }
    if let Some(t) = tag.map(str::trim).filter(|s| !s.is_empty()) {
        fields.insert("tag".into(), json!(t));
    }
    if let Some(l) = link.map(str::trim).filter(|s| !s.is_empty()) {
        fields.insert("link".into(), json!(l));
    }
    append_event_at(&cfg.track_file(date), KIND_IN, ts, fields.clone())?;
    Ok(PunchOutcome {
        punch: Punch {
            ts,
            is_in: true,
            activity: activity.trim().to_string(),
            repo: repo.map(str::to_string),
            branch: branch.map(str::to_string),
            tag: tag
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            link: link
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        },
        closed,
    })
}

/// Append a `track_out` punch for `date` at `ts`, closing the currently-open activity (if any).
pub fn punch_out(cfg: &Config, date: &str, ts: i64) -> std::io::Result<PunchOutcome> {
    let closed = currently_open(cfg, date, ts);
    append_event_at(&cfg.track_file(date), KIND_OUT, ts, Map::new())?;
    Ok(PunchOutcome {
        punch: Punch {
            ts,
            is_in: false,
            activity: String::new(),
            repo: None,
            branch: None,
            tag: None,
            link: None,
        },
        closed,
    })
}

/// The activity currently running on `date` as of `ts` (the open interval's label), if any.
fn currently_open(cfg: &Config, date: &str, ts: i64) -> Option<String> {
    open_interval(cfg, date, ts).map(|iv| iv.activity)
}

/// The currently-open interval on `date` as of `ts` — the punch you're "in" right now, carrying
/// its auto-stamped repo/branch. `None` when punched out or nothing tracked yet. This is what
/// momentum-aware ranking (`bn next`) reads to know where you currently are.
pub fn open_interval(cfg: &Config, date: &str, ts: i64) -> Option<Interval> {
    let punches = read_day(cfg, date);
    intervals(&punches, ts).into_iter().find(|iv| iv.open)
}

/// Whether an activity is currently open (the user is "punched in") as of `now`. Derives `now`'s
/// local date so it reads the right day's punch log.
pub fn is_punched_in(cfg: &Config, now: i64) -> bool {
    currently_open(cfg, &crate::util::date_of(now), now).is_some()
}

/// Format a duration in seconds as a compact `2h10m` / `45m` / `30s` string.
pub fn fmt_dur(secs: i64) -> String {
    let secs = secs.max(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// Human render for `bn track`: a header line, then one row per interval
/// (`HH:MM–HH:MM  2h10m  activity [branch] [#tag]`), then the day total. The open activity
/// shows `now` as its end and a `·` marker.
pub fn render_day(date: &str, intervals: &[Interval]) -> String {
    let mut out = format!("Track — {date}\n");
    if intervals.is_empty() {
        out.push_str("  (nothing tracked yet — `bn in \"<activity>\"`)\n");
        return out;
    }
    for iv in intervals {
        let end = if iv.open {
            "now  ".to_string()
        } else {
            hhmm(iv.end)
        };
        let marker = if iv.open { "·" } else { " " };
        out.push_str(&format!(
            "{} {}–{}  {:>6}  {}",
            marker,
            hhmm(iv.start),
            end,
            fmt_dur(iv.secs),
            iv.activity
        ));
        if let Some(tag) = &iv.tag {
            out.push_str(&format!("  #{tag}"));
        }
        if let Some(branch) = &iv.branch {
            out.push_str(&format!("  [{branch}]"));
        }
        out.push('\n');
    }
    out.push_str(&format!("  total {}\n", fmt_dur(total_secs(intervals))));
    out
}

/// Human render for `bn track report`: one row per bucket (`2h10m  label`) then the total.
pub fn render_report(rows: &[ReportRow], by: ReportBy) -> String {
    let axis = match by {
        ReportBy::Activity => "activity",
        ReportBy::Tag => "tag",
        ReportBy::Branch => "branch",
        ReportBy::Link => "link",
    };
    let mut out = format!("Track report — by {axis}\n");
    if rows.is_empty() {
        out.push_str("  (nothing tracked)\n");
        return out;
    }
    let total: i64 = rows.iter().map(|r| r.secs).sum();
    for r in rows {
        out.push_str(&format!("  {:>6}  {}\n", fmt_dur(r.secs), r.label));
    }
    out.push_str(&format!("  {:>6}  total\n", fmt_dur(total)));
    out
}

/// Resolve the effective timestamp for a `bn in` punch, honoring an optional backdate.
///
/// Exactly one of `at` / `ago` may be supplied:
/// - `at`  — a `HH:MM` wall-clock time *today* (local), e.g. the morning you actually started;
/// - `ago` — a duration before `now` (`45m`, `2h`, `1h30m`), e.g. "I started 45m ago".
///
/// With neither, returns `now`. Rejects a future time, an unparseable value, or both flags at
/// once — `bn in` is a *past-or-present* punch, never a future one. Callers derive the punch's
/// day from the returned epoch (via [`crate::util::date_of`]) so a backdate lands in the right
/// day's log even across midnight.
pub fn resolve_in_ts(
    today: &str,
    now: i64,
    at: Option<&str>,
    ago: Option<&str>,
) -> Result<i64, String> {
    match (at, ago) {
        (Some(_), Some(_)) => Err("use only one of --at / --ago".into()),
        (Some(hm), None) => {
            let ts = parse_at(today, hm)
                .ok_or_else(|| format!("invalid --at time '{hm}' (expected HH:MM)"))?;
            if ts > now {
                return Err(format!("--at {hm} is in the future"));
            }
            Ok(ts)
        }
        (None, Some(d)) => {
            let secs = crate::focus::parse_duration(d)
                .ok_or_else(|| format!("invalid --ago duration '{d}' (e.g. 45m, 2h, 1h30m)"))?;
            Ok(now - secs)
        }
        (None, None) => Ok(now),
    }
}

/// Resolve a `HH:MM` clock time on `date` (YYYY-MM-DD) to local-time epoch seconds. `None`
/// for an unparseable date/time. Backs `bn track at <HH:MM>`.
pub fn parse_at(date: &str, hm: &str) -> Option<i64> {
    use chrono::TimeZone;
    let naive =
        chrono::NaiveDateTime::parse_from_str(&format!("{date} {}", hm.trim()), "%Y-%m-%d %H:%M")
            .ok()?;
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            Some(dt.timestamp())
        }
        chrono::LocalResult::None => None,
    }
}

/// Local `HH:MM` of an epoch timestamp.
pub fn hhmm(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}
