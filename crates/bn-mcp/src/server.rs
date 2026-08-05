//! The MCP server: current-worktree branch-note tools plus the workflow prompts from every skill
//! tier. Deliberately narrower than the CLI: cross-branch, whole-person, and explicit repo/branch
//! targeting stay in `bn` so an MCP-connected agent cannot discover or act on unrelated sessions.
//!
//! The *tool* surface is what enforces that narrowness. Prompts are inert text — serving one only
//! puts a workflow's instructions in front of the agent, and every action it then takes still goes
//! through the same scoped tools. So the prompt list is not a security boundary and is no longer
//! filtered: an allowlist here only hid skills from discovery while `bn skills` listed them all.

use std::path::PathBuf;

use bn_core::commands::{self, CmdError};
use bn_core::payload;
use bn_core::resolver::{Config, Resolved};
use bn_core::rollups;
use bn_core::util::today;
use bn_core::{render, store};

use crate::Scope;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, GetPromptRequestParams, GetPromptResult, Implementation, ListPromptsResult,
    PaginatedRequestParams, PromptMessage, PromptMessageRole, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use bn_core::skills::{load_skill_set, Skill};

pub struct BnServer {
    cfg: Config,
    /// How each call decides which branch note to act on: a shared HTTP transport hands the
    /// server [`Scope::Request`] and re-derives a per-call `(repo, branch)` from the request's
    /// headers/query. Note tools resolve their context from this per call rather than from a
    /// baked directory.
    scope: Scope,
    /// Ordered skill source directories (low->high precedence) the MCP prompts are
    /// loaded from. Re-scanned on each prompt list/get so newly added skill files appear without
    /// restarting the server. The shared HTTP server seeds these from the global tiers
    /// ([`bn_core::skills::global_skill_dirs`]).
    skill_dirs: Vec<PathBuf>,
    /// Serializes note mutations. The engine is stateless (the CLI runs one command per
    /// process); this long-lived server can receive parallel tool calls, and each
    /// add/done is a read-modify-write on the note file, so they must not interleave.
    write_lock: std::sync::Mutex<()>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    /// Section: todo | blocker | decision | research | collab | ask | done-when.
    section: String,
    /// The item text.
    text: String,
    /// Idempotent add: skip if an existing item in the section contains this key.
    #[serde(default)]
    if_absent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DoneArgs {
    /// Todo id (e.g. `t3`) or a case-insensitive substring of its text.
    query: String,
    /// Partial progress percent (1-99): record how far along the item is instead of closing
    /// it. Omit (or pass 100) to mark it done.
    pct: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrArgs {
    /// The pull-request URL (or number) to record on the branch note.
    pr: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalArgs {
    /// The one-line Goal to set under the note's `## Goal` heading.
    goal: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HandoffWriteArgs {
    /// Narrative (layer 2): a few sentences on why the work looks the way it does.
    #[serde(default)]
    narrative: Option<String>,
    /// Decisions deferred to the next session.
    #[serde(default)]
    deferred: Vec<String>,
    /// Extra open questions beyond the note's `## To Ask`.
    #[serde(default)]
    questions: Vec<String>,
    /// Explicit priority queue, in order. Omit to default to the note's open todos.
    #[serde(default)]
    priority_queue: Vec<String>,
}

#[tool_router]
impl BnServer {
    pub fn new(cfg: Config, scope: Scope, skill_dirs: Vec<PathBuf>) -> Self {
        Self {
            cfg,
            scope,
            skill_dirs,
            write_lock: std::sync::Mutex::new(()),
        }
    }

    /// Every effective skill, freshly merged from `skill_dirs` (higher tiers override lower ones
    /// by name, so each prompt appears once). Re-read per call, so a newly authored skill shows up
    /// without restarting the server.
    fn skills(&self) -> Vec<Skill> {
        load_skill_set(&self.skill_dirs)
    }

    /// Resolve this call's branch-note context from its [`Scope`], failing closed when the scope
    /// resolves to no branch. A [`Scope::Cwd`] server (the per-worktree stdio launcher) derives its
    /// scope from the client's working directory; a [`Scope::Request`] server (the shared HTTP
    /// transport) derives a per-call [`Scope::Target`] from the request's headers/query.
    fn resolve(&self, ctx: &RequestContext<RoleServer>) -> Result<Resolved, ErrorData> {
        let scope = match &self.scope {
            Scope::Request => request_scope(ctx)?,
            other => other.clone(),
        };
        let hint = match &self.scope {
            Scope::Request => "supply X-Bn-Repo/X-Bn-Branch (or ?repo=&branch=)",
            _ => "run `bn mcp serve` from inside a git worktree",
        };
        scope
            .resolve(&self.cfg)
            .map_err(|_| ErrorData::invalid_params(format!("no branch scope -- {hint}"), None))
    }

    #[tool(
        name = "bn_add",
        description = "Add an item to this worktree's current branch note. Sections: todo, blocker, decision, research, collab, ask, done-when. MCP cannot target another repo or branch; use the bn CLI for cross-branch operations."
    )]
    async fn bn_add(
        &self,
        Parameters(a): Parameters<AddArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            commands::add(
                &self.cfg,
                &ctx,
                &a.section,
                &a.text,
                a.if_absent.as_deref(),
                &today(),
            )
            .map_err(cmd_err)?
        };
        Ok(CallToolResult::structured(payload::add_data(&out)))
    }

    #[tool(
        name = "bn_done",
        description = "Mark a todo done on this worktree's current branch note by id or text substring. Pass pct (1-99) to record partial progress instead of closing."
    )]
    async fn bn_done(
        &self,
        Parameters(a): Parameters<DoneArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            commands::done(&self.cfg, &ctx, &a.query, &today(), a.pct).map_err(cmd_err)?
        };
        Ok(CallToolResult::structured(payload::done_data(&out)))
    }

    #[tool(
        name = "bn_pr",
        description = "Record the pull-request URL (or number) on this worktree's current branch note."
    )]
    async fn bn_pr(
        &self,
        Parameters(a): Parameters<PrArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            commands::set_pr(&self.cfg, &ctx, &a.pr, &today()).map_err(cmd_err)?
        };
        Ok(CallToolResult::structured(payload::pr_data(&out)))
    }

    #[tool(
        name = "bn_goal",
        description = "Set the one-line Goal on this worktree's current branch note -- the first thing to record in a fresh worktree."
    )]
    async fn bn_goal(
        &self,
        Parameters(a): Parameters<GoalArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            commands::set_goal(&self.cfg, &ctx, &a.goal, &today()).map_err(cmd_err)?
        };
        Ok(CallToolResult::structured(payload::goal_data(&out)))
    }

    #[tool(
        name = "bn_cat",
        description = "Render this worktree's current branch note action view (open todos, blockers, decisions, ...)."
    )]
    async fn bn_cat(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let note = store::read(&self.cfg.note_file(&ctx))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let rendered = render::cat(&note);
        Ok(CallToolResult::structured(
            json!({ "rendered": rendered, "front": note.front, "body": note.body }),
        ))
    }

    #[tool(
        name = "bn_context",
        description = "Compact session-start context for this worktree's current branch in one call: goal, open todo/blocker counts, top open todos, blockers, PR/work item, handoff availability, and next-step hints. Call this first in a fresh session; use bn_cat for the full note and bn_handoff_read for the full orientation."
    )]
    async fn bn_context(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let note = store::read(&self.cfg.note_file(&ctx)).ok();
        let handoff_available = bn_core::handoff::read(&self.cfg, &ctx).is_some();
        Ok(CallToolResult::structured(payload::context_data(
            note.as_ref(),
            &ctx,
            handoff_available,
        )))
    }

    #[tool(
        name = "bn_status",
        description = "Status for this worktree's current branch: open-todo and blocker counts, work item, PR."
    )]
    async fn bn_status(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let status = rollups::status(&self.cfg, &ctx);
        Ok(CallToolResult::structured(payload::status_data(
            &status, &ctx,
        )))
    }

    #[tool(
        name = "bn_sync",
        description = "Reconcile this worktree's current branch note against git/PR ground truth and return proposals (read-only, never writes). MCP cannot target another repo or branch; use the bn CLI for cross-branch sync."
    )]
    async fn bn_sync(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let note = store::read(&self.cfg.note_file(&ctx))
            .map_err(|_| ErrorData::invalid_params("No note for this branch", None))?;
        let proposals = bn_core::sync::reconcile(
            &note,
            &ctx.repo,
            &ctx.branch,
            &bn_core::runner::RealRunner,
            &bn_core::clock::RealClock,
        );
        Ok(CallToolResult::structured(payload::sync_data(&proposals)))
    }

    #[tool(
        name = "bn_handoff_write",
        description = "Write a structured session resume packet beside this worktree's current branch note. It is scoped to this branch note and does not mine other agent sessions."
    )]
    async fn bn_handoff_write(
        &self,
        Parameters(a): Parameters<HandoffWriteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let input = bn_core::handoff::HandoffInput {
            narrative: a.narrative.filter(|s| !s.trim().is_empty()),
            deferred: a.deferred,
            questions: a.questions,
            priority_queue: a.priority_queue,
        };
        let handoff = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            bn_core::handoff::write(&self.cfg, &ctx, &today(), &input)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        };
        Ok(CallToolResult::structured(payload::handoff_data(&handoff)))
    }

    #[tool(
        name = "bn_handoff_read",
        description = "Read this worktree's current-branch session orientation. Falls back to composing one live from this branch note when none has been written."
    )]
    async fn bn_handoff_read(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve(&ctx)?;
        let handoff = bn_core::handoff::read(&self.cfg, &ctx).or_else(|| {
            store::read(&self.cfg.note_file(&ctx))
                .ok()
                .map(|note| bn_core::handoff::compose(&note, &today(), &Default::default()))
        });
        let handoff =
            handoff.ok_or_else(|| ErrorData::invalid_params("No note for this branch", None))?;
        Ok(CallToolResult::structured(payload::handoff_data(&handoff)))
    }
}

