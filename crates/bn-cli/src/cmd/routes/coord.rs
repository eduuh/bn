use crate::*;

pub(super) fn route(cfg: &Config, action: CmdCoord, json: bool) {
    match action {
        CmdCoord::Queue { action } => crate::cmd::queue::emit_queue(cfg, action, json),
        CmdCoord::Watchdog { action } => crate::cmd::watchdog::emit_watchdog(cfg, action, json),
        CmdCoord::Proof { action } => crate::cmd::proof::emit_proof(cfg, action, json),
        CmdCoord::Compact { action } => crate::cmd::compact::emit_compact(cfg, action, json),
        CmdCoord::Context { action } => crate::cmd::context::emit_context(cfg, action, json),
    }
}
