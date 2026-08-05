//! Write-action audit trail (design §5).
//!
//! Every accepted (and every rejected) write funnels through [`record`], which appends a
//! `web_action` event to the machine-local `events.jsonl`. Because the [`crate::watch`] task tails
//! that same log and fans each new line out on the SSE stream, an audited action shows up live in
//! the UI activity feed for free — and is greppable after the fact:
//!
//! ```text
//! {"kind":"web_action","ts":…,"action":"dispatch","target":"bn","remote_addr":"127.0.0.1:53344","ok":true}
//! ```

use crate::state::AppState;
use bn_core::events;
use serde_json::{json, Map, Value};
use std::net::SocketAddr;

/// Record one web control action. `action` is the logical verb (`dispatch`, `queue-add`,
/// `queue-remove`, `scale`, `stop`, `send`, `relaunch`, `kill`, `land`, `start`, `auth-reject`),
/// `target` its subject (repo, item id, or agent target), `remote_addr` the caller, and `ok`
/// whether it was accepted. Best-effort: an audit-write failure is logged to stderr, never
/// propagated — losing a log line must not fail the underlying action.
pub fn record(
    _state: &AppState,
    action: &str,
    target: &str,
    remote_addr: Option<SocketAddr>,
    ok: bool,
) {
    let mut fields: Map<String, Value> = Map::new();
    fields.insert("action".into(), json!(action));
    fields.insert("target".into(), json!(target));
    fields.insert(
        "remote_addr".into(),
        json!(remote_addr
            .map(|a| a.to_string())
            .unwrap_or_else(|| "-".into())),
    );
    fields.insert("ok".into(), json!(ok));
    if let Err(e) = events::append("web_action", fields) {
        eprintln!("bn-web: audit append failed: {e}");
    }
}
