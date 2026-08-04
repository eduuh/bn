//! PR throughput, review latency, in-flight PRs, close-out lag, and focus-flow
//! blocks — plus the repo-visibility filter every metrics surface reads through.

use crate::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// PR metrics over a trailing window (plus all-time in-flight).
#[derive(Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub window_days: i64,
    /// pr_opened events in the window.
    pub opened: usize,
    /// pr_merged events in the window.
    pub merged: usize,
    /// pr_closed (unmerged) events in the window.
    pub closed: usize,
    /// PRs currently open: seen opened, no terminal event (any time).
    pub in_flight: usize,
    /// merged / (merged + closed) over the window — of PRs that *finished*, how many landed.
    /// `None` when nothing finished in the window.
    pub completion_rate: Option<f64>,
    /// Median open→merge time (hours) for PRs merged in the window.
    pub cycle_median_hours: Option<f64>,
    /// 90th-percentile open→merge time (hours).
    pub cycle_p90_hours: Option<f64>,
    /// Median *change lead time* (hours): first-commit→merge, from the `lead_secs` field the
    /// pr-observe job stamps on `pr_merged`. `None` until that enrichment lands. This is the
    /// DORA lead time; cycle time above is the narrower open→merge slice.
    pub lead_median_hours: Option<f64>,
    /// 90th-percentile change lead time (hours).
    pub lead_p90_hours: Option<f64>,
}

pub(crate) const DAY: i64 = 86_400;

/// Compute PR throughput metrics from `events` as of `now` (epoch secs) over the trailing
/// `window_days`. Pure over the event slice — the caller chooses the source (this machine's log
/// or the unioned whole-person view), so the same logic serves both scopes.
pub fn compute(events: &[Event], now: i64, window_days: i64) -> Metrics {
    let cutoff = now - window_days.max(0) * DAY;

    // earliest opened ts per PR key (cycle time + in-flight), and terminal state per key
    let mut opened_ts: HashMap<String, i64> = HashMap::new();
    let mut terminal: HashMap<String, ()> = HashMap::new();
    let mut opened = 0;
    for ev in events {
        if ev.kind == "pr_opened" {
            if ev.ts >= cutoff {
                opened += 1;
            }
            if let Some(k) = pr_key(ev) {
                opened_ts
                    .entry(k)
                    .and_modify(|t| *t = (*t).min(ev.ts))
                    .or_insert(ev.ts);
            }
        }
    }

    let mut merged = 0;
    let mut closed = 0;
    let mut cycle_secs: Vec<i64> = Vec::new();
    let mut lead_secs: Vec<i64> = Vec::new();
    for ev in events {
        match ev.kind.as_str() {
            "pr_merged" => {
                if let Some(k) = pr_key(ev) {
                    terminal.insert(k.clone(), ());
                    if ev.ts >= cutoff {
                        merged += 1;
                        if let Some(&ots) = opened_ts.get(&k) {
                            if ev.ts >= ots {
                                cycle_secs.push(ev.ts - ots);
                            }
                        }
                        // Change lead time: prefer an explicit lead_secs, else derive from a
                        // first_commit_ts the job may stamp instead.
                        let lead =
                            ev.fields
                                .get("lead_secs")
                                .and_then(Value::as_i64)
                                .or_else(|| {
                                    ev.fields
                                        .get("first_commit_ts")
                                        .and_then(Value::as_i64)
                                        .map(|fc| ev.ts - fc)
                                });
                        if let Some(s) = lead.filter(|&s| s >= 0) {
                            lead_secs.push(s);
                        }
                    }
                }
            }
            "pr_closed" => {
                if let Some(k) = pr_key(ev) {
                    terminal.insert(k, ());
                    if ev.ts >= cutoff {
                        closed += 1;
                    }
                }
            }
            _ => {}
        }
    }

    let in_flight = opened_ts
        .keys()
        .filter(|k| !terminal.contains_key(*k))
        .count();
    let finished = merged + closed;
    let completion_rate = (finished > 0).then(|| merged as f64 / finished as f64);

    cycle_secs.sort_unstable();
    lead_secs.sort_unstable();
    let to_hours = |s: i64| s as f64 / 3600.0;
    Metrics {
        window_days,
        opened,
        merged,
        closed,
        in_flight,
        completion_rate,
        cycle_median_hours: percentile(&cycle_secs, 0.5).map(to_hours),
        cycle_p90_hours: percentile(&cycle_secs, 0.9).map(to_hours),
        lead_median_hours: percentile(&lead_secs, 0.5).map(to_hours),
        lead_p90_hours: percentile(&lead_secs, 0.9).map(to_hours),
    }
}

