use crate::*;

pub(super) fn route(cfg: &Config, action: CmdTracking, json: bool) {
    match action {
        CmdTracking::Insights {
            since,
            write,
            set_baseline,
        } => emit_insights(cfg, since, write, set_baseline, json),
    }
}
