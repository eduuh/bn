//! `bn daily` — daily goals log.
use crate::*;

/// The one-line routine context header for the bare "today" `bn daily` view (#801): what's
/// current (with remaining minutes) or, absent that, what's next (with the countdown). Never
/// deletes the goal log below it — purely additive context.
fn routine_context_line(snap: &bn_core::routine::Snapshot) -> String {
    match (&snap.current, &snap.next) {
        (Some(cur), _) => {
            let remaining = snap
                .remaining_minutes
                .map(|m| format!(" · {m}m left"))
                .unwrap_or_default();
            format!("🗓 {} now{remaining}", cur.name)
        }
        (None, Some(next)) => {
            let until = snap
                .until_next_minutes
                .map(|m| format!(" in {m}m"))
                .unwrap_or_default();
            format!("🗓 next: {}{until}", next.name)
        }
        (None, None) => "🗓 Routine: nothing scheduled".to_string(),
    }
}

/// `bn daily [date]` / `add` / `done` / `edit`. The bare view auto-creates today (carrying
/// forward yesterday's open goals); a past `date` is read-only.
/// Resolve a scheduling target from the mutually-exclusive `--on <date>` / `--in <days>`
/// flags. `None` means neither was given (act on today); `Err` means both were given or the
/// `--in` offset couldn't be computed. `--on` is passed through verbatim for the daily layer
/// to calendar-validate.
pub(crate) fn resolve_on(
    today: &str,
    on: Option<String>,
    in_days: Option<i64>,
) -> Result<Option<String>, String> {
    bn_core::util::resolve_schedule_target(today, on, in_days)
}

