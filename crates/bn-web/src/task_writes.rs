//! The task write endpoints, split from `actions.rs` to keep the write surface under its size
//! ratchet (#1006, EPIC #993).
//!
//! Two shapes, deliberately kept apart:
//!
//! - **Synchronous** (create, day, priority, done, snooze, doc and attachment writes) — file
//!   writes measured in milliseconds, served natively in a `spawn_blocking` and answered with the
//!   result.
//! - **Asynchronous** (enhance, triage) — LLM agents measured in *minutes*. `bn_runner::run` is
//!   blocking and has no timeout, so serving one of these the same way would hold an axum worker
//!   for the whole run with no progress and no way to cancel. They spawn detached and return a
//!   handle immediately; progress shows up on the existing fleet surface.
//!
//! # Capture is one request
//!
//! `POST /api/tasks` takes the title *and* everything recorded alongside it — the prose, the links,
//! the pasted screenshots — because a capture split across three requests is a capture that can
//! half-fail. The operator watched an upload succeed and the task fail, or the reverse, and either
//! way what they typed is gone. One request either records the whole thing or records nothing.

use crate::actions::{
    bad_request, conflict, not_found_json, parse_body, respond_process, run_bn, server_error,
    valid_repo, MaybeAddr,
};
use crate::audit;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde_json::json;

/// The longest captured body accepted. Matches the doc edit endpoint's cap — the same document is
/// on the other end of both.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Links per task. Past this it is a bookmark list, not a task.
const MAX_LINKS: usize = 25;

/// Files per capture. The per-file and per-task byte caps in `bn_core::task_attach` are the real
/// limits; this only stops a pathological request from being decoded at all.
const MAX_ATTACHMENTS: usize = 20;

/// The request body limit for the routes that carry attachments.
///
/// Base64 inflates by a third, and a capture may carry several files up to
/// [`bn_core::task_attach::MAX_BYTES`], so the transport cap has to sit above the storage cap or
/// the storage cap could never be reached. Axum's 2 MB default is what this replaces — on those
/// routes only, since every other endpoint takes a small JSON object.
pub const ATTACH_BODY_LIMIT: usize =
    (bn_core::task_attach::MAX_TOTAL_BYTES as usize) * 4 / 3 + 1024 * 1024;

/// Only the schemes a task link may use.
///
/// An allowlist rather than a blocklist: this URL is rendered as a markdown link the operator (and
/// an agent) may follow, and `javascript:` / `data:` are exactly what a pasted "source" from a
/// third-party issue would carry.
fn valid_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() <= 2048
        && !url.contains(char::is_whitespace)
        && !url.contains('\n')
}

/// Validate and normalize the captured links.
#[allow(clippy::result_large_err)]
fn collect_links(cmd: &str, raw: &[LinkBody]) -> Result<Vec<bn_core::task_doc::Link>, Response> {
    let mut out = Vec::with_capacity(raw.len());
    for link in raw {
        let url = link.url.trim();
        if url.is_empty() {
            continue;
        }
        if !valid_link(url) {
            return Err(bad_request(
                cmd,
                "invalid_link",
                "links must be http(s) URLs",
            ));
        }
        out.push(bn_core::task_doc::Link {
            url: url.to_string(),
            title: link
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(url)
                .chars()
                .take(200)
                .collect(),
        });
    }
    Ok(out)
}

/// One attachment that has been decoded and bounds-checked, ready to store.
pub(crate) struct DecodedFile {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
}

/// Decode one base64 payload, tolerating the `data:<mime>;base64,` prefix a browser's `FileReader`
/// produces so the client does not have to strip it and get it wrong.
fn decode_data_url(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let encoded = match data.find(";base64,") {
        Some(at) if data.starts_with("data:") => &data[at + ";base64,".len()..],
        _ => data,
    };
    // Whitespace is not valid base64 but is what a wrapped payload carries.
    let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .ok()
}

