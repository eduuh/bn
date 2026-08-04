//! Pure cross-repo orchestration policy (#455).
//!
//! Two multi-repo gaps close here, and both split the same way as the rest of the orchestrator:
//! the *policy* — which repo tmux sessions a supervisor sweeps, and the order a one-shot cross-repo
//! landing rolls through its repos — lives here as pure string/collection logic, while the CLI does
//! the tmux / git / filesystem IO. That keeps the decisions unit-testable without a live fleet.
//!
//! `bn dispatch` opens each agent in *the repo's* tmux session as `<repo>:<sanitized-branch>`
//! (session-per-repo), so a supervisor pinned to one session never sees another repo's agents
//! ([`repos_with_live_agents`] is the fix). And `bn wt merge --all` keys off the cwd's repo, so
//! landing across repos means N invocations ([`cross_repo_land_order`] gives the rollup its order).

/// One tmux window as `bn dispatch` names it — the sanitized-branch suffix of a `<repo>:<window>`
/// entry — is an **agent lane** unless it's the repo's own `main`/`master` checkout, a warm
/// `wt pool` slot (`_pool*`), or empty. Pure predicate so both the per-repo and cross-repo lane
/// discovery agree on what counts as a supervisable lane.
fn is_agent_lane_window(window: &str) -> bool {
    !window.is_empty()
        && !window.eq_ignore_ascii_case("main")
        && !window.eq_ignore_ascii_case("master")
        && !window.starts_with("_pool")
}

/// The sanitized-branch **agent lane windows** of `repo`'s tmux session, given the live
/// `<session>:<window>` list (as `tmux list-windows -a -F '#{session_name}:#{window_name}'`
/// yields). Keeps only windows in this repo's session (`<repo>:…`), drops its `main`/`master`
/// checkout, warm `_pool` slots, and empty names, and dedupes in first-seen order.
///
/// This is the session half of supervisor lane discovery, generalized across repos: the single-repo
/// supervisor (#426) filtered to one session; the cross-repo sweep (#455) calls this per repo to
/// group every session's lanes. The window name is the sanitized branch (`bn dispatch`'s naming),
/// which the CLI then resolves back to the real git branch on disk.
pub fn lane_windows_in_session(repo: &str, windows: &[String]) -> Vec<String> {
    let prefix = format!("{repo}:");
    let mut out: Vec<String> = Vec::new();
    for w in windows {
        let Some(window) = w.strip_prefix(&prefix) else {
            continue;
        };
        if is_agent_lane_window(window) && !out.iter().any(|e| e == window) {
            out.push(window.to_string());
        }
    }
    out
}

/// The repos a cross-repo supervisor should sweep this pass (#455): every `known_repo` with at
/// least one live agent lane window in its tmux session ([`lane_windows_in_session`]), in
/// deterministic sorted order. Bounding discovery to `known_repos` (bn's configured clones) means a
/// stray session for a repo bn doesn't manage is ignored, so the sweep only ever adopts lanes for
/// repos it can actually supervise + land.
pub fn repos_with_live_agents(windows: &[String], known_repos: &[String]) -> Vec<String> {
    let mut out: Vec<String> = known_repos
        .iter()
        .filter(|r| !lane_windows_in_session(r, windows).is_empty())
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

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


