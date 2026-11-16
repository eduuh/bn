use super::*;

/// Resolve an explicit or current-worktree pipeline target.
pub(super) fn resolve_target(
    ctx: Option<&Resolved>,
    repo: Option<&str>,
    branch: Option<&str>,
) -> Resolved {
    let repo = repo
        .map(str::to_string)
        .or_else(|| ctx.map(|current| current.repo.clone()));
    let branch = branch
        .map(bn_core::plan_review::sanitize_branch)
        .or_else(|| ctx.map(|current| current.branch.clone()));
    match (repo, branch) {
        (Some(repo), Some(branch)) => Resolved { repo, branch },
        _ => {
            fail("bn pipeline: no repo/branch — run inside a worktree, or pass --repo and --branch")
        }
    }
}

pub(super) fn current_stage(_cfg: &Config, ctx: &Resolved) -> (Stage, bool) {
    let stage = stage::current(&ctx.repo, &ctx.branch);
    (stage.unwrap_or(Stage::DEFAULT), stage.is_some())
}
