//! Skill CRUD (#483) — create / edit / delete one skill source file, each write followed by a
//! Claude/Copilot native-skill sync so neither harness serves a stale copy.
//!
//! Split out of `actions.rs` when the agent-roster CRUD next door needed the same shape: the two
//! are siblings (a validated write of one Markdown definition, then a native materialization), and
//! keeping them as one file would have been the third time that file grew a section nobody could
//! find. The `use super::*` below is the module's whole import list — `parse_body`, `bad_request`,
//! `not_found_json`, `audit` and the axum extractors all live in the parent.

use super::*;

#[derive(Debug, Default, Deserialize)]
pub struct SkillEditBody {
    /// The full replacement markdown source (YAML frontmatter + body).
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct SkillCreateBody {
    /// The skill name — also the file stem (`<name>.md`) and how it is invoked.
    #[serde(default)]
    pub name: String,
    /// The one-line `description:` frontmatter. Required — a skill without one is unservable.
    #[serde(default)]
    pub description: String,
    /// Which tier to write into ([`bn_core::skills::SkillScope::parse`] labels). Defaults to
    /// `global-personal`: private, present in every repo, and available even when bn-web runs
    /// outside a git checkout.
    #[serde(default)]
    pub scope: Option<String>,
    /// The markdown body below the frontmatter. May be empty (a stub to fill in later).
    #[serde(default)]
    pub body: String,
    /// Replace an existing skill of the same name in that tier instead of failing with a conflict.
    #[serde(default)]
    pub overwrite: bool,
}

/// The tier a `POST /api/skills` with no explicit `scope` writes into.
const DEFAULT_CREATE_SCOPE: &str = "global-personal";

/// Validate a skill name for the CRUD endpoints: the file-stem charset (letters, digits, `-`, `_`),
/// non-empty and bounded. Keeps a bogus segment out of discovery and off any path.
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Re-materialize the native Agent Skills for Claude and Copilot after a source mutation, and
/// summarize the result for the response body.
///
/// Deliberately **non-fatal**: the bn source file is the system of record and has already been
/// written, so a sync failure (an unwritable harness root, a name collision with a skill bn does
/// not own) is reported alongside the success rather than turning a completed write into a `500`.
/// The caller sees `{ synced: true, created, updated, removed, unchanged }` or
/// `{ synced: false, error }` and can surface it.
fn sync_native_skills_summary(cfg: &bn_core::resolver::Config) -> serde_json::Value {
    use bn_core::skills::NativeInstallAction as A;
    let home = dirs::home_dir().unwrap_or_default();
    let report = bn_core::skills::sync_native_skills(cfg, std::path::Path::new("."), &home);
    let count = |want: A| {
        report
            .changes
            .iter()
            .filter(|change| change.action == want)
            .count()
    };
    // The harnesses that actually got refreshed — reported separately from the ones that failed, so
    // a partial sync (one harness clashing with a non-bn skill) is legible instead of all-or-nothing.
    let harnesses: std::collections::BTreeSet<&str> = report
        .changes
        .iter()
        .map(|change| change.harness.label())
        .collect();
    json!({
        "synced": report.complete(),
        "harnesses": harnesses,
        "created": count(A::Created),
        "updated": count(A::Updated),
        "removed": count(A::Removed),
        "unchanged": count(A::Unchanged),
        "failures": report.failures,
    })
}

/// `POST /api/skills {name, description, scope?, body?, overwrite?}` — create a skill from the
/// dashboard. Writes `<tier>/<name>.md` through [`bn_core::skills::create_skill`] (the same engine
/// as `bn skills new`), then syncs the native Claude/Copilot skills so the new workflow is
/// immediately invocable in both harnesses. `400` on a bad name/description/scope; `409` when the
/// tier already defines that name and `overwrite` is false.
pub async fn skill_create(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "skill-create";
    let body: SkillCreateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = body.name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }
    let description = body.description.trim().to_string();
    if description.is_empty() {
        return bad_request(cmd, "empty_description", "description is required");
    }
    let scope_label = body.scope.as_deref().unwrap_or(DEFAULT_CREATE_SCOPE);
    let Some(scope) = bn_core::skills::SkillScope::parse(scope_label) else {
        return bad_request(
            cmd,
            "invalid_scope",
            "scope must be global-shared | global-personal | repo-shared | repo-personal-notes | repo-personal-local",
        );
    };

