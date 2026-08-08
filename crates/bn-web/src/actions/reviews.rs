use crate::actions::{bad_request, not_found_json, parse_body, MaybeAddr};
use crate::{audit, state::AppState};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct ReviewDecisionBody {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// `POST /api/reviews/:id {action, notes?}` — the human's plan-review decision from the dashboard.
pub async fn review_decide(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "review-decide";
    let body: ReviewDecisionBody = match parse_body(cmd, &raw) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(
            cmd,
            "invalid_id",
            "review id must be alphanumeric (e.g. r1)",
        );
    }
    let Some(decision) = bn_core::plan_review::Decision::parse(&body.action) else {
        return bad_request(
            cmd,
            "invalid_action",
            "action must be approve | request_changes | reject",
        );
    };
    let notes = body
        .notes
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let now = bn_core::util::now_epoch_secs();
    let action_label = format!(
        "review-{}",
        body.action.trim().to_lowercase().replace('_', "-")
    );

    let id_for_call = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::plan_review::decide(&id_for_call, decision, notes.as_deref(), now)
    })
    .await;

    match result {
        Ok(Ok(review)) => {
            audit::record(&state, &action_label, &id, addr, true);
            Json(Envelope::success(
                cmd,
                bn_core::payload::plan_review_data(&review),
            ))
            .into_response()
        }
        Ok(Err(error)) => {
            use bn_core::plan_review::DecideError;
            audit::record(&state, &action_label, &id, addr, false);
            match error {
                DecideError::NotFound(_) => not_found_json(cmd, &error.to_string()),
                DecideError::MissingNotes => bad_request(cmd, "missing_notes", &error.to_string()),
                DecideError::EmptyPlan => bad_request(cmd, "empty_plan", &error.to_string()),
                DecideError::NotPending { .. } => (
                    StatusCode::CONFLICT,
                    Json(Envelope::failure(cmd, "not_pending", error.to_string())),
                )
                    .into_response(),
                DecideError::Io(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(Envelope::failure(cmd, "io", error.to_string())),
                )
                    .into_response(),
            }
        }
        Err(error) => {
            audit::record(&state, &action_label, &id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", error.to_string())),
            )
                .into_response()
        }
    }
}
