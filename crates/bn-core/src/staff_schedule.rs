//! Staff schedule awareness (#273): failure vs silence semantics.
//!
//! A missing morning brief means one of two very different things — the agent *failed* (an expired
//! login, a real bug; a human needs to act) or it *ran and correctly said nothing* (a zero-merge
//! Mentor day). Silence is only trustworthy if the system can tell those apart; the moment you
//! start checking by hand whether the staff ran, the framework has failed its job. The run ledger
//! ([`crate::staff::RunStatus`] on every `staff_run` event) already records each dispatch outcome;
//! this module adds the two pieces that close the gap:
//!
//! 1. **Schedule awareness** — [`Schedule::parse`] reads the subset of systemd `OnCalendar`
//!    grammar the roster actually uses (`[WEEKDAYS] *-*-* HH:MM:SS`, plus the `daily` shorthand)
//!    into a [`Schedule`], and [`Schedule::previous_occurrence`] computes the most recent instant a
//!    run *should* have fired at or before now. A sub-daily or otherwise unsupported spec parses to
//!    `None` and is never checked for misses — a job that fires every few minutes has no meaningful
//!    "missed one run" story.
//! 2. **Missed-run detection** — [`missed_runs`] joins the schedule against each agent's last
//!    recorded run. A slot that fired more than a grace window ago with no ledger row at or after
//!    it is a **missed** run: silence that is *death*, not by design. A skipped run (backpressure
//!    self-throttle, budget shed) and a silent run both still write a ledger row, so neither is
//!    ever mistaken for a miss — the acceptance's "no false missed on homing no-ops or budget
//!    skips".
//!
//! Silence itself is recorded on the run event by [`crate::staff::run_pipeline`] (a validated card
//! with no routed items is `ok-silent`, flagged on [`crate::staff::LastRun::silent`]); this module
//! only reads it back. The taxonomy every surface renders — `ok-with-card` / `ok-silent` /
//! `missed` / `fail` / `auth` / `rate-limited` / `skipped` / `never` — is spelled in exactly one
//! place, [`health_label`], so the `bn staff` board, the `bn doctor` list, and the nvim cockpit
//! panel can never drift.

use crate::staff::{LastRun, ResolvedAgent, RunStatus};
use chrono::{Datelike, Local, NaiveDate, TimeZone, Weekday};
use std::collections::HashMap;

/// Grace window (seconds) after a scheduled slot before a no-show counts as *missed*. It has to
/// clear every legitimate reason a run lands late: systemd `RandomizedDelaySec` (staff agents use
/// up to 10m), a `Persistent=` catch-up fire after the box wakes, and a `catch_up` agent that
/// waits for the first bn event of the day. Deliberately generous — a *false* "missed" is worse
/// than a *late* "missed", because it trains you to distrust the one signal that is supposed to
/// make silence safe.
pub const DEFAULT_MISS_GRACE_SECS: i64 = 3 * 60 * 60;

/// A staff agent's schedule, parsed from the subset of systemd `OnCalendar` grammar the roster
/// uses. Only daily-at-a-fixed-time shapes are modelled; anything else parses to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Allowed weekdays as `0 = Sun .. 6 = Sat` (matching [`chrono::Weekday::num_days_from_sunday`]
    /// and [`crate::util::weekday_index`]); `None` = every day.
    weekdays: Option<Vec<u32>>,
    hour: u32,
    minute: u32,
    second: u32,
}

