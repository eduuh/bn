//! systemd `OnCalendar` schedule parsing — the subset the repo actually uses.
//!
//! [`Schedule::parse`] reads `[WEEKDAYS] *-*-* HH:MM:SS` (plus the `daily` shorthand) into a
//! [`Schedule`]; [`previous_occurrence`] / [`next_occurrence`] / [`slots_on`] / [`fires_on`] answer
//! "when did this last fire" and "when does it fire next" without shelling out to systemd.
//!
//! Used by the agent catalog (schedule validation), the pool refresher,
//! and the action center. Named `staff_schedule` while it lived under the staff roster; the roster
//! is gone and the parser is not staff-specific, so it is just `schedule` now.

use chrono::{Datelike, Local, NaiveDate, TimeZone, Weekday};

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

/// The clock slots (`hour`, `minute`, `second`) a spec fires at within any day it fires on.
/// Covers the full `OnCalendar` subset the cron/agent manifests use: comma lists, ranges, stepped
/// fields, optional weekday/date prefixes, and the `daily`/`hourly`/`minutely` keywords.
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
        "minutely" => {
            return Some(
                (0..24)
                    .flat_map(|hour| (0..60).map(move |minute| (hour, minute, 0)))
                    .collect(),
            )
        }
        _ => {}
    }

    // Validate the day/date prefix here even though `fires_on` evaluates it later, so catalog
    // validation rejects malformed schedules before the common scheduler starts.
    let rest = if spec.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        let (weekdays, rest) = spec.split_once(char::is_whitespace)?;
        parse_weekdays(weekdays)?;
        rest.trim()
    } else {
        spec
    };

    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let time_tok = match tokens.as_slice() {
        [time] => *time,
        [date, time] if valid_date_spec(date) => *time,
        _ => return None,
    };
    let mut parts = time_tok.split(':');
    let hour_field = parts.next()?;
    let minute_field = parts.next()?;
    let second_field = parts.next().unwrap_or("0");
    if parts.next().is_some() {
        return None;
    }
    let hours = expand_calendar_field(hour_field, 23)?;
    let minutes = expand_calendar_field(minute_field, 59)?;
    let seconds = expand_calendar_field(second_field, 59)?;
    let mut slots = Vec::new();
    for hour in hours {
        for minute in &minutes {
            for second in &seconds {
                slots.push((hour, *minute, *second));
            }
        }
    }
    slots.sort_unstable();
    slots.dedup();
    Some(slots)
}

/// Whether an `OnCalendar` string is in the enumerable subset the common agent scheduler supports.
pub fn calendar_supported(on_calendar: &str) -> bool {
    day_slots(on_calendar).is_some()
}

