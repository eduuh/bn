//! `bn in / out / track` — time tracking.
use crate::*;

/// anywhere (e.g. `bn in "lunch"`), so a non-repo cwd just yields `(None, None)`.
pub(crate) fn track_ctx(cfg: &Config) -> (Option<String>, Option<String>) {
    let cwd = std::env::current_dir().unwrap_or_default();
    match resolver::resolve(cfg, &cwd) {
        Ok(ctx) => (Some(ctx.repo), Some(ctx.branch)),
        Err(_) => (None, None),
    }
}

/// `bn in <activity>` — start/switch the current activity (a time-tracking punch). With `--at
/// <HH:MM>` or `--ago <dur>` the punch is backdated (the day is derived from the resolved
/// timestamp so a cross-midnight backdate lands in the right day's log).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_track_in(
    cfg: &Config,
    activity: &str,
    task: Option<String>,
    link: Option<String>,
    tag: Option<String>,
    at: Option<String>,
    ago: Option<String>,
    as_json: bool,
) {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now = clock.now_epoch_secs();
    let ts = match bn_core::track::resolve_in_ts(&clock.today(), now, at.as_deref(), ago.as_deref())
    {
        Ok(ts) => ts,
        Err(e) => fail(&format!("bn in: {e}")),
    };
    let date = bn_core::util::date_of(ts);
    let (repo, branch) = track_ctx(cfg);
    let resolved = bn_core::daily::resolve_link(task.as_deref(), link.as_deref());
    match bn_core::track::punch_in(
        cfg,
        &date,
        ts,
        activity,
        repo.as_deref(),
        branch.as_deref(),
        tag.as_deref(),
        resolved.as_deref(),
    ) {
        Ok(o) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("track_in", payload::track_punch_data(&o)).to_json()
                );
            } else {
                if let Some(prev) = &o.closed {
                    println!("Ended: {prev}");
                }
                let tag = o
                    .punch
                    .tag
                    .as_deref()
                    .map(|t| format!("  #{t}"))
                    .unwrap_or_default();
                println!("Tracking: {}{tag}", o.punch.activity);
            }
        }
        Err(e) => fail(&format!("bn in: {e}")),
    }
}

/// `bn out` — close the current activity / tracked day.
pub(crate) fn emit_track_out(cfg: &Config, as_json: bool) {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    match bn_core::track::punch_out(cfg, &clock.today(), clock.now_epoch_secs()) {
        Ok(o) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("track_out", payload::track_punch_data(&o)).to_json()
                );
            } else if let Some(prev) = &o.closed {
                println!("Ended: {prev}");
            } else {
                println!("Nothing was being tracked");
            }
        }
        Err(e) => fail(&format!("bn out: {e}")),
    }
}

/// `bn track [at <HH:MM>] [report [--by ...]] [--day <date>]` — the timeline + query views.
pub(crate) fn emit_track(
    cfg: &Config,
    action: Option<TrackCmd>,
    day: Option<String>,
    as_json: bool,
) {
    use bn_core::clock::Clock;
    use bn_core::track;
    let clock = bn_core::clock::RealClock;
    let today = clock.today();
    let now = clock.now_epoch_secs();
    let view = day.unwrap_or_else(|| today.clone());
    let punches = track::read_day(cfg, &view);
    let ivs = track::intervals(&punches, now);

    match action {
        None => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("track", payload::track_data(&view, &ivs)).to_json()
                );
            } else {
                print!("{}", track::render_day(&view, &ivs));
            }
        }
        Some(TrackCmd::At { time }) => {
            let Some(at_ts) = track::parse_at(&view, &time) else {
                fail(&format!(
                    "bn track at: invalid time '{time}' (expected HH:MM)"
                ));
            };
            let hit = track::at(&ivs, at_ts);
            if as_json {
                println!(
                    "{}",
                    Envelope::success("track_at", payload::track_at_data(at_ts, hit)).to_json()
                );
            } else if let Some(iv) = hit {
                let tag = iv
                    .tag
                    .as_deref()
                    .map(|t| format!("  #{t}"))
                    .unwrap_or_default();
                println!(
                    "{time}: {} (since {}){tag}",
                    iv.activity,
                    track::hhmm(iv.start)
                );
            } else {
                println!("{time}: nothing tracked");
            }
        }
        Some(TrackCmd::Report { by }) => {
            let by = track::ReportBy::parse(&by);
            let rows = track::report(&ivs, by);
            if as_json {
                println!(
                    "{}",
                    Envelope::success("track_report", payload::track_report_data(&rows, by))
                        .to_json()
                );
            } else {
                print!("{}", track::render_report(&rows, by));
            }
        }
    }
}
