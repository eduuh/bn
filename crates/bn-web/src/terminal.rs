//! The live agent terminal — a WebSocket bridge into an agent's tmux pane (#491, design §4(c)).
//!
//! Upgrades the polled "live pane tail" into a real, streamed terminal: `GET /ws/agents/{target}/
//! terminal` opens a socket that pushes a full pane frame every [`FRAME_INTERVAL_MS`] and forwards
//! token-gated input back into the pane. Two doctrine rules keep it safe to expose over HTTP:
//!
//! 1. **CLI boundary.** Every tmux touch stays behind the `bn` binary ([`crate::bn_runner`]): the
//!    output stream is `bn agent pane --ansi` (a capture with SGR escapes, rendered by xterm.js),
//!    and input is `bn agent send --keys` (paste into the live pane + verified submit — the
//!    reliable path against the copilot/claude TUIs). bn-web never shells tmux itself.
//! 2. **Auth.** The dashboard is fully open (#523) — the web token was removed; the LAN is trusted
//!    and external exposure is fronted by an authenticated dev tunnel — so both the view and the
//!    input are open, with no per-message token check.
//!
//! Resize is *soft*: the client's row count caps how many pane lines each capture returns; we never
//! resize the agent's real tmux geometry (that would disrupt the human watching the same window).

use crate::actions::{self, MaybeAddr};
use crate::audit;
use crate::bn_runner;
use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

/// How often the server pushes a fresh pane frame. A live-terminal cadence (faster than the 2s
/// poll the tail used), balanced against the cost of a `bn agent pane` spawn per tick.
const FRAME_INTERVAL_MS: u64 = 800;
/// Pane rows to capture when the client hasn't reported its viewport yet.
const DEFAULT_ROWS: usize = 48;
/// Upper bound on captured rows (mirrors the `agent_pane` tail clamp), so a bogus `rows=` can't ask
/// tmux for an unbounded capture.
const MAX_ROWS: usize = 400;

/// Query params for the terminal socket. `rows` seeds the initial capture height. Columns follow
/// the agent's real pane, so there's no `cols`.
#[derive(Debug, Deserialize)]
pub struct TerminalQuery {
    pub rows: Option<usize>,
}

/// `GET /ws/agents/{target}/terminal` — upgrade to the live-terminal WebSocket. Validates the
/// `repo:branch` target, applies the read/write [`access`] policy, audits the open, and hands the
/// socket to [`run_terminal`]. A malformed target is a `400`; a view denied on a LAN bind is a
/// `401` — both before any upgrade.
pub async fn agent_terminal(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(target): Path<String>,
    Query(q): Query<TerminalQuery>,
) -> Response {
    if !actions::valid_target(&target) {
        return (
            StatusCode::BAD_REQUEST,
            Json(Envelope::failure(
                "agent_terminal",
                "invalid_target",
                "target must be repo:branch",
            )),
        )
            .into_response();
    }

    // #523: the web token was removed — the terminal is fully open (view + input). External
    // exposure is fronted by an authenticated dev tunnel, so bn-web itself never gates.
    let can_write = true;

    let rows = q.rows.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
    audit::record(&state, "terminal-open", &target, addr, true);
    ws.on_upgrade(move |socket| run_terminal(socket, state, target, addr, can_write, rows))
}

