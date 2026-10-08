//! Links by day: "what did I save yesterday", "the PR from Monday". A link belongs to the day it
//! was saved (`created`), which never changes. Opening a link does not move it: the day it lives
//! in is fixed, and only the word shown for that day (`yesterday`, then `tue`, then `Oct 6`) is
//! worked out against today when it is displayed.
//!
//! Weeks start on Monday. A bare weekday means the most recent one up to today, so on a
//! Wednesday `monday` is two days ago and `friday` is last week's.

use super::Link;
use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// An inclusive range of days, `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub from: String,
    pub to: String,
}

impl Span {
    fn new(from: NaiveDate, to: NaiveDate) -> Self {
        Self {
            from: from.format("%Y-%m-%d").to_string(),
            to: to.format("%Y-%m-%d").to_string(),
        }
    }

    fn holds(&self, date: &str) -> bool {
        // `created`/`last` are `YYYY-MM-DD`; a longer timestamp still compares by its day.
        let day = date.get(..10).unwrap_or(date);
        self.from.as_str() <= day && day <= self.to.as_str()
    }
}

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

/// Monday of the week `d` is in.
fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(i64::from(d.weekday().num_days_from_monday()))
}

/// Read a `--when`: `today`, `yesterday`, a weekday (`monday`, `mon`), `week` / `this-week`
/// (Monday to today), `last-week` (the Monday-to-Sunday before), `Nd` (the last N days,
/// today included) or a date `YYYY-MM-DD`. `None` for anything else.
pub fn parse(spec: &str, today: &str) -> Option<Span> {
    let today = date(today)?;
    let spec = spec.trim().to_lowercase().replace(' ', "-");
    let span = match spec.as_str() {
        "today" => Span::new(today, today),
        "yesterday" => {
            let y = today - Duration::days(1);
            Span::new(y, y)
        }
        "week" | "this-week" => Span::new(monday_of(today), today),
        "last-week" => {
            let start = monday_of(today) - Duration::days(7);
            Span::new(start, start + Duration::days(6))
        }
        s => {
            if let Some(w) = weekday(s) {
                let back =
                    (7 + today.weekday().num_days_from_monday() - w.num_days_from_monday()) % 7;
                let d = today - Duration::days(i64::from(back));
                Span::new(d, d)
            } else if let Some(n) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
                if n < 1 {
                    return None;
                }
                Span::new(today - Duration::days(n - 1), today)
            } else {
                let d = date(s)?;
                Span::new(d, d)
            }
        }
    };
    Some(span)
}

/// Was the link saved inside the span?
pub fn saved_in(link: &Link, span: &Span) -> bool {
    span.holds(&link.created)
}

/// The day a link was saved, as a word worth typing into the picker — see [`day_label`].
pub fn label(link: &Link, today: &str) -> String {
    day_label(&link.created, today)
}

/// A `YYYY-MM-DD` day as a word relative to today: `today`, `yesterday`, a weekday this week
/// (`mon`), else `Sep 28` this year or `2025-09-28` before it. The day is fixed; the word moves.
pub fn day_label(day: &str, today: &str) -> String {
    let Some(day) = date(day) else {
        return String::new();
    };
    let Some(today) = date(today) else {
        return String::new();
    };
    match (today - day).num_days() {
        0 => "today".into(),
        1 => "yesterday".into(),
        _ if day >= monday_of(today) && day < today => day.format("%a").to_string().to_lowercase(),
        _ if day.year() == today.year() => day.format("%b %-d").to_string(),
        _ => day.format("%Y-%m-%d").to_string(),
    }
}

/// A `YYYY-MM-DD` day spelled out for a group row: `Tue Oct 6`. The input as-is if it is not
/// a date.
pub fn day_long(day: &str) -> String {
    date(day)
        .map(|d| d.format("%a %b %-d").to_string())
        .unwrap_or_else(|| day.to_string())
}

/// How far back the picker offers a day as a group: this week and last week.
pub const DAY_GROUP_DAYS: i64 = 14;

