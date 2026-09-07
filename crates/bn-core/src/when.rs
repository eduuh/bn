//! "When should this run?" — the one place bn turns a person's phrasing into an instant.
//!
//! The queue could already say *how long from now* (`--for 2h`, via [`crate::focus::parse_duration`])
//! and nothing else. You could not say `12:30`, you could not say `lunch`, and you could not say
//! `tomorrow` — so "run the build at lunch when I'm away", which is the thing a warm pool exists
//! for, had no way to be expressed except by editing `config.toml` by hand.
//!
//! Four shapes, in the order someone reaches for them:
//!
//! | input | means |
//! |---|---|
//! | `in 2h`, `2h` | that far from now — [`crate::focus::parse_duration`] |
//! | `12:30`, `5pm`, `17:30` | the next time the clock reads that |
//! | `lunch`, `after-work` | the next opening of the window with that name |
//! | `tomorrow`, `tonight` | 09:00 and 20:00 of the relevant day |
//!
//! **A clock time always resolves forward.** `bn queue at <id> 09:00` typed at 2pm means tomorrow
//! morning, not nine hours ago — a time in the past would set `not_before` to an instant that has
//! already passed, which is indistinguishable from not scheduling at all and is never what someone
//! meant by naming a time.
//!
//! Named windows resolve through [`crate::schedule`], so a window keeps one definition and the
//! parser cannot drift from what the dispatcher will actually do with it.

use crate::queue::window::PriorityWindow;

/// Turn `input` into an epoch second, or say why it could not be read.
///
/// `windows` is the machine's configured schedule, consulted only for the named-window form, so a
/// caller with no windows still gets clock and relative parsing.
pub fn parse(input: &str, windows: &[PriorityWindow], now: i64) -> Result<i64, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("say when: a time (`12:30`), a delay (`in 2h`), or a window (`lunch`)".into());
    }
    let lower = raw.to_ascii_lowercase();
    let body = lower.strip_prefix("in ").unwrap_or(&lower).trim();

    if let Some(secs) = crate::focus::parse_duration(body) {
        return Ok(now.saturating_add(secs));
    }
    if let Some(at) = parse_clock(body, now) {
        return Ok(at);
    }
    if let Some(at) = named_day(body, now) {
        return Ok(at);
    }
    if let Some(at) = named_window(body, windows, now) {
        return Ok(at);
    }
    Err(format!(
        "could not read '{raw}' as a time — try `12:30`, `5pm`, `in 2h`, `tomorrow`, \
         or a window name (`bn queue schedule` lists them)"
    ))
}

/// `12:30`, `5pm`, `9am`, `17:30` → the next instant the clock reads that.
fn parse_clock(s: &str, now: i64) -> Option<i64> {
    let (body, shift) = match () {
        _ if s.ends_with("am") => (s.trim_end_matches("am").trim(), Some(false)),
        _ if s.ends_with("pm") => (s.trim_end_matches("pm").trim(), Some(true)),
        _ => (s, None),
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
        // A bare number is only a clock time in the 12-hour forms. Without `am`/`pm` it is
        // ambiguous with a duration, and `parse_duration` has already claimed it.
        None => (body.parse::<u32>().ok().filter(|_| shift.is_some())?, 0),
    };
    let hour = match shift {
        Some(_) if h == 0 || h > 12 => return None,
        Some(true) if h == 12 => 12,
        Some(true) => h + 12,
        Some(false) if h == 12 => 0,
        Some(false) => h,
        None => h,
    };
    if hour > 23 || m > 59 {
        return None;
    }
    next_local_time(hour, m, now)
}

/// `tomorrow`, `tonight`, `today` — the coarse words, given concrete hours.
///
/// Arbitrary but stated: morning is 09:00 and evening 20:00. Someone who wants a different hour has
/// the clock form, and picking *a* time beats refusing the word people actually type.
fn named_day(s: &str, now: i64) -> Option<i64> {
    match s {
        "tonight" => next_local_time(20, 0, now),
        "tomorrow" => next_local_time(9, 0, now).map(|at| {
            // "tomorrow" said before 09:00 must still mean the next day, not this morning.
            match at.saturating_sub(now) < 86_400
                && crate::util::local_date_of(at) == crate::util::local_date_of(now)
            {
                true => at + 86_400,
                false => at,
            }
        }),
        _ => None,
    }
}

