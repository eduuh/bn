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
        CmdTracking::Insights {
            since,
            write,
            set_baseline,
        } => emit_insights(cfg, since, write, set_baseline, json),
    }
}