impl Schedule {
    /// Parse an `OnCalendar` spec into a daily-at-a-time [`Schedule`]. Returns `None` for a spec
    /// this module does not model — a sub-daily repeat (`*:0/15`), a one-shot with a concrete date
    /// (`2026-06-01 …`), or an unrecognized keyword — so those agents are simply never
    /// missed-checked.
    pub fn parse(on_calendar: &str) -> Option<Schedule> {
        let spec = on_calendar.trim();
        if spec.is_empty() {
            return None;
        }
        // systemd shorthand keywords. Only `daily` maps to a single fixed daily slot; `hourly`,
        // `minutely`, `weekly`, etc. are either sub-daily or need a reference epoch we don't model.
        if spec.eq_ignore_ascii_case("daily") {
            return Some(Schedule {
                weekdays: None,
                hour: 0,
                minute: 0,
                second: 0,
            });
        }

        // A leading alphabetic token is the day-of-week spec, separated from the date/time by
        // whitespace (`Mon..Fri *-*-* 05:30:00`). Otherwise the whole spec is the date/time part.
        let (weekdays, rest) = if spec.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            let (dow, tail) = spec.split_once(char::is_whitespace)?;
            (Some(parse_weekdays(dow)?), tail.trim())
        } else {
            (None, spec)
        };

        // `rest` is now `*-*-* HH:MM:SS`, `*-*-* HH:MM`, or a bare `HH:MM[:SS]`. A concrete date
        // component (anything but the `*-*-*` wildcard) is a one-shot, not a recurring schedule.
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let time_tok = match tokens.as_slice() {
            [date, time] => {
                if *date != "*-*-*" {
                    return None;
                }
                *time
            }
            [time] => *time,
            _ => return None,
        };
        let (hour, minute, second) = parse_hms(time_tok)?;
        Some(Schedule {
            weekdays,
            hour,
            minute,
            second,
        })
    }

    /// The most recent instant this schedule fired at or before `now` (epoch seconds), in the
    /// local timezone. `None` when nothing has fired within the last week (only reachable for a
    /// weekday-restricted schedule whose days all fall outside the window, which cannot happen for
    /// a 7-day lookback) or when the candidate local time does not exist (a DST spring-forward
    /// gap), in which case that day is skipped.
    pub fn previous_occurrence(&self, now: i64) -> Option<i64> {
        let now_local = chrono::DateTime::from_timestamp(now, 0)?.with_timezone(&Local);
        let today = now_local.date_naive();
        for back in 0..=7 {
            let date = today.checked_sub_signed(chrono::Duration::days(back))?;
            if let Some(days) = &self.weekdays {
                if !days.contains(&date.weekday().num_days_from_sunday()) {
                    continue;
                }
            }
            let candidate = match Local.with_ymd_and_hms(
                date.year(),
                date.month(),
                date.day(),
                self.hour,
                self.minute,
                self.second,
            ) {
                chrono::LocalResult::Single(dt) => dt,
                // A fall-back DST repeat — the earlier instant is the one that fired.
                chrono::LocalResult::Ambiguous(dt, _) => dt,
                // A spring-forward gap — this wall-clock time never existed; skip the day.
                chrono::LocalResult::None => continue,
            };
            let epoch = candidate.timestamp();
            if epoch <= now {
                return Some(epoch);
            }
        }
        None
    }
}