/// Review-latency over the window: how long your PRs waited on their first review. The single
/// number the code-review research centers on — `pr_opened` → the earliest `pr_review` for the
/// same PR, in hours. `None`s until the pr-observe job stamps `pr_review` events.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ReviewLatency {
    /// PRs in the window that got a first review (an open + a later review).
    pub reviewed: usize,
    /// Median time-to-first-review (hours).
    pub first_review_median_hours: Option<f64>,
    /// 90th-percentile time-to-first-review (hours).
    pub first_review_p90_hours: Option<f64>,
}

/// Compute review latency from `events` as of `now` over the trailing `window_days`: for each PR,
/// the gap from its earliest `pr_opened` to its earliest `pr_review`, counted when the review
/// landed in the window. Pure over the slice — same shape as [`compute`]. A PR with no `pr_review`
/// contributes nothing (still awaiting review), so an absent enrichment reads as "no data", not 0.
pub fn review_latency(events: &[Event], now: i64, window_days: i64) -> ReviewLatency {
    let cutoff = now - window_days.max(0) * DAY;
    // earliest opened ts per PR key
    let mut opened_ts: HashMap<String, i64> = HashMap::new();
    // earliest review ts per PR key
    let mut review_ts: HashMap<String, i64> = HashMap::new();
    for ev in events {
        let Some(k) = pr_key(ev) else { continue };
        match ev.kind.as_str() {
            "pr_opened" => {
                opened_ts
                    .entry(k)
                    .and_modify(|t| *t = (*t).min(ev.ts))
                    .or_insert(ev.ts);
            }
            "pr_review" => {
                review_ts
                    .entry(k)
                    .and_modify(|t| *t = (*t).min(ev.ts))
                    .or_insert(ev.ts);
            }
            _ => {}
        }
    }
    let mut waits: Vec<i64> = Vec::new();
    for (k, &rts) in &review_ts {
        if rts < cutoff {
            continue;
        }
        if let Some(&ots) = opened_ts.get(k) {
            if rts >= ots {
                waits.push(rts - ots);
            }
        }
    }
    waits.sort_unstable();
    let to_hours = |s: i64| s as f64 / 3600.0;
    ReviewLatency {
        reviewed: waits.len(),
        first_review_median_hours: percentile(&waits, 0.5).map(to_hours),
        first_review_p90_hours: percentile(&waits, 0.9).map(to_hours),
    }
}

/// `repo#number` identity for a PR event, or `None` if the fields are absent.
fn pr_key(ev: &Event) -> Option<String> {
    let repo = ev.fields.get("repo").and_then(Value::as_str)?;
    let number = ev.fields.get("number").and_then(Value::as_i64)?;
    Some(format!("{repo}#{number}"))
}

/// One PR seen opened with no terminal (merged/closed) event — still open right now. The
/// per-PR detail behind [`Metrics::in_flight`], so `bn next` can name each stale PR and how
/// long it's been waiting instead of only counting them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InflightPr {
    pub repo: String,
    pub number: i64,
    /// Epoch seconds of the earliest `pr_opened` for this PR.
    pub opened_ts: i64,
    /// Whole days since it opened, as of `now`.
    pub age_days: i64,
}

