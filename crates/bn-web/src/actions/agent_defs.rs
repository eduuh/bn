//! Markdown agent-roster CRUD — create / edit / delete one `agents/<name>.md` from the dashboard.
//!
//! The sibling of [`super::skills`], and the write half of the read-only `/api/agent-defs` routes in
//! [`crate::api::automation`]. Same doctrine as the skill endpoints: a native in-process write of
//! the source file, validated *before* anything touches the disk, with the path taken from
//! discovery rather than from the request — there is no arbitrary-path write here.
//!
//! Two things make an agent stricter than a skill:
//!
//! * **A bad agent breaks the whole catalog, not just itself.** `bn_core::agents::discover` fails
//!   the entire roster on one unparseable file, which would stop the agent-scheduler dispatching
//!   *anything*. Every write is therefore parsed through
//!   [`bn_core::agents::parse_agent`] first and rejected as a `400` if it would not survive the
//!   next tick.
//! * **Only specialists compile anywhere else.** A skill write always re-syncs the native Claude
//!   and Copilot roots; an agent write only needs to when the agent is a specialist, which is the
//!   one kind that materializes into a native harness agent file. For every other kind the
//!   response carries `native: null` — nothing to sync is different from synced nothing.

use super::*;
use bn_core::agents::write::NewAgent;
use bn_core::agents::AgentKind;

#[derive(Debug, Default, Deserialize)]
pub struct AgentDefEditBody {
    /// The full replacement markdown source (YAML frontmatter + prompt body).
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentDefCreateBody {
    /// The agent name — also the file stem (`<name>.md`) and how `bn run` addresses it.
    #[serde(default)]
    pub name: String,
    /// `command` | `staff` | `hosted` | `specialist`. Optional in the type only so the body can
    /// derive `Default` for [`parse_body`]; a missing kind is a `400`, because the kind decides
    /// which fields below are mandatory and which are forbidden.
    #[serde(default)]
    pub kind: Option<AgentKind>,
    /// The one-line `description:` frontmatter.
    #[serde(default)]
    pub description: String,
    /// Which tier to write into ([`bn_core::agents::write::CREATE_SCOPES`]). Defaults to
    /// `global-personal` — private, and present whatever repo bn-web was started in.
    #[serde(default)]
    pub scope: Option<String>,
    /// Staff only: the command whose output the prompt renders over.
    #[serde(default)]
    pub gather: Option<String>,
    /// Staff only: where the briefing card is written.
    #[serde(default)]
    pub sinks: Vec<String>,
    /// Specialist only: the repo the reviewer is gated to.
    #[serde(default)]
    pub repo: Option<String>,
    /// The prompt below the frontmatter. Required for every kind but `command`; left empty for a
    /// specialist it means "scaffold me one" (see [`agent_def_create`]).
    #[serde(default)]
    pub body: String,
    /// Replace a same-named agent in that tier instead of failing with a conflict.
    #[serde(default)]
    pub overwrite: bool,
}

/// The tier a `POST /api/agent-defs` with no explicit `scope` writes into.
const DEFAULT_CREATE_SCOPE: &str = "global-personal";

/// Re-materialize the native Copilot/Claude specialist agents after a source mutation, and
/// summarize it for the response.
///
/// The global-scope equivalent of `bn agents install`, down to the install anchor: the stable
/// `config_dir()/repo` checkout when it exists, so the result does not depend on which directory
/// bn-web was launched from, falling back to the cwd.
///
/// **Non-fatal by design**, exactly as the skill sync is: the `.md` is the system of record and has
/// already been written, so a harness root that refuses (a non-bn agent file of the same name)
/// is reported next to the success rather than turning a completed write into a `500`.
fn sync_native_agents_summary(cfg: &bn_core::resolver::Config) -> serde_json::Value {
    use bn_core::skills::{NativeHarness, NativeInstallAction as A, NativeScope};
    let home = dirs::home_dir().unwrap_or_default();
    let anchor = bn_core::resolver::config_dir().join("repo");
    let base = if anchor.is_dir() {
        anchor
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let ctx = bn_core::resolver::resolve(cfg, &base).ok();
    let report = bn_core::specialists::install_native_agents(
        cfg,
        ctx.as_ref(),
        &base,
        &home,
        NativeScope::Global,
        &[NativeHarness::Claude, NativeHarness::Copilot],
    );
    match report {
        Ok(report) => {
            let count = |want: A| {
                report
                    .changes
                    .iter()
                    .filter(|change| change.action == want)
                    .count()
            };
            let harnesses: std::collections::BTreeSet<&str> = report
                .changes
                .iter()
                .map(|change| change.harness.label())
                .collect();
            json!({
                "synced": true,
                "harnesses": harnesses,
                "created": count(A::Created),
                "updated": count(A::Updated),
                "removed": count(A::Removed),
                "unchanged": count(A::Unchanged),
            })
        }
        Err(error) => json!({ "synced": false, "harnesses": [], "error": error }),
    }
}

/// One agent's identity for a mutation response — never its body, which the caller already has.
fn agent_ref(agent: &bn_core::agents::AgentDef) -> serde_json::Value {
    json!({
        "name": agent.name,
        "scope": agent.scope.label(),
        "path": agent.path,
        "specialist": agent.is_specialist(),
        "kind": agent.kind,
    })
}

/// `POST /api/agent-defs {name, kind, description, scope?, body?, gather?, sinks?, repo?}` — author
/// a new Markdown agent.
///
/// A specialist with no `body` is scaffolded through [`bn_core::specialists::scaffold`] — the same
/// engine as `bn agents new`, which wires up the `repo:` gate, the mined-standards `references:`,
/// the `auto-review-pr` skill and a reviewer persona. Hand-assembling that from a web form would
/// produce a specialist that parses and reviews nothing. Every other create (and any create that
/// supplies its own body) goes through [`bn_core::agents::write::create_agent`], which assembles
/// only the frontmatter the kind actually needs.
///
/// `400` on a bad name/kind/scope or a manifest its kind would reject; `409` when that tier already
/// defines the name and `overwrite` is false.
pub async fn agent_def_create(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "agent-def-create";
    let body: AgentDefCreateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = body.name.trim().to_string();
    if !bn_core::agents::write::valid_agent_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "agent name may contain letters, digits, '-', '_'",
        );
    }
    let Some(kind) = body.kind else {
        return bad_request(
            cmd,
            "missing_kind",
            "kind must be command | staff | hosted | specialist",
        );
    };
    let description = body.description.trim().to_string();
    if description.is_empty() {
        return bad_request(cmd, "empty_description", "description is required");
    }
    let scope = body
        .scope
        .as_deref()
        .unwrap_or(DEFAULT_CREATE_SCOPE)
        .to_string();
    if !bn_core::agents::write::CREATE_SCOPES.contains(&scope.as_str()) {
        return bad_request(
            cmd,
            "invalid_scope",
            &format!(
                "scope must be {}",
                bn_core::agents::write::CREATE_SCOPES.join(" | ")
            ),
        );
    }

