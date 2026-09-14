//! `bn run keep-warm` — top up one repo's issue-derived task reservoir and seed brainstorm scouts.
//!
//! The ranking policy is shared with `bn task pull` in `bn_core::keep_warm`; this module is the
//! CLI realization layer: repo targeting, task-file writes, scout launches, and operator-facing
//! status lines.
use crate::*;
use bn_core::keep_warm::KeepWarmPolicy;
use bn_core::orchestrate::OrchestrateState;
use bn_core::runner::RealRunner;
use std::path::Path;

pub(crate) fn configured_policy(cfg: &Config, low_water: usize) -> KeepWarmPolicy {
    KeepWarmPolicy {
        low_water: low_water.max(1),
        brainstorm_count: cfg.orchestrate_brainstorm_count,
        brainstorm_interval_secs: cfg.orchestrate_brainstorm_interval_secs,
        exclude_titles: cfg.orchestrate_exclude_titles.clone(),
        exclude_labels: cfg.orchestrate_exclude_labels.clone(),
    }
}

/// `keep-warm` is repo-scoped but not branch-scoped: `bn run keep-warm --repo R` targets that
/// repo's main-note layer directly rather than forcing the caller to invent a branch.
pub(crate) fn run_target_ctx(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
) -> Option<Resolved> {
    match (repo, branch) {
        (Some(repo), None) => {
            let (repo, _) = match crate::wt::resolve_bare_by_name(cfg, Some(repo.as_str())) {
                Ok(Some(pair)) => pair,
                Ok(None) => fail("bn run keep-warm: pass --repo <name>"),
                Err(error) => fail(&format!("bn run keep-warm: {error}")),
            };
            Some(Resolved {
                repo: repo.to_lowercase(),
                branch: "main".to_string(),
            })
        }
        (repo, branch) => crate::target_ctx(cfg, repo, branch),
    }
}

pub(crate) fn job_keep_warm(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    if ctx.is_none() && !cfg.orchestrate_keep_warm {
        crate::cmd::jobs::native_job_line("keep-warm: disabled", false);
        return 0;
    }

    let repos = match target_repos(cfg, ctx) {
        Ok(repos) => repos,
        Err(code) => return code,
    };
    let policy = configured_policy(cfg, cfg.orchestrate_low_water);
    let now = bn_core::util::now_epoch_secs();
    let today = crate::today();
    let runner = RealRunner;
    let mut failed = 0;

    for repo in &repos {
        if !cfg.repo_available_here(repo) {
            crate::cmd::jobs::native_job_line(
                &format!(
                    "keep-warm: {repo} is not registered and available on '{}'; skipped",
                    cfg.machine()
                ),
                false,
            );
            continue;
        }
        let state_path = bn_core::orchestrate::state_path_for(repo);
        let state = match persist_keep_warm_policy(&state_path, repo, &policy, now) {
            Ok(state) => state,
            Err(error) => {
                crate::cmd::jobs::native_job_line(
                    &format!("keep-warm: could not persist {repo} state: {error}"),
                    true,
                );
                failed += 1;
                continue;
            }
        };
        let cached = bn_core::issues::for_repo(cfg, &runner, repo, now);
        if let Some(error) = &cached.error {
            if cached.stale {
                crate::cmd::jobs::native_job_line(
                    &format!("keep-warm: {repo}: using stale issue cache — {error}"),
                    false,
                );
            } else {
                crate::cmd::jobs::native_job_line(
                    &format!("keep-warm: could not list open issues for {repo} — {error}"),
                    true,
                );
                failed += 1;
                continue;
            }
        }

        let plan = match plan_repo_pull(
            cfg,
            repo,
            &policy,
            &cached.issues,
            state.keep_warm_last_brainstorm_at,
            now,
            &today,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                crate::cmd::jobs::native_job_line(&error, true);
                failed += 1;
                continue;
            }
        };

        if plan.pulled > 0 {
            crate::cmd::jobs::native_job_line(
                &format!("keep-warm: {repo}: pulled {} task(s)", plan.pulled),
                false,
            );
        }
        if !plan.brainstorm.is_empty() {
            match start_brainstorm(&state_path, repo, &plan.brainstorm, now) {
                Ok(0) => {}
                Ok(launched) => crate::cmd::jobs::native_job_line(
                    &format!("keep-warm: {repo}: launched {launched} brainstorm scout(s)"),
                    false,
                ),
                Err(error) => {
                    crate::cmd::jobs::native_job_line(&error, true);
                    failed += 1;
                    continue;
                }
            }
        }
        if plan.pulled == 0 && plan.brainstorm.is_empty() {
            crate::cmd::jobs::native_job_line(&format!("keep-warm: {repo}: already warm"), false);
        }
    }

    if failed == 0 { 0 } else { 1 }
}

fn target_repos(cfg: &Config, ctx: Option<&Resolved>) -> Result<Vec<String>, i32> {
    if let Some(ctx) = ctx {
        return Ok(vec![ctx.repo.clone()]);
    }
    if cfg.orchestrate_owned_repos.is_empty() {
        crate::cmd::jobs::native_job_line("keep-warm: [orchestrate].own is empty", true);
        return Err(1);
    }
    let mut repos = cfg.orchestrate_owned_repos.clone();
    repos.sort();
    repos.dedup();
    Ok(repos)
}

