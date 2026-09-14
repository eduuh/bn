//! The dashboard's stops and restarts: one agent, or a repo-wide relaunch.
//!
//! Kept together and apart from the rest of `actions` because they only make sense read together —
//! what separates them is blast radius, and that is the whole reason each is its own route rather
//! than one route with a flag. A target that can only ever name a branch, and a path segment that
//! can only ever name a repo, cannot be widened into each other by a malformed input.
//!
//! Restart lives here rather than beside the launch actions because it is a stop that happens to be
//! followed by a start: it tears the same process trees down by the same rules, and reading it next
//! to the other stop paths keeps the two from drifting into different ideas of what "gone" means.
use super::{
    bad_request, normalize_harness, parse_body, respond_process, run_bn, valid_repo, valid_target,
    MaybeAddr,
};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Path, State},
    response::Response,
};
use serde::Deserialize;

/// `POST /api/agents/:target/kill` → `bn agent kill <target>` — reap one agent's process tree, close
/// its tmux window, release its lease and hand its warm pool slot back.
pub async fn agent_kill(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(target): Path<String>,
) -> Response {
    let cmd = "agent_kill";
    if !valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    let args = vec!["agent".to_string(), "kill".to_string(), target.clone()];
    let result = run_bn(args, None).await;
    respond_process(&state, "kill", &target, addr, result)
}

/// `POST /api/repos/:repo/relaunch-session {hard?, harness?}` — restart every agent in the repo.
#[derive(Debug, Default, Deserialize)]
pub struct RepoRelaunchBody {
    /// Start FRESH sessions seeded from each note's handoff instead of resuming the transcripts.
    #[serde(default)]
    pub hard: bool,
    /// Restart every agent under a different harness. Implies `hard`.
    #[serde(default)]
    pub harness: Option<String>,
}

/// `POST /api/repos/:repo/relaunch-session` → `bn agent relaunch-repo <repo>`: stop every agent in
/// the repo and start it again in the worktree it was already in.
///
/// The button most reaches for actually wants a repair, not a wipe. A soft relaunch resumes each
/// agent's recorded transcript, so it keeps their context; `hard` is the escape hatch for when
/// that context is the problem.
pub async fn repo_session_relaunch(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "repo_session_relaunch";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let body: RepoRelaunchBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    // Availability-gate an explicit harness here so an org-disabled one is a `400` the client can
    // show, rather than N subprocess round trips that each fail the same way. The CLI gates it
    // again per agent — this is the fast path, not the enforcement.
    let harness = match body
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(h) => match normalize_harness(Some(h)) {
            Some(c) => Some(c),
            None => {
                return bad_request(cmd, "invalid_harness", "harness must be claude or copilot")
            }
        },
        None => None,
    };

    let mut args = vec![
        "agent".to_string(),
        "relaunch-repo".to_string(),
        repo.clone(),
    ];
    if body.hard {
        args.push("--hard".to_string());
    }
    if let Some(h) = harness {
        args.extend(["--harness".to_string(), h.to_string()]);
    }
    let result = run_bn(args, None).await;
    // Session ids and health change for every agent in the repo, and the board reads both from the
    // cached `/api/repos` view.
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(&state, "relaunch-session", &repo, addr, result)
}