/// Map a systemd weekday spec (`Mon`, `Mon..Fri`, `Sat,Sun`, `Mon,Wed..Fri`) to the set of allowed
/// day indices (`0 = Sun .. 6 = Sat`). `None` if any token is unrecognized.
fn parse_weekdays(spec: &str) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.split_once("..") {
            Some((from, to)) => {
                let (from, to) = (weekday_num(from)?, weekday_num(to)?);
                // Inclusive range, wrapping through the week (systemd allows e.g. `Fri..Mon`).
                let mut day = from;
                for _ in 0..7 {
                    out.push(day);
                    if day == to {
                        break;
                    }
                    day = (day + 1) % 7;
                }
            }
            None => out.push(weekday_num(part)?),
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// Whether the systemd `OnCalendar` spec `on_calendar` has an occurrence on the local date
/// `date` — the "fires today" predicate the dashboard's Today tab uses to pick out the crons /
/// staff agents relevant *now* (#575). Unlike [`Schedule::parse`] (which only models a single
/// daily-at-a-time slot for miss detection), this covers the whole shape space the roster + cron
/// manifests actually use: sub-daily repeats (`*:0/15`), hour-stepped days (`*-*-* 0/6:00:00`),
/// plain daily times, weekday-restricted times (`Mon..Fri *-*-* 05:30:00`), the `daily`/`hourly`/…
/// keywords, and concrete one-shot dates (`2026-06-01 12:00:00`). Only a leading weekday spec or a
/// concrete date component can *exclude* a day; anything unrecognized errs toward firing (so a
/// relevant job is shown rather than silently dropped).
pub fn fires_on(on_calendar: &str, date: NaiveDate) -> bool {
    let spec = on_calendar.trim();
    if spec.is_empty() {
        return false;
    }
    // systemd shorthand keywords — the recurring ones map to a per-day / per-period test.
    match spec.to_ascii_lowercase().as_str() {
        "minutely" | "hourly" | "daily" => return true,
        "weekly" => return date.weekday() == Weekday::Mon,
        "monthly" => return date.day() == 1,
        "quarterly" => return date.day() == 1 && matches!(date.month(), 1 | 4 | 7 | 10),
        "semiannually" => return date.day() == 1 && matches!(date.month(), 1 | 7),
        "yearly" | "annually" => return date.day() == 1 && date.month() == 1,
        _ => {}
    }

    // A leading alphabetic token is the day-of-week restriction (`Mon..Fri *-*-* 05:30:00`); an
    // unrecognized leading token is a spec shape we don't model — don't exclude on it.
    let (weekdays, rest) = if spec.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        match spec.split_once(char::is_whitespace) {
            Some((dow, tail)) => match parse_weekdays(dow) {
                Some(days) => (Some(days), tail.trim()),
                None => return true,
            },
            None => return true,
        }
    } else {
        (None, spec)
    };
    if let Some(days) = &weekdays {
        if !days.contains(&date.weekday().num_days_from_sunday()) {
            return false;
        }
    }

    // What's left is the date/time part. Only a concrete date component (not the `*-*-*` wildcard)
    // can rule *today* out; a bare or sub-daily time fires every day the weekday test allows.
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let date_tok = match tokens.as_slice() {
        [d, _time] => Some(*d),
        // A lone token with a colon is the time part (`*:0/15`, `05:30`); otherwise it's a date.
        [one] if !one.contains(':') => Some(*one),
        _ => None,
    };
    match date_tok {
        None => true,
        Some(d) => date_component_matches(d, date),
    }
}

/// Whether the `OnCalendar` spec fires on today's local date (honouring `$BN_TODAY` for
/// deterministic tests). The date-only convenience wrapper over [`fires_on`].
pub fn fires_today(on_calendar: &str) -> bool {
    match NaiveDate::parse_from_str(&crate::util::today(), "%Y-%m-%d") {
        Ok(date) => fires_on(on_calendar, date),
        // An unparseable `$BN_TODAY` shouldn't hide the whole registry — fall back to local now.
        Err(_) => fires_on(on_calendar, Local::now().date_naive()),
    }
}