pub(crate) fn emit_daily(
    cfg: &Config,
    action: Option<DailyCmd>,
    date: Option<String>,
    as_json: bool,
) {
    use bn_core::daily;
    match action {
        None => {
            let today = today();
            let view = date.unwrap_or_else(|| today.clone());
            let is_today = view == today;
            let daily = if is_today {
                daily::ensure_today(cfg, &today).unwrap_or_else(|e| fail(&format!("bn daily: {e}")))
            } else {
                match daily::read(&cfg.daily_file(&view)) {
                    Ok(d) => d,
                    Err(_) => {
                        if as_json {
                            println!(
                                "{}",
                                Envelope::success("daily", serde_json::json!({ "date": view, "goals": [], "open": 0, "done": 0, "upcoming": [] })).to_json()
                            );
                        } else {
                            println!("No daily goals for {view}");
                        }
                        return;
                    }
                }
            };
            // Upcoming follow-ups are a "today" concept; past-day views stay historical.
            let upcoming = if is_today {
                daily::scheduled_pending(cfg, &today)
            } else {
                vec![]
            };
            // Routine context (#801): only for the bare "today" view — a past-day daily log has
            // no "now" to be inside a block for. Folded in without touching `daily::render`/
            // `payload::daily_data` so the existing goal-log output is preserved verbatim.
            let routine_snap = if is_today {
                use bn_core::clock::Clock;
                let now = bn_core::clock::RealClock.now_epoch_secs();
                bn_core::routine::current_snapshot(cfg, now)
            } else {
                None
            };
            if as_json {
                let mut data = payload::daily_data(&daily, &upcoming);
                if let (Some(obj), Some(snap)) = (data.as_object_mut(), &routine_snap) {
                    obj.insert(
                        "routine".into(),
                        serde_json::to_value(snap).unwrap_or(serde_json::Value::Null),
                    );
                }
                println!("{}", Envelope::success("daily", data).to_json());
            } else {
                if let Some(snap) = &routine_snap {
                    println!("{}", routine_context_line(snap));
                    if let Some(reason) = snap.explanation() {
                        println!("{reason}");
                    }
                    println!();
                }
                print!("{}", daily::render(&daily, &upcoming));
            }
        }
        Some(DailyCmd::Add {
            text,
            task,
            link,
            on,
            in_days,
            est,
            q,
        }) => {
            let today = today();
            let resolved = daily::resolve_link(task.as_deref(), link.as_deref());
            let on = match resolve_on(&today, on, in_days) {
                Err(msg) => fail(&format!("bn daily add: {msg}")),
                Ok(d) => d,
            };
            let est = parse_est_or_exit("bn daily add", est.as_deref());
            match daily::add_full(
                cfg,
                &today,
                &text.join(" "),
                resolved,
                q,
                on.as_deref(),
                est,
            ) {
                Ok(a) => {
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success("daily_add", payload::daily_add_data(&a)).to_json()
                        );
                    } else if let Some(date) = a.scheduled.filter(|_| !a.landed_today) {
                        println!("Scheduled for {} [{}]: {}", date, a.id, a.text);
                    } else {
                        let link = a.link.map(|l| format!("  → {l}")).unwrap_or_default();
                        println!("Added to today [{}]: {}{link}", a.id, a.text);
                    }
                }
                Err(e) => emit_err("daily_add", &e, as_json),
            }
        }
        Some(DailyCmd::Drop { query }) => {
            match daily::drop_goal(cfg, &today(), &query.join(" ")) {
                Ok(d) => {
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success("daily_drop", payload::daily_drop_data(&d)).to_json()
                        );
                    } else {
                        println!("Dropped [{}]: {}", d.id, d.text);
                    }
                }
                Err(e) => emit_err("daily_drop", &e, as_json),
            }
        }
        Some(DailyCmd::Defer { query, on, in_days }) => {
            let today = today();
            let date = match resolve_on(&today, on, in_days) {
                Err(msg) => fail(&format!("bn daily defer: {msg}")),
                Ok(Some(d)) => d,
                Ok(None) => fail("bn daily defer: needs a target day (--on <date> or --in <days>)"),
            };
            match daily::defer(cfg, &today, &query.join(" "), &date) {
                Ok(d) => {
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success("daily_defer", payload::daily_defer_data(&d))
                                .to_json()
                        );
                    } else {
                        println!("Deferred [{}] to {}: {}", d.id, d.scheduled, d.text);
                    }
                }
                Err(e) => emit_err("daily_defer", &e, as_json),
            }
        }
        Some(DailyCmd::Rollover) => match daily::rollover(cfg, &today()) {
            Ok(r) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success("daily_rollover", payload::rollover_data(&r)).to_json()
                    );
                } else {
                    print!("{}", payload::rollover_text(&r));
                }
            }
            Err(e) => fail(&format!("bn daily rollover: {e}")),
        },
        Some(DailyCmd::Done { query, pct }) => {
            match daily::done(cfg, &today(), &query.join(" "), pct) {
                Ok(d) => {
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success("daily_done", payload::daily_done_data(&d)).to_json()
                        );
                    } else if let Some(p) = d.pct {
                        println!("Progress [{}] {p}%: {}", d.id, d.text);
                    } else {
                        let cal = d
                            .actual
                            .as_deref()
                            .map(|a| format!("  (actual {a})"))
                            .unwrap_or_default();
                        println!("Done [{}]: {}{cal}", d.id, d.text);
                    }
                }
                Err(e) => emit_err("daily_done", &e, as_json),
            }
        }
        Some(DailyCmd::Edit) => {
            let today = today();
            if let Err(e) = daily::ensure_today(cfg, &today) {
                fail(&format!("bn daily: {e}"));
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let status = std::process::Command::new(editor)
                .arg(cfg.daily_file(&today))
                .status();
            std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::routine::{RoutineBlockFile, RoutineConfig, RoutineFileConfig};

    fn snap_with(current: bool, has_next: bool) -> bn_core::routine::Snapshot {
        let mut blocks = Vec::new();
        if current {
            blocks.push(RoutineBlockFile {
                name: "Deep work".into(),
                id: Some("deep-work".into()),
                days: vec!["sat".into()],
                start: "00:00".into(),
                end: "23:59".into(),
                kind: Some("work".into()),
                protected: false,
            });
        }
        if has_next && !current {
            blocks.push(RoutineBlockFile {
                name: "Family day".into(),
                id: Some("family-day".into()),
                days: vec!["sun".into()],
                start: "09:00".into(),
                end: "18:00".into(),
                kind: Some("family".into()),
                protected: false,
            });
        }
        let raw = RoutineFileConfig {
            enabled: Some(true),
            timezone: Some("UTC".into()),
            reminder_minutes_before: None,
            notify_on_start: None,
            catchup_horizon_minutes: None,
            blocks,
        };
        let cfg = RoutineConfig::resolve(Some(raw)).expect("valid config");
        // 2026-01-03T10:00:00Z is a Saturday.
        bn_core::routine::snapshot(&cfg, 1_767_434_400)
    }

    #[test]
    fn routine_context_line_prefers_current_over_next() {
        let line = routine_context_line(&snap_with(true, false));
        assert!(line.starts_with("🗓 Deep work now"), "{line}");
    }

    #[test]
    fn routine_context_line_falls_back_to_next_when_nothing_current() {
        let line = routine_context_line(&snap_with(false, true));
        assert!(line.starts_with("🗓 next: Family day"), "{line}");
    }

    #[test]
    fn routine_context_line_says_nothing_scheduled_with_no_blocks() {
        assert_eq!(
            routine_context_line(&snap_with(false, false)),
            "🗓 Routine: nothing scheduled"
        );
    }
}