/// Currently-open PRs (opened, no terminal event), oldest first. Same opened/terminal pass as
/// [`compute`], but keeps the repo+number so callers can link and age each one. Pure over the
/// event slice.
pub fn in_flight_prs(events: &[Event], now: i64) -> Vec<InflightPr> {
    // earliest opened ts per PR, keyed by (repo, number)
    let mut opened: HashMap<(String, i64), i64> = HashMap::new();
    let mut terminal: HashMap<(String, i64), ()> = HashMap::new();
    for ev in events {
        let key = match (
            ev.fields.get("repo").and_then(Value::as_str),
            ev.fields.get("number").and_then(Value::as_i64),
        ) {
            (Some(r), Some(n)) => (r.to_string(), n),
            _ => continue,
        };
        match ev.kind.as_str() {
            "pr_opened" => {
                opened
                    .entry(key)
                    .and_modify(|t| *t = (*t).min(ev.ts))
                    .or_insert(ev.ts);
            }
            "pr_merged" | "pr_closed" => {
                terminal.insert(key, ());
            }
            _ => {}
        }
    }

    let mut out: Vec<InflightPr> = opened
        .into_iter()
        .filter(|(k, _)| !terminal.contains_key(k))
        .map(|((repo, number), opened_ts)| InflightPr {
            repo,
            number,
            opened_ts,
            age_days: ((now - opened_ts).max(0) / DAY),
        })
        .collect();
    out.sort_by(|a, b| b.age_days.cmp(&a.age_days).then(a.repo.cmp(&b.repo)));
    out
}

/// Nearest-rank percentile of a pre-sorted slice. `None` if empty.
pub(crate) fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    Some(sorted[idx])
}

/// A productivity event clustered into the same focus block as its neighbour when the gap is no
/// larger than this. 30 minutes: long enough to span a thinking pause or a quick lookup, short
/// enough that a lunch break ends the block. Config-overridable later.
pub(crate) const FLOW_GAP_SECS: i64 = 30 * 60;

/// Close-out lag — how long work lingered after it landed: the gap from `pr_merged` to the
/// matching `note_closed` (the literal "time to close out work"). Matched on `(owner/repo,
/// number)` — PR numbers are repo-local, so number alone collides across repos (repo A's #42
/// wrongly matching repo B's #42). The `owner/repo` join key comes from the gh `repo` field on
/// `pr_merged` and from the gh URL in `note_closed`'s `pr` field (a bn `note_closed` carries the
/// bare bn repo name, e.g. `bn`, in its own `repo` field, which needn't match the gh
/// `owner/repo`, so it isn't used as the join key). A `note_closed` whose `pr` is a bare number
/// (no repo) can't be disambiguated and matches no merge.
#[derive(Debug, Serialize, Deserialize)]
pub struct CloseOut {
    /// note_closed events in the window.
    pub closed_in_window: usize,
    /// Median merge→close lag (hours) for closes matched to a merge.
    pub lag_median_hours: Option<f64>,
    /// 90th-percentile merge→close lag (hours).
    pub lag_p90_hours: Option<f64>,
}

