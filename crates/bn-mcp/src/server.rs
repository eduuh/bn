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
use bn_core::links;
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
struct LinksAddArgs {
    /// The http(s) URL to store. Anything else is rejected — a stored link is handed straight
    /// to a browser.
    url: String,
    /// What to call it in the picker. Omit and it becomes the host plus the last path segment.
    #[serde(default)]
    name: Option<String>,
    /// Tags to file it under, e.g. ["dri", "telemetry"]. Unlisted tags are kept, not rejected.
    #[serde(default)]
    tags: Vec<String>,
    /// Repository this link belongs to. Omit to use this worktree's repo; pass "" for none.
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RefArgs {
    /// Reference to read in full (fuzzy/prefix match, e.g. `build-and-run` or
    /// `architecture/runtime-concepts`). Omit to get the index instead: every reference's name
    /// and one-line summary, which is the cheap call — read a body only once you know you need it.
    #[serde(default)]
    name: Option<String>,
    /// Which store: `repo` (this repo's reference notes), `global` (my coding standards), or
    /// `all` (both). Defaults to `all`.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RefAppendArgs {
    /// Reference note to write to, in this repo's store. Created from a template if it doesn't
    /// exist yet.
    name: String,
    /// The fact, as one line. Include where it came from and when you checked it, e.g.
    /// `(src: web/.nvmrc, 2026-09-03)` or `(observed, 2026-09-03)`.
    text: String,
    /// Section heading to append under: `facts` (default), `gotchas`, or any other.
    #[serde(default)]
    section: Option<String>,
}

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
    fn skills(&self) -> Result<Vec<Skill>, ErrorData> {
        load_skill_set(&self.skill_dirs)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
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
        name = "bn_links_add",
        description = "Store a link worth going back to — a PR, a work item or PBI, an Azure portal blade, a dashboard, a docs page. Call this whenever a URL surfaces that the user will want again, without waiting to be asked: the store is person-scoped and cross-machine, so a link kept here outlives this branch and this worktree. Unlike every other tool on this server, it does NOT fail closed when no branch resolves — it stores the link without a repo instead, because losing a capture over the client's working directory is the wrong trade. Re-adding a URL already stored updates it in place. Nothing is fetched over the network."
    )]
    async fn bn_links_add(
        &self,
        Parameters(a): Parameters<LinksAddArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Unlike every other tool here, the store is person-scoped, so a call that cannot
        // resolve a branch still succeeds — it just stores the link without a repo. Refusing to
        // keep a link because of where the client happens to be standing would be the wrong
        // trade for a tool whose whole point is that capture is free.
        let repo = match a.repo.as_deref() {
            Some("") => None,
            Some(r) => Some(r.to_string()),
            None => self.resolve(&ctx).ok().map(|c| c.repo),
        };
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            links::add(
                &self.cfg,
                &a.url,
                a.name.as_deref(),
                &a.tags,
                repo.as_deref(),
                &today(),
            )
            .map_err(|e| ErrorData::invalid_params(e.message(), None))?
        };
        Ok(CallToolResult::structured(serde_json::json!({
            "id": out.id,
            "url": out.url,
            "name": out.name,
            "tags": out.tags,
            "repo": out.repo,
        })))
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

    #[tool(
        name = "bn_ref",
        description = "Reference docs: this repo's reference notes (what past sessions discovered about it -- how it builds, where the seams are, which failures mislead) and my global coding standards. Call with no name FIRST to get the index -- every note's name and one-line summary, a cheap call -- then call again with a name to read the one document that bears on your task. Prefer this over re-deriving repo knowledge from the code."
    )]
    async fn bn_ref(
        &self,
        Parameters(a): Parameters<RefArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        use bn_core::references::{self, Scope as RefScope};
        let ctx = self.resolve(&ctx)?;
        let want = a.scope.as_deref().unwrap_or("all");
        let scopes: Vec<RefScope> = match want {
            "repo" => vec![RefScope::repo(&ctx.repo)],
            "global" => vec![RefScope::Global],
            "all" => vec![RefScope::repo(&ctx.repo), RefScope::Global],
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown scope '{other}' -- use repo, global, or all"),
                    None,
                ));
            }
        };

        // With a name: the first scope that has a match wins, so a repo note shadows a global
        // standard of the same name -- the more specific knowledge is the one you asked for.
        if let Some(name) = a.name.as_deref() {
            for scope in &scopes {
                if let Some((r, body)) = references::read_in(scope, name) {
                    return Ok(CallToolResult::structured(json!({
                        "scope": scope.label(),
                        "name": r.name,
                        "summary": r.summary,
                        "path": r.path.display().to_string(),
                        "body": body,
                    })));
                }
            }
            return Err(ErrorData::invalid_params(
                format!(
                    "no reference matches '{name}' -- call bn_ref with no name to see what exists"
                ),
                None,
            ));
        }

        // No name: the index. Names + summaries only, so knowing what exists stays cheap.
        let mut references_out = Vec::new();
        for scope in &scopes {
            for r in references::list_in(scope) {
                references_out.push(json!({
                    "scope": scope.label(),
                    "name": r.name,
                    "summary": r.summary,
                }));
            }
        }
        Ok(CallToolResult::structured(json!({
            "repo": ctx.repo,
            "count": references_out.len(),
            "references": references_out,
        })))
    }

    #[tool(
        name = "bn_ref_append",
        description = "Record ONE durable fact in this repo's reference notes, as a bullet under a section (facts by default, or gotchas). Use it the moment you learn something that will still be true next month and would save the next session time -- a root cause, a build gotcha, where a subsystem actually lives. Creates the note if it doesn't exist. Include where the fact came from and today's date in the text. Returns appended:false when that exact fact is already recorded. Branch-specific state belongs in bn_add, not here."
    )]
    async fn bn_ref_append(
        &self,
        Parameters(a): Parameters<RefAppendArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        use bn_core::references::{self, Scope as RefScope};
        let ctx = self.resolve(&ctx)?;
        if a.text.trim().is_empty() {
            return Err(ErrorData::invalid_params("nothing to record", None));
        }
        let scope = RefScope::repo(&ctx.repo);
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            references::append(&scope, &a.name, a.section.as_deref(), &a.text)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        };
        Ok(CallToolResult::structured(json!({
            "scope": scope.label(),
            "name": a.name,
            "heading": out.heading,
            "path": out.path.display().to_string(),
            "appended": out.appended,
        })))
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
             Either way, a call that resolves to no branch fails closed — with one deliberate \
             exception, bn_links_add, described below. Start a session with \
             bn_context (compact orientation: goal, counts, top todos, blockers, PR, next-step \
             hints), then use only the current-branch tools: bn_goal, bn_add, bn_done, bn_pr, \
             bn_cat, bn_status, bn_sync, bn_handoff_read, and bn_handoff_write. bn_links_add is \
             the one exception to branch scope: it stores a link in the person-scoped, \
             cross-machine link store, and succeeds even when no branch resolves (the link is \
             then stored without a repo). Call it whenever a URL surfaces that will be wanted \
             again — a PR, a work item or PBI, a portal blade, a dashboard, a docs page — \
             without waiting to be asked, and especially as a pull request completes, when those \
             links are about to leave with the worktree. \
             Two tools are repo-scoped rather than branch-scoped, because the knowledge \
             outlives the branch: bn_ref reads this repo's reference notes (call it with no \
             name for the cheap index before assuming anything about how the repo builds or \
             behaves) and bn_ref_append records one durable fact back into them. \
             Keeping those notes current is part of the work, not a chore at the end: the \
             moment you learn something about this repo that will still be true next month -- \
             a root cause, a build gotcha, where a subsystem actually lives -- append it with \
             its source and today's date, and CORRECT a note this session proved wrong rather \
             than appending a contradiction. A reference note nobody updates decays into \
             folklore; one nobody reads gets re-derived every session. \
             The MCP surface \
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
            .skills()?
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
        let skills = self.skills()?;
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