/// The clock slots (`hour`, `minute`, `second`) a spec fires at within any day it fires on.
/// Covers the full `OnCalendar` subset the cron/agent manifests use: the sub-daily minute repeat
/// (`*:M/N`), plain and hour-stepped daily times (`*-*-* HH:MM:SS`, `*-*-* H/S:MM:SS`, with an
/// optional weekday prefix that [`fires_on`] resolves separately), and the `daily`/`hourly`
/// keywords. `None` for a shape without an enumerable within-day clock (e.g. `minutely`, or a
/// spec this subset does not model), so [`previous_occurrence`] simply reports no slot.
///
/// This is the single within-day enumerator shared with [`fires_on`]'s date/weekday parser — the
/// "due" check in [`crate::agents`] composes the two rather than re-parsing `OnCalendar`.
fn day_slots(on_calendar: &str) -> Option<Vec<(u32, u32, u32)>> {
    let spec = on_calendar.trim();
    if spec.is_empty() {
        return None;
    }
    match spec.to_ascii_lowercase().as_str() {
        "daily" => return Some(vec![(0, 0, 0)]),
        "hourly" => return Some((0..24).map(|h| (h, 0, 0)).collect()),
        _ => {}
    }
    // Sub-daily minute repeat `*:M/N` — fires at minutes M, M+N, … (< 60) of every hour.
    if let Some((start, step)) = spec.strip_prefix("*:").and_then(|r| r.split_once('/')) {
        let start: u32 = start.parse().ok()?;
        let step: u32 = step.parse().ok().filter(|s| *s > 0)?;
        if start > 59 {
            return None;
        }
        let mut slots = Vec::new();
        for h in 0..24 {
            let mut m = start;
            while m < 60 {
                slots.push((h, m, 0));
                m += step;
            }
        }
        return Some(slots);
    }
    // Otherwise a `[WEEKDAY] [*-*-*] HH[/S]:MM[:SS]` form — the time is the last whitespace token,
    // the weekday/date part is handled by `fires_on` for day eligibility.
    let rest = if spec.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        spec.split_once(char::is_whitespace)
            .map(|(_, t)| t.trim())?
    } else {
        spec
    };
    let time_tok = rest.split_whitespace().last()?;
    if !time_tok.contains(':') {
        return None;
    }
    let mut parts = time_tok.split(':');
    let hour_field = parts.next()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || minute > 59 || second > 59 {
        return None;
    }
    let hours: Vec<u32> = if let Some((base, step)) = hour_field.split_once('/') {
        let step: u32 = step.parse().ok().filter(|s| *s > 0)?;
        let start: u32 = if base == "*" || base.is_empty() {
            0
        } else {
            base.parse().ok()?
        };
        (start..24).step_by(step as usize).collect()
    } else if hour_field == "*" {
        (0..24).collect()
    } else {
        vec![hour_field.parse().ok()?]
    };
    if hours.iter().any(|h| *h > 23) {
        return None;
    }
    Some(hours.into_iter().map(|h| (h, minute, second)).collect())
}

/// The most recent instant the `OnCalendar` spec `on_calendar` fired at or before `now` (epoch
/// seconds), in the local timezone — the general form of [`Schedule::previous_occurrence`] that
/// also covers sub-daily and hour-stepped specs. `None` when the spec has no enumerable slot
/// ([`day_slots`] returns `None`) or nothing fired within the last week. Backs the agent "due"
/// check: an agent is due when its previous occurrence is newer than its last successful run.
pub fn previous_occurrence(on_calendar: &str, now: i64) -> Option<i64> {
    let slots = day_slots(on_calendar)?;
    if slots.is_empty() {
        return None;
    }
    let now_local = chrono::DateTime::from_timestamp(now, 0)?.with_timezone(&Local);
    let today = now_local.date_naive();
    for back in 0..=8 {
        let date = today.checked_sub_signed(chrono::Duration::days(back))?;
        if !fires_on(on_calendar, date) {
            continue;
        }
        // The latest slot at or before `now` on this eligible day, if any.
        let mut best: Option<i64> = None;
        for (h, m, s) in &slots {
            let epoch =
                match Local.with_ymd_and_hms(date.year(), date.month(), date.day(), *h, *m, *s) {
                    chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
                        dt.timestamp()
                    }
                    chrono::LocalResult::None => continue,
                };
            if epoch <= now {
                best = Some(best.map_or(epoch, |b: i64| b.max(epoch)));
            }
        }
        if let Some(b) = best {
            return Some(b);
        }
    }
    None
}

/// Whether a systemd `Y-M-D` date component (each field `*`, a number, an `a..b` range, a
/// `base/step` repeat, or a comma list of those) has a match on `date`. Unknown shape → `true`.
fn date_component_matches(spec: &str, date: NaiveDate) -> bool {
    match spec.split('-').collect::<Vec<_>>().as_slice() {
        [y, m, d] => {
            date_field_matches(y, date.year().max(0) as u32)
                && date_field_matches(m, date.month())
                && date_field_matches(d, date.day())
        }
        _ => true,
    }
}

