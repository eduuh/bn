//! The task-attachment endpoints, split from `task_writes.rs` under the file-size ratchet.
//!
//! A real seam rather than an arbitrary cut: these are the only handlers in bn that move *bytes*.
//! Everything else on the write surface takes a small JSON object and answers with one, while
//! these need a raised body limit, a base64 decode, a byte quota and a `Content-Type` — and they
//! are the only place a stored file is ever served back out.
//!
//! Capture-time attachments do not live here. They arrive inside `POST /api/tasks` so a capture
//! and its files land in one request (see `task_writes`); this module is the after-the-fact path,
//! for when the screenshot only turns up once the bug reproduces.

use crate::actions::{bad_request, conflict, not_found_json, parse_body, server_error, MaybeAddr};
use crate::audit;
use crate::state::AppState;
use crate::task_writes::{checked_id, decode_attachments, load_task, AttachmentBody};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde_json::json;

/// `POST /api/tasks/{id}/attach` — add files to a task that already exists.
///
/// The same shape the create endpoint takes, so the dashboard has one encoder for both the "record
/// it now" and "I found the screenshot afterwards" paths.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AttachBody {
    #[serde(default)]
    pub attachments: Vec<AttachmentBody>,
}

pub async fn task_attach_add(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-attach-add";
    let body: AttachBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = match checked_id(cmd, &id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let files = match decode_attachments(cmd, &body.attachments) {
        Ok(files) => files,
        Err(resp) => return resp,
    };
    if files.is_empty() {
        return bad_request(cmd, "no_files", "nothing to attach");
    }

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let task = load_task(&cfg, &task_id)?;
        bn_core::task_doc::ensure(&cfg, &task).map_err(|e| e.to_string())?;
        let mut stored = Vec::new();
        for file in &files {
            let saved = bn_core::task_attach::save(&cfg, &task, &file.name, &file.bytes)
                .map_err(|e| e.to_string())?;
            stored.push(saved.name);
        }
        bn_core::task_doc::sync_attachments(&cfg, &task).map_err(|e| e.to_string())?;
        Ok::<Vec<String>, String>(stored)
    })
    .await;

    match result {
        Ok(Ok(stored)) => {
            audit::record(&state, "task-attach-add", &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "attachments": stored }),
            ))
            .into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => not_found_json(cmd, &e),
        // The storage caps are a state conflict, not a malformed request: the same bytes would
        // have been accepted against a task with room for them.
        Ok(Err(e)) if e.contains("over the") => {
            audit::record(&state, "task-attach-add", &id, addr, false);
            conflict(cmd, "attachment_quota", &e)
        }
        Ok(Err(e)) => {
            audit::record(&state, "task-attach-add", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the attachment write panicked"),
    }
}