fn expand_calendar_field(spec: &str, max: u32) -> Option<Vec<u32>> {
    if !calendar_field_valid(spec, max) {
        return None;
    }
    let mut values = Vec::new();
    for term in spec.split(',') {
        let (base, step) = match term.split_once('/') {
            Some((base, step)) => (base, step.parse::<u32>().ok()?),
            None => (term, 1),
        };
        let (start, end) = if base == "*" {
            (0, max)
        } else if let Some((start, end)) = base.split_once("..") {
            (start.parse::<u32>().ok()?, end.parse::<u32>().ok()?)
        } else {
            let start = base.parse::<u32>().ok()?;
            (start, if term.contains('/') { max } else { start })
        };
        values.extend((start..=end).step_by(step as usize));
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}

fn valid_date_spec(spec: &str) -> bool {
    let fields: Vec<&str> = spec.split('-').collect();
    matches!(
        fields.as_slice(),
        [year, month, day]
            if calendar_field_valid(year, 9999)
                && calendar_field_valid(month, 12)
                && calendar_field_valid(day, 31)
    )
}

fn calendar_field_valid(spec: &str, max: u32) -> bool {
    !spec.is_empty()
        && spec.split(',').all(|term| {
            if term.is_empty() {
                return false;
            }
            let (base, step) = match term.split_once('/') {
                Some((base, step)) => match step.parse::<u32>() {
                    Ok(step) if step > 0 => (base, Some(step)),
                    _ => return false,
                },
                None => (term, None),
            };
            if base == "*" {
                return true;
            }
            let (start, end) = match base.split_once("..") {
                Some((start, end)) => match (start.parse::<u32>(), end.parse::<u32>()) {
                    (Ok(start), Ok(end)) => (start, end),
                    _ => return false,
                },
                None => match base.parse::<u32>() {
                    Ok(value) => (value, if step.is_some() { max } else { value }),
                    Err(_) => return false,
                },
            };
            start <= end && end <= max
        })
}

/// The local-epoch instants `on_calendar` fires at on `date`, ascending and deduped.
///
/// Composes the two halves that already exist: [`fires_on`] decides whether the day is eligible
/// (weekday and concrete-date restrictions), [`day_slots`] enumerates the within-day clock.
///
/// Empty when the spec does not fire that day **or** has no enumerable clock (`minutely`, shapes
/// this subset does not model). A timeline then shows such a job as unscheduled rather than
/// inventing a time for it — the difference between a schedule you can trust and one you cannot.
pub fn slots_on(on_calendar: &str, date: chrono::NaiveDate) -> Vec<i64> {
    if !fires_on(on_calendar, date) {
        return Vec::new();
    }
    let Some(slots) = day_slots(on_calendar) else {
        return Vec::new();
    };
    let mut out: Vec<i64> = slots
        .iter()
        .filter_map(|(h, m, sec)| {
            match Local.with_ymd_and_hms(date.year(), date.month(), date.day(), *h, *m, *sec) {
                chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
                    Some(dt.timestamp())
                }
                chrono::LocalResult::None => None,
            }
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The next instant `on_calendar` fires at **after** `now`, searching today and the next 8 days.
///
/// A scheduled job's own next firing is its natural deadline: once tomorrow's 09:00 digest is due,
/// running yesterday's is worse than not running it, because the fresh one supersedes it. `None`
/// when the spec has no enumerable clock or nothing fires within the window.
pub fn next_occurrence(on_calendar: &str, now: i64) -> Option<i64> {
    let today = chrono::DateTime::from_timestamp(now, 0)?
        .with_timezone(&Local)
        .date_naive();
    for ahead in 0..=8 {
        let date = today.checked_add_signed(chrono::Duration::days(ahead))?;
        if let Some(at) = slots_on(on_calendar, date).into_iter().find(|t| *t > now) {
            return Some(at);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_scheduler_accepts_manifest_schedule_shapes() {
        for schedule in [
            "*:0/15",
            "*-*-* 0/2:00:00",
            "Mon..Fri *-*-* 08,11,14,17:00:00",
            "Mon..Fri *-*-* 08..18/2:00:00",
            "Sun *-*-* 18:00:00",
            "daily",
            "hourly",
            "minutely",
        ] {
            assert!(calendar_supported(schedule), "{schedule}");
        }
    }

    #[test]
    fn common_scheduler_rejects_malformed_schedules() {
        for schedule in [
            "",
            "sometimes",
            "*:0/0",
            "*-*-* 25:00:00",
            "Mon..Nope *-*-* 08:00:00",
            "Mon *-*-* 18..08:00:00",
        ] {
            assert!(!calendar_supported(schedule), "{schedule}");
        }
    }
}

#[cfg(test)]
mod every_other_day_tests {
    use super::*;

    /// The `worktree-sweep` agent runs on a stepped day-of-month spec, so the scheduler has to
    /// fire it on exactly the odd days and count the shape as supported, or the due check would
    /// never see it.
    #[test]
    fn a_stepped_day_of_month_fires_on_odd_days_only() {
        let spec = "*-*-01/2 09:00:00";
        let on = |d: u32| fires_on(spec, NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        assert!(on(1), "the 1st is the base of the step");
        assert!(on(3));
        assert!(on(5));
        assert!(!on(2), "even days are between steps");
        assert!(!on(4));
        assert!(
            calendar_supported(spec),
            "the due check must understand this shape"
        );
    }
}
