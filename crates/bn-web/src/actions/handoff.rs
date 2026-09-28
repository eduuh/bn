//! `POST /api/handoff` (#509) — the native in-process resume-packet write, open per #526. Split out
//! of [`super`] under the never-grow ratchet.
use crate::actions::{bad_request, parse_body, valid_repo, MaybeAddr};
use crate::{audit, state::AppState};
use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct HandoffWriteBody {
    /// The target repo (normalized to lowercase by the resolver).
    #[serde(default)]
    pub repo: Option<String>,
    /// The target branch (slashes normalized to dashes by the resolver).
    #[serde(default)]
    pub branch: Option<String>,
    /// Layer 2: the narrative — why it looks this way. Blank/absent leaves it empty.
    #[serde(default)]
    pub narrative: Option<String>,
    /// Layer 3: decisions deferred to the next session (the half the note can't supply).
    #[serde(default)]
    pub deferred: Vec<String>,
    /// Layer 4: extra open questions beyond the note's asks.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Layer 5: an explicit priority queue; empty defaults to the note's open todos.
    #[serde(default)]
    pub priority_queue: Vec<String>,
}

/// `POST /api/handoff {repo, branch, narrative?, deferred?, questions?, priority_queue?}` — compose
/// and persist the branch's resume packet from the dashboard (#509), the write counterpart to the
/// `GET /api/handoff` read. A native in-process write via [`bn_core::handoff::write`]: it reads the
/// branch note, composes the five layers with the author-supplied ones, and writes `handoff.json`
/// (+ the `.md` mirror) beside the note. Open per #526 (loopback/LAN trust boundary) and audited as
/// a `web_action`. `400` on a missing/invalid target; `500` (`no_note`) when the branch has no note
/// to compose from.
pub async fn handoff_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "handoff-write";
    let body: HandoffWriteBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let repo = body.repo.map(|s| s.trim().to_string()).unwrap_or_default();
    let branch = body
        .branch
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if repo.is_empty() || branch.is_empty() {
        return bad_request(cmd, "missing_target", "repo and branch are required");
    }
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }

    let Ok(ctx) = bn_core::resolver::resolve_target(
        &state.cfg,
        std::path::Path::new("."),
        Some(repo.clone()),
        Some(branch.clone()),
    ) else {
        return bad_request(cmd, "invalid_target", "could not resolve repo/branch");
    };

    let input = bn_core::handoff::HandoffInput {
        narrative: body.narrative.filter(|s| !s.trim().is_empty()),
        deferred: body.deferred,
        questions: body.questions,
        priority_queue: body.priority_queue,
        // The dashboard editor is pre-filled with the current packet, so what it posts is the
        // whole intended state: an emptied field means "clear it", not "keep the old one".
        replace: true,
    };

    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::handoff::write(&cfg, &ctx, &bn_core::util::today(), &input)
    })
    .await;

    let target = format!("{repo}/{branch}");
    match result {
        Ok(Ok(h)) => {
            audit::record(&state, "handoff-write", &target, addr, true);
            let data = json!({
                "handoff": h,
                "markdown": bn_core::handoff::render_markdown(&h),
                "rendered": bn_core::handoff::render_orientation(&h),
            });
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, "handoff-write", &target, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    cmd,
                    "no_note",
                    format!("handoff write failed: {e}"),
                )),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, "handoff-write", &target, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}