/// Decode every attachment up front, before the task is written.
///
/// Deliberately eager: a payload that cannot be decoded is a client bug, and finding out *after*
/// creating the task would leave a task claiming attachments it never got.
#[allow(clippy::result_large_err)]
pub(crate) fn decode_attachments(
    cmd: &str,
    raw: &[AttachmentBody],
) -> Result<Vec<DecodedFile>, Response> {
    if raw.len() > MAX_ATTACHMENTS {
        return Err(bad_request(
            cmd,
            "too_many_files",
            "too many files on one task",
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for file in raw {
        let Some(bytes) = decode_data_url(&file.data) else {
            return Err(bad_request(
                cmd,
                "invalid_base64",
                "an attachment was not base64",
            ));
        };
        if bytes.is_empty() {
            continue;
        }
        if bytes.len() as u64 > bn_core::task_attach::MAX_BYTES {
            return Err(bad_request(
                cmd,
                "file_too_large",
                "an attachment is over the limit",
            ));
        }
        out.push(DecodedFile {
            name: file.name.clone(),
            bytes,
        });
    }
    Ok(out)
}

/// The shared id guard: a task id is `t` plus digits, and it is about to be used to resolve a path.
#[allow(clippy::result_large_err)]
pub(crate) fn checked_id(cmd: &str, id: &str) -> Result<String, Response> {
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(bad_request(
            cmd,
            "invalid_id",
            "task id must be alphanumeric (e.g. t42)",
        ));
    }
    Ok(id)
}

/// Load one task by id, for the handlers that need the task before touching its doc or files.
pub(crate) fn load_task(
    cfg: &bn_core::resolver::Config,
    id: &str,
) -> Result<bn_core::todo::Task, String> {
    let path = cfg.tasks_file();
    let _lock = bn_core::todo::lock(&path);
    bn_core::todo::load(&path)
        .map_err(|e| e.to_string())?
        .tasks()
        .find(|t| t.id() == Some(id))
        .cloned()
        .ok_or_else(|| format!("no task {id}"))
}

/// A URL captured with a task.
#[derive(Debug, Default, serde::Deserialize)]
pub struct LinkBody {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// One file captured with a task, its bytes base64-encoded into the create request.
///
/// Base64 rather than multipart: it costs a third more bytes on a payload that is already bounded,
/// and it buys the thing that matters — the task and its files are one request, so a capture
/// cannot half-land.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AttachmentBody {
    pub name: String,
    /// Base64, with or without a `data:<mime>;base64,` prefix (what a browser's FileReader gives).
    pub data: String,
}

/// `POST /api/tasks` — create a task, with everything recorded alongside it.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskCreateBody {
    pub title: String,
    pub repo: Option<String>,
    pub day: Option<String>,
    pub est: Option<String>,
    pub priority: Option<String>,
    /// Markdown prose captured with the task — what the operator knows right now.
    pub body: Option<String>,
    /// Where this came from: an issue, a thread, a doc.
    #[serde(default)]
    pub links: Vec<LinkBody>,
    /// Screenshots, logs, specs.
    #[serde(default)]
    pub attachments: Vec<AttachmentBody>,
    /// Provenance for the `src:` meta — an issue URL scores higher in `task_agents::pre_score`.
    pub src: Option<String>,
}

pub async fn task_create(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "task-create";
    let body: TaskCreateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return bad_request(cmd, "empty_title", "a task needs a title");
    }
    if let Some(repo) = body.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        if !valid_repo(repo.trim()) {
            return bad_request(cmd, "invalid_repo", "repo has invalid characters");
        }
    }
    for (field, value) in [("day", &body.day), ("est", &body.est)] {
        if let Some(v) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            if field == "day" && bn_core::util::epoch_day(v.trim()).is_none() {
                return bad_request(cmd, "invalid_day", "day must be YYYY-MM-DD");
            }
            if field == "est" && bn_core::smart::parse_est_minutes(v.trim()).is_none() {
                return bad_request(cmd, "invalid_est", "est must be like 90m, 2h or 1h30m");
            }
        }
    }
    let priority = match body
        .priority
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        None => None,
        Some(p) if p.len() == 1 && ('A'..='C').contains(&p.chars().next().unwrap_or('z')) => {
            p.chars().next()
        }
        Some(_) => return bad_request(cmd, "invalid_priority", "priority must be A, B or C"),
    };
    if body
        .body
        .as_deref()
        .is_some_and(|b| b.len() > MAX_BODY_BYTES)
    {
        return bad_request(cmd, "body_too_large", "the captured body is too long");
    }
    if body.links.len() > MAX_LINKS {
        return bad_request(cmd, "too_many_links", "too many links on one task");
    }
    let links = match collect_links(cmd, &body.links) {
        Ok(links) => links,
        Err(resp) => return resp,
    };
    let attachments = match decode_attachments(cmd, &body.attachments) {
        Ok(files) => files,
        Err(resp) => return resp,
    };
    let src = body
        .src
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // `src:` is a meta value on the task line, so it must not contain the separators that would
        // let it become other meta.
        .filter(|s| s.len() <= 400 && !s.contains(char::is_whitespace))
        .map(str::to_string);

    let cfg = state.cfg.clone();
    let audit_title = title.clone();
    let created = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let task = {
            let _lock = bn_core::todo::lock(&path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
            let today = bn_core::util::today();
            let id = file.next_id();
            // The title comes off the wire, so defang it before it becomes a description — a task
            // line's words are its syntax, and #1003 made `smart:`/`repo:` security signals.
            let text = bn_core::todo::neutralize_meta(&title);
            let mut task = bn_core::todo::Task::new_todo(&id, &text, priority, "", "web", &today);
            for (key, value) in [
                ("repo", body.repo.as_deref()),
                ("day", body.day.as_deref()),
                ("est", body.est.as_deref()),
            ] {
                if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                    task.set_meta_raw(key, &v.to_ascii_lowercase());
                }
            }
            // Not lowercased: a URL's path is case-sensitive, and `src:` is the one meta that
            // carries one.
            if let Some(src) = src.as_deref() {
                task.set_meta_raw("src", src);
            }
            file.push_task(task.clone());
            bn_core::todo::save(&path, &file).map_err(|e| e.to_string())?;
            task
        };
        // The store write is what makes the task exist, so it goes first and alone. Everything
        // below enriches a task that is already durable: a failure here costs context, never the
        // task itself, and the operator is told which of the two happened.
        let id = task.id().unwrap_or_default().to_string();
        let mut warnings: Vec<String> = Vec::new();
        if let Err(e) = bn_core::task_doc::ensure(&cfg, &task) {
            warnings.push(format!("could not create the context doc: {e}"));
        }
        if body.body.is_some() || !links.is_empty() {
            if let Err(e) = bn_core::task_doc::capture(&cfg, &task, body.body.as_deref(), &links) {
                warnings.push(format!("could not record the captured context: {e}"));
            }
        }
        let mut stored: Vec<String> = Vec::new();
        for file in &attachments {
            match bn_core::task_attach::save(&cfg, &task, &file.name, &file.bytes) {
                Ok(saved) => stored.push(saved.name),
                Err(e) => warnings.push(format!("could not store {}: {e}", file.name)),
            }
        }
        if !stored.is_empty() {
            if let Err(e) = bn_core::task_doc::sync_attachments(&cfg, &task) {
                warnings.push(format!("could not list the attachments in the doc: {e}"));
            }
        }
        Ok::<(String, Vec<String>, Vec<String>), String>((id, stored, warnings))
    })
    .await;

    match created {
        Ok(Ok((id, stored, warnings))) => {
            audit::record(&state, "task-create", &audit_title, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "attachments": stored, "warnings": warnings }),
            ))
            .into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, "task-create", &audit_title, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the task write panicked"),
    }
}

