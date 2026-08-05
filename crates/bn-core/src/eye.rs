//! Eye-health habit tracking and deterministic staff packets.

use crate::{resolver::Config, store};
use chrono::{Duration, Months, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const AGENT_DAILY: &str = "eye-daily";
pub const AGENT_WEEKLY: &str = "eye-weekly";
pub const REFERENCE_NAME: &str = "keratoconus-care";

const CARE_REFERENCE: &str = r#"# Keratoconus care standard

This reference is the bounded care guidance used by the bn eye-health coach. It is a behavioural
support aid, not a diagnosis or a replacement for the treating eye clinic's plan.

## Protective routine

- Do not rub the eyes. When the urge arrives: pause, keep hands away, blink slowly, use the
  clinician-approved preservative-free lubricating drops, and address allergy or dryness through
  the existing care plan.
- Use preservative-free lubricating drops as directed.
- Use sunglasses outdoors, take 20-20-20 screen breaks with deliberate blinking, and prefer
  back-sleeping when practical.
- Track dark leafy greens and a weekly omega-3 food as supportive habits, without treating food as
  a substitute for clinical monitoring or treatment.

## Monitoring

Keep corneal topography and ophthalmology reviews on the clinician-set 6-12 month cadence. Ask the
clinic whether corneal cross-linking or a different review interval is appropriate; the coach must
not decide this.

## Red flags

A sudden drop in vision, eye pain, or corneal whitening needs prompt assessment by an eye clinic.
The coach should surface this guidance clearly and avoid adding unsupported medical claims.
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HabitLog {
    pub date: String,
    pub rubbed_eyes: bool,
    pub lubricating_drops: bool,
    pub dark_leafy_greens: bool,
    pub omega_3: bool,
    pub sunglasses_outdoors: bool,
    pub screen_breaks_and_blinks: bool,
    pub slept_on_back: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckIn {
    pub rubbed_eyes: bool,
    pub lubricating_drops: bool,
    pub dark_leafy_greens: bool,
    pub omega_3: bool,
    pub sunglasses_outdoors: bool,
    pub screen_breaks_and_blinks: bool,
    pub slept_on_back: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewState {
    pub last_review: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub as_of: String,
    pub no_rubbing_streak: u32,
    pub adherence_pct: f64,
    pub completed_checks: u32,
    pub expected_checks: u32,
    pub logged_days: u32,
    pub missing_days: Vec<String>,
    pub misses: Vec<String>,
    pub review_last: Option<String>,
    pub review_due_on: Option<String>,
    pub review_due: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Packet {
    pub mode: String,
    pub summary: Summary,
    pub recent_logs: Vec<HabitLog>,
    pub urge_response: String,
    pub red_flags: String,
    pub care_reference: String,
}

pub fn log_path(cfg: &Config, date: &str) -> PathBuf {
    cfg.eye_dir().join(format!("{date}.json"))
}

pub fn review_path(cfg: &Config) -> PathBuf {
    cfg.eye_dir().join("review.json")
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("invalid date '{value}' (want YYYY-MM-DD)"))
}

pub fn record(cfg: &Config, date: &str, input: CheckIn) -> Result<HabitLog, String> {
    parse_date(date)?;
    std::fs::create_dir_all(cfg.eye_dir()).map_err(|e| format!("create eye log directory: {e}"))?;
    let log = HabitLog {
        date: date.to_string(),
        rubbed_eyes: input.rubbed_eyes,
        lubricating_drops: input.lubricating_drops,
        dark_leafy_greens: input.dark_leafy_greens,
        omega_3: input.omega_3,
        sunglasses_outdoors: input.sunglasses_outdoors,
        screen_breaks_and_blinks: input.screen_breaks_and_blinks,
        slept_on_back: input.slept_on_back,
    };
    let body = serde_json::to_string_pretty(&log).map_err(|e| e.to_string())?;
    store::write_atomic(&log_path(cfg, date), format!("{body}\n").as_bytes())
        .map_err(|e| format!("write eye check-in: {e}"))?;
    Ok(log)
}

pub fn record_review(cfg: &Config, date: &str) -> Result<ReviewState, String> {
    parse_date(date)?;
    std::fs::create_dir_all(cfg.eye_dir()).map_err(|e| format!("create eye log directory: {e}"))?;
    let state = ReviewState {
        last_review: date.to_string(),
    };
    let body = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    store::write_atomic(&review_path(cfg), format!("{body}\n").as_bytes())
        .map_err(|e| format!("write eye review: {e}"))?;
    Ok(state)
}

pub fn read_log(path: &Path) -> Option<HabitLog> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn logs_between(cfg: &Config, start: NaiveDate, end: NaiveDate) -> Vec<HabitLog> {
    let mut logs = Vec::new();
    let mut date = start;
    while date <= end {
        let key = date.format("%Y-%m-%d").to_string();
        if let Some(log) = read_log(&log_path(cfg, &key)) {
            logs.push(log);
        }
        date += Duration::days(1);
    }
    logs
}

fn review_last(cfg: &Config, configured: Option<&str>) -> Result<Option<NaiveDate>, String> {
    let path = review_path(cfg);
    if path.is_file() {
        let body =
            std::fs::read_to_string(&path).map_err(|e| format!("read eye review state: {e}"))?;
        let state: ReviewState =
            serde_json::from_str(&body).map_err(|e| format!("parse eye review state: {e}"))?;
        return parse_date(&state.last_review).map(Some);
    }
    configured.map(parse_date).transpose()
}

pub fn summarize(
    cfg: &Config,
    as_of: &str,
    configured_review: Option<&str>,
    review_interval_months: u32,
) -> Result<Summary, String> {
    let end = parse_date(as_of)?;
    let start = end - Duration::days(6);
    let logs = logs_between(cfg, start, end);
    let by_date: std::collections::HashMap<&str, &HabitLog> =
        logs.iter().map(|log| (log.date.as_str(), log)).collect();

    let mut completed = 0_u32;
    let mut missing_days = Vec::new();
    let mut misses = Vec::new();
    let mut date = start;
    while date <= end {
        let key = date.format("%Y-%m-%d").to_string();
        if let Some(log) = by_date.get(key.as_str()) {
            let checks = [
                (!log.rubbed_eyes, "no eye rubbing"),
                (log.lubricating_drops, "lubricating drops"),
                (log.dark_leafy_greens, "dark leafy greens"),
                (log.sunglasses_outdoors, "sunglasses outdoors"),
                (log.screen_breaks_and_blinks, "screen breaks + blinking"),
                (log.slept_on_back, "back sleeping"),
            ];
            for (done, label) in checks {
                if done {
                    completed += 1;
                } else {
                    misses.push(format!("{key}: {label}"));
                }
            }
        } else {
            missing_days.push(key.clone());
            misses.push(format!("{key}: no check-in"));
        }
        date += Duration::days(1);
    }
    if logs.iter().any(|log| log.omega_3) {
        completed += 1;
    } else {
        misses.push(format!("{as_of}: weekly omega-3"));
    }
    let expected = 7 * 6 + 1;

    let mut streak = 0_u32;
    let mut date = end;
    loop {
        let key = date.format("%Y-%m-%d").to_string();
        match by_date.get(key.as_str()) {
            Some(log) if !log.rubbed_eyes => streak += 1,
            _ => break,
        }
        date -= Duration::days(1);
    }

    let last = review_last(cfg, configured_review)?;
    let interval = review_interval_months.clamp(6, 12);
    let due_on = last.and_then(|date| date.checked_add_months(Months::new(interval)));
    Ok(Summary {
        as_of: as_of.to_string(),
        no_rubbing_streak: streak,
        adherence_pct: (completed as f64 / expected as f64 * 1000.0).round() / 10.0,
        completed_checks: completed,
        expected_checks: expected,
        logged_days: logs.len() as u32,
        missing_days,
        misses,
        review_last: last.map(|date| date.format("%Y-%m-%d").to_string()),
        review_due_on: due_on.map(|date| date.format("%Y-%m-%d").to_string()),
        review_due: due_on.is_some_and(|date| date <= end),
    })
}

pub fn gather_packet(
    cfg: &Config,
    mode: &str,
    as_of: &str,
    configured_review: Option<&str>,
    review_interval_months: u32,
) -> Result<Packet, String> {
    let end = parse_date(as_of)?;
    let start = end - Duration::days(6);
    Ok(Packet {
        mode: mode.to_string(),
        summary: summarize(
            cfg,
            as_of,
            configured_review,
            review_interval_months,
        )?,
        recent_logs: logs_between(cfg, start, end),
        urge_response: "Pause; keep hands away; blink slowly; use clinician-approved preservative-free lubricating drops; address allergy or dryness through the existing care plan.".into(),
        red_flags: "A sudden drop in vision, eye pain, or corneal whitening needs prompt assessment by an eye clinic.".into(),
        care_reference: REFERENCE_NAME.into(),
    })
}

pub fn seed_reference(cfg: &Config) -> Result<PathBuf, String> {
    let path = cfg.references_dir().join(format!("{REFERENCE_NAME}.md"));
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(cfg.references_dir())
        .map_err(|e| format!("create references directory: {e}"))?;
    store::write_atomic(&path, CARE_REFERENCE.as_bytes())
        .map_err(|e| format!("seed eye care reference: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_clean() -> CheckIn {
        CheckIn {
            lubricating_drops: true,
            dark_leafy_greens: true,
            omega_3: true,
            sunglasses_outdoors: true,
            screen_breaks_and_blinks: true,
            slept_on_back: true,
            ..Default::default()
        }
    }

    #[test]
    fn consecutive_clean_days_build_the_headline_streak() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        record(&cfg, "2026-07-16", full_clean()).unwrap();
        record(&cfg, "2026-07-17", full_clean()).unwrap();
        record(&cfg, "2026-07-18", full_clean()).unwrap();
        assert_eq!(
            summarize(&cfg, "2026-07-18", None, 12)
                .unwrap()
                .no_rubbing_streak,
            3
        );
    }

    #[test]
    fn rubbing_resets_the_streak() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        record(&cfg, "2026-07-17", full_clean()).unwrap();
        let mut rubbed = full_clean();
        rubbed.rubbed_eyes = true;
        record(&cfg, "2026-07-18", rubbed).unwrap();
        assert_eq!(
            summarize(&cfg, "2026-07-18", None, 12)
                .unwrap()
                .no_rubbing_streak,
            0
        );
    }

    #[test]
    fn a_missing_day_breaks_the_streak() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        record(&cfg, "2026-07-16", full_clean()).unwrap();
        record(&cfg, "2026-07-18", full_clean()).unwrap();
        let summary = summarize(&cfg, "2026-07-18", None, 12).unwrap();
        assert_eq!(summary.no_rubbing_streak, 1);
        assert!(summary.missing_days.contains(&"2026-07-17".to_string()));
    }

    #[test]
    fn weekly_adherence_counts_missing_days_and_one_omega_target() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        for day in 12..=18 {
            record(&cfg, &format!("2026-07-{day}"), full_clean()).unwrap();
        }
        let perfect = summarize(&cfg, "2026-07-18", None, 12).unwrap();
        assert_eq!(perfect.completed_checks, 43);
        assert_eq!(perfect.adherence_pct, 100.0);

        std::fs::remove_file(log_path(&cfg, "2026-07-15")).unwrap();
        let missing = summarize(&cfg, "2026-07-18", None, 12).unwrap();
        assert_eq!(missing.completed_checks, 37);
        assert_eq!(missing.adherence_pct, 86.0);
    }

    #[test]
    fn monitoring_due_uses_persisted_review_and_clamps_to_care_range() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        record_review(&cfg, "2025-07-18").unwrap();
        let summary = summarize(&cfg, "2026-07-18", None, 18).unwrap();
        assert_eq!(summary.review_due_on.as_deref(), Some("2026-07-18"));
        assert!(summary.review_due);
    }
}
