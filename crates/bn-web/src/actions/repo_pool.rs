//! The per-repo warm-pool writes: turn pooling on/off for a repo, and trigger a warm.
//!
//! Split out of `actions.rs` rather than grown inside it. Both handlers end by dropping the cached
//! `/api/repos` view — the board reads pool shape from that view, so without the invalidation the
//! slot the user just resized would keep rendering its old size until the TTL lapsed, which reads
//! as the click having failed.

use super::{bad_request, respond_process, run_bn, valid_repo, MaybeAddr, RunResult};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    response::{Json, Response},
};
use serde::Deserialize;
use serde_json::json;

fn pool_warm_queue_args(repo: &str) -> Vec<String> {
    vec![
        "queue".to_string(),
        "add".to_string(),
        format!("Warm pool for {repo}"),
        "--repo".to_string(),
        "machine".to_string(),
        "--kind".to_string(),
        "cron".to_string(),
        "--job".to_string(),
        "pool-warm".to_string(),
        "--job-repo".to_string(),
        repo.to_string(),
        "--job-branch".to_string(),
        "main".to_string(),
    ]
}

/// `POST /api/repos/{repo}/pool/warm` → enqueue `pool-warm` for this repo (#497): pre-build every
/// slot of a repo's warm worktree pool from the dashboard. Warming is LONG-RUNNING (a checkout +
/// the repo's `build.sh` per slot), so the request schedules it through the machine-wide queue
/// rather than spawning an uncapped detached build. The dispatcher admits the heavy job when the box
/// has capacity and records it in the queue timeline.
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
    // Only a repo actually in the pools config can be warmed (warming is what materializes its
    // slots), so reject an unpooled/unknown repo up front rather than enqueueing a job that will
    // later no-op. Read via the shared bn-core pool.
    if bn_core::pool::pool_size(&repo).is_none() {
        return bad_request(
            cmd,
            "not_pooled",
            "repo is not pooled — add it to the pools config first",
        );
    }
    let result = run_bn(pool_warm_queue_args(&repo), None).await;
    // The board reads pool state from the cached `/api/repos` view; drop it so the next poll shows
    // the warm the user just asked for rather than the shape from before the click.
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(&state, "repo-pool-warm", &repo, addr, result)
}

/// `POST /api/repos/{repo}/pool` body: the slot count to pool at, or `null`/omitted to stop pooling.
#[derive(Debug, Default, Deserialize)]
pub struct PoolSetBody {
    #[serde(default)]
    pub size: Option<usize>,
}

/// `POST /api/repos/{repo}/pool {size?}` — turn pooling on for a repo at `size` slots, or off when
/// `size` is absent. The counterpart to the warm trigger: warming could only ever act on a repo
/// someone had already hand-added to `~/.config/wt/pools`, so on a machine that had never edited
/// that file every card read "pooling off" with no way out of it from here.
///
/// Writes the config in-process through `bn_core::pool` (the same edit `bn wt pool set` performs)
/// rather than spawning the CLI — it's a small atomic file rewrite, not a long job, so the response
/// can report the real outcome instead of an ack. When pooling is enabled/resized, the handler also
/// schedules the repo's `pool-warm` heavy job so the queue builds the new slots under the machine's
/// normal capacity rules.
pub async fn repo_pool_set(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    Json(body): Json<PoolSetBody>,
) -> Response {
    let cmd = "repo_pool_set";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    // `default` and `*` are the config's reserved any-repo keys; writing one as a repo name would
    // silently re-point every unlisted repo's pool.
    if repo == "default" || repo == "*" {
        return bad_request(cmd, "reserved_repo", "that name is reserved by the config");
    }
    // Only a repo bn actually has a clone of — a typo here would become a dead config line that
    // nothing ever warms.
    let cloned = bn_core::lifecycle::cloned_repos(&state.cfg);
    if !cloned.contains(&repo.to_ascii_lowercase()) {
        return bad_request(cmd, "unknown_repo", "no bare clone for that repo here");
    }
    if let Some(n) = body.size {
        // Slot 1 is the controller's, so a size of 1 is a pool with nowhere to dispatch. Refuse it
        // rather than let the writer clamp it up — a caller that asked for 1 should hear that back.
        if !(bn_core::pool::MIN_POOL_SIZE..=bn_core::pool::MAX_POOL_SIZE).contains(&n) {
            return bad_request(
                cmd,
                "invalid_size",
                &format!(
                    "size must be {}..={} (slot 1 is the controller's)",
                    bn_core::pool::MIN_POOL_SIZE,
                    bn_core::pool::MAX_POOL_SIZE
                ),
            );
        }
    }
    let (repo_w, size) = (repo.clone(), body.size);
    let wrote = tokio::task::spawn_blocking(move || bn_core::pool::set_pool_size(&repo_w, size))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let result = match wrote {
        Ok(()) => {
            let queued = if size.is_some() {
                Some(run_bn(pool_warm_queue_args(&repo), None).await)
            } else {
                None
            };
            if let Some(result) = queued {
                if !result.ok {
                    return respond_process(&state, "repo-pool-set", &repo, addr, result);
                }
            }
            RunResult {
                ok: true,
                envelope: json!({
                    "ok": true,
                    "command": cmd,
                    "data": {
                        "repo": repo,
                        "size": size,
                        "message": match size {
                            Some(n) => format!("pooling {repo} at {n} slot(s); queued pool warm"),
                            None => format!("stopped pooling {repo}"),
                        },
                    },
                    "error": null,
                }),
            }
        }
        Err(e) => RunResult {
            ok: false,
            envelope: json!({
                "ok": false, "command": cmd, "data": null,
                "error": { "code": "write_failed", "message": format!("could not write the pools config: {e}") },
            }),
        },
    };
    // A resize changes exactly what the repo cards render, so the cached view must not outlive it.
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(&state, "repo-pool-set", &repo, addr, result)
}
