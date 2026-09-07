//! The per-repo warm-pool writes: trigger a warm, and set the warm schedule.
//!
//! Split out of `actions.rs` rather than grown inside it. There is no "pool this repo at N slots"
//! handler any more, and no config behind one: every clone pools, and the size follows the slots
//! that exist (`bn_core::pool::derived_size`). The handlers end by dropping the cached
//! `/api/repos` view — the board reads pool shape from that view, so without the invalidation it
//! would keep rendering the shape from before the click, which reads as the click having failed.

use super::{bad_request, respond_process, run_bn, server_error, valid_repo, MaybeAddr, RunResult};
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
        repo.to_string(),
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
    // Every repo with a clone pools — there is no opt-in and no opt-out — so the only case warming
    // cannot serve is a repo bn has no clone of. Refusing up front beats enqueueing a job that
    // would later find nothing to warm.
    if !bn_core::lifecycle::cloned_repos(&state.cfg).contains(&repo.to_ascii_lowercase()) {
        return bad_request(cmd, "unknown_repo", "no bare clone for that repo here");
    }
    let result = run_bn(pool_warm_queue_args(&repo), None).await;
    // The board reads pool state from the cached `/api/repos` view; drop it so the next poll shows
    // the warm the user just asked for rather than the shape from before the click.
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(&state, "repo-pool-warm", &repo, addr, result)
}

#[derive(Debug, Deserialize)]
pub struct PoolScheduleBody {
    #[serde(default = "default_lunch_time")]
    pub lunch_time: String,
    #[serde(default = "default_after_work_time")]
    pub after_work_time: String,
    #[serde(default = "default_warm_duration_mins")]
    pub duration_mins: i64,
}

fn default_lunch_time() -> String {
    "12:30".to_string()
}

fn default_after_work_time() -> String {
    "17:30".to_string()
}

fn default_warm_duration_mins() -> i64 {
    90
}

/// `POST /api/repos/{repo}/pool/schedule` — configure this machine's best-effort lunch opportunity
/// and mandatory daily pool-warm deadline. The repo path keeps the control next to the warm-pool
/// UI, but the setting is global to this machine because one sweep refreshes every due pool.
pub async fn repo_pool_schedule_set(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    Json(body): Json<PoolScheduleBody>,
) -> Response {
    let cmd = "repo_pool_schedule_set";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let lunch = match warm_window(
        bn_core::queue::window::POOL_WARM_LUNCH,
        &body.lunch_time,
        body.duration_mins,
        false,
    ) {
        Ok(w) => w,
        Err(e) => return bad_request(cmd, "invalid_lunch_time", &e),
    };
    let after_work = match warm_window(
        bn_core::queue::window::POOL_WARM_AFTER_WORK,
        &body.after_work_time,
        body.duration_mins,
        true,
    ) {
        Ok(w) => w,
        Err(e) => return bad_request(cmd, "invalid_after_work_time", &e),
    };
    let windows = vec![lunch, after_work];
    let wrote = tokio::task::spawn_blocking({
        let windows = windows.clone();
        move || {
            bn_core::queue::window::write_pool_warm_windows(
                &bn_core::resolver::config_file_path(),
                &windows,
            )
        }
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = wrote {
        return server_error(
            cmd,
            "write_failed",
            &format!("could not write pool warm schedule: {e}"),
        );
    }
    state.views.invalidate(crate::view_cache::REPOS).await;
    respond_process(
        &state,
        "repo-pool-schedule-set",
        &repo,
        addr,
        RunResult {
            ok: true,
            envelope: json!({
                "ok": true,
                "command": cmd,
                "data": { "repo": repo, "warm_windows": windows },
                "error": null,
            }),
        },
    )
}

fn warm_window(
    name: &str,
    time: &str,
    duration_mins: i64,
    deadline: bool,
) -> Result<bn_core::queue::window::PoolWarmWindow, String> {
    let time = normalize_hhmm(time)?;
    let duration_mins = duration_mins.clamp(15, 240);
    Ok(bn_core::queue::window::PoolWarmWindow {
        name: name.to_string(),
        at: if deadline {
            format!("*-*-* {time}:00")
        } else {
            format!("Mon..Fri *-*-* {time}:00")
        },
        fore: format!("{duration_mins}m"),
        mode: if deadline {
            bn_core::queue::window::WindowMode::Deadline
        } else {
            bn_core::queue::window::WindowMode::Away
        },
    })
}

fn normalize_hhmm(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let (h, m) = raw
        .split_once(':')
        .ok_or_else(|| "expected HH:MM, like 12:30".to_string())?;
    let h: u32 = h
        .parse()
        .map_err(|_| "expected HH:MM, like 12:30".to_string())?;
    let m: u32 = m
        .parse()
        .map_err(|_| "expected HH:MM, like 12:30".to_string())?;
    if h > 23 || m > 59 {
        return Err("time must be within 00:00..23:59".to_string());
    }
    Ok(format!("{h:02}:{m:02}"))
}