/// Whether a single systemd calendar field matches `value`: `*` (any), a comma list, a `base/step`
/// repeat (`0/6` = 0,6,12,…; `*` base = from 0), an `a..b` inclusive range, or a plain number.
fn date_field_matches(field: &str, value: u32) -> bool {
    let field = field.trim();
    if field == "*" {
        return true;
    }
    if field.contains(',') {
        return field.split(',').any(|p| date_field_matches(p, value));
    }
    if let Some((base, step)) = field.split_once('/') {
        let step: u32 = match step.parse() {
            Ok(s) if s > 0 => s,
            _ => return false,
        };
        let start: u32 = if base == "*" || base.is_empty() {
            0
        } else {
            match base.parse() {
                Ok(n) => n,
                Err(_) => return false,
            }
        };
        return value >= start && (value - start).is_multiple_of(step);
    }
    if let Some((a, b)) = field.split_once("..") {
        return match (a.parse::<u32>(), b.parse::<u32>()) {
            (Ok(a), Ok(b)) => value >= a && value <= b,
            _ => false,
        };
    }
    field.parse::<u32>().map(|n| n == value).unwrap_or(false)
}

/// A single systemd weekday token → `0 = Sun .. 6 = Sat`.
fn weekday_num(token: &str) -> Option<u32> {
    Some(match token.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" => 0,
        "mon" | "monday" => 1,
        "tue" | "tuesday" => 2,
        "wed" | "wednesday" => 3,
        "thu" | "thursday" => 4,
        "fri" | "friday" => 5,
        "sat" | "saturday" => 6,
        _ => return None,
    })
}

/// Parse `HH:MM` or `HH:MM:SS` into `(hour, minute, second)`, rejecting anything else (a repeating
/// `*:0/15` time component fails here, which is how sub-daily schedules are excluded).
fn parse_hms(token: &str) -> Option<(u32, u32, u32)> {
    let mut parts = token.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

/// A scheduled run that should have fired but left no ledger row — a *missed* run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissedRun {
    pub agent: String,
    pub on_calendar: String,
    /// Epoch of the missed scheduled slot.
    pub scheduled: i64,
    /// Epoch of the agent's last recorded run, or `0` if it has never run.
    pub last_run: i64,
}

/// The missed scheduled slot for one agent, or `None` when it is on schedule. A run is *missed*
/// when its most recent scheduled slot (per `on_calendar`) fired more than `grace` seconds before
/// `now` and no run is recorded at or after that slot. Every recorded status counts as "it fired"
/// — an `ok`/`ok-silent` run, and equally a `skipped` (throttle/budget) or `fail`/`auth` run — so
/// only true absence is a miss. `None` for a schedule this module does not model.
pub fn missed_for(on_calendar: &str, last_run: Option<i64>, now: i64, grace: i64) -> Option<i64> {
    let slot = Schedule::parse(on_calendar)?.previous_occurrence(now)?;
    if now.saturating_sub(slot) < grace {
        // Still inside the grace window: the run may be pending, delayed, or in flight.
        return None;
    }
    match last_run {
        Some(ts) if ts >= slot => None,
        _ => Some(slot),
    }
}

