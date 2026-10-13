//! `bn build` + target-context resolution helpers.
use crate::*;

/// `bn build [name] [--repo R --branch B] [-- <args…>]` — resolve the worktree's `<name>.sh`
/// (default `build`) via the same branch → repo → global `job_script` lookup the cron uses, then
/// run it in the current directory. Any trailing argv is forwarded to the script unchanged. This
/// is the one build entry point: `wt pool warm` warms each slot by invoking this command, so the
/// pool can never drift from what `bn build` does.
///
/// An explicit `--repo`/`--branch` pair replaces cwd resolution, which is what lets the pool warm
/// build a slot at all — a pool slot is a *detached* checkout, so `git branch --show-current` is
/// empty there and cwd resolution fails.
pub(crate) fn run_build_cmd(
    cfg: &Config,
    name: Option<String>,
    repo: Option<String>,
    branch: Option<String>,
    argv: &[String],
    json: bool,
) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = match resolver::resolve_target(cfg, &cwd, repo, branch) {
        Ok(ctx) => ctx,
        Err(resolver::ContextError::PartialTarget) => {
            return build_failure(json, "bad_target", "bn build: --branch needs --repo", 2);
        }
        Err(resolver::ContextError::NotInGitRepo) => {
            return build_failure(
                json,
                "not_in_git_repo",
                "bn build: not in a git repo (on a detached HEAD? pass --repo/--branch)",
                2,
            );
        }
    };
    let job = name.as_deref().unwrap_or("build");
    let Some((_scope, script)) = resolver::job_script(cfg, Some(&ctx), job) else {
        let repo_dir = cfg.notes_dir_for(&ctx.repo).join("jobs").join(&ctx.repo);
        return build_failure(
            json,
            "missing_job",
            &format!(
                "bn build: no '{job}' script for {} — create {}/{job}.sh (synced to every \
                 machine through the notes repo), or, to commit it with the project's own git \
                 history, {}/.bn/jobs/{job}.sh",
                ctx.repo,
                repo_dir.display(),
                cwd.display()
            ),
            2,
        );
    };
    // The cron runner's PATH, so tools the script shells out to (yarn, gh, `bn` itself) resolve
    // under a non-login shell.
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg(&script)
        .args(argv)
        .current_dir(&cwd)
        .env("PATH", crate::cmd::cron::job_path());
    if !json {
        return match cmd.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => build_failure(
                false,
                "io",
                &format!("bn build: failed to run {}: {e}", script.display()),
                127,
            ),
        };
    }
    match cmd.output() {
        Ok(output) => {
            relay_build_output_to_stderr(&output.stdout, &output.stderr);
            let code = output.status.code().unwrap_or(1);
            println!(
                "{}",
                Envelope::success("build", serde_json::json!({ "job": job, "code": code }))
                    .to_json()
            );
            code
        }
        Err(e) => build_failure(
            true,
            "io",
            &format!("bn build: failed to run {}: {e}", script.display()),
            127,
        ),
    }
}

fn build_failure(json: bool, code: &str, msg: &str, exit: i32) -> i32 {
    if json {
        println!("{}", Envelope::failure("build", code, msg).to_json());
    } else {
        eprintln!("{msg}");
    }
    exit
}

fn relay_build_output_to_stderr(stdout: &[u8], stderr: &[u8]) {
    use std::io::Write;

    let mut out = std::io::stderr();
    let _ = out.write_all(stdout);
    let _ = out.write_all(stderr);
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
            fail("bn: --branch needs --repo")
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
