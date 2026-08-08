use super::{
    audit, bad_request, conflict, normalize_harness, parse_body, respond_process, run_bn_env,
    server_error, valid_repo, MaybeAddr,
};
use crate::bn_runner::RunResult;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Debug, Default, Deserialize)]
pub struct RepoSetupAgentBody {
    /// Launch harness — `copilot` (default) or `claude`.
    #[serde(default)]
    pub harness: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoOrchestrateBody {
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
}

fn valid_script_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn live_setup_agent_repos(cfg: &bn_core::resolver::Config) -> BTreeSet<String> {
    bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    )
    .agents
    .iter()
    .filter(|a| {
        a.task_name
            .as_deref()
            .is_some_and(|name| name == "repo-setup")
    })
    .map(|a| a.repo.to_ascii_lowercase())
    .collect()
}

async fn launch_repo_setup_agent(
    cfg: &bn_core::resolver::Config,
    repo: &str,
    harness: &str,
) -> RunResult {
    let goal = match super::prompts::repo_setup_goal(cfg, repo) {
        Ok(goal) => goal,
        Err(error) => {
            return RunResult {
                ok: false,
                envelope: json!({
                    "ok": false,
                    "command": "repo-setup-agent",
                    "data": null,
                    "error": { "code": "prompt_error", "message": error }
                }),
            };
        }
    };
    let args = vec![
        "dispatch".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--branch".to_string(),
        "repo-setup".to_string(),
        "--name".to_string(),
        "repo-setup".to_string(),
        "--agent".to_string(),
        harness.to_string(),
        "--yolo".to_string(),
        "--".to_string(),
        goal,
    ];
    run_bn_env(
        args,
        None,
        vec![("BN_WORKER_REPO".to_string(), repo.to_string())],
    )
    .await
}

/// `POST /api/repos/{repo}/orchestrate {goal?, harness?}` (#556) — start a guarded main-branch
/// orchestrator agent for one repo.
pub async fn repo_orchestrate_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "repo-orchestrate";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let body: RepoOrchestrateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(harness) = normalize_harness(body.harness.as_deref()) else {
        return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
    };

    let cfg = state.cfg.clone();
    let repo_key = repo.to_ascii_lowercase();
    let already = tokio::task::spawn_blocking(move || {
        let fleet = bn_core::agent::supervise(
            &cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        );
        fleet.agents.iter().any(|a| {
            a.repo.eq_ignore_ascii_case(&repo_key)
                && a.role == bn_core::agent::AgentRole::Orchestrator
        })
    })
    .await
    .unwrap_or(false);
    if already {
        return conflict(
            cmd,
            "already_running",
            &format!("an orchestrator is already running for '{repo}'"),
        );
    }

    let goal = match body
        .goal
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(goal) => goal,
        None => match super::prompts::repo_orchestrator_goal(&state.cfg, &repo) {
            Ok(goal) => goal,
            Err(error) => return server_error(cmd, "prompt_error", &error),
        },
    };

    let args = vec![
        "dispatch".to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--orchestrator".to_string(),
        "--agent".to_string(),
        harness.to_string(),
        "--yolo".to_string(),
        "--".to_string(),
        goal,
    ];
    let result = run_bn_env(
        args,
        None,
        vec![("BN_WORKER_REPO".to_string(), repo.clone())],
    )
    .await;
    respond_process(&state, "repo-orchestrate", &repo, addr, result)
}

