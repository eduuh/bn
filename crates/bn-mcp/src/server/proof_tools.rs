//! Proof tools on the scoped server. An agent can read readiness and attach evidence for its own
//! branch; there is deliberately no judge tool — the branch's own seat is the author of its
//! proof, and nobody judges their own.

use super::BnServer;
use bn_core::proof::{self, Kind, NewProof};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProofShowArgs {
    /// A proof id ("p3") for that proof in full; omit for readiness per done-when criterion.
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProofAddArgs {
    /// verify | gate | test | screenshot | log | review | manual.
    kind: String,
    /// The done-when criteria ("w1", …) this is evidence for.
    #[serde(default, rename = "for")]
    for_: Vec<String>,
    /// One line: what was checked.
    title: String,
    /// What it shows, in words. Paste the decisive output lines, not the whole log.
    #[serde(default)]
    text: String,
    /// The command that produced it.
    #[serde(default)]
    cmd: Option<String>,
    /// That command's exit code. Non-zero evidence never proves a criterion done.
    #[serde(default)]
    exit: Option<i32>,
}

#[tool_router(router = proof_router, vis = "pub(super)")]
impl BnServer {
    #[tool(
        name = "bn_proof_show",
        description = "Readiness of this branch's done-when criteria, derived from recorded evidence and verdicts at the current HEAD: unproven, attached (awaiting a verdict), accepted, stale (accepted at an older commit), or rejected. Pass id=\"pN\" for one proof in full. Read-only. A clean readiness says what was accepted, not that the work is good."
    )]
    async fn bn_proof_show(
        &self,
        Parameters(a): Parameters<ProofShowArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        if let Some(id) = a.id.as_deref() {
            let listing = proof::list(&self.cfg, &ctx);
            let p = listing
                .get(id)
                .ok_or_else(|| ErrorData::invalid_params(format!("no proof {id}"), None))?;
            let (verdicts, _) = proof::verdicts(&self.cfg, &ctx);
            let mine: Vec<_> = verdicts.iter().filter(|v| v.proof == p.id).collect();
            return Ok(CallToolResult::structured(
                json!({"proof": p, "body": p.body, "verdicts": mine}),
            ));
        }
        let (r, head) = proof::readiness_now(&self.cfg, &ctx, None);
        Ok(CallToolResult::structured(
            json!({"head": head.sha, "readiness": r}),
        ))
    }

    #[tool(
        name = "bn_proof_add",
        description = "Attach evidence for this branch's done-when criteria, taken at the current HEAD and authored by this branch. Attaching is not acceptance: it counts once someone else judges it (`bn proof judge`). Prefer `bn verify --for w1`, which keeps the full run log as proof automatically."
    )]
    async fn bn_proof_add(
        &self,
        Parameters(a): Parameters<ProofAddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = self.resolve()?;
        let kind = Kind::parse(&a.kind)
            .ok_or_else(|| ErrorData::invalid_params(format!("unknown kind '{}'", a.kind), None))?;
        let head = proof::head(&self.cfg, &ctx);
        let new = NewProof {
            kind,
            for_: a.for_,
            title: a.title,
            body: a.text,
            file: None,
            attach_bytes: None,
            truncated: false,
            sha: head.sha,
            dirty: head.dirty,
            // Normalized like every other identity, so the author-can't-judge check sees the
            // same seat whether the judge comes through the CLI or here.
            author: bn_core::coord::normalize_seat(&format!("{}:{}", ctx.repo, ctx.branch))
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
            source: "bn_proof_add".into(),
            cmd: a.cmd,
            exit: a.exit,
        };
        let p = {
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            proof::add(&self.cfg, &ctx, &new, &bn_core::util::now_rfc3339_utc())
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
        };
        Ok(CallToolResult::structured(json!(p)))
    }
}
