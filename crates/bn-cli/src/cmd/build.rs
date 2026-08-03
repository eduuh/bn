//! `bn build` + target-context resolution helpers.
use crate::*;

/// `bn build [name]` — resolve the worktree's `<name>.sh` (default `build`) via the same
/// branch → repo → global `job_script` lookup `wt pool warm` and the cron use, then run it in the
/// current directory. This unifies `bn build` with the pool's build script (one `jobs/build.sh`).
pub(crate) fn run_build_cmd(cfg: &Config, name: Option<String>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let Ok(ctx) = resolver::resolve(cfg, &cwd) else {
        eprintln!("bn build: not in a git repo");
        return 2;
    };
    let job = name.as_deref().unwrap_or("build");
    let Some((_scope, script)) = resolver::job_script(cfg, Some(&ctx), job) else {
        eprintln!(
            "bn build: no '{job}' script for {} — create {}/jobs/{job}.sh",
            ctx.repo,
            cfg.note_dir(&ctx).display()
        );
        return 2;
    };
    // PATH-prepend like the cron runner so tools the script shells out to (yarn, gh, …) resolve
    // under a non-login shell.
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    match std::process::Command::new("/bin/sh")
        .arg(&script)
        .current_dir(&cwd)
        .env("PATH", format!("{home}/.local/bin:{home}/.bin:{path}"))
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("bn build: failed to run {}: {e}", script.display());
            127
        }
    }
}

/// Explicit `--repo`/`--branch` win (for cron); otherwise resolve from the current dir.
/// `None` when neither is available — only global jobs are then in scope.
pub(crate) fn target_ctx(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
) -> Option<Resolved> {
    match target_ctx_result(cfg, repo, branch) {
        Ok(ctx) => Some(ctx),
        Err(resolver::ContextError::PartialTarget) => {
            fail("bn: --repo and --branch must be given together")
        }
        Err(resolver::ContextError::NotInGitRepo) => None,
    }
}

pub(crate) fn target_ctx_result(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
) -> Result<Resolved, resolver::ContextError> {
    let cwd = std::env::current_dir().unwrap_or_default();
    resolver::resolve_target(cfg, &cwd, repo, branch)
}

pub(crate) fn target_ctx_or_exit(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
    command: &str,
    not_in_git: &str,
) -> Resolved {
    match target_ctx_result(cfg, repo, branch) {
        Ok(ctx) => ctx,
        Err(resolver::ContextError::PartialTarget) => fail(&format!(
            "{command}: --repo and --branch must be given together"
        )),
        Err(resolver::ContextError::NotInGitRepo) => fail(not_in_git),
    }
}
