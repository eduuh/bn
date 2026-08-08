use crate::api::bad_request;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde_json::json;

fn valid_script_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// `GET /api/repos/{repo}/scripts/{name}` — read the effective bn job script for a repo and the
/// repo-tier path a save would write. Reads branch-independent repo-main scripts by design.
pub async fn repo_script(
    State(state): State<AppState>,
    Path((repo, name)): Path<(String, String)>,
) -> Response {
    let cmd = "repo-script";
    let repo = repo.trim().to_ascii_lowercase();
    let name = name.trim().to_ascii_lowercase();
    if !crate::actions::valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    if !valid_script_name(&name) {
        return bad_request(cmd, "invalid_script", "script name has invalid characters");
    }
    let ctx = bn_core::resolver::Resolved {
        repo: repo.clone(),
        branch: "main".to_string(),
    };
    let repo_path = state
        .cfg
        .note_dir(&ctx)
        .join("jobs")
        .join(format!("{name}.sh"));
    let resolved = bn_core::resolver::job_script(&state.cfg, Some(&ctx), &name);
    let (scope, path, content) = match resolved {
        Some((scope, path)) => {
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => {
                    return bad_request(
                        cmd,
                        "read_failed",
                        &format!("could not read script: {error}"),
                    )
                }
            };
            (Some(scope.label().to_string()), Some(path), Some(content))
        }
        None => (None, None, None),
    };
    let exists = content.is_some();
    Json(Envelope::success(
        cmd,
        json!({
            "repo": repo,
            "name": name,
            "scope": scope,
            "path": path.map(|p| p.display().to_string()),
            "repo_path": repo_path.display().to_string(),
            "content": content.unwrap_or_default(),
            "exists": exists,
        }),
    ))
    .into_response()
}
