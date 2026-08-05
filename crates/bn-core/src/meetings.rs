//! The meetings cache — a normalized, offline snapshot of today's calendar, so `bn gap` and
//! `bn next --fit` can size work against the time until your next meeting *without* a live network
//! call or reaching into [`jobs/meetings.sh`](../../../jobs/meetings.sh) internals (#247). The
//! `meetings` cron already fetches the day's calendar via WorkIQ and fans it out to the feed +
//! attention list; here it *also* pipes the same events through `bn gap ingest`, which stamps them
//! into one documented file (`state_dir()/meetings.json`). North star, same as [`crate::feed`]: the
//! script fetches, bn renders — nothing in this module touches the network.
//!
//! Read path: [`current_gap`] returns the earliest event still ahead of `now`, gated by a
//! freshness window ([`CACHE_MAX_AGE_SECS`]) so a prior working session's cache reads as no data —
//! the same "old snapshot ⇒ treat as absent" rule the prefix-J picker applies to its 10-minute
//! attention cache. A missing/stale cache, or no upcoming event, is simply "no upcoming events".

use crate::resolver::state_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How long a meetings snapshot stays usable, in seconds. The `meetings` cron refreshes ~twice a
/// day (a morning run on the first bn event of the day + an afternoon run), so half a day
/// comfortably spans the gap between refreshes while a cache from a *prior* working session (e.g.
/// yesterday afternoon read this morning before the day's first run) falls outside the window and
/// is treated as no data. Mirrors [`crate::next`]'s attention-cache staleness rule, scaled to the
/// meetings cadence.
pub const CACHE_MAX_AGE_SECS: i64 = 12 * 3600;

/// How close (in minutes) the next event must be for `bn next` to surface the gap line in its
/// header. Beyond this the dead-zone framing doesn't apply — there's real time to start something.
pub const GAP_HEADER_WINDOW_MINS: i64 = 120;

/// The buffer (seconds) `--fit` subtracts from the raw gap before sizing actions into it, so a
/// "fitting" action still leaves a moment to wrap up and walk into the meeting rather than running
/// right up to its start.
pub const FIT_BUFFER_SECS: i64 = 5 * 60;

/// One calendar event in the cache: a start time and a subject, plus an optional join link. Times
/// are `HH:MM` 24-hour; a missing/empty start means the event can't be placed on the timeline and
/// is skipped by [`current_gap`] (it can't bound a gap without a start).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meeting {
    /// Start time as `HH:MM` (24-hour), local. Empty when the source didn't supply one.
    #[serde(default)]
    pub start: String,
    /// The meeting subject.
    pub title: String,
    /// A join link (Teams URL, …) when known; empty otherwise.
    #[serde(default)]
    pub url: String,
}

/// The whole meetings snapshot: when it was taken, the day its events belong to, and the events.
/// A run REPLACES this file wholesale (like a feed source), so finished meetings and prior days
/// drop off on the next ingest rather than piling up.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MeetingsCache {
    /// When the cron wrote this snapshot (epoch seconds) — drives the freshness window.
    #[serde(default)]
    pub generated_at: i64,
    /// The local date (`YYYY-MM-DD`) the events belong to, so each event's `HH:MM` resolves to an
    /// unambiguous instant even when read across a midnight boundary.
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub events: Vec<Meeting>,
}

/// The computed gap: how long until the next event, and which event bounds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gap {
    /// Whole seconds from `now` to the event start.
    pub seconds: i64,
    /// Whole minutes (floored) from `now` to the event start — the human-facing figure.
    pub minutes: i64,
    /// The bounding event's subject.
    pub title: String,
    /// The bounding event's start time (`HH:MM`).
    pub start: String,
}

/// The one documented cache path: `state_dir()/meetings.json`.
pub fn cache_path() -> PathBuf {
    state_dir().join("meetings.json")
}

/// Read the cache, or `None` when it's missing or unparseable — a half-written file just means the
/// next cron run will fix it, never an error to the reader.
pub fn read() -> Option<MeetingsCache> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Replace the cache with `events` for `today`, stamped `now`. Creates the state dir on first
/// write. Called by `bn gap ingest` — the cron's write path.
pub fn ingest(events: Vec<Meeting>, today: &str, now: i64) -> std::io::Result<usize> {
    let n = events.len();
    let rec = MeetingsCache {
        generated_at: now,
        date: today.to_string(),
        events,
    };
    crate::store::save_json_atomic(&cache_path(), &rec)?;
    Ok(n)
}

/// Parse ingest input from the cron: either a JSON array of [`Meeting`] objects or JSONL (one per
/// line) — both natural to emit from `jq`. Blank lines and lines that don't parse are skipped, so a
/// stray log line in the pipe doesn't sink the batch. Mirrors [`crate::feed::parse_items`].
pub fn parse_items(input: &str) -> Vec<Meeting> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<Meeting>>(trimmed) {
            return items;
        }
    }
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Meeting>(l).ok())
        .collect()
}

/// Whether the snapshot is recent enough to trust: stamped in the past and within
/// [`CACHE_MAX_AGE_SECS`]. A future-dated `generated_at` (clock skew / corruption) reads as stale
/// rather than freezing a snapshot until the wall clock catches up — the same guard
/// [`crate::next`] applies to its attention cache.
pub fn is_fresh(cache: &MeetingsCache, now: i64) -> bool {
    cache.generated_at > 0
        && cache.generated_at <= now
        && now.saturating_sub(cache.generated_at) <= CACHE_MAX_AGE_SECS
}

/// The gap from `now` to the next event in `cache`, or `None` when the cache is stale ([`is_fresh`])
/// or nothing is still ahead. Events with an empty/unparseable start are skipped (they can't bound
/// a gap); of the rest, the earliest one still after `now` wins.
pub fn current_gap(cache: &MeetingsCache, now: i64) -> Option<Gap> {
    if !is_fresh(cache, now) {
        return None;
    }
    cache
        .events
        .iter()
        .filter_map(|e| crate::track::parse_at(&cache.date, &e.start).map(|epoch| (epoch, e)))
        .filter(|(epoch, _)| *epoch > now)
        .min_by_key(|(epoch, _)| *epoch)
        .map(|(epoch, e)| {
            let seconds = epoch - now;
            Gap {
                seconds,
                minutes: seconds / 60,
                title: e.title.clone(),
                start: e.start.clone(),
            }
        })
}

/// Read the cache and compute the current gap in one step — the `bn gap` / `bn next` entry point.
pub fn gap(now: i64) -> Option<Gap> {
    current_gap(&read()?, now)
}

/// The gap as a one-liner: `38m until standup (10:30)`, or `no upcoming events` when there's no
/// (fresh) upcoming event. Duration rendered compactly via [`crate::track::fmt_dur`].
pub fn render_gap(gap: Option<&Gap>) -> String {
    match gap {
        Some(g) => format!(
            "{} until {} ({})",
            crate::track::fmt_dur(g.seconds),
            g.title,
            g.start
        ),
        None => "no upcoming events".to_string(),
    }
}