#[tool_handler]
impl ServerHandler for BnServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("bn-mcp", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Branch-notes MCP server, scoped to a single (repo, branch). The per-worktree stdio \
             launcher (`bn mcp serve`) derives its scope from the client's current working \
             directory, so no per-call headers are needed; the shared HTTP transport instead \
             derives it per request (X-Bn-Repo/X-Bn-Branch headers or ?repo=&branch= query). \
             Either way, a call that resolves to no branch fails closed. Start a session with \
             bn_context (compact orientation: goal, counts, top todos, blockers, PR, next-step \
             hints), then use only the current-branch tools: bn_goal, bn_add, bn_done, bn_pr, \
             bn_cat, bn_status, bn_sync, bn_handoff_read, and bn_handoff_write. The MCP surface \
             cannot target another repo/branch and does not expose cross-branch rollups, search, \
             daily goals, reviews, metrics, attention queues, cron status, or agent fleet/session \
             mining; use the `bn` CLI for those explicit whole-person workflows. Every effective \
             skill is served as a prompt (start-session, wrap-session, stow, axi, plan, debug, \
             execute, investigate, triage and the rest); list them to see what this worktree \
             offers. A prompt only supplies a workflow's instructions — carrying one out still \
             goes through the scoped tools above, so some skills (PR review, orchestration) \
             describe steps that need the `bn` CLI and cannot be completed over MCP alone."
                .to_string(),
        )
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let prompts = self
            .skills()
            .iter()
            .map(crate::prompts::to_prompt)
            .collect();
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let skills = self.skills();
        let skill = skills
            .iter()
            .find(|s| s.name == request.name)
            .ok_or_else(|| {
                ErrorData::invalid_params(format!("unknown prompt: {}", request.name), None)
            })?;
        let body = skill.render(request.arguments.as_ref());
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, body)])
                .with_description(skill.description.clone()),
        )
    }
}