/// Has this task earned the right to be started?
///
/// Two different agents contribute, and both are required. `triage` decides what is genuinely
/// doable **today** and stamps `day:`; `improve` makes one task SMART and records the `smart:`
/// verdict. Requiring both means an agent is never handed work that is either unscheduled or
/// unscoped — and it is exactly what `task_agents::pre_score` already *ranks* by (+250 due today,
/// +200 `smart:ok`). The gate only enforces what the ranker already believed.
///
/// A `day:` in the future is deliberately not startable: it was triaged *for another day*.
fn is_triaged(task: &bn_core::todo::Task, today: &str) -> bool {
    let scheduled = task
        .meta("day")
        .is_some_and(|d| bn_core::util::epoch_day(d).is_some() && d <= today);
    scheduled && task.meta("smart") == Some("ok")
}

/// `POST /api/tasks/{id}` — a fast mutation on one task.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskActionBody {
    pub action: String,
    pub value: Option<String>,
}

pub async fn task_action(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-action";
    let body: TaskActionBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let action = body.action.trim().to_ascii_lowercase();
    let value = body.value.map(|v| v.trim().to_string()).unwrap_or_default();

    match action.as_str() {
        "day" | "snooze" if bn_core::util::epoch_day(&value).is_none() => {
            return bad_request(cmd, "invalid_date", "value must be YYYY-MM-DD")
        }
        "priority"
            if !(value.len() == 1
                && ('A'..='C').contains(&value.chars().next().unwrap_or('z'))) =>
        {
            return bad_request(cmd, "invalid_priority", "value must be A, B or C")
        }
        "day" | "snooze" | "priority" | "done" | "wip" | "cancel" => {}
        _ => {
            return bad_request(
                cmd,
                "invalid_action",
                "action must be day | priority | done | wip | snooze | cancel",
            )
        }
    }

    let cfg = state.cfg.clone();
    let (act, val, task_id) = (action.clone(), value.clone(), id.clone());
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let _lock = bn_core::todo::lock(&path);
        let mut file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
        let today = bn_core::util::today();
        // The Start gate. Enforced here rather than by hiding a button, because the dashboard is
        // not the only caller: the CLI and every agent hit this same endpoint, and a rule that
        // only the UI honours is a suggestion. `smart:` alone would not do it — it is a *ranking*
        // signal (`task_agents::pre_score` adds +200 for `ok`), and ranking never blocked anything.
        if act == "wip" {
            match file.tasks().find(|t| t.id() == Some(task_id.as_str())) {
                Some(t) if !is_triaged(t, &today) => {
                    return Err(format!("not_triaged: {task_id} has not been triaged"))
                }
                _ => {}
            }
        }
        let ok = match act.as_str() {
            "done" => file.mark_done(&task_id, &today),
            "wip" => file.mark_wip(&task_id),
            // Cancelling *is* the delete: the store never purges (done and cancelled lines stay,
            // so the file is the complete record), and `cancelled:1` keeps abandoned work from
            // ever being counted as a completion. An empty reason records no reason at all.
            "cancel" => file.cancel(
                &task_id,
                &today,
                Some(val.as_str()).filter(|r| !r.is_empty()),
            ),
            "snooze" => file.snooze(&task_id, &val),
            other => match file.task_mut(&task_id) {
                Some(task) => {
                    if other == "priority" {
                        task.priority = val.chars().next();
                    } else {
                        task.set_meta(other, &val);
                    }
                    true
                }
                None => false,
            },
        };
        if !ok {
            return Err(format!("no task {task_id}"));
        }
        bn_core::todo::save(&path, &file).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
    .await;

    let label = format!("task-{action}");
    match result {
        Ok(Ok(())) => {
            audit::record(&state, &label, &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "action": action }),
            ))
            .into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, &label, &id, addr, false);
            not_found_json(cmd, &e)
        }
        // A refused Start is a conflict with the task's state, not a malformed request: the caller
        // sent something valid that this task has not earned yet, and triaging it makes the same
        // call succeed unchanged.
        Ok(Err(e)) if e.starts_with("not_triaged") => {
            audit::record(&state, &label, &id, addr, false);
            conflict(
                cmd,
                "not_triaged",
                "only triaged tasks can be started — run triage, or improve it past the SMART gate",
            )
        }
        Ok(Err(e)) => {
            audit::record(&state, &label, &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the task write panicked"),
    }
}