/// `GET /api/tasks/{id}/attach/{name}` — the bytes, so the dashboard can show the screenshot the
/// operator captured rather than just its name.
///
/// `Content-Disposition: inline` with a fixed `X-Content-Type-Options: nosniff`: an attachment is
/// third-party content served from the dashboard's own origin, so the browser must be told to
/// honour the declared type and nothing else. `bn_core::task_attach::mime_for` never returns
/// anything executable.
pub async fn task_attach_get(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Response {
    let cmd = "task-attach-get";
    let id = match checked_id(cmd, &id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let cfg = state.cfg.clone();
    let found = tokio::task::spawn_blocking(move || {
        let task = load_task(&cfg, &id)?;
        let attachment =
            bn_core::task_attach::find(&cfg, &task, &name).ok_or_else(|| "no file".to_string())?;
        let bytes = std::fs::read(&attachment.path).map_err(|e| e.to_string())?;
        Ok::<_, String>((attachment.mime, attachment.name, bytes))
    })
    .await;

    match found {
        Ok(Ok((mime, name, bytes))) => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("inline; filename=\"{name}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(_)) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(_) => server_error(cmd, "panicked", "the attachment read panicked"),
    }
}

/// `DELETE /api/tasks/{id}/attach/{name}` — drop one file and re-sync the doc's manifest.
pub async fn task_attach_delete(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path((id, name)): Path<(String, String)>,
) -> Response {
    let cmd = "task-attach-delete";
    let id = match checked_id(cmd, &id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let task = load_task(&cfg, &task_id)?;
        let removed =
            bn_core::task_attach::remove(&cfg, &task, &name).map_err(|e| e.to_string())?;
        if removed {
            bn_core::task_doc::sync_attachments(&cfg, &task).map_err(|e| e.to_string())?;
        }
        Ok::<bool, String>(removed)
    })
    .await;

    match result {
        Ok(Ok(removed)) => {
            audit::record(&state, "task-attach-delete", &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "removed": removed }),
            ))
            .into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => not_found_json(cmd, &e),
        Ok(Err(e)) => {
            audit::record(&state, "task-attach-delete", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the attachment delete panicked"),
    }
}

/// `GET /api/tasks/{id}/attach` — what is attached, without the bytes.
///
/// A route of its own rather than a field on `GET /api/tasks/{id}`: the detail read is at its file
/// size ratchet, and an attachment list is a directory scan that the detail view should not pay for
/// on every poll.
pub async fn task_attach_list(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let cmd = "task-attach-list";
    let id = match checked_id(cmd, &id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let listed = tokio::task::spawn_blocking(move || {
        let task = load_task(&cfg, &task_id)?;
        Ok::<_, String>(
            bn_core::task_attach::list(&cfg, &task)
                .into_iter()
                .map(|a| {
                    json!({
                        "name": a.name,
                        "size": a.size,
                        "human_size": a.human_size(),
                        "mime": a.mime,
                        "is_image": a.is_image(),
                    })
                })
                .collect::<Vec<_>>(),
        )
    })
    .await;

    match listed {
        Ok(Ok(files)) => Json(Envelope::success(
            cmd,
            json!({ "id": id, "count": files.len(), "attachments": files }),
        ))
        .into_response(),
        Ok(Err(e)) if e.starts_with("no task") => not_found_json(cmd, &e),
        Ok(Err(e)) => server_error(cmd, "read_failed", &e),
        Err(_) => server_error(cmd, "panicked", "the attachment list panicked"),
    }
}

#[cfg(test)]
mod tests {
    //! `POST /api/tasks` driven through the real router.
    //!
    //! The unit tests in `task_writes` cover the validators in isolation. These cover what they
    //! cannot: that a capture actually *lands* — one request in, and a task line, a context doc, a
    //! links section and a file on disk out. Every piece passing individually is not the same as
    //! the chain working.
    //!
    //! In-crate rather than under `tests/` because `bn-web` is a binary with no lib target, so an
    //! integration test could not import the router.

    use crate::routes::router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use bn_core::resolver::Config;
    use tower::ServiceExt;

    fn cfg(root: &std::path::Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    /// Drive one request through the full router and return `(status, parsed envelope)`.
    async fn call(cfg: &Config, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = router(AppState::with_live_addr(
            cfg.clone(),
            cfg.web.bind.clone(),
            cfg.web.port,
        ))
            .oneshot(req)
            .await
            .expect("router");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    fn get(path: String) -> Request<Body> {
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn a_capture_lands_as_a_task_a_doc_and_a_file_in_one_request() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());

        // "hi", encoded the way a browser's FileReader hands it over.
        let (status, body) = call(
            &cfg,
            post(
                "/api/tasks",
                serde_json::json!({
                    "title": "fix the SSE reconnect storm",
                    "repo": "bn",
                    "body": "the loop fires every 200ms on flaky wifi",
                    "links": [{"url": "https://github.com/o/r/issues/42", "title": "the report"}],
                    "attachments": [{"name": "shot.png", "data": "data:image/png;base64,aGk="}],
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let id = body["data"]["id"].as_str().expect("id").to_string();
        assert_eq!(body["data"]["attachments"][0], "shot.png");
        assert_eq!(
            body["data"]["warnings"].as_array().map(Vec::len),
            Some(0),
            "nothing should have been dropped: {body}"
        );

        let store = std::fs::read_to_string(cfg.tasks_file()).expect("store");
        assert!(store.contains("fix the SSE reconnect storm"), "{store}");
        assert!(store.contains(&format!("id:{id}")), "{store}");

        let doc = std::fs::read_to_string(root.path().join(format!("tasks/{id}.md"))).expect("doc");
        assert!(doc.contains("## Context"), "{doc}");
        assert!(doc.contains("the loop fires every 200ms"), "{doc}");
        assert!(
            doc.contains("[the report](<https://github.com/o/r/issues/42>)"),
            "{doc}"
        );
        assert!(doc.contains("- `shot.png`"), "{doc}");
        // Names, never paths — this document syncs to machines where this path does not exist.
        assert!(!doc.contains(root.path().to_str().expect("utf8")), "{doc}");

        let stored =
            std::fs::read(root.path().join(format!("tasks/{id}.files/shot.png"))).expect("file");
        assert_eq!(stored, b"hi");

        let (status, body) = call(&cfg, get(format!("/api/tasks/{id}/attach"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["attachments"][0]["name"], "shot.png");
        assert_eq!(body["data"]["attachments"][0]["is_image"], true);
    }

    /// A refused attachment must not leave a task behind claiming a file it never got, so decoding
    /// happens before the store write rather than after it.
    #[tokio::test]
    async fn a_bad_attachment_is_refused_before_the_task_is_written() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());

        let (status, body) = call(
            &cfg,
            post(
                "/api/tasks",
                serde_json::json!({
                    "title": "a task nobody should see",
                    "attachments": [{"name": "x.bin", "data": "!!! not base64 !!!"}],
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_base64");
        let store = std::fs::read_to_string(cfg.tasks_file()).unwrap_or_default();
        assert!(!store.contains("a task nobody should see"), "{store}");
    }

    #[tokio::test]
    async fn a_javascript_link_never_reaches_the_document() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());

        let (status, body) = call(
            &cfg,
            post(
                "/api/tasks",
                serde_json::json!({
                    "title": "pasted from a third-party issue",
                    "links": [{"url": "javascript:alert(1)"}],
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_link");
    }

    /// A traversal in the requested name resolves to a file that does not exist, never to one
    /// outside the task's own directory.
    #[tokio::test]
    async fn an_attachment_name_cannot_escape_its_task() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        std::fs::write(root.path().join("secret.txt"), b"do not serve me").expect("seed");

        let (_, body) = call(
            &cfg,
            post(
                "/api/tasks",
                serde_json::json!({ "title": "a normal task", "body": "hello" }),
            ),
        )
        .await;
        let id = body["data"]["id"].as_str().expect("id").to_string();

        let (status, _) = call(
            &cfg,
            get(format!("/api/tasks/{id}/attach/..%2F..%2Fsecret.txt")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
