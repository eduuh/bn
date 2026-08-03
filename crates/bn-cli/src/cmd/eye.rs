//! `bn eye` and the opt-in daily/weekly eye-health staff jobs.

use crate::*;
use bn_core::{eye, staff};

fn settings() -> resolver::JobConfig {
    resolver::JobConfig::load()
}

fn packet_json(cfg: &Config, mode: &str, date: &str) -> Result<String, String> {
    let settings = settings();
    eye::seed_reference(cfg)?;
    let packet = eye::gather_packet(
        cfg,
        mode,
        date,
        settings.eye_last_review.as_deref(),
        settings.eye_review_interval_months,
    )?;
    serde_json::to_string_pretty(&packet).map_err(|e| e.to_string())
}

fn print_summary(summary: &eye::Summary) {
    println!(
        "No-eye-rubbing streak: {} day{}",
        summary.no_rubbing_streak,
        if summary.no_rubbing_streak == 1 {
            ""
        } else {
            "s"
        }
    );
    println!(
        "Weekly adherence: {:.1}% ({}/{}) · logged {} of 7 days",
        summary.adherence_pct,
        summary.completed_checks,
        summary.expected_checks,
        summary.logged_days
    );
    if !summary.missing_days.is_empty() {
        println!("Missing check-ins: {}", summary.missing_days.join(", "));
    }
    match (&summary.review_due_on, summary.review_due) {
        (Some(date), true) => println!("Monitoring review: DUE (due {date})"),
        (Some(date), false) => println!("Monitoring review: next due {date}"),
        (None, _) => println!(
            "Monitoring review: not configured — set [eye].last_review or run `bn eye review`"
        ),
    }
}

fn strip_frontmatter(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("---") {
        if let Some((_, body)) = rest.split_once("---") {
            return body.trim_start().to_string();
        }
    }
    text.to_string()
}

fn eye_prompt() -> String {
    let text = include_str!("../../../../config/skills/staff/eye.md");
    strip_frontmatter(text)
}

fn eye_agent(mode: &str) -> staff::ResolvedAgent {
    let (name, schedule) = if mode == "weekly" {
        (eye::AGENT_WEEKLY, "Sun *-*-* 09:00:00")
    } else {
        (eye::AGENT_DAILY, "*-*-* 19:30:00")
    };
    staff::ResolvedAgent {
        name: name.into(),
        on_calendar: Some(schedule.into()),
        run: format!("bn run {name}"),
        network: false,
        triggers: Vec::new(),
        description: Some("Keratoconus protective-behaviour coach".into()),
        gather: Some(format!("bn eye gather {mode}")),
        sinks: vec!["attn".into()],
        skills: Vec::new(),
        mcps: Vec::new(),
        link_domains: Vec::new(),
        harness: Some("copilot".into()),
        repo: None,
        references: Vec::new(),
        source_repo: None,
        worlds: Vec::new(),
        tools: Vec::new(),
        model: None,
        body: eye_prompt(),
        scope: bn_core::resolver::JobScope::Global,
        path: std::path::PathBuf::from("config/skills/staff/eye.md"),
    }
}

fn run_eye_staff(cfg: &Config, mode: &str, date: &str, json: bool) -> i32 {
    let entry = eye_agent(mode);
    match staff::home_gate(cfg, &entry) {
        staff::HomeGate::RunsHere => {}
        staff::HomeGate::NotHome { home } => {
            println!("eye {mode}: skipped — staff home is {home}");
            return 0;
        }
    }
    let packet = packet_json(cfg, mode, date);
    let report = staff::run_pipeline(
        cfg,
        &entry,
        packet,
        |prompt| {
            let Some(cmd) = staff_dispatch_command("copilot", prompt, None, None) else {
                return staff::Dispatched::Failed(
                    staff::RunStatus::Fail,
                    "unknown harness 'copilot'".into(),
                );
            };
            let (success, stdout, combined) = dispatch_capture(cmd, None);
            match staff::classify_run(success, &combined) {
                staff::RunStatus::Ok => staff::Dispatched::Output(stdout),
                status => staff::Dispatched::Failed(status, staff::failure_detail(&combined)),
            }
        },
        date,
    );
    if json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else if report.ok {
        println!(
            "eye {mode}: card landed{}",
            report
                .card_path
                .as_ref()
                .map(|path| format!(" at {}", path.display()))
                .unwrap_or_default()
        );
    } else if report.status == staff::RunStatus::Skipped {
        if let Some(note) = report.note {
            println!("eye {mode}: {note}");
        }
    } else {
        eprintln!(
            "eye {mode}: {} [{}]",
            report.error.as_deref().unwrap_or("failed"),
            report.status.as_str()
        );
    }
    if report.ok || report.status == staff::RunStatus::Skipped {
        0
    } else {
        1
    }
}