    let cfg = state.cfg.clone();
    let (name_call, overwrite) = (name.clone(), body.overwrite);
    let scaffold_specialist = kind == AgentKind::Specialist && body.body.trim().is_empty();
    let repo = body.repo.clone().unwrap_or_default();
    let spec = NewAgent {
        kind,
        description: description.clone(),
        gather: body.gather.clone(),
        sinks: body.sinks.clone(),
        repo: body.repo.clone(),
        body: body.body.clone(),
    };
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let path = if scaffold_specialist {
            if repo.trim().is_empty() {
                return Err("a specialist needs a repo: to review".to_string());
            }
            bn_core::specialists::scaffold(
                &cfg,
                &scope,
                &name_call,
                repo.trim(),
                Some(&description),
                None,
                overwrite,
            )?
        } else {
            bn_core::agents::write::create_agent(&cfg, &scope, &name_call, &spec, overwrite)?
        };
        // A new specialist has to reach the harnesses to exist as far as they are concerned; every
        // other kind is dispatched by bn itself and needs no materialization.
        let native = if spec.kind == AgentKind::Specialist {
            sync_native_agents_summary(&cfg)
        } else {
            serde_json::Value::Null
        };
        Ok(json!({
            "agent": {
                "name": name_call,
                "scope": scope,
                "path": path.display().to_string(),
                "kind": spec.kind,
                "specialist": spec.kind == AgentKind::Specialist,
            },
            "native": native,
        }))
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(msg)) => {
            audit::record(&state, cmd, &name, addr, false);
            // `create_agent` refuses to clobber unless `overwrite` — a conflict the UI resolves by
            // re-submitting, not a malformed request.
            if msg.contains("already exists") {
                (
                    StatusCode::CONFLICT,
                    Json(Envelope::failure(cmd, "agent_exists", msg)),
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

/// `PUT /api/agent-defs/:name {content}` — overwrite the effective agent's `.md` source.
///
/// The file edited is the one `/api/agent-defs/:name` served: resolved through the same discovery,
/// so a branch or repo tier shadowing a global default is edited where it actually lives. The
/// content is parsed as the agent it claims to be before the write, which rejects both a malformed
/// manifest and a frontmatter `name:` that renames the agent out from under its filename.
///
/// `404` when no tier defines `name`; `400` when the new content would not parse.
pub async fn agent_def_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "agent-def-edit";
    let body: AgentDefEditBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = name.trim().to_string();
    if !bn_core::agents::write::valid_agent_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "agent name may contain letters, digits, '-', '_'",
        );
    }
    if body.content.trim().is_empty() {
        return bad_request(cmd, "empty_content", "content is required");
    }

    let cfg = state.cfg.clone();
    let (name_call, content) = (name.clone(), body.content.clone());
    // `Err(None)` = no such agent (404); `Err(Some(msg))` = the write itself refused (400/500).
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let catalog = bn_core::agents::discover(&cfg, None).map_err(|e| Some(e.to_string()))?;
            let agent = catalog.find(&name_call).ok_or(None)?;
            let reference = agent_ref(agent);
            let was_specialist = agent.is_specialist();
            bn_core::agents::write::save_agent(&agent.path, &name_call, agent.scope, &content)
                .map_err(Some)?;
            // Re-synced when the agent is a specialist either side of the edit: turning one *into*
            // a specialist has to install it, and turning one *out of* specialist has to prune the
            // native file that would otherwise keep answering for it.
            let is_specialist = bn_core::agents::discover(&cfg, None)
                .ok()
                .and_then(|c| c.find(&name_call).map(|a| a.is_specialist()))
                .unwrap_or(was_specialist);
            let native = if was_specialist || is_specialist {
                sync_native_agents_summary(&cfg)
            } else {
                serde_json::Value::Null
            };
            Ok(json!({ "agent": reference, "native": native }))
        })
        .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(None)) => {
            audit::record(&state, cmd, &name, addr, false);
            not_found_json(cmd, &format!("no Markdown agent named '{name}'"))
        }
        Ok(Err(Some(msg))) => {
            audit::record(&state, cmd, &name, addr, false);
            // A rejected manifest is the caller's content, not a server fault — the editor shows
            // the parse error verbatim, which is the only way to fix it.
            if msg.contains("not a valid agent") || msg.contains("renames it") {
                bad_request(cmd, "invalid_content", &msg)
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(Envelope::failure(cmd, "write_failed", msg)),
                )
                    .into_response()
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

/// `DELETE /api/agent-defs/:name` — delete the effective definition of `name`.
///
/// Removes one definition, not the name: when a lower tier also defines it, that agent becomes the
/// effective one and keeps running. `remaining_scope` says which happened, so the UI can tell
/// "gone" from "unshadowed". A deleted specialist re-syncs the harnesses, which is what actually
/// removes the compiled native agent file.
pub async fn agent_def_delete(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
) -> Response {
    let cmd = "agent-def-delete";
    let name = name.trim().to_string();
    if !bn_core::agents::write::valid_agent_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "agent name may contain letters, digits, '-', '_'",
        );
    }

    let cfg = state.cfg.clone();
    let name_call = name.clone();
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let catalog = bn_core::agents::discover(&cfg, None).map_err(|e| Some(e.to_string()))?;
            let agent = catalog.find(&name_call).ok_or(None)?;
            let reference = agent_ref(agent);
            let was_specialist = agent.is_specialist();
            bn_core::agents::write::delete_agent(&agent.path).map_err(Some)?;
            // Re-discover: a lower tier may have been shadowed by the file just removed.
            let remaining = bn_core::agents::discover(&cfg, None)
                .ok()
                .and_then(|c| c.find(&name_call).map(|a| a.scope.label().to_string()));
            let native = if was_specialist {
                sync_native_agents_summary(&cfg)
            } else {
                serde_json::Value::Null
            };
            Ok(json!({
                "agent": reference,
                "remaining_scope": remaining,
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
            not_found_json(cmd, &format!("no Markdown agent named '{name}'"))
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
