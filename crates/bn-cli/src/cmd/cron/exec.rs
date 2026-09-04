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
pub(crate) fn run_script(script: &std::path::Path) -> (i32, String) {
    if cfg!(windows) {
        let error = "job scripts are unix-only (sh); not supported on native Windows";
        eprintln!("bn run: {error}");
        return (2, error.to_string());
    }
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(script).env("PATH", job_path());
    capture_command(command, &script.display().to_string())
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

/// Run a user-supplied shell command under [`job_path`] and return `(exited_zero, stdout + stderr)`
/// in full — for callers that *parse* the output rather than log it, so neither the 15-line tail nor
/// the stream echo of [`capture_command`] fits. `bn orchestrate queue-from --check` is the caller:
/// it turns the check's diagnostics into queued work, and a failing check is its normal case.
///
/// Lives here, beside the other user-script launchers, because the PATH is the load-bearing part:
/// a check that shells out to `bn` exits 127 with `/bin/sh: bn: command not found` under any caller
/// that is a scheduler shell rather than a login shell, exactly as staff gathers once did.
pub(crate) fn capture_user_command(command_text: &str) -> std::io::Result<(bool, String)> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command_text)
        .env("PATH", job_path())
        .output()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), combined))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parsed user command is still a user script that commonly calls `bn`, so it must get the
    /// same PATH every other job process gets. Asserting the shared prelude (rather than an
    /// equivalent-looking string) is what keeps this from drifting back into the
    /// `/bin/sh: bn: command not found` 127 that silenced scheduled staff gathers.
    ///
    /// Pinned under [`env_lock`] because the assertion compares two *separate* evaluations of
    /// [`job_path`], which reads `HOME` and `PATH` at call time — once inside the spawn, once
    /// here. Any sibling test that moves `HOME` in that window (the launchd drift test does
    /// exactly that) made this fail with a temp home spliced into the left side only. Snapshotting
    /// the expected value first would not help: the mutation window is the spawn itself, so the
    /// inputs have to be held still rather than merely read early. `/bin/sh` is spawned by
    /// absolute path, so pinning `PATH` cannot break program resolution.
    #[test]
    fn a_captured_user_command_runs_under_the_shared_job_path() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = bn_core::test_support::EnvGuard::set("HOME", temp.path());
        let _path = bn_core::test_support::EnvGuard::set("PATH", "/usr/bin:/bin");

        let (ok, combined) =
            capture_user_command("printf %s \"$PATH\"").expect("the command echoes its PATH");

        assert!(ok, "the echo-path command should exit 0");
        assert_eq!(combined.trim_end(), job_path());
    }

    /// A failing command still has to hand its diagnostics back — the caller parses them out of the
    /// combined streams, and a non-zero exit is the normal case for a lint sweep.
    #[test]
    fn a_failing_captured_command_returns_its_stderr() {
        let (ok, combined) = capture_user_command("echo boom >&2; exit 1").expect("command runs");

        assert!(!ok, "a command exiting 1 is not ok");
        assert!(combined.contains("boom"), "{combined:?}");
    }
}