/// The next opening of a configured window, by name.
///
/// Matched loosely — `lunch` finds `pool-warm-lunch` — because the names in `config.toml` are
/// namespaced for the file's sake and nobody types them in full.
fn named_window(s: &str, windows: &[PriorityWindow], now: i64) -> Option<i64> {
    let want = s.replace(' ', "-");
    windows
        .iter()
        .find(|w| {
            let name = w.name.to_ascii_lowercase();
            name == want
                || name.ends_with(&format!("-{want}"))
                || name.starts_with(&format!("{want}-"))
        })
        .and_then(|w| crate::schedule::next_occurrence(&w.at, now))
}

/// The next epoch second at which local time reads `hour:min`.
fn next_local_time(hour: u32, min: u32, now: i64) -> Option<i64> {
    // Expressed as an `OnCalendar` spec so this shares the one implementation of "when does a clock
    // spec next fire", including its DST handling, rather than doing date arithmetic here.
    crate::schedule::next_occurrence(&format!("*-*-* {hour:02}:{min:02}:00"), now)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_700_000;

    fn window(name: &str, at: &str) -> PriorityWindow {
        PriorityWindow {
            name: name.to_string(),
            at: at.to_string(),
            secs: 3600,
            boost: 100,
            matches: vec!["*".to_string()],
            mode: crate::queue::window::WindowMode::Away,
        }
    }

    #[test]
    fn a_delay_is_measured_from_now() {
        assert_eq!(parse("in 2h", &[], NOW), Ok(NOW + 7200));
        assert_eq!(parse("2h", &[], NOW), Ok(NOW + 7200));
        assert_eq!(
            parse("90", &[], NOW),
            Ok(NOW + 5400),
            "a bare number is minutes"
        );
        assert_eq!(
            parse("1d", &[], NOW),
            Ok(NOW + 86_400),
            "the help text's own example"
        );
    }

    /// The property that makes naming a time safe: it never lands in the past, where it would be
    /// indistinguishable from not having scheduled anything.
    #[test]
    fn a_clock_time_always_resolves_forward() {
        for input in ["12:30", "5pm", "9am", "00:15", "23:59"] {
            let at = parse(input, &[], NOW).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert!(at > NOW, "{input} resolved to the past");
            assert!(
                at - NOW <= 86_400,
                "{input} should be within a day, was {}s away",
                at - NOW
            );
        }
    }

    #[test]
    fn twelve_hour_times_land_in_the_right_half_of_the_day() {
        let noon = parse("12pm", &[], NOW).expect("12pm");
        let midnight = parse("12am", &[], NOW).expect("12am");
        assert_ne!(noon, midnight);
        // Both are within a day and forward, and they are twelve hours apart on the clock.
        assert!(
            (noon - midnight).abs() % 86_400 == 43_200
                || (midnight - noon).abs() % 86_400 == 43_200
        );
    }

    #[test]
    fn a_window_can_be_named_without_its_prefix() {
        let windows = vec![window("pool-warm-lunch", "Mon..Fri *-*-* 12:30:00")];
        let at = parse("lunch", &windows, NOW).expect("lunch resolves");
        assert!(at > NOW);
        assert_eq!(
            at,
            parse("pool-warm-lunch", &windows, NOW).expect("full name too"),
            "the short and full names name the same window"
        );
    }

    #[test]
    fn tomorrow_is_not_this_morning() {
        let at = parse("tomorrow", &[], NOW).expect("tomorrow");
        assert!(at > NOW);
        assert_ne!(
            crate::util::local_date_of(at),
            crate::util::local_date_of(NOW),
            "tomorrow must land on a different day than today"
        );
    }

    /// An unreadable time is an error naming the shapes that work, never a silent fallback. A
    /// caller that guessed would schedule work for a moment nobody chose.
    #[test]
    fn nonsense_is_refused_with_the_shapes_that_work() {
        let err = parse("whenever", &[], NOW).expect_err("not a time");
        assert!(err.contains("12:30") && err.contains("in 2h"), "{err}");
        assert!(parse("", &[], NOW).is_err());
        assert!(
            parse("25:00", &[], NOW).is_err(),
            "an impossible clock time"
        );
        assert!(parse("13pm", &[], NOW).is_err(), "13 has no pm");
    }
}
