use crate::*;

pub(super) fn route(cfg: &Config, action: CmdTracking, json: bool) {
    match action {
        CmdTracking::In {
            activity,
            task,
            link,
            tag,
            at,
            ago,
        } => emit_track_in(cfg, &activity.join(" "), task, link, tag, at, ago, json),
        CmdTracking::Out => emit_track_out(cfg, json),
        CmdTracking::Track { action, day } => emit_track(cfg, action, day, json),
        CmdTracking::Calib => emit_calib(cfg, json),
        CmdTracking::Stand => {
            let s = bn_core::reports::stand(cfg, &today());
            if json {
                println!(
                    "{}",
                    Envelope::success("stand", payload::stand_data(&s)).to_json()
                );
            } else {
                print!("{}", bn_core::reports::render_stand(&s));
            }
        }
        CmdTracking::Wrap { cron } => {
            // Cron gate: the wrap-digest job no-ops unless the digest is enabled.
            if cron && !resolver::JobConfig::load().wrap_enabled {
                return;
            }
            emit_wrap(cfg, json);
        }
        CmdTracking::Rate {
            focus,
            friction,
            mood,
        } => emit_rate(cfg, focus, friction, mood.as_deref(), json),
        CmdTracking::Weekly { since } => {
            let dxi = resolver::JobConfig::load().dxi_enabled;
            let w = bn_core::reports::weekly(cfg, &today(), since, dxi);
            if json {
                println!(
                    "{}",
                    Envelope::success("weekly", payload::weekly_data(&w)).to_json()
                );
            } else {
                print!("{}", bn_core::reports::render_weekly(&w));
            }
        }
        CmdTracking::Insights {
            since,
            write,
            set_baseline,
        } => emit_insights(cfg, since, write, set_baseline, json),
        CmdTracking::Brag { since } => {
            let b = bn_core::brag::brag(
                cfg,
                &today(),
                since.as_deref(),
                &bn_core::runner::RealRunner,
            );
            if json {
                println!(
                    "{}",
                    Envelope::success("brag", payload::brag_data(&b)).to_json()
                );
            } else {
                print!("{}", bn_core::brag::render_markdown(&b));
            }
        }
    }
}