/// Every roster agent whose most recent scheduled run is missed (see [`missed_for`]), joined
/// against the run ledger `last`. Pure — reads the passed roster + last-run map and raises
/// nothing; the caller decides how to surface the result. Sorted oldest-missed-slot first, then by
/// agent name, for stable rendering.
pub fn missed_runs(
    agents: &[ResolvedAgent],
    last: &HashMap<String, LastRun>,
    now: i64,
    grace: i64,
) -> Vec<MissedRun> {
    let mut out: Vec<MissedRun> = agents
        .iter()
        .filter_map(|a| {
            let last_ts = last.get(&a.name).map(|r| r.ts);
            missed_for(a.on_calendar.as_deref().unwrap_or(""), last_ts, now, grace).map(|slot| {
                MissedRun {
                    agent: a.name.clone(),
                    on_calendar: a.on_calendar.clone().unwrap_or_default(),
                    scheduled: slot,
                    last_run: last_ts.unwrap_or(0),
                }
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.scheduled
            .cmp(&b.scheduled)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    out
}

/// The health word a surface renders for an agent, given its last run (if any) and whether a
/// scheduled slot was missed. The single spelling of the #273 taxonomy: `missed` (a slot with no
/// run — a hard miss) outranks any stale last-run word; then a clean run splits into `ok-silent`
/// (ran, correctly said nothing) vs `ok-with-card`; and `fail` / `auth` / `rate-limited` /
/// `skipped` pass the run status through. `never` when the agent has no run and nothing was missed.
pub fn health_label(last: Option<&LastRun>, missed: bool) -> &'static str {
    if missed {
        return "missed";
    }
    match last {
        None => "never",
        Some(run) => match run.status {
            RunStatus::Ok if run.silent => "ok-silent",
            RunStatus::Ok => "ok-with-card",
            RunStatus::Fail => "fail",
            RunStatus::Auth => "auth",
            RunStatus::RateLimited => "rate-limited",
            RunStatus::Skipped => "skipped",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local-time epoch for a wall-clock instant, computed the same way [`Schedule`] does so the
    /// assertions are independent of the test machine's timezone.
    fn local_epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .expect("valid local time")
            .timestamp()
    }

    #[test]
    fn parses_daily_at_time() {
        let s = Schedule::parse("*-*-* 06:30:00").unwrap();
        assert_eq!(s.weekdays, None);
        assert_eq!((s.hour, s.minute, s.second), (6, 30, 0));
    }

    #[test]
    fn parses_weekday_range() {
        let s = Schedule::parse("Mon..Fri *-*-* 05:30:00").unwrap();
        assert_eq!(s.weekdays, Some(vec![1, 2, 3, 4, 5]));
        assert_eq!((s.hour, s.minute, s.second), (5, 30, 0));
    }

    #[test]
    fn parses_single_weekday_and_lists() {
        assert_eq!(
            Schedule::parse("Fri *-*-* 17:30:00").unwrap().weekdays,
            Some(vec![5])
        );
        assert_eq!(
            Schedule::parse("Sat,Sun *-*-* 09:00:00").unwrap().weekdays,
            Some(vec![0, 6])
        );
        // A wrapping range and the `daily` keyword.
        assert_eq!(
            Schedule::parse("Fri..Mon *-*-* 09:00").unwrap().weekdays,
            Some(vec![0, 1, 5, 6])
        );
        assert_eq!(Schedule::parse("daily").unwrap().weekdays, None);
    }

    #[test]
    fn rejects_sub_daily_and_dated_and_garbage() {
        // Sub-daily repeats have no single-run miss story.
        assert!(Schedule::parse("*:0/15").is_none());
        assert!(Schedule::parse("*:0/2").is_none());
        // A concrete date is a one-shot, not a recurring daily slot.
        assert!(Schedule::parse("2026-06-01 06:30:00").is_none());
        // Garbage and unknown keywords.
        assert!(Schedule::parse("").is_none());
        assert!(Schedule::parse("Xyz *-*-* 06:00:00").is_none());
        assert!(Schedule::parse("weekly").is_none());
        assert!(Schedule::parse("*-*-* 25:00:00").is_none());
    }

    #[test]
    fn previous_occurrence_same_day_when_time_passed() {
        // 2026-06-22 is a Monday. now = 09:00, slot 05:30 → today's 05:30.
        let now = local_epoch(2026, 6, 22, 9, 0, 0);
        let s = Schedule::parse("Mon..Fri *-*-* 05:30:00").unwrap();
        assert_eq!(
            s.previous_occurrence(now),
            Some(local_epoch(2026, 6, 22, 5, 30, 0))
        );
    }

    #[test]
    fn previous_occurrence_yesterday_when_time_not_yet_reached() {
        // now = Monday 05:00, before today's 05:30 → the previous weekday slot: Friday 06-19.
        let now = local_epoch(2026, 6, 22, 5, 0, 0);
        let s = Schedule::parse("Mon..Fri *-*-* 05:30:00").unwrap();
        assert_eq!(
            s.previous_occurrence(now),
            Some(local_epoch(2026, 6, 19, 5, 30, 0))
        );
    }

    #[test]
    fn previous_occurrence_skips_disallowed_weekdays() {
        // 2026-06-21 is a Sunday; a weekday-only schedule's last fire was Friday 06-19.
        let now = local_epoch(2026, 6, 21, 9, 0, 0);
        let s = Schedule::parse("Mon..Fri *-*-* 05:30:00").unwrap();
        assert_eq!(
            s.previous_occurrence(now),
            Some(local_epoch(2026, 6, 19, 5, 30, 0))
        );
    }

    #[test]
    fn missed_when_no_run_since_slot() {
        let now = local_epoch(2026, 6, 22, 12, 0, 0);
        let slot = local_epoch(2026, 6, 22, 5, 30, 0);
        // Last run was yesterday — nothing at/after today's slot, well past grace → missed.
        let last = local_epoch(2026, 6, 21, 5, 30, 0);
        assert_eq!(
            missed_for(
                "Mon..Fri *-*-* 05:30:00",
                Some(last),
                now,
                DEFAULT_MISS_GRACE_SECS
            ),
            Some(slot)
        );
    }

    #[test]
    fn not_missed_when_run_recorded_at_or_after_slot() {
        let now = local_epoch(2026, 6, 22, 12, 0, 0);
        let slot = local_epoch(2026, 6, 22, 5, 30, 0);
        // A run exactly at the slot, and one after it, both clear the miss.
        assert_eq!(
            missed_for(
                "Mon..Fri *-*-* 05:30:00",
                Some(slot),
                now,
                DEFAULT_MISS_GRACE_SECS
            ),
            None
        );
        assert_eq!(
            missed_for(
                "Mon..Fri *-*-* 05:30:00",
                Some(slot + 600),
                now,
                DEFAULT_MISS_GRACE_SECS
            ),
            None
        );
    }

    #[test]
    fn not_missed_inside_grace_window() {
        // now = 06:00, only 30m past the 05:30 slot — inside the 3h grace, so not yet a miss even
        // with no run recorded (the run may be delayed by RandomizedDelaySec or catch-up).
        let now = local_epoch(2026, 6, 22, 6, 0, 0);
        assert_eq!(
            missed_for(
                "Mon..Fri *-*-* 05:30:00",
                None,
                now,
                DEFAULT_MISS_GRACE_SECS
            ),
            None
        );
    }

    #[test]
    fn never_run_agent_past_grace_is_missed() {
        // A declared agent that never ran and whose slot is well past grace → surfaced (its cron
        // may not be registered). last_run defaults to 0 in the join.
        let now = local_epoch(2026, 6, 22, 12, 0, 0);
        let slot = local_epoch(2026, 6, 22, 5, 30, 0);
        assert_eq!(
            missed_for(
                "Mon..Fri *-*-* 05:30:00",
                None,
                now,
                DEFAULT_MISS_GRACE_SECS
            ),
            Some(slot)
        );
    }

    #[test]
    fn unsupported_schedule_never_missed() {
        // A sub-daily job is never missed-checked regardless of the ledger.
        let now = local_epoch(2026, 6, 22, 12, 0, 0);
        assert_eq!(
            missed_for("*:0/15", None, now, DEFAULT_MISS_GRACE_SECS),
            None
        );
    }

    #[test]
    fn health_label_taxonomy() {
        let run = |status: RunStatus, silent: bool| LastRun {
            status,
            ts: 1_700_000_000,
            error: None,
            silent,
        };
        // Missed outranks any stale last-run word.
        assert_eq!(
            health_label(Some(&run(RunStatus::Ok, false)), true),
            "missed"
        );
        assert_eq!(health_label(None, true), "missed");
        // A clean run splits by whether it had anything to say.
        assert_eq!(
            health_label(Some(&run(RunStatus::Ok, false)), false),
            "ok-with-card"
        );
        assert_eq!(
            health_label(Some(&run(RunStatus::Ok, true)), false),
            "ok-silent"
        );
        // Failure statuses pass through; no run and no miss reads as never.
        assert_eq!(
            health_label(Some(&run(RunStatus::Fail, false)), false),
            "fail"
        );
        assert_eq!(
            health_label(Some(&run(RunStatus::Auth, false)), false),
            "auth"
        );
        assert_eq!(
            health_label(Some(&run(RunStatus::RateLimited, false)), false),
            "rate-limited"
        );
        assert_eq!(
            health_label(Some(&run(RunStatus::Skipped, false)), false),
            "skipped"
        );
        assert_eq!(health_label(None, false), "never");
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn fires_on_sub_daily_and_daily_every_day() {
        // Sub-daily repeats and plain daily times fire on any date.
        let mon = date(2026, 6, 22); // Monday
        let sun = date(2026, 6, 21); // Sunday
        for spec in ["*:0/15", "*:0/2", "*:7/15", "daily", "hourly", "minutely"] {
            assert!(fires_on(spec, mon), "{spec} should fire Monday");
            assert!(fires_on(spec, sun), "{spec} should fire Sunday");
        }
        // Wildcard-date daily times + hour-stepped days also fire every day.
        assert!(fires_on("*-*-* 06:30:00", mon));
        assert!(fires_on("*-*-* 0/6:00:00", sun));
        assert!(fires_on("*-*-* 0/3:20:00", mon));
    }

    #[test]
    fn fires_on_respects_weekday_restriction() {
        let mon = date(2026, 6, 22);
        let sat = date(2026, 6, 20);
        let sun = date(2026, 6, 21);
        assert!(fires_on("Mon..Fri *-*-* 05:30:00", mon));
        assert!(!fires_on("Mon..Fri *-*-* 05:30:00", sat));
        assert!(!fires_on("Mon..Fri *-*-* 05:30:00", sun));
        assert!(fires_on("Mon *-*-* 06:00:00", mon));
        assert!(!fires_on("Mon *-*-* 06:00:00", sun));
        assert!(fires_on("Sun *-*-* 18:00:00", sun));
        assert!(!fires_on("Sun *-*-* 18:00:00", mon));
        assert!(fires_on("Sat *-*-* 10:00:00", sat));
    }

    #[test]
    fn fires_on_keyword_periods() {
        assert!(fires_on("weekly", date(2026, 6, 22))); // Monday
        assert!(!fires_on("weekly", date(2026, 6, 23))); // Tuesday
        assert!(fires_on("monthly", date(2026, 6, 1)));
        assert!(!fires_on("monthly", date(2026, 6, 2)));
        assert!(fires_on("yearly", date(2026, 1, 1)));
        assert!(!fires_on("yearly", date(2026, 2, 1)));
        assert!(fires_on("quarterly", date(2026, 4, 1)));
        assert!(!fires_on("quarterly", date(2026, 5, 1)));
    }

    #[test]
    fn fires_on_concrete_one_shot_date() {
        // A concrete date fires only on that day.
        assert!(fires_on("2026-06-22 12:00:00", date(2026, 6, 22)));
        assert!(!fires_on("2026-06-22 12:00:00", date(2026, 6, 23)));
        // Stepped/wildcard date fields.
        assert!(fires_on("*-*-01 12:00:00", date(2026, 6, 1)));
        assert!(!fires_on("*-*-01 12:00:00", date(2026, 6, 2)));
    }

    #[test]
    fn fires_on_empty_and_unknown() {
        // Empty never fires; an unmodelled shape errs toward firing (shown, not dropped).
        assert!(!fires_on("", date(2026, 6, 22)));
        assert!(!fires_on("   ", date(2026, 6, 22)));
        assert!(fires_on("Xyz *-*-* 06:00:00", date(2026, 6, 22)));
    }
}
