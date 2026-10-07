//! Context-profile tools on the scoped server: compose a profile for this branch, or serve one
//! piece of it. Named apart from `bn_context` (the session-start orientation).

use super::BnServer;
use bn_core::context::{self, Atom, AtomInputs};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfileArgs {
    /// Profile name: ship, scout, compact-restore, orient, or one from `.bn/context.toml`.
    name: String,
    /// Override the profile's token budget.
    #[serde(default)]
    budget: Option<usize>,
    /// Drop atoms from the end to fit the budget.
    #[serde(default)]
    fit: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AckArgs {
    /// The `digest` a bn_context_profile / bn_context_get call returned.
    digest: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct GetArgs {
    /// `atom:<name>`, `ref:<name>[#section]`, `skill:<name>` or `file:<path>[#section]`.
    what: String,
}

fn bad(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(e.to_string(), None)
}

#[tool_router(router = context_router, vis = "pub(super)")]
impl BnServer {
    #[tool(
        name = "bn_context_profile",
        description = "Compose a context profile for this branch — directives, goal, done-when readiness, recap, handoff, reference sections, skills, file sections, queue rows, as the profile declares — and return the text with each atom's token cost and the total against the budget. Built-in: ship, scout, compact-restore, orient; a repo can declare more in .bn/context.toml. Receiving it is not the same as having read it."
    )]
    async fn bn_context_profile(
        &self,
        Parameters(a): Parameters<ProfileArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let root = context::root_for(&self.cfg, &ctx);
        let m = context::load(root.as_deref()).map_err(bad)?;
        let inputs = AtomInputs {
            cfg: &self.cfg,
            ctx: &ctx,
            root: root.as_deref(),
            global_refs: &[],
        };
        let c = context::compose(&m, &a.name, &inputs, a.budget, a.fit).map_err(bad)?;
        let text = c.text();
        let atoms: Vec<String> = c.pieces.iter().map(|p| p.atom.clone()).collect();
        let digest = served(&self.cfg, &ctx, &a.name, &atoms, &text);
        Ok(CallToolResult::structured(
            json!({"costs": c, "digest": digest, "text": text}),
        ))
    }

    #[tool(
        name = "bn_context_get",
        description = "Serve one piece of context for this branch: `atom:<name>`, `ref:<name>#<section>` (one reference section instead of the whole note), `skill:<name>`, or `file:<path>#<section>`. Returns the text and its token cost."
    )]
    async fn bn_context_get(
        &self,
        Parameters(a): Parameters<GetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let root = context::root_for(&self.cfg, &ctx);
        let m = context::load(root.as_deref()).map_err(bad)?;
        let (kind, rest) = a
            .what
            .split_once(':')
            .ok_or_else(|| bad("say what to get: atom:, ref:, skill: or file:"))?;
        let atom = match kind {
            "atom" => m
                .atom(rest)
                .ok_or_else(|| bad(format!("no atom '{rest}'")))?,
            "ref" => Atom::Ref {
                r: rest.to_string(),
            },
            "skill" => Atom::Skill {
                name: rest.to_string(),
            },
            "file" => Atom::File {
                path: rest.to_string(),
            },
            other => return Err(bad(format!("unknown kind '{other}'"))),
        };
        let inputs = AtomInputs {
            cfg: &self.cfg,
            ctx: &ctx,
            root: root.as_deref(),
            global_refs: &[],
        };
        let text = context::render_atom(&atom, &inputs)
            .map_err(bad)?
            .unwrap_or_default();
        let digest = served(&self.cfg, &ctx, &a.what, &[], &text);
        Ok(CallToolResult::structured(json!({
            "what": a.what, "digest": digest,
            "tokens": bn_core::tokens::estimate(&text), "text": text,
        })))
    }

    #[tool(
        name = "bn_context_ack",
        description = "Acknowledge that you read served context, by the digest it came with. Serving context does not prove it was read; this records that it was."
    )]
    async fn bn_context_ack(
        &self,
        Parameters(a): Parameters<AckArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let by = format!("{}:{}", ctx.repo, ctx.branch);
        context::served::ack(&self.cfg, &ctx, &a.digest, &by).map_err(bad)?;
        Ok(CallToolResult::structured(
            json!({"digest": a.digest, "by": by}),
        ))
    }
}

fn served(
    cfg: &bn_core::resolver::Config,
    ctx: &bn_core::resolver::Resolved,
    what: &str,
    atoms: &[String],
    text: &str,
) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    context::served::record(cfg, ctx, what, atoms, text, "mcp").ok()
}