/// `POST /api/tasks/{id}/doc/append` — append context under a section (default `Investigation`).
///
/// Additive and unguarded: it re-reads the current doc and inserts under the heading, so an
/// agent's append and a human's edit never fight over it. Creates the doc from the template first
/// if the task has none yet.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DocAppendBody {
    pub text: String,
    pub section: Option<String>,
}

pub async fn task_doc_append(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-doc-append";
    let body: DocAppendBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return bad_request(cmd, "empty_text", "nothing to append");
    }
    // The section becomes a `## <heading>` line, so keep it a plain heading.
    let section = body
        .section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Investigation")
        .to_string();
    if section.len() > 40
        || !section
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return bad_request(cmd, "invalid_section", "section must be a plain heading");
    }

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let task = {
            let _lock = bn_core::todo::lock(&path);
            let file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
            let found = file
                .tasks()
                .find(|t| t.id() == Some(task_id.as_str()))
                .cloned()
                .ok_or_else(|| format!("no task {task_id}"))?;
            found
        };
        match bn_core::task_doc::append_section(&cfg, &task, &section, &text) {
            Ok(d) => {
                Ok::<_, String>(json!({ "path": d.path, "body": d.text, "version": d.version() }))
            }
            Err(bn_core::task_doc::DocError::NoPath) => Err("no_doc".to_string()),
            Err(bn_core::task_doc::DocError::Io(e)) => Err(e.to_string()),
            Err(bn_core::task_doc::DocError::Conflict(_)) => Err("conflict".to_string()),
        }
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "task-doc-append", &id, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, "task-doc-append", &id, addr, false);
            not_found_json(cmd, &e)
        }
        Ok(Err(e)) if e == "no_doc" => bad_request(cmd, "no_doc", "this task has no context doc"),
        Ok(Err(e)) => {
            audit::record(&state, "task-doc-append", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the doc append panicked"),
    }
}

