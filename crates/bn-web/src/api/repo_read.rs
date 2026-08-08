use crate::api::bad_request;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde_json::json;

/// `GET /api/repos` — the local repo inventory: repos with local clone/worktree/live-agent evidence.
pub async fn repos(State(state): State<AppState>) -> Json<Envelope> {
    Json(Envelope::success(
        "repos",
        crate::repos::repos_view(&state.cfg),
    ))
}

/// `GET /api/repos/{repo}/issues` — one repo's open GitHub issues (#556), backed by the #999 cache.
pub async fn repo_issues(State(state): State<AppState>, Path(repo): Path<String>) -> Response {
    let cmd = "repo-issues";
    let repo = repo.trim().to_ascii_lowercase();
    if !crate::actions::valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }

    let cfg = state.cfg.clone();
    let name = repo.clone();
    let found = tokio::task::spawn_blocking(move || {
        let runner = bn_core::runner::RealRunner;
        bn_core::issues::for_repo(&cfg, &runner, &name, bn_core::util::now_epoch_secs())
    })
    .await
    .ok();

    match found {
        Some(r) => Json(Envelope::success(
            cmd,
            json!({
                "repo": r.repo,
                "slug": r.slug,
                "issues": r.issues,
                "fetched_at": r.fetched_at,
                "stale": r.stale,
                "error": r.error,
            }),
        ))
        .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                cmd,
                "fetch_panicked",
                "issue fetch failed",
            )),
        )
            .into_response(),
    }
}
