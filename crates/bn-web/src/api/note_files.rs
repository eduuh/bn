use crate::api::{bad_request, not_found};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn enrich_fleet_plans(cfg: &bn_core::resolver::Config, data: &mut Value) {
    let Some(agents) = data.get_mut("agents").and_then(Value::as_array_mut) else {
        return;
    };
    for agent in agents {
        let Some(repo) = agent.get("repo").and_then(Value::as_str) else {
            continue;
        };
        let Some(branch) = agent.get("branch").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(plan) = bn_core::plan::status(
            cfg,
            &bn_core::resolver::Resolved {
                repo: repo.to_lowercase(),
                branch: branch.to_string(),
            },
        ) {
            agent["plan"] = json!({
                "status": plan.status,
                "active": plan.active,
                "active_exists": plan.active_exists,
                "drafts": plan.drafts.len(),
            });
        }
    }
}

/// `GET /api/notes/{repo}/{branch}/plan` — plan artifact status for one branch note.
pub async fn note_plan(
    State(state): State<AppState>,
    Path((repo, branch)): Path<(String, String)>,
) -> Response {
    let ctx = resolved(repo, branch);
    match bn_core::plan::status(&state.cfg, &ctx) {
        Ok(status) => Json(Envelope::success("note_plan", json!(status))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                "note_plan",
                "plan_status_failed",
                error.to_string(),
            )),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct NoteFileQuery {
    pub path: Option<String>,
}

/// `GET /api/notes/{repo}/{branch}/files?path=plans` — list one note-relative directory.
pub async fn note_files(
    State(state): State<AppState>,
    Path((repo, branch)): Path<(String, String)>,
    Query(query): Query<NoteFileQuery>,
) -> Response {
    let ctx = resolved(repo, branch);
    let dir = match note_relative_path(&state.cfg, &ctx, &query.path.unwrap_or_default()) {
        Ok(path) => path,
        Err(error) => return invalid_path("note_files", &error),
    };
    let meta = match std::fs::metadata(&dir) {
        Ok(meta) if meta.is_dir() => meta,
        Ok(_) => return bad_request("note_files", "not_directory", "path is not a directory"),
        Err(_) => return not_found("note_files", "directory not found"),
    };
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    "note_files",
                    "read_dir_failed",
                    error.to_string(),
                )),
            )
                .into_response();
        }
    };
    let mut entries = read
        .flatten()
        .filter_map(|entry| note_file_entry(&state.cfg, &ctx, &entry.path()))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        let a_dir = a.get("kind").and_then(Value::as_str) == Some("dir");
        let b_dir = b.get("kind").and_then(Value::as_str) == Some("dir");
        b_dir.cmp(&a_dir).then_with(|| {
            a.get("path")
                .and_then(Value::as_str)
                .cmp(&b.get("path").and_then(Value::as_str))
        })
    });
    Json(Envelope::success(
        "note_files",
        json!({
            "path": state.cfg.note_relative_path(&ctx, &dir).unwrap_or_default(),
            "absolute_path": dir.display().to_string(),
            "modified": modified(&meta),
            "entries": entries,
        }),
    ))
    .into_response()
}

/// `GET /api/notes/{repo}/{branch}/file?path=plans/active.md` — read one note-relative file.
pub async fn note_file(
    State(state): State<AppState>,
    Path((repo, branch)): Path<(String, String)>,
    Query(query): Query<NoteFileQuery>,
) -> Response {
    let ctx = resolved(repo, branch);
    let Some(rel) = query.path else {
        return bad_request(
            "note_file",
            "missing_path",
            "path query parameter is required",
        );
    };
    let path = match note_relative_path(&state.cfg, &ctx, &rel) {
        Ok(path) => path,
        Err(error) => return invalid_path("note_file", &error),
    };
    let meta = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return bad_request("note_file", "not_file", "path is not a file"),
        Err(_) => return not_found("note_file", "file not found"),
    };
    const MAX_FILE_BYTES: u64 = 512 * 1024;
    if meta.len() > MAX_FILE_BYTES {
        return bad_request(
            "note_file",
            "file_too_large",
            "file is too large to preview",
        );
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    "note_file",
                    "read_file_failed",
                    error.to_string(),
                )),
            )
                .into_response();
        }
    };
    Json(Envelope::success(
        "note_file",
        json!({ "file": note_file_entry(&state.cfg, &ctx, &path), "content": content }),
    ))
    .into_response()
}

fn resolved(repo: String, branch: String) -> bn_core::resolver::Resolved {
    bn_core::resolver::Resolved {
        repo: repo.to_lowercase(),
        branch,
    }
}

fn note_relative_path(
    cfg: &bn_core::resolver::Config,
    ctx: &bn_core::resolver::Resolved,
    rel: &str,
) -> Result<std::path::PathBuf, String> {
    bn_core::plan::resolve_relative(cfg, ctx, rel)
        .map_err(|error| format!("invalid note-relative path: {error}"))
}

fn invalid_path(command: &str, error: &str) -> Response {
    bad_request(command, "invalid_path", error)
}

fn note_file_entry(
    cfg: &bn_core::resolver::Config,
    ctx: &bn_core::resolver::Resolved,
    path: &std::path::Path,
) -> Option<Value> {
    let rel = cfg.note_relative_path(ctx, path)?;
    let meta = std::fs::metadata(path).ok()?;
    let kind = if meta.is_dir() {
        "dir"
    } else {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
        {
            "md" | "markdown" => "markdown",
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
            "mov" | "mp4" | "webm" => "video",
            _ => "file",
        }
    };
    Some(json!({
        "path": rel,
        "name": path.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        "kind": kind,
        "size": meta.len(),
        "modified": modified(&meta),
    }))
}

fn modified(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}
