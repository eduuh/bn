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
use bn_core::resolver::{self, Config, Resolved};
use bn_core::rollups;
use bn_core::util::today;
use bn_core::{render, store};

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

mod context_tools;
mod proof_tools;
#[cfg(test)]
mod tests;
mod watchdog_tools;

pub struct BnServer {
    cfg: Config,
    /// Fixed launch directory. Resolve its current branch per call, never from request data.
    worktree: PathBuf,
    /// Ordered skill source directories (low->high precedence) the MCP prompts are
    /// loaded from. Re-scanned on each prompt list/get so newly added skill files appear without
    /// restarting the server.
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
struct LinksListArgs {
    /// Only links carrying this tag, e.g. "icm", "permissions", "docs", "security".
    #[serde(default)]
    tag: Option<String>,
    /// Only links belonging to this repo. Omit for every repo (this worktree's repo sorts first).
    #[serde(default)]
    repo: Option<String>,
    /// Case-insensitive substring of the name, tags or URL.
    #[serde(default)]
    query: Option<String>,
    /// At most this many links, best-ranked first. Default 20.
    #[serde(default)]
    limit: Option<usize>,
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
    /// Tags to file it under, e.g. ["icm", "security"]. Unlisted tags are kept, not rejected.
    /// Omit and tags are guessed from the host (ICM → icm, learn.microsoft.com → docs).
    #[serde(default)]
    tags: Vec<String>,
    /// Repository this link belongs to. Omit to use this worktree's repo; pass "" for none.
    #[serde(default)]
    repo: Option<String>,
    /// Hide the link from listings after this long: "7d", "2w", or a day count. For incident
    /// bridges and one-off meeting links. Omit for a link that stays.
    #[serde(default)]
    ttl: Option<String>,
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
    /// Read only this `## ` section of the named reference (e.g. `gotchas`), not the whole body.
    /// `name#section` works too.
    #[serde(default)]
    section: Option<String>,
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
    /// Drop the previous packet's author layers. By default any layer left empty here is carried
    /// forward from the last packet, so writing only a narrative keeps the deferred decisions.
    #[serde(default)]
    replace: bool,
}

#[tool_router]
impl BnServer {
    pub fn new(cfg: Config, worktree: PathBuf, skill_dirs: Vec<PathBuf>) -> Self {
        Self {
            cfg,
            worktree,
            skill_dirs,
            write_lock: std::sync::Mutex::new(()),
        }
    }

    /// Every tool this server serves: the note tools here plus the watchdog, proof and context
    /// tools, which live apart to keep this file under the size ratchet.
    fn all_tools() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::tool_router()
            + Self::watchdog_router()
            + Self::proof_router()
            + Self::context_router()
    }

    /// Every effective skill, freshly merged from `skill_dirs` (higher tiers override lower ones
    /// by name, so each prompt appears once). Re-read per call, so a newly authored skill shows up
    /// without restarting the server.
    fn skills(&self) -> Result<Vec<Skill>, ErrorData> {
        load_skill_set(&self.skill_dirs)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }

    /// Resolve the launch worktree's current branch, failing closed outside a git worktree.
    fn resolve(&self) -> Result<Resolved, ErrorData> {
        resolver::resolve(&self.cfg, &self.worktree).map_err(|_| {
            ErrorData::invalid_params(
                "no branch scope -- run `bn mcp serve` from inside a git worktree",
                None,
            )
        })
    }

