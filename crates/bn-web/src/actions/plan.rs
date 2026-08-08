use crate::actions::{bad_request, parse_body, server_error, MaybeAddr};
use crate::{audit, state::AppState};
use axum::{
    body::Bytes,
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct PlanPromoteBody {
    #[serde(default)]
    pub draft: String,
}

/// `POST /api/notes/:repo/:branch/plan/promote {draft}` — promote a note-relative draft plan to
/// `plans/active.md`.
pub async fn plan_promote(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path((repo, branch)): Path<(String, String)>,
    raw: Bytes,
) -> Response {
    let cmd = "plan-promote";
    let body: PlanPromoteBody = match parse_body(cmd, &raw) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let draft = body.draft.trim().to_string();
    if draft.is_empty() {
        return bad_request(cmd, "missing_draft", "draft is required");
    }
    if draft.len() > 300 {
        return bad_request(cmd, "invalid_draft", "draft path is too long");
    }
    let ctx = bn_core::resolver::Resolved {
        repo: repo.to_lowercase(),
        branch,
    };
    let cfg = state.cfg.clone();
    let draft_for_call = draft.clone();
    let result =
        tokio::task::spawn_blocking(move || bn_core::plan::promote(&cfg, &ctx, &draft_for_call))
            .await;
    let audit_target = format!("{repo}/{draft}");
    match result {
        Ok(Ok(path)) => {
            audit::record(&state, "plan-promote", &audit_target, addr, true);
            Json(Envelope::success(
                cmd,
                json!({
                    "path": path.display().to_string(),
                    "relative": bn_core::plan::ACTIVE_PLAN_REL,
                    "source": draft,
                }),
            ))
            .into_response()
        }
        Ok(Err(error)) => {
            audit::record(&state, "plan-promote", &audit_target, addr, false);
            match error {
                bn_core::plan::PlanError::InvalidPath(_) => {
                    bad_request(cmd, "invalid_path", error.to_string().as_str())
                }
                bn_core::plan::PlanError::Io(_) | bn_core::plan::PlanError::Store(_) => {
                    server_error(cmd, "promote_failed", error.to_string().as_str())
                }
            }
        }
        Err(error) => {
            audit::record(&state, "plan-promote", &audit_target, addr, false);
            server_error(cmd, "join_error", error.to_string().as_str())
        }
    }
}