pub(crate) fn emit_eye(cfg: &Config, action: Option<EyeCmd>, json: bool) {
    let today_date = today();
    match action.unwrap_or(EyeCmd::Status { date: None }) {
        EyeCmd::CheckIn {
            date,
            rubbed,
            drops,
            greens,
            omega_3,
            sunglasses,
            screen_breaks,
            slept_back,
        } => {
            let date = date.unwrap_or_else(today);
            let input = eye::CheckIn {
                rubbed_eyes: rubbed,
                lubricating_drops: drops,
                dark_leafy_greens: greens,
                omega_3,
                sunglasses_outdoors: sunglasses,
                screen_breaks_and_blinks: screen_breaks,
                slept_on_back: slept_back,
            };
            let log = eye::record(cfg, &date, input)
                .unwrap_or_else(|e| fail_json(json, "eye_check_in", "io_error", &e));
            let settings = settings();
            let summary = eye::summarize(
                cfg,
                &date,
                settings.eye_last_review.as_deref(),
                settings.eye_review_interval_months,
            )
            .unwrap_or_else(|e| fail_json(json, "eye_check_in", "summary_error", &e));
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "eye_check_in",
                        serde_json::json!({"log": log, "summary": summary})
                    )
                    .to_json()
                );
            } else {
                println!("Eye check-in recorded for {date}.");
                print_summary(&summary);
            }
        }
        EyeCmd::Status { date } => {
            let date = date.unwrap_or_else(|| today_date.clone());
            let settings = settings();
            let summary = eye::summarize(
                cfg,
                &date,
                settings.eye_last_review.as_deref(),
                settings.eye_review_interval_months,
            )
            .unwrap_or_else(|e| fail_json(json, "eye_status", "summary_error", &e));
            if json {
                println!(
                    "{}",
                    Envelope::success("eye_status", serde_json::json!(summary)).to_json()
                );
            } else {
                print_summary(&summary);
            }
        }
        EyeCmd::Gather { mode, date } => {
            let date = date.unwrap_or_else(|| today_date.clone());
            match packet_json(cfg, mode.as_str(), &date) {
                Ok(packet) => println!("{packet}"),
                Err(e) => fail_json(json, "eye_gather", "gather_error", &e),
            }
        }
        EyeCmd::Run { mode, date } => {
            let date = date.unwrap_or_else(|| today_date.clone());
            std::process::exit(run_eye_staff(cfg, mode.as_str(), &date, json));
        }
        EyeCmd::Review { date } => {
            let date = date.unwrap_or(today_date);
            let review = eye::record_review(cfg, &date)
                .unwrap_or_else(|e| fail_json(json, "eye_review", "io_error", &e));
            if json {
                println!(
                    "{}",
                    Envelope::success("eye_review", serde_json::json!(review)).to_json()
                );
            } else {
                println!("Eye monitoring review recorded for {}.", review.last_review);
            }
        }
    }
}

fn eye_job(cfg: &Config, mode: &str) -> i32 {
    if !settings().eye_enabled {
        println!("staff-eye-{mode}: disabled — set [eye].enabled to opt in");
        return 0;
    }
    run_eye_staff(cfg, mode, &today(), false)
}

pub(crate) fn job_staff_eye_daily(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    eye_job(cfg, "daily")
}

pub(crate) fn job_staff_eye_weekly(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    eye_job(cfg, "weekly")
}