/// `PUT /api/tasks/{id}/doc` — replace the whole doc, guarded by `if_match` (the version the caller
/// read). A stale `if_match` is a `409 doc_conflict`, not a lost write.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DocEditBody {
    pub body: String,
    pub if_match: Option<String>,
}

pub async fn task_doc_edit(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-doc-edit";
    let body: DocEditBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let new_body = body.body;
    if new_body.len() > 256 * 1024 {
        return bad_request(cmd, "too_large", "the doc is unexpectedly large");
    }
    let if_match = body.if_match;

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let task = {
            let _lock = bn_core::todo::lock(&path);
            let file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
            let found = file
                .tasks()
                .find(|t| t.id() == Some(task_id.as_str()))
                .cloned()
                .ok_or_else(|| format!("no task {task_id}"))?;
            found
        };
        match bn_core::task_doc::write(&cfg, &task, new_body, if_match.as_deref()) {
            Ok(d) => {
                Ok::<_, String>(json!({ "path": d.path, "body": d.text, "version": d.version() }))
            }
            Err(bn_core::task_doc::DocError::Conflict(_)) => Err("conflict".to_string()),
            Err(bn_core::task_doc::DocError::NoPath) => Err("no_doc".to_string()),
            Err(bn_core::task_doc::DocError::Io(e)) => Err(e.to_string()),
        }
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "task-doc-edit", &id, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            not_found_json(cmd, &e)
        }
        Ok(Err(e)) if e == "no_doc" => bad_request(cmd, "no_doc", "this task has no context doc"),
        // Lost the race: the doc moved under the editor. 409 so the client keeps the operator's
        // text and reloads to show what actually landed.
        Ok(Err(e)) if e == "conflict" => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            conflict(
                cmd,
                "doc_conflict",
                "the doc changed since you opened it — your text is kept; reload to see what landed",
            )
        }
        Ok(Err(e)) => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the doc write panicked"),
    }
}

/// `POST /api/tasks/{id}/agent` — dispatch a task agent and return immediately.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskAgentBody {
    /// `enhance` (or legacy `improve`) or `triage`.
    pub agent: String,
    /// The operator's "things to look out for", recorded on the task doc so the agent reads it.
    pub steer: Option<String>,
}

/// Where a task agent stands when the task itself does not say.
///
/// Enhance is the stage that runs *before* a task has a `repo:` — `pipeline::sweep` skips
/// ungrounded tasks precisely because "the enhance pass owns it until it has a repo". A steerable
/// agent still needs a worktree to live in, and these agents only ever touch the task and its
/// context doc (both under the notes dir, not a product checkout), so the worktree is just a place
/// to stand. bn is the one repo guaranteed to be present.
const AGENT_HOME_REPO: &str = "bn";

/// The workflow each task agent is driven by, and the fleet name it launches under.
fn task_workflow(agent: &str) -> &'static str {
    match agent {
        "triage" => "task-triage",
        _ => "task-enhance",
    }
}

