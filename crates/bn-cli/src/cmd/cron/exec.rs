//! How a job's process is actually launched: the shell, the PATH it inherits, and the output tail
//! the `cron_run` ledger keeps.
//!
//! Split out of `cron.rs` so the resolver (which job does this name mean?) and the executor (how
//! does a job become a process?) are separately readable — they fail in very different ways, and
//! [`job_path`] in particular is the whole reason a headless cron can call `bn` at all.
use crate::*;

/// Run `command` to completion, echoing its streams and keeping the 15-line tail the event log
/// records. A launch failure reports 127 — the shell's own "could not run it" code — so a job that
/// never started is not mistaken for one that ran and exited 0.
fn capture_command(mut command: std::process::Command, launch_target: &str) -> (i32, String) {
    match command.output() {
        Ok(out) => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out.stdout);
            let _ = std::io::stderr().write_all(&out.stderr);
            let mut combined = String::from_utf8_lossy(&out.stderr).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            (out.status.code().unwrap_or(-1), tail_lines(&combined, 15))
        }
        Err(e) => {
            eprintln!("bn run: failed to launch {launch_target}: {e}");
            (127, e.to_string())
        }
    }
}

/// The PATH every job process gets.
///
/// The leading entry is this executable's own directory, and it is load-bearing: on a stock install
/// that is the *only* place `bn` exists (`~/.bn/bin`), and a scheduler shell — launchd, systemd,
/// cron — inherits none of the profile that normally puts it on PATH. A hand-rolled
/// `~/.local/bin:~/.bin:$PATH` left every `run:` command that called `bn` exiting 127 with
/// `/bin/sh: bn: command not found`.
///
/// Every subprocess bn spawns to run *the user's own scripts* — job scripts, `run:` commands,
/// staff gathers, harness dispatches, and the `verify`/`build` gates — takes its PATH from here.
/// Re-deriving the prelude at a call site is what produced that 127, so there is one of these.
pub(crate) fn job_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    [
        exe_dir,
        format!("{home}/.local/bin"),
        format!("{home}/.bin"),
        path,
    ]
    .into_iter()
    .filter(|entry| !entry.is_empty())
    .collect::<Vec<_>>()
    .join(":")
}

/// A `<job>.sh` script from one of the job tiers.
fn run_script(script: &std::path::Path) -> (i32, String) {
    if cfg!(windows) {
        let error = "job scripts are unix-only (sh); not supported on native Windows";
        eprintln!("bn run: {error}");
        return (2, error.to_string());
    }
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(script).env("PATH", job_path());
    capture_command(command, &script.display().to_string())
}

/// A repo's own `<job>.sh`, with what its header asked for done first.
///
/// The one directive today is `# bn: latest-main` ([`resolver::ScriptHeader`]): fetch and
/// fast-forward the repo's main checkout and main slot before the script starts. Honoured here,
/// in the runner, rather than by whichever surface queued the script — a cron firing, `bn run`
/// and the picker all reach this line, so the script gets the same precondition however it was
/// asked for. The sync is [`crate::cmd::main_sync::sync_repo`], the same piece `build-main` uses.
///
/// A failed sync is a failed job: running a script that said "on latest main" against a stale
/// main would report success for work that did not happen as declared.
pub(crate) fn run_repo_script(
    cfg: &Config,
    ctx: Option<&Resolved>,
    script: &std::path::Path,
) -> (i32, String) {
    if resolver::script_header(script).latest_main {
        let Some(ctx) = ctx else {
            let msg = format!(
                "{} asks for latest main, but no repo is in scope to sync",
                script.display()
            );
            eprintln!("bn run: {msg}");
            return (2, msg);
        };
        let Some(bare) = crate::wt::bare_for(cfg, &ctx.repo) else {
            let msg = format!("{} asks for latest main, but {} has no bare repo here", script.display(), ctx.repo);
            eprintln!("bn run: {msg}");
            return (1, msg);
        };
        let (_, worktree_dir) = crate::wt::layout(cfg);
        println!("bn run: syncing {} to latest main first (# bn: latest-main)", ctx.repo);
        if !crate::cmd::main_sync::sync_repo(&bare, &worktree_dir, &ctx.repo) {
            let msg = format!("could not bring {} up to date", ctx.repo);
            eprintln!("bn run: {msg}");
            return (1, msg);
        }
    }
    run_script(script)
}

/// An agent manifest's `run:` line, verbatim.
pub(crate) fn run_custom_command(command_text: &str) -> (i32, String) {
    if cfg!(windows) {
        let error = "custom agent commands are unix-only (sh); not supported on native Windows";
        eprintln!("bn run: {error}");
        return (2, error.to_string());
    }
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(command_text).env("PATH", job_path());
    capture_command(command, "custom agent command")
}