    let cfg = state.cfg.clone();
    let (name_call, skill_body, overwrite) = (name.clone(), body.body.clone(), body.overwrite);
    let result = tokio::task::spawn_blocking(move || {
        let path = bn_core::skills::create_skill(
            &cfg,
            std::path::Path::new("."),
            scope,
            &name_call,
            &description,
            &skill_body,
            &[],
            overwrite,
        )?;
        let native = sync_native_skills_summary(&cfg);
        Ok::<_, std::io::Error>(json!({
            "skill": { "name": name_call, "scope": scope, "path": path.display().to_string() },
            "native": native,
        }))
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, &name, addr, false);
            let msg = e.to_string();
            // `create_skill` refuses to clobber an existing file unless `overwrite` — that's a
            // conflict the UI resolves by re-submitting, not a malformed request.
            if msg.contains("already exists") {
                (
                    StatusCode::CONFLICT,
                    Json(Envelope::failure(cmd, "skill_exists", msg)),
                )
                    .into_response()
            } else {
                bad_request(cmd, "create_failed", &msg)
            }
        }
        Err(e) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

/// `DELETE /api/skills/:name` — delete the *effective* skill `name` from the dashboard, then sync
/// the native harnesses so Claude and Copilot drop it too. The path comes from discovery, never
/// from the request (no arbitrary-path delete). `404` when no tier defines `name`.
///
/// Removes one definition, not the name: if a lower tier also defines `name`, that skill becomes
/// the effective one and stays installed. The response's `remaining_scope` says whether that
/// happened, so the UI can tell "gone" from "unshadowed".
pub async fn skill_delete(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
) -> Response {
    let cmd = "skill-delete";
    let name = name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }

    let cfg = state.cfg.clone();
    let name_call = name.clone();
    // `Err(None)` = no such skill (404); `Err(Some(msg))` = the delete itself failed (500).
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let here = std::path::Path::new(".");
            let info = bn_core::skills::effective_info(&cfg, here, &name_call)
                .map_err(|error| Some(error.to_string()))?
                .ok_or(None)?;
            bn_core::skills::delete_skill(std::path::Path::new(&info.path)).map_err(Some)?;
            // A lower tier may now be the effective definition — report it rather than implying the
            // name is gone.
            let remaining = bn_core::skills::effective_info(&cfg, here, &name_call)
                .map_err(|error| Some(error.to_string()))?;
            let native = sync_native_skills_summary(&cfg);
            Ok(json!({
                "skill": info,
                "remaining_scope": remaining.map(|r| r.scope),
                "native": native,
            }))
        })
        .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(None)) => {
            audit::record(&state, cmd, &name, addr, false);
            not_found_json(cmd, &format!("no skill named '{name}'"))
        }
        Ok(Err(Some(msg))) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "delete_failed", msg)),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

/// `PUT /api/skills/:name {content}` — edit the *effective* skill `name` from the dashboard
/// (#483). A native in-process write: it resolves the highest-precedence skill file (the one
/// bn-mcp serves), validates the new `content` parses as a skill so a bad edit can't corrupt it,
/// then overwrites the file and syncs the native Claude/Copilot skills so both harnesses pick the
/// edit up immediately. Audited as a `web_action`. `404` when no tier defines `name`; `400` when
/// the content isn't a valid skill. Only edits an existing skill — the file path comes from
/// discovery, never from the request (no arbitrary-path write).
pub async fn skill_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "skill-edit";
    let body: SkillEditBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }
    if body.content.trim().is_empty() {
        return bad_request(cmd, "empty_content", "content is required");
    }
    // Validate before touching the filesystem, so a bad edit is a clean 400 (never a corrupt file).
    if bn_core::skills::parse_skill(&body.content, &name).is_err() {
        return bad_request(
            cmd,
            "invalid_content",
            "content must be a skill: `---` YAML frontmatter with a `description`, then a body",
        );
    }

    let cfg = state.cfg.clone();
    let (name_call, content) = (name.clone(), body.content.clone());
    // `Err(None)` = no such skill (404); `Err(Some(msg))` = an IO failure on a known path (500).
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let info = bn_core::skills::effective_info(&cfg, std::path::Path::new("."), &name_call)
                .map_err(|error| Some(error.to_string()))?
                .ok_or(None)?;
            bn_core::skills::save_skill(std::path::Path::new(&info.path), &content)
                .map_err(Some)?;
            let native = sync_native_skills_summary(&cfg);
            Ok(json!({ "skill": info, "native": native }))
        })
        .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "skill-edit", &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(None)) => {
            audit::record(&state, "skill-edit", &name, addr, false);
            not_found_json(cmd, &format!("no skill named '{name}'"))
        }
        Ok(Err(Some(msg))) => {
            audit::record(&state, "skill-edit", &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "write_failed", msg)),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, "skill-edit", &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}
