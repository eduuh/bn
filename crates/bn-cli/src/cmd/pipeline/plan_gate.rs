use crate::cmd::dispatch::{plan_gate_seed, PlanGateContext};
use crate::*;
use std::path::Path;

pub(crate) fn append_plan_gate_seed(
    cfg: &Config,
    ctx: &Resolved,
    wt_path: &Path,
    sid: &str,
    plan_path: &str,
    seed: &mut String,
) {
    let policy = bn_core::pipeline::execution::policy(&ctx.repo, &ctx.branch);
    if !policy.plan_review {
        return;
    }
    let gate_seed = plan_gate_seed(
        cfg,
        wt_path,
        PlanGateContext {
            repo: &ctx.repo,
            branch: &ctx.branch,
            sid,
            verify: policy.verify,
            reviewers: cfg.dispatch_plan_reviewers,
            quorum: cfg.dispatch_plan_review_quorum,
            plan_file: Some(plan_path),
        },
    )
    .unwrap_or_else(|error| fail(&format!("bn pipeline plan: {error}")));
    seed.push_str(&gate_seed);
}
