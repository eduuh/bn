//! The dashboard's supervision surface: read the dialog an agent is blocked on, answer it, and
//! give it a standing instruction.
//!
//! Until this existed, the dashboard could see *that* an agent needed you and had exactly one thing
//! to offer: a free-text box wired to `bn agent send`, which forked a headless `--resume` turn the
//! blocked pane never saw. You could watch an agent sit on a permission prompt and be unable to do
//! anything about it from the browser.
//!
//! Every handler goes through the CLI boundary ([`crate::bn_runner`]) rather than shelling tmux
//! here — the same rule `agent_pane` and `agent_activity` already follow, so the blast radius of
//! the web process stays "spawns bn".
//!
//! Kept out of `api.rs` / `actions.rs`, both of which are at their file-size ratchet.

use crate::{actions, bn_runner, state::AppState};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use bn_core::render::Envelope;
use serde::Deserialize;

fn bad_request(cmd: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(Envelope::failure(cmd, code, msg)),
    )
        .into_response()
}

/// Run `bn` with `args` and hand back its envelope verbatim.
///
/// The envelope's `ok` is the answer — a refusal from `bn agent answer` (`prompt_moved`,
/// `option_unavailable`) is a *correct* outcome the client must render, not an error to flatten, so
/// the status stays 200 and the client reads `ok`.
async fn run(cmd: &'static str, args: Vec<String>) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        bn_runner::run(&refs, None)
    })
    .await;
    match result {
        Ok(rr) => (StatusCode::OK, Json(rr.envelope)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(cmd, "join_error", e.to_string())),
        )
            .into_response(),
    }
}

/// `GET /api/agents/{target}/prompt` — the dialog this agent is blocked on, or `ok: false` with
/// `no_pending_prompt` when it is not blocked on one.
pub async fn agent_prompt(Path(target): Path<String>) -> Response {
    let cmd = "agent_prompt";
    if !actions::valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    run(cmd, vec!["agent".into(), "prompt".into(), target]).await
}

/// `GET /api/fleet/prompts` — every blocked agent's dialog, in one call.
///
/// One request rather than one per card. A fleet page that fetched a prompt per agent would issue a
/// tmux capture per agent per poll; batching keeps the "who needs me?" read a single round trip no
/// matter how wide the fleet gets.
pub async fn fleet_prompts(State(state): State<AppState>) -> Response {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        let fleet = bn_core::agent::supervise(
            &cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        );
        let mut out = Vec::new();
        for agent in &fleet.agents {
            let rr = bn_runner::run(&["agent", "prompt", &agent.target], None);
            if !rr.ok {
                continue;
            }
            if let Some(prompt) = rr.envelope.get("data").and_then(|d| d.get("prompt")) {
                out.push(serde_json::json!({
                    "target": agent.target,
                    "session_id": agent.session_id,
                    "prompt": prompt,
                }));
            }
        }
        out
    })
    .await;
    match result {
        Ok(prompts) => (
            StatusCode::OK,
            Json(Envelope::success(
                "fleet_prompts",
                serde_json::json!({ "count": prompts.len(), "prompts": prompts }),
            )),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                "fleet_prompts",
                "join_error",
                e.to_string(),
            )),
        )
            .into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct AnswerBody {
    /// The exact option number to pick.
    pub option: Option<u8>,
    /// Or a semantic: `approve` / `approve-session` / `reject`.
    pub semantic: Option<String>,
    /// The `fingerprint` from the prompt read this answer was decided against.
    pub fingerprint: Option<u64>,
}

/// `POST /api/agents/{target}/answer` — answer the dialog.
///
/// `fingerprint` is the whole point of this endpoint over a raw keystroke. A browser tab can sit
/// open for minutes while the agent moves on to a *different* dialog; without the round-trip the
/// click would answer whatever is on screen when it lands. Clients should always send it, and
/// re-read on a `prompt_moved` refusal rather than retrying.
pub async fn agent_answer(Path(target): Path<String>, raw: Bytes) -> Response {
    let cmd = "agent_answer";
    if !actions::valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    let body: AnswerBody = if raw.is_empty() {
        AnswerBody::default()
    } else {
        match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return bad_request(cmd, "bad_body", &e.to_string()),
        }
    };

    let mut args = vec!["agent".to_string(), "answer".to_string(), target];
    match (body.option, body.semantic.as_deref().map(str::trim)) {
        (Some(n), _) => {
            args.push("--option".into());
            args.push(n.to_string());
        }
        (_, Some("approve")) => args.push("--approve".into()),
        (_, Some("approve-session")) => args.push("--approve-session".into()),
        (_, Some("reject")) => args.push("--reject".into()),
        _ => {
            return bad_request(
                cmd,
                "no_choice",
                "send an `option` number, or a `semantic` of approve / approve-session / reject",
            )
        }
    }
    if let Some(fp) = body.fingerprint {
        args.push("--expect".into());
        args.push(fp.to_string());
    }
    run(cmd, args).await
}

#[derive(Debug, Default, Deserialize)]
pub struct InstructBody {
    pub text: String,
    /// Record the directive without delivering it to the live session.
    #[serde(default)]
    pub no_send: bool,
}

/// `POST /api/agents/{target}/instruct` — record a standing instruction and deliver it.
pub async fn agent_instruct(Path(target): Path<String>, raw: Bytes) -> Response {
    let cmd = "agent_instruct";
    if !actions::valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    let body: InstructBody = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return bad_request(cmd, "bad_body", &e.to_string()),
    };
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return bad_request(cmd, "empty_instruction", "text is required");
    }
    let mut args = vec!["agent".to_string(), "instruct".to_string(), target];
    if body.no_send {
        args.push("--no-send".into());
    }
    // `--` guards an instruction that begins with `-` from being read as a flag. It is still one
    // argv element, so nothing in it is shell-interpreted.
    args.push("--".into());
    args.push(text);
    run(cmd, args).await
}