    #[tool(
        name = "bn_add",
        description = "Add an item to this worktree's current branch note. Sections: todo, blocker, decision, research, collab, ask, done-when. MCP cannot target another repo or branch; use the bn CLI for cross-branch operations."
    )]
    async fn bn_add(
        &self,
        Parameters(a): Parameters<AddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        // Unlike every other tool here, the store is person-scoped, so a call that cannot
        // resolve a branch still succeeds — it just stores the link without a repo. Refusing to
        // keep a link because of where the client happens to be standing would be the wrong
        // trade for a tool whose whole point is that capture is free.
        // An explicit repo says nothing about a branch; an inherited one brings this
        // worktree's branch with it, so the link heads that branch's note.
        let (repo, branch) = match a.repo.as_deref() {
            Some("") => (None, None),
            Some(r) => (Some(r.to_string()), None),
            None => match self.resolve() {
                Ok(c) => (Some(c.repo), Some(c.branch)),
                Err(_) => (None, None),
            },
        };
        // Checked before anything is stored, so a bad ttl never leaves a link without one.
        let ttl_days = match a.ttl.as_deref() {
            Some(raw) => Some(links::parse_ttl(raw).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("ttl wants 7d, 2w or a day count, got '{raw}'"),
                    None,
                )
            })?),
            None => None,
        };
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            // Untagged: file it by host, the same guess the CLI makes.
            let tags = if a.tags.is_empty() {
                links::guess_tags(&a.url, &bn_core::resolver::link_tags())
            } else {
                a.tags.clone()
            };
            let link = links::add(
                &self.cfg,
                &a.url,
                a.name.as_deref(),
                &tags,
                repo.as_deref(),
                branch.as_deref(),
                &today(),
            )
            .map_err(|e| ErrorData::invalid_params(e.message(), None))?;
            match ttl_days {
                Some(days) => links::set_expiry(&self.cfg, &link.id, &today(), days)
                    .map_err(|e| ErrorData::invalid_params(e.message(), None))?,
                None => link,
            }
        };
        Ok(CallToolResult::structured(serde_json::json!({
            "id": out.id,
            "url": out.url,
            "name": out.name,
            "tags": out.tags,
            "repo": out.repo,
            "expires": out.expires,
        })))
    }

    #[tool(
        name = "bn_links_list",
        description = "Find links in the person-scoped link store, ranked the way the user's picker ranks them (this repo first, then by how often and how recently each was used). Filter by tag (icm, permissions, docs, security, ...), repo, or a substring of name/tags/URL. Each item carries a ready-to-paste `markdown` link. Read-only, and like bn_links_add it does not fail closed when no branch resolves. Use it before asking the user for a URL they have probably saved; for a paste into Teams, point them at `bn links copy <id>`, which copies a rich hyperlink."
    )]
    async fn bn_links_list(
        &self,
        Parameters(a): Parameters<LinksListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let here = self.resolve().ok().map(|c| c.repo);
        let items = links::read(&self.cfg);
        let tag = a.tag.map(|t| t.to_lowercase());
        let query = a.query.map(|q| q.to_lowercase());
        let out: Vec<_> = links::ranked(&items, here.as_deref(), &today())
            .into_iter()
            .filter(|l| tag.as_ref().is_none_or(|t| l.tags.contains(t)))
            .filter(|l| a.repo.as_ref().is_none_or(|r| l.repo.as_ref() == Some(r)))
            .filter(|l| {
                query.as_ref().is_none_or(|q| {
                    format!("{} {} {}", l.name, l.tags.join(" "), l.url)
                        .to_lowercase()
                        .contains(q)
                })
            })
            .take(a.limit.unwrap_or(20))
            .map(|l| {
                serde_json::json!({
                    "id": l.id,
                    "url": l.url,
                    "name": l.name,
                    "tags": l.tags,
                    "repo": l.repo,
                    "markdown": format!("[{}]({})", l.name.replace(']', "\\]"), l.url),
                })
            })
            .collect();
        Ok(CallToolResult::structured(
            serde_json::json!({ "items": out }),
        ))
    }

    #[tool(
        name = "bn_done",
        description = "Mark a todo done on this worktree's current branch note by id or text substring. Pass pct (1-99) to record partial progress instead of closing."
    )]
    async fn bn_done(
        &self,
        Parameters(a): Parameters<DoneArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let check = match a.pct {
            Some(p) if p < 100 => bn_core::proof::DoneCheck::Clear,
            _ => bn_core::proof::check_done(&self.cfg, &ctx, &a.query),
        };
        if let bn_core::proof::DoneCheck::Refuse(why) = &check {
            return Err(ErrorData::invalid_params(
                format!("not ticked: {why}"),
                None,
            ));
        }
        let out = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            commands::done(&self.cfg, &ctx, &a.query, &today(), a.pct).map_err(cmd_err)?
        };
        let mut data = payload::done_data(&out);
        if let bn_core::proof::DoneCheck::Warn(why) = check {
            data["unproven"] = serde_json::Value::String(why);
        }
        Ok(CallToolResult::structured(data))
    }

    #[tool(
        name = "bn_pr",
        description = "Record the pull-request URL (or number) on this worktree's current branch note."
    )]
    async fn bn_pr(&self, Parameters(a): Parameters<PrArgs>) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let note = store::read(&self.cfg.note_file(&ctx))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let saved = links::for_note(
            &self.cfg,
            &ctx.repo,
            &ctx.branch,
            &today(),
            links::NOTE_LINKS,
        );
        let rendered = format!("{}{}", render::cat(&note), render::links_footer(&saved));
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let note = store::read(&self.cfg.note_file(&ctx)).ok();
        let handoff_available = bn_core::handoff::read(&self.cfg, &ctx).is_some();
        let data = payload::context_data(note.as_ref(), &ctx, handoff_available);
        Ok(CallToolResult::structured(data))
    }

    #[tool(
        name = "bn_status",
        description = "Status for this worktree's current branch: open-todo and blocker counts, work item, PR."
    )]
    async fn bn_status(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let input = bn_core::handoff::HandoffInput {
            narrative: a.narrative.filter(|s| !s.trim().is_empty()),
            deferred: a.deferred,
            questions: a.questions,
            priority_queue: a.priority_queue,
            replace: a.replace,
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
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
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
    ) -> Result<CallToolResult, ErrorData> {
        use bn_core::references::{self, Scope as RefScope};
        let ctx = self.resolve()?;
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
        if let Some(query) = a.name.as_deref() {
            let (name, inline) = references::split_ref(query);
            let section = a.section.as_deref().or(inline);
            for scope in &scopes {
                if let Some((r, body)) = references::read_in(scope, name) {
                    let sections: Vec<_> = references::sections(&body)
                        .into_iter()
                        .map(
                            |(h, t)| json!({"heading": h, "tokens": bn_core::tokens::estimate(&t)}),
                        )
                        .collect();
                    let body = match section {
                        None => body,
                        Some(sec) => references::section_of(&body, sec).ok_or_else(|| {
                            ErrorData::invalid_params(
                                format!("{} has no section '{sec}' -- see `sections`", r.name),
                                None,
                            )
                        })?,
                    };
                    return Ok(CallToolResult::structured(json!({
                        "scope": scope.label(),
                        "name": r.name,
                        "section": section,
                        "summary": r.summary,
                        "path": r.path.display().to_string(),
                        "tokens": bn_core::tokens::estimate(&body),
                        "sections": sections,
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
                // What reading it would cost, so a caller can budget before it asks.
                let tokens = std::fs::read_to_string(&r.path)
                    .map(|b| bn_core::tokens::estimate(&b))
                    .unwrap_or(0);
                references_out.push(json!({
                    "scope": scope.label(),
                    "name": r.name,
                    "summary": r.summary,
                    "tokens": tokens,
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
    ) -> Result<CallToolResult, ErrorData> {
        use bn_core::references::{self, Scope as RefScope};
        let ctx = self.resolve()?;
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

#[tool_handler(router = Self::all_tools())]
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
             directory. A call that resolves to no branch fails closed — with one deliberate \
             exception, the link tools bn_links_add and bn_links_list, described below. Start a session with \
             bn_context (compact orientation: goal, counts, top todos, blockers, PR, next-step \
             hints), then use only the current-branch tools: bn_goal, bn_add, bn_done, bn_pr, \
             bn_cat, bn_status, bn_sync, bn_handoff_read, and bn_handoff_write. bn_links_add is \
             the one exception to branch scope: it stores a link in the person-scoped, \
             cross-machine link store, and succeeds even when no branch resolves (the link is \
             then stored without a repo). Call it whenever a URL surfaces that will be wanted \
             again — a PR, a work item or PBI, a portal blade, a dashboard, a docs page — \
             without waiting to be asked, and especially as a pull request completes, when those \
             links are about to leave with the worktree. bn_links_list finds stored links by \
             tag (icm, permissions, docs, security, ...), repo or substring — check it before \
             asking the user for a URL they have likely saved. \
             bn_watchdog_checkin schedules a durable \
             check-in to yourself. bn_proof_show reports done-when readiness from recorded \
             evidence; bn_proof_add attaches evidence, which counts only once someone else \
             accepts it. bn_context_profile composes a token-budgeted context profile for this \
             branch; bn_context_get serves one piece (a reference section, a skill, a file \
             section). \
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
