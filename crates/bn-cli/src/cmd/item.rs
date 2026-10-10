//! `bn drop` / `bn undone` / `bn reword` — repair one branch-note item (see
//! [`bn_core::commands::drop_item`] and siblings).
use crate::*;

pub(crate) fn emit_item(cfg: &Config, verb: &str, query: &[String], to: Option<&str>, json: bool) {
    let ctx = resolve_or_exit(cfg);
    let query = query.join(" ");
    let res = match verb {
        "drop" => commands::drop_item(cfg, &ctx, &query),
        "undone" => commands::reopen_item(cfg, &ctx, &query),
        _ => commands::reword_item(cfg, &ctx, &query, to.unwrap_or_default()),
    };
    match res {
        Ok(out) if json => {
            let env = Envelope::success(verb, payload::item_data(&out));
            println!("{}", env.to_json());
        }
        Ok(out) => {
            let label = match verb {
                "drop" => "Dropped",
                "undone" => "Reopened",
                _ => "Reworded",
            };
            println!("{label} [{}]: {}", out.id, out.text);
        }
        Err(e) => emit_err(verb, &e, json),
    }
    sync_tmux(cfg, &ctx);
}
