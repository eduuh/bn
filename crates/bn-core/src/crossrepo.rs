//! Pure cross-repo orchestration policy (#455).
//!
//! Two multi-repo gaps close here, and both split the same way as the rest of the orchestrator:
//! the *policy* — which repo tmux sessions a supervisor sweeps, and the order a one-shot cross-repo
//! landing rolls through its repos — lives here as pure string/collection logic, while the CLI does
//! the tmux / git / filesystem IO. That keeps the decisions unit-testable without a live fleet.
//!
//! `bn dispatch` opens each agent in *the repo's* tmux session as `<repo>:<sanitized-branch>`
//! (session-per-repo), so a supervisor pinned to one session never sees another repo's agents
//! (`repos_with_live_agents` is the fix). And `bn wt merge --all` keys off the cwd's repo, so
//! landing across repos means N invocations ([`cross_repo_land_order`] gives the rollup its order).

/// The deterministic order a one-shot cross-repo landing (`bn wt merge --all-repos`) rolls through
/// its repos (#455): trimmed, non-empty, sorted, deduped. A stable order makes the rollup
/// reproducible and lets each repo be gated + landed independently regardless of the filesystem's
/// directory-enumeration order.
pub fn cross_repo_land_order(repos: &[String]) -> Vec<String> {
    let mut out: Vec<String> = repos
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}