/// Compute close-out lag from `events` over the trailing window.
pub fn close_out_lag(events: &[Event], now: i64, window_days: i64) -> CloseOut {
    let cutoff = now - window_days.max(0) * DAY;
    // latest merge ts per (repo, PR number) — a re-merge or amended event shouldn't lengthen the
    // lag, and PR numbers are repo-local so the repo must be part of the key (else repo A's #42
    // would match repo B's #42). `repo` here is the gh `owner/repo` the pr-observe job stamps.
    let mut merged_ts: HashMap<(String, i64), i64> = HashMap::new();
    for ev in events {
        if ev.kind == "pr_merged" {
            let repo = ev.fields.get("repo").and_then(Value::as_str);
            let num = ev.fields.get("number").and_then(Value::as_i64);
            if let (Some(repo), Some(n)) = (repo, num) {
                merged_ts
                    .entry((repo.to_string(), n))
                    .and_modify(|t| *t = (*t).max(ev.ts))
                    .or_insert(ev.ts);
            }
        }
    }
    let mut lags: Vec<i64> = Vec::new();
    let mut closed = 0;
    for ev in events {
        if ev.kind == "note_closed" && ev.ts >= cutoff {
            closed += 1;
            // Join on (owner/repo, number) parsed from the note's gh PR URL — a bare number
            // can't name a repo, so it can't be matched to a merge without colliding.
            let key = ev
                .fields
                .get("pr")
                .and_then(Value::as_str)
                .and_then(|s| Some((pr_repo_from_str(s)?, pr_number_from_str(s)?)));
            if let Some(&mts) = key.as_ref().and_then(|k| merged_ts.get(k)) {
                if ev.ts >= mts {
                    lags.push(ev.ts - mts);
                }
            }
        }
    }
    lags.sort_unstable();
    let to_hours = |s: i64| s as f64 / 3600.0;
    CloseOut {
        closed_in_window: closed,
        lag_median_hours: percentile(&lags, 0.5).map(to_hours),
        lag_p90_hours: percentile(&lags, 0.9).map(to_hours),
    }
}

/// Uninterrupted focus time, sessionized from the activity event stream over the window. Events
/// closer than [`FLOW_GAP_SECS`] join one block; a block's duration is the span from its first
/// to last event. Single-event blocks contribute zero (an isolated action isn't flow). NOTE: v1
/// sees only bn mutations; git-commit timestamps fold in at Phase 4 for a fuller picture.
#[derive(Debug, Serialize, Deserialize)]
pub struct Flow {
    pub total_minutes: i64,
    pub longest_block_minutes: i64,
}

/// Compute flow time from `events` over the trailing window.
pub fn flow(events: &[Event], now: i64, window_days: i64) -> Flow {
    let cutoff = now - window_days.max(0) * DAY;
    let mut ts: Vec<i64> = events
        .iter()
        .filter(|e| e.ts >= cutoff && matches!(e.kind.as_str(), "activity" | "note_closed"))
        .map(|e| e.ts)
        .collect();
    ts.sort_unstable();

    let mut total = 0i64;
    let mut longest = 0i64;
    let mut block_start: Option<i64> = None;
    let mut prev: Option<i64> = None;
    for &t in &ts {
        match prev {
            None => block_start = Some(t),
            Some(p) if t - p > FLOW_GAP_SECS => {
                let dur = p - block_start.unwrap_or(p);
                total += dur;
                longest = longest.max(dur);
                block_start = Some(t);
            }
            _ => {}
        }
        prev = Some(t);
    }
    if let (Some(bs), Some(p)) = (block_start, prev) {
        let dur = p - bs;
        total += dur;
        longest = longest.max(dur);
    }
    Flow {
        total_minutes: total / 60,
        longest_block_minutes: longest / 60,
    }
}

/// The trailing-most run of digits in a string → a PR number. Handles `123`, `#123`, and
/// `https://github.com/o/r/pull/123`. `None` when there are no trailing digits.
fn pr_number_from_str(s: &str) -> Option<i64> {
    let digits: String = s
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// The `owner/repo` slug from a gh PR URL like `https://github.com/o/r/pull/123` → `o/r` (the
/// two path segments immediately before the `pull` segment). `None` for a bare number, `#123`,
/// or any string without a `pull` segment — there's no repo to name, so it can't join a
/// repo-keyed merge. Robust to a missing scheme (`o/r/pull/123`) and won't panic on a truncated
/// `pull/123`.
fn pr_repo_from_str(s: &str) -> Option<String> {
    let segs: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    let pull_idx = segs.iter().position(|&p| p == "pull")?;
    let owner = segs.get(pull_idx.checked_sub(2)?)?;
    let repo = segs.get(pull_idx.checked_sub(1)?)?;
    Some(format!("{owner}/{repo}"))
}