/// Map a bn-core command error onto an MCP error: caller mistakes -> invalid params,
/// storage failures -> internal error. Uses the same stable codes as the CLI envelope.
fn cmd_err(e: CmdError) -> ErrorData {
    match e.code() {
        "io" => ErrorData::internal_error(e.message(), None),
        _ => ErrorData::invalid_params(e.message(), None),
    }
}

/// Derive a per-request [`Scope::Target`] from the incoming HTTP request, failing closed when the
/// scope is absent or blank. The shared HTTP transport (epic #637 P2) injects the request's
/// [`http::request::Parts`] into the tool-call [`RequestContext`]; the branch identity comes from
/// the `X-Bn-Repo`/`X-Bn-Branch` headers, falling back to the `?repo=&branch=` query. A missing
/// HTTP context, or a missing/blank repo or branch, is an MCP `invalid_params` error — never a
/// silent cross-branch action.
fn request_scope(ctx: &RequestContext<RoleServer>) -> Result<Scope, ErrorData> {
    let parts = ctx
        .extensions
        .get::<http::request::Parts>()
        .ok_or_else(|| {
            ErrorData::invalid_params(
                "bn MCP over HTTP requires a request scope; this call carried no HTTP context",
                None,
            )
        })?;

    let repo = scope_field(parts, "x-bn-repo", "repo");
    let branch = scope_field(parts, "x-bn-branch", "branch");

    match (repo, branch) {
        (Some(repo), Some(branch)) => Ok(Scope::target(repo, branch)),
        _ => Err(ErrorData::invalid_params(
            "bn MCP over HTTP requires a branch scope: set X-Bn-Repo and X-Bn-Branch headers \
             (or ?repo=&branch= query)",
            None,
        )),
    }
}

/// Read one scope field from an HTTP request: the `header` first (trimmed), else the `query` key
/// from the URI query string. Returns `None` for a missing or blank value so the caller fails
/// closed rather than acting on an empty scope.
fn scope_field(parts: &http::request::Parts, header: &str, query: &str) -> Option<String> {
    let from_header = parts
        .headers
        .get(header)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    from_header.or_else(|| query_value(parts.uri.query(), query))
}

/// Extract the first value for `key` from a `application/x-www-form-urlencoded` query string,
/// percent-decoding it. Returns `None` when the key is absent or its value is blank.
fn query_value(query: Option<&str>, key: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Minimal percent-decoder for query values (`%20`, `+` -> space). Keeps bn-mcp dependency-free of
/// a urlencoding crate; scope values are simple repo/branch strings.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