/// Drive one upgraded terminal socket: a send half that streams pane frames (and relays the recv
/// half's out-of-band acks), and a recv half that applies client input/resize. Ends when either
/// half finishes — a client disconnect, a socket error, or the pane going away.
async fn run_terminal(
    socket: WebSocket,
    state: AppState,
    target: String,
    addr: Option<SocketAddr>,
    can_write: bool,
    initial_rows: usize,
) {
    let (mut sender, mut receiver) = socket.split();
    // Shared capture height — the recv half updates it on a client resize, the send half reads it
    // each tick. `tx`/`rx` let the recv half push control replies (input ack / denied) out through
    // the single owner of the sink.
    let rows = Arc::new(AtomicUsize::new(initial_rows));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(32);

    let frame_rows = rows.clone();
    let frame_target = target.clone();
    let mut send_task = tokio::spawn(async move {
        let mut ticker = time::interval(Duration::from_millis(FRAME_INTERVAL_MS));
        // Never bunch missed ticks into a burst if a capture runs long.
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let n = frame_rows.load(Ordering::Relaxed);
                    let (msg, gone) = frame_message(&frame_target, n).await;
                    if sender.send(msg).await.is_err() {
                        break;
                    }
                    if gone {
                        break;
                    }
                }
                Some(out) = rx.recv() => {
                    if sender.send(out).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => match parse_client_msg(text.as_str()) {
                    ClientMsg::Input(data) => {
                        if !can_write {
                            audit::record(&state, "terminal-input", &target, addr, false);
                            let _ = tx.send(denied_message()).await;
                            continue;
                        }
                        let ok = deliver_input(&target, &data).await;
                        audit::record(&state, "terminal-input", &target, addr, ok);
                        let _ = tx.send(ack_message(ok)).await;
                    }
                    ClientMsg::Resize(n) => rows.store(n, Ordering::Relaxed),
                    ClientMsg::Ignore => {}
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}

/// One capture, framed for the wire: a `frame` message with the pane lines, or a `gone` message
/// (and `true`, to close the socket) when the window has vanished / tmux is unreachable.
async fn frame_message(target: &str, rows: usize) -> (Message, bool) {
    match capture_frame(target, rows).await {
        FrameOutcome::Frame(lines) => (
            Message::Text(
                json!({ "type": "frame", "lines": lines })
                    .to_string()
                    .into(),
            ),
            false,
        ),
        FrameOutcome::Gone(reason) => (
            Message::Text(
                json!({ "type": "gone", "reason": reason })
                    .to_string()
                    .into(),
            ),
            true,
        ),
    }
}

/// The outcome of one pane capture: the frame lines, or the pane is gone (reason for the client).
enum FrameOutcome {
    Frame(Vec<String>),
    Gone(String),
}

/// Capture the pane via `bn agent pane <target> --lines <rows> --ansi` (SGR escapes kept for the
/// xterm render), off the async runtime in a blocking task. A failure envelope (`capture_failed` /
/// invalid target) means the pane is gone.
async fn capture_frame(target: &str, rows: usize) -> FrameOutcome {
    let target = target.to_string();
    let res = tokio::task::spawn_blocking(move || {
        let args = [
            "agent".to_string(),
            "pane".to_string(),
            target,
            "--lines".to_string(),
            rows.to_string(),
            "--ansi".to_string(),
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        bn_runner::run(&refs, None)
    })
    .await;
    match res {
        Ok(rr) => frame_from_envelope(&rr.envelope),
        Err(_) => FrameOutcome::Gone("join_error".to_string()),
    }
}

/// Extract frame lines from a `bn agent pane --json` envelope. A success envelope carries
/// `data.lines: [String]`; any failure envelope means the pane is gone, surfaced with its error
/// code. Pure — testable without a spawn.
fn frame_from_envelope(envelope: &Value) -> FrameOutcome {
    if envelope.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let lines = envelope
            .get("data")
            .and_then(|d| d.get("lines"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        FrameOutcome::Frame(lines)
    } else {
        let reason = envelope
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("unavailable")
            .to_string();
        FrameOutcome::Gone(reason)
    }
}

/// Deliver `data` into the live pane via `bn agent send <target> --keys -- <data>` (paste + verified
/// submit), off the runtime in a blocking task. `--` guards a payload starting with `-`; every field
/// is a distinct argv element, so there is no shell. Returns whether the command's envelope was `ok`.
async fn deliver_input(target: &str, data: &str) -> bool {
    let target = target.to_string();
    let data = data.to_string();
    let res = tokio::task::spawn_blocking(move || {
        let args = [
            "agent".to_string(),
            "send".to_string(),
            target,
            "--keys".to_string(),
            "--".to_string(),
            data,
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        bn_runner::run(&refs, None)
    })
    .await;
    res.map(|rr| {
        rr.envelope
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// A parsed client control message. Text lines from xterm arrive as `{"type":"input","data":…}`;
/// the client reports its viewport with `{"type":"resize","rows":…}`.
enum ClientMsg {
    Input(String),
    Resize(usize),
    Ignore,
}

/// Parse a client WebSocket text frame into a [`ClientMsg`]. Unknown types, malformed JSON, and an
/// empty `input` all collapse to [`ClientMsg::Ignore`]; `resize` rows are clamped to [`MAX_ROWS`].
/// Pure.
fn parse_client_msg(text: &str) -> ClientMsg {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return ClientMsg::Ignore;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("input") => v
            .get("data")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| ClientMsg::Input(s.to_string()))
            .unwrap_or(ClientMsg::Ignore),
        Some("resize") => v
            .get("rows")
            .and_then(Value::as_u64)
            .map(|r| ClientMsg::Resize((r as usize).clamp(1, MAX_ROWS)))
            .unwrap_or(ClientMsg::Ignore),
        _ => ClientMsg::Ignore,
    }
}

/// The `input-ack` control reply the client shows as a toast (delivered / failed).
fn ack_message(ok: bool) -> Message {
    Message::Text(json!({ "type": "input-ack", "ok": ok }).to_string().into())
}

/// The `denied` reply when input arrives without write authorization (defense-in-depth; the UI
/// only offers the input once a token is paired).
fn denied_message() -> Message {
    Message::Text(
        json!({ "type": "denied", "reason": "input requires the web token" })
            .to_string()
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_from_a_success_envelope_pulls_the_lines() {
        let env = json!({
            "ok": true,
            "command": "agent_pane",
            "data": { "target": "bn:main", "lines": ["\u{1b}[32m● building", "✓ done"], "count": 2 },
            "error": null,
        });
        match frame_from_envelope(&env) {
            FrameOutcome::Frame(lines) => {
                assert_eq!(
                    lines,
                    vec!["\u{1b}[32m● building".to_string(), "✓ done".to_string()]
                );
            }
            FrameOutcome::Gone(_) => panic!("a success envelope is a frame, not gone"),
        }
    }

    #[test]
    fn frame_from_a_failure_envelope_is_gone_with_the_code() {
        let env = json!({
            "ok": false,
            "command": "agent_pane",
            "data": null,
            "error": { "code": "capture_failed", "message": "no such window" },
        });
        match frame_from_envelope(&env) {
            FrameOutcome::Gone(reason) => assert_eq!(reason, "capture_failed"),
            FrameOutcome::Frame(_) => panic!("a failure envelope means the pane is gone"),
        }
    }

    #[test]
    fn parse_client_msg_reads_input_resize_and_ignores_the_rest() {
        assert!(matches!(
            parse_client_msg(r#"{"type":"input","data":"ls -la"}"#),
            ClientMsg::Input(s) if s == "ls -la"
        ));
        // Empty input is a no-op, not a blank send.
        assert!(matches!(
            parse_client_msg(r#"{"type":"input","data":""}"#),
            ClientMsg::Ignore
        ));
        assert!(matches!(
            parse_client_msg(r#"{"type":"resize","rows":30,"cols":120}"#),
            ClientMsg::Resize(30)
        ));
        // Rows are clamped to the capture ceiling.
        assert!(matches!(
            parse_client_msg(r#"{"type":"resize","rows":100000}"#),
            ClientMsg::Resize(n) if n == MAX_ROWS
        ));
        assert!(matches!(parse_client_msg("not json"), ClientMsg::Ignore));
        assert!(matches!(
            parse_client_msg(r#"{"type":"other"}"#),
            ClientMsg::Ignore
        ));
    }
}
