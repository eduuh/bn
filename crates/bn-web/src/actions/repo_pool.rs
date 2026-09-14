//! The per-repo warm-pool write: start a warm.
//!
//! Split out of `actions.rs` rather than grown inside it. There is no "pool this repo at N slots"
//! handler any more, and no config behind one: every clone pools, and the size follows the slots
//! that exist (`bn_core::pool::derived_size`). The handler ends by dropping the cached
//! `/api/repos` view — the board reads pool shape from that view, so without the invalidation it
//! would keep rendering the shape from before the click, which reads as the click having failed.

use super::{bad_request, respond_process, server_error, valid_repo, MaybeAddr, RunResult};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    response::Response,
};
use serde_json::json;

/// `POST /api/repos/{repo}/pool/warm` → start `bn run pool-warm --repo R --branch main` detached
/// (#497): pre-build every slot of a repo's warm worktree pool from the dashboard. Warming is
/// LONG-RUNNING (a checkout + the repo's `build.sh` per slot), so the request only starts it and
/// answers; the run records its own `cron_run` outcome, which `bn cron` shows.
pub async fn repo_pool_warm(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
) -> Response {
    let cmd = "repo_pool_warm";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    // Every repo with a clone pools — there is no opt-in and no opt-out — so the only case warming
    // cannot serve is a repo bn has no clone of. Refusing up front beats starting a job that would
    // only find nothing to warm.
    if !bn_core::lifecycle::cloned_repos(&state.cfg).contains(&repo.to_ascii_lowercase()) {
        return bad_request(cmd, "unknown_repo", "no bare clone for that repo here");
    }
    let spawned = bn_core::bn_subprocess::spawn_detached(&[
        "run",
        "pool-warm",
        "--repo",
        &repo,
        "--branch",
        "main",
    ]);
    let pid = match spawned {
        Ok(pid) => pid,
        Err(e) => {
            return server_error(
                cmd,
                "spawn_failed",
                &format!("could not start pool-warm: {e}"),
            )
        }
    };
    // The board reads pool state from the cached `/api/repos` view; drop it so the next poll shows
    // the warm the user just asked for rather than the shape from before the click.
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(
        &state,
        "repo-pool-warm",
        &repo,
        addr,
        RunResult {
            ok: true,
            envelope: json!({
                "ok": true,
                "command": cmd,
                "data": { "repo": repo, "job": "pool-warm", "pid": pid },
                "error": null,
            }),
        },
    )
}