pub async fn task_agent(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-agent";
    let body: TaskAgentBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let requested_agent = body.agent.trim().to_ascii_lowercase();
    if !matches!(requested_agent.as_str(), "enhance" | "improve" | "triage") {
        return bad_request(
            cmd,
            "invalid_agent",
            "agent must be enhance | improve | triage",
        );
    }
    let agent = if requested_agent == "improve" {
        "enhance".to_string()
    } else {
        requested_agent
    };

    // Triage covers the whole store rather than one task, so only enhance needs the task to exist.
    // Resolving it here also decides where the agent stands and gives the board a subject a human
    // can read.
    let (repo, subject) = if agent == "enhance" {
        let cfg = state.cfg.clone();
        let task_id = id.clone();
        let loaded = tokio::task::spawn_blocking(move || load_task(&cfg, &task_id)).await;
        let task = match loaded {
            Ok(Ok(task)) => task,
            Ok(Err(e)) => {
                audit::record(&state, "task-enhance", &id, addr, false);
                return not_found_json(cmd, &e);
            }
            Err(_) => return server_error(cmd, "panicked", "the task read panicked"),
        };
        // Record the operator's steering on the doc rather than passing it as a flag: the agent is
        // seeded from the workflow, not from this request, so steering that lives only in the
        // command line never reaches it. On the doc it is durable, visible in the UI, and still
        // there if the run is retried.
        if let Some(steer) = body
            .steer
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let cfg = state.cfg.clone();
            let task = task.clone();
            let steer = steer.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                bn_core::task_doc::append_section(&cfg, &task, "Steering", &steer)
            })
            .await;
        }
        let repo = task
            .meta("repo")
            .map(str::trim)
            .filter(|r| !r.is_empty() && valid_repo(r))
            .unwrap_or(AGENT_HOME_REPO)
            .to_string();
        (repo, format!("enhance {id}: {}", task.summary()))
    } else {
        (
            AGENT_HOME_REPO.to_string(),
            "triage: pick what is doable today".to_string(),
        )
    };

    let workflow = task_workflow(&agent);
    let mut args = vec![
        "queue".into(),
        "add".into(),
        subject,
        "--repo".into(),
        repo,
        "--kind".into(),
        "enhance".into(),
        "--priority".into(),
        "25".into(),
        "--skill".into(),
        workflow.to_string(),
        "--name".into(),
        format!("{id}-{agent}"),
        // Nobody is sitting in the pane at launch, so a permission prompt would simply stall it.
        "--yolo".into(),
    ];
    if agent == "enhance" {
        // Point the workflow at the bare id while the subject above stays human-readable. Triage
        // takes no argument — it covers the whole store — so it is seeded from the subject.
        args.push("--skill-arg".into());
        args.push(id.clone());
    }
    let result = run_bn(args, None).await;
    respond_process(&state, &format!("task-{agent}"), &id, addr, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_accepted_bare_or_as_a_browser_data_url() {
        // "hi" — what `FileReader.readAsDataURL` produces, and the bare form.
        assert_eq!(decode_data_url("aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(
            decode_data_url("data:image/png;base64,aGk=").as_deref(),
            Some(&b"hi"[..])
        );
        // A wrapped payload is still one payload.
        assert_eq!(decode_data_url("aG\nk=").as_deref(), Some(&b"hi"[..]));
        assert!(decode_data_url("not base64!!").is_none());
    }

    /// The URL goes into a markdown link the operator and an agent may follow, so the scheme is an
    /// allowlist. A third-party issue is exactly where a `javascript:` "source" comes from.
    #[test]
    fn only_http_links_are_accepted() {
        assert!(valid_link("https://github.com/o/r/issues/7"));
        assert!(valid_link("http://localhost:8080/thing"));
        assert!(!valid_link("javascript:alert(1)"));
        assert!(!valid_link("JavaScript:alert(1)"));
        assert!(!valid_link("data:text/html,<script>"));
        assert!(!valid_link("file:///etc/passwd"));
        assert!(!valid_link("https://example.com/a b"));
        assert!(!valid_link(&format!(
            "https://example.com/{}",
            "x".repeat(3000)
        )));
    }

    #[test]
    fn a_blank_link_is_dropped_rather_than_rejected() {
        let links = collect_links(
            "cmd",
            &[
                LinkBody {
                    url: "  ".into(),
                    title: None,
                },
                LinkBody {
                    url: "https://example.com".into(),
                    title: Some("  ".into()),
                },
            ],
        )
        .unwrap_or_else(|_| panic!("blank links must not fail the capture"));
        assert_eq!(links.len(), 1);
        // A blank title falls back to the URL, so a link always renders as something clickable.
        assert_eq!(links[0].title, "https://example.com");
    }

    #[test]
    fn an_oversized_attachment_is_refused_before_anything_is_written() {
        use base64::Engine;
        let huge = base64::engine::general_purpose::STANDARD.encode(vec![
            0u8;
            bn_core::task_attach::MAX_BYTES
                as usize
                + 1
        ]);
        assert!(decode_attachments(
            "cmd",
            &[AttachmentBody {
                name: "big.bin".into(),
                data: huge,
            }]
        )
        .is_err());
    }

    #[test]
    fn the_body_limit_leaves_room_for_the_storage_quota() {
        // Base64 inflates by a third; a transport cap below the storage cap would make the storage
        // cap unreachable and the error message a lie.
        assert!(ATTACH_BODY_LIMIT > (bn_core::task_attach::MAX_TOTAL_BYTES as usize) * 4 / 3);
    }

    #[test]
    fn a_task_id_is_never_allowed_to_be_a_path() {
        assert!(checked_id("cmd", "../../etc").is_err());
        assert!(checked_id("cmd", "t42/../..").is_err());
        assert!(checked_id("cmd", "").is_err());
        assert_eq!(checked_id("cmd", " t42 ").ok().as_deref(), Some("t42"));
    }
}