/// The days the picker offers as groups: each save date in the last [`DAY_GROUP_DAYS`] that holds
/// at least [`super::GROUP_MIN`] of `ranked`, newest first, with its links in the order given.
pub fn day_groups<'a>(ranked: &[&'a Link], today: &str) -> Vec<(String, Vec<&'a Link>)> {
    let Some(oldest) = date(today).map(|t| t - Duration::days(DAY_GROUP_DAYS - 1)) else {
        return vec![];
    };
    let mut out: Vec<(String, Vec<&'a Link>)> = Vec::new();
    for l in ranked {
        let Some(day) = date(&l.created).filter(|d| *d >= oldest) else {
            continue;
        };
        let key = day.format("%Y-%m-%d").to_string();
        match out.iter_mut().find(|(d, _)| *d == key) {
            Some((_, members)) => members.push(l),
            None => out.push((key, vec![l])),
        }
    }
    out.retain(|(_, m)| m.len() >= super::GROUP_MIN);
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-07 is a Wednesday.
    const WED: &str = "2026-10-07";

    fn span(from: &str, to: &str) -> Option<Span> {
        Some(Span {
            from: from.into(),
            to: to.into(),
        })
    }

    #[test]
    fn days_and_weeks_resolve_from_today() {
        assert_eq!(parse("today", WED), span(WED, WED));
        assert_eq!(parse("Yesterday", WED), span("2026-10-06", "2026-10-06"));
        assert_eq!(parse("week", WED), span("2026-10-05", WED));
        assert_eq!(parse("this week", WED), span("2026-10-05", WED));
        assert_eq!(parse("last-week", WED), span("2026-09-28", "2026-10-04"));
        assert_eq!(parse("7d", WED), span("2026-10-01", WED));
        assert_eq!(parse("2026-09-30", WED), span("2026-09-30", "2026-09-30"));
        assert_eq!(parse("0d", WED), None);
        assert_eq!(parse("someday", WED), None);
    }

    #[test]
    fn a_weekday_is_the_most_recent_one_up_to_today() {
        assert_eq!(parse("monday", WED), span("2026-10-05", "2026-10-05"));
        assert_eq!(parse("tue", WED), span("2026-10-06", "2026-10-06"));
        assert_eq!(parse("wednesday", WED), span(WED, WED), "today counts");
        assert_eq!(
            parse("fri", WED),
            span("2026-10-02", "2026-10-02"),
            "last week's"
        );
    }

    fn saved(created: &str, last: Option<&str>) -> Link {
        Link {
            id: "k1".into(),
            url: "https://example.com".into(),
            name: "x".into(),
            tags: vec![],
            repo: None,
            branch: None,
            created: created.into(),
            uses: 0,
            last: last.map(str::to_string),
            new: false,
            expires: None,
        }
    }

    #[test]
    fn a_link_stays_on_the_day_it_was_saved_however_often_it_is_opened() {
        let monday = parse("monday", WED).unwrap();
        assert!(saved_in(&saved("2026-10-05", Some(WED)), &monday));
        assert!(!saved_in(&saved("2026-01-01", Some("2026-10-05")), &monday));
        assert_eq!(label(&saved("2026-10-05", Some(WED)), WED), "mon");
    }

    #[test]
    fn the_same_day_reads_differently_as_the_week_moves_on() {
        let oct6 = "2026-10-06";
        assert_eq!(day_label(oct6, WED), "yesterday");
        assert_eq!(day_label(oct6, "2026-10-08"), "tue");
        assert_eq!(day_label(oct6, "2026-10-13"), "Oct 6");
        assert_eq!(
            day_long(oct6),
            "Tue Oct 6",
            "the date under the word never moves"
        );
    }

    #[test]
    fn recent_days_with_two_or_more_links_become_groups() {
        let items = [
            saved(WED, None),
            saved("2026-10-06", None),
            saved("2026-10-06", None),
            saved("2026-10-05", None),
            saved("2026-09-01", None),
            saved("2026-09-01", None),
        ];
        let refs: Vec<&Link> = items.iter().collect();
        let days: Vec<String> = day_groups(&refs, WED).into_iter().map(|(d, _)| d).collect();
        assert_eq!(
            days,
            ["2026-10-06"],
            "singletons and old days are not groups"
        );
    }

    #[test]
    fn the_label_is_a_word_you_would_type() {
        assert_eq!(label(&saved(WED, None), WED), "today");
        assert_eq!(label(&saved("2026-10-06", None), WED), "yesterday");
        assert_eq!(label(&saved("2026-10-05", None), WED), "mon");
        assert_eq!(label(&saved("2026-09-28", None), WED), "Sep 28");
        assert_eq!(label(&saved("2025-12-31", None), WED), "2025-12-31");
    }
}
