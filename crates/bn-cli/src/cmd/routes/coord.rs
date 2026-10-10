use crate::*;

pub(super) fn route(cfg: &Config, action: CmdCoord, json: bool) {
    match action {
        CmdCoord::Watchdog { action } => crate::cmd::watchdog::emit_watchdog(cfg, action, json),
        CmdCoord::Proof { action } => crate::cmd::proof::emit_proof(cfg, action, json),
        CmdCoord::Compact { action } => crate::cmd::compact::emit_compact(cfg, action, json),
        CmdCoord::Context { action } => crate::cmd::context::emit_context(cfg, action, json),
        CmdCoord::Discover { task } => crate::cmd::discover::emit_discover(cfg, &task, json),
        CmdCoord::Drop { query } => crate::cmd::item::emit_item(cfg, "drop", &query, None, json),
        CmdCoord::Undone { query } => crate::cmd::item::emit_item(cfg, "undone", &query, None, json),
        CmdCoord::Reword { query, text } => {
            let text = text.join(" ");
            crate::cmd::item::emit_item(cfg, "reword", &[query], Some(&text), json)
        }
    }
}