fn persist_keep_warm_policy(
    path: &Path,
    repo: &str,
    policy: &KeepWarmPolicy,
    now: i64,
) -> std::io::Result<OrchestrateState> {
    let _lock = bn_core::store::FileLock::acquire_required(path)?;
    let mut state =
        OrchestrateState::load(path).unwrap_or_else(|| OrchestrateState::new(repo, &[], 1, false, now));
    state.keep_warm = Some(policy.clone());
    state.updated_at = now;
    state.save(path)?;
    Ok(state)
}

struct RepoPullPlan {
    pulled: usize,
    brainstorm: Vec<String>,
}

fn plan_repo_pull(
    cfg: &Config,
    repo: &str,
    policy: &KeepWarmPolicy,
    issues: &[bn_core::issues::IssueRef],
    last_brainstorm_at: Option<i64>,
    now: i64,
    today: &str,
) -> Result<RepoPullPlan, String> {
    let path = crate::cmd::task::task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("keep-warm: could not prepare task store: {error}"))?;
    }
    let mut file = bn_core::todo::load(&path)
        .map_err(|error| format!("keep-warm: could not load task store: {error}"))?;

    let existing = bn_core::task_pull::existing_srcs(&file);
    let depth = bn_core::task_pull::active_depth(&file, repo, today);
    let plan =
        bn_core::task_pull::plan_pull_actions(repo, policy, depth, &existing, issues, last_brainstorm_at, now);

    if !plan.issues.is_empty() {
        crate::cmd::task::append_pulled_tasks(&mut file, &plan.issues, today);
        bn_core::todo::save(&path, &file)
            .map_err(|error| format!("keep-warm: could not save task store: {error}"))?;
    }

    Ok(RepoPullPlan {
        pulled: plan.issues.len(),
        brainstorm: plan.brainstorm,
    })
}

/// Start one brainstorm scout per prompt through `bn dispatch`, and stamp the brainstorm clock when
/// any of them started. Returns how many started; a failed launch is reported once the rest have had
/// their turn, so one refusal does not cost the others.
fn start_brainstorm(
    state_path: &Path,
    repo: &str,
    brainstorm: &[String],
    now: i64,
) -> Result<usize, String> {
    let mut started = 0;
    let mut failures = Vec::new();
    for prompt in brainstorm {
        match crate::cmd::dispatch::DispatchInvocation::new(prompt, repo, "auto")
            .scout(true)
            .launch()
        {
            Ok(()) => started += 1,
            Err(error) => failures.push(error),
        }
    }
    if started > 0 {
        bn_core::orchestrate::mutate(state_path, |state| {
            state.keep_warm_last_brainstorm_at = Some(now);
        })
        .map_err(|error| format!("keep-warm: could not update {repo} brainstorm timestamp: {error}"))?;
    }
    match failures.first() {
        Some(error) => Err(format!("keep-warm: brainstorm failed for {repo}: {error}")),
        None => Ok(started),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_repo(temp: &tempfile::TempDir, repo: &str) -> Config {
        let worktree_dir = temp.path().join("worktree");
        std::fs::create_dir_all(temp.path().join("bare").join(format!("{repo}.git")))
            .expect("create bare repo");
        Config {
            machine_profile: Some(bn_core::resolver::MachineProfile {
                repos: vec![repo.to_string()],
                ..Default::default()
            }),
            personal_root: temp.path().join("personal"),
            work_root: temp.path().join("work"),
            worktree_dir,
            hostname: "test-host".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn run_target_ctx_accepts_repo_without_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_with_repo(&temp, "Demo");

        let ctx = run_target_ctx(&cfg, Some("demo".to_string()), None).expect("resolved ctx");

        assert_eq!(
            ctx,
            Resolved {
                repo: "demo".to_string(),
                branch: "main".to_string(),
            }
        );
    }

    #[test]
    fn explicit_repo_runs_even_without_global_keep_warm_opt_in() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
        std::fs::create_dir_all(state.join("issues")).expect("issues dir");
        std::fs::write(
            state.join("issues").join("demo.json"),
            serde_json::json!({
                "repo": "demo",
                "slug": "owner/demo",
                "fetched_at": 10,
                "stale": false,
                "error": null,
                "issues": [{
                    "repo": "demo",
                    "number": 7,
                    "title": "Pulled by keep warm",
                    "url": "https://example.test/demo/issues/7",
                    "state": "OPEN",
                    "labels": [],
                    "updated_at": "2026-09-07T00:00:00Z",
                    "assignees": [],
                    "is_assigned": false
                }]
            })
            .to_string(),
        )
        .expect("cache write");
        let cfg = cfg_with_repo(&temp, "demo");
        let ctx = Resolved {
            repo: "demo".to_string(),
            branch: "main".to_string(),
        };

        assert_eq!(job_keep_warm(&cfg, Some(&ctx)), 0);

        let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("task file");
        let task = tasks.tasks().next().expect("pulled task");
        assert_eq!(task.meta("repo"), Some("demo"));
        assert_eq!(task.summary(), "Pulled by keep warm");
    }
}