/// `POST /api/repos/{repo}/setup-agent {harness?}` — start a normal repo-scoped, Fleet-visible
/// setup agent through `bn dispatch`, so fleet, transcript, approvals, steering, relaunch, and
/// starting-state machinery all apply.
pub async fn repo_setup_agent_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "repo-setup-agent";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    if !bn_core::lifecycle::cloned_repos(&state.cfg).contains(&repo.to_ascii_lowercase()) {
        return bad_request(cmd, "unknown_repo", "no bare clone for that repo here");
    }
    let body: RepoSetupAgentBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(harness) = normalize_harness(body.harness.as_deref()) else {
        return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
    };

    let cfg = state.cfg.clone();
    let repo_key = repo.to_ascii_lowercase();
    let already =
        tokio::task::spawn_blocking(move || live_setup_agent_repos(&cfg).contains(&repo_key))
            .await
            .unwrap_or(false);
    if already {
        return conflict(
            cmd,
            "already_running",
            &format!("a repo setup agent is already running for '{repo}'"),
        );
    }

    let result = launch_repo_setup_agent(&state.cfg, &repo, harness).await;
    respond_process(&state, "repo-setup-agent", &repo, addr, result)
}

/// `POST /api/repos/setup-agents {harness?}` — scan local repos, detect the ones whose setup rollup
/// says they need setup, and start one normal Fleet-visible setup agent for each.
pub async fn repo_setup_agents_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "repo-setup-agents";
    let body: RepoSetupAgentBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(harness) = normalize_harness(body.harness.as_deref()) else {
        return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
    };

    let cfg = state.cfg.clone();
    let (repos, live_setup) = tokio::task::spawn_blocking(move || {
        let mut repos: Vec<String> = bn_core::lifecycle::cloned_repos(&cfg)
            .into_iter()
            .filter(|repo| cfg.is_visible_repo(repo))
            .filter(|repo| {
                crate::repos::setup_json(&cfg, repo)
                    .get("needs_setup")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
            })
            .collect();
        repos.sort();
        repos.dedup();
        (repos, live_setup_agent_repos(&cfg))
    })
    .await
    .unwrap_or_default();

    let mut started = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    for repo in repos {
        if live_setup.contains(&repo.to_ascii_lowercase()) {
            skipped.push(json!({ "repo": repo, "reason": "already-running" }));
            continue;
        }
        let result = launch_repo_setup_agent(&state.cfg, &repo, harness).await;
        if result.ok {
            started.push(repo);
        } else {
            failed.push(json!({ "repo": repo, "envelope": result.envelope }));
        }
    }
    let ok = failed.is_empty();
    audit::record(&state, "repo-setup-agents", "needed", addr, ok);
    Json(Envelope::success(
        cmd,
        json!({ "started": started, "skipped": skipped, "failed": failed }),
    ))
    .into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoScriptWriteBody {
    #[serde(default)]
    pub content: String,
}

/// `PUT /api/repos/{repo}/scripts/{name} {content}` — write the repo-main tier job script.
pub async fn repo_script_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path((repo, name)): Path<(String, String)>,
    raw: Bytes,
) -> Response {
    let cmd = "repo-script-write";
    let repo = repo.trim().to_ascii_lowercase();
    let name = name.trim().to_ascii_lowercase();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    if !valid_script_name(&name) {
        return bad_request(cmd, "invalid_script", "script name has invalid characters");
    }
    let body: RepoScriptWriteBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if body.content.trim().is_empty() {
        return bad_request(cmd, "empty", "script content must not be empty");
    }
    let ctx = bn_core::resolver::Resolved {
        repo: repo.clone(),
        branch: "main".to_string(),
    };
    let path = state
        .cfg
        .note_dir(&ctx)
        .join("jobs")
        .join(format!("{name}.sh"));
    let wrote = tokio::task::spawn_blocking({
        let path = path.clone();
        let content = body.content;
        move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension(format!("sh.{}.tmp", std::process::id()));
            std::fs::write(&tmp, content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            }
            std::fs::rename(tmp, path)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| e.to_string()));
    match wrote {
        Ok(()) => {
            audit::record(&state, "repo-script-write", &repo, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "repo": repo, "name": name, "path": path.display().to_string() }),
            ))
            .into_response()
        }
        Err(error) => {
            audit::record(&state, "repo-script-write", &repo, addr, false);
            server_error(
                cmd,
                "write_failed",
                &format!("could not write script: {error}"),
            )
        }
    }
}
