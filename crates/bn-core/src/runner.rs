//! Injectable command-runner seam for external `git` / `gh` reads. Features that read git history
//! (`git log`, `git diff`) or PR state (`gh pr view`, `gh pr list`) go through a [`CommandRunner`]
//! so core tests can drive them with canned fixtures via [`FakeRunner`], instead of shelling out.
//!
//! Only the read calls features depend on live here. Interactive / side-effecting spawns
//! (editor, systemctl) still spawn at their CLI call sites. tmux integration is no longer
//! CLI-only: its shared wrappers live in [`crate::tmux`] so any crate can reuse them in-process
//! (issue #428).

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The captured result of running an external command: exit success plus trimmed stdout/stderr.
/// Mirrors the slice of [`std::process::Output`] the read call sites use.
#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    /// Whether the process exited 0.
    pub success: bool,
    /// Captured stdout (lossy UTF-8, untrimmed).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8, untrimmed).
    pub stderr: String,
}

impl CommandOutput {
    /// Trimmed stdout when the command succeeded and produced non-empty output, else `None`.
    /// The common "give me the value or nothing" shape the git/gh read helpers want.
    pub fn ok_stdout(&self) -> Option<String> {
        if !self.success {
            return None;
        }
        let s = self.stdout.trim();
        (!s.is_empty()).then(|| s.to_string())
    }
}

/// Runs external read commands (`git`, `gh`). The real impl shells out; [`FakeRunner`] replays
/// fixtures. Convenience [`Self::git`] / [`Self::gh`] wrap [`Self::run`] for the two programs the
/// features need.
pub trait CommandRunner {
    /// Run `program` with `args` in the current working directory, capturing its output.
    fn run(&self, program: &str, args: &[&str]) -> CommandOutput;

    /// Run a command with a wall-clock deadline. Test runners default to their immediate fixture
    /// lookup; the real runner overrides this with bounded subprocess execution.
    fn run_with_timeout(&self, program: &str, args: &[&str], _timeout: Duration) -> CommandOutput {
        self.run(program, args)
    }

    /// Run `git <args>`.
    fn git(&self, args: &[&str]) -> CommandOutput {
        self.run("git", args)
    }

    /// Run `gh <args>`.
    fn gh(&self, args: &[&str]) -> CommandOutput {
        self.run("gh", args)
    }
}

/// The production runner: shells out with [`std::process::Command`], capturing output. A missing
/// binary (e.g. `gh` not installed) yields an unsuccessful [`CommandOutput`] rather than an error,
/// matching the best-effort callers.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandOutput {
        let timer = crate::log::enabled().then(crate::log::Timer::start);
        let mut command = std::process::Command::new(program);
        command.args(args);
        // A captured read has nowhere to show a prompt and no deadline of its own, so an unbounded
        // `git fetch` here hangs its caller silently. See [`crate::gitnet`].
        if program == "git" {
            crate::gitnet::bound(&mut command);
        }
        let out = match command.output() {
            Ok(out) => CommandOutput {
                success: out.status.success(),
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            },
            Err(_) => CommandOutput::default(),
        };
        log_command(program, args, &out, timer);
        out
    }

    fn run_with_timeout(&self, program: &str, args: &[&str], timeout: Duration) -> CommandOutput {
        let timer = crate::log::enabled().then(crate::log::Timer::start);
        let out = run_bounded(program, args, timeout);
        log_command(program, args, &out, timer);
        out
    }
}

/// Emit a redacted action-log record for an external `git`/`gh` (or other) subprocess run through
/// the runner seam. No-op when the action log is disabled (`timer` is `None`). The `subsystem` is
/// the program name so `git`/`gh` calls are filterable; stdout/stderr are intentionally omitted
/// (they can be large and leak secrets) — only argv, success, and duration are logged.
fn log_command(
    program: &str,
    args: &[&str],
    out: &CommandOutput,
    timer: Option<crate::log::Timer>,
) {
    let Some(timer) = timer else {
        return;
    };
    crate::log::record(
        crate::log::LogLevel::Debug,
        program,
        "run",
        serde_json::json!({ "argv": crate::log::redact_args(args) }),
        serde_json::json!({ "success": out.success }),
        Some(timer.elapsed_ms()),
    );
}

fn command_label(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        format!("`{program}`")
    } else {
        format!("`{program} {}`", args.join(" "))
    }
}

fn timeout_label(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!("{}s", timeout.as_secs())
    } else {
        format!("{}ms", timeout.as_millis())
    }
}

fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> CommandOutput {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if program == "git" {
        crate::gitnet::bound(&mut command);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: format!("failed to spawn {}: {e}", command_label(program, args)),
            };
        }
    };

    let process_group = child.id();
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = kill_process_tree(process_group, &mut child);
            let _ = child.wait();
            return CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: format!(
                    "failed to capture stdout from {}",
                    command_label(program, args)
                ),
            };
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = kill_process_tree(process_group, &mut child);
            let _ = child.wait();
            return CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: format!(
                    "failed to capture stderr from {}",
                    command_label(program, args)
                ),
            };
        }
    };
    let (stdout_tx, stdout_rx) = std::sync::mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = std::sync::mpsc::sync_channel(1);
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stdout).read_to_end(&mut bytes);
        let _ = stdout_tx.send(bytes);
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stderr).read_to_end(&mut bytes);
        let _ = stderr_tx.send(bytes);
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut wait_error = None;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(done)) => status = Some(done),
                Ok(None) => {}
                Err(error) => {
                    wait_error = Some(error);
                    let _ = kill_process_tree(process_group, &mut child);
                    status = child.wait().ok();
                }
            }
        }
        if stdout.is_none() {
            stdout = stdout_rx.try_recv().ok();
        }
        if stderr.is_none() {
            stderr = stderr_rx.try_recv().ok();
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            break;
        }
        if wait_error.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = kill_process_tree(process_group, &mut child);
            if status.is_none() {
                status = child.wait().ok();
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let stdout = stdout.unwrap_or_else(|| stdout_rx.recv().unwrap_or_default());
    let stderr = stderr.unwrap_or_else(|| stderr_rx.recv().unwrap_or_default());
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "command timed out after {}: {}",
            timeout_label(timeout),
            command_label(program, args)
        ));
    }
    if let Some(error) = wait_error {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "failed while waiting for {}: {error}",
            command_label(program, args)
        ));
    }

    CommandOutput {
        success: !timed_out && status.is_some_and(|status| status.success()),
        stdout,
        stderr,
    }
}

/// SIGKILL a whole process group, falling back to the child alone if the group kill fails.
///
/// Public because killing only the child is not enough for the callers that need this: the thing
/// that hangs is usually a *grandchild* (`bn dispatch` → `git fetch` → `ssh`), and reaping the
/// parent while the network-blocked grandchild lives on leaks the very process you were trying to
/// stop. Requires the child to have been spawned with `process_group(0)`.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::kill is required to terminate the bounded command's process group"
)]
pub fn kill_process_tree(
    process_group: u32,
    child: &mut std::process::Child,
) -> std::io::Result<()> {
    // SAFETY: the child was spawned as the leader of its own process group, so the negative pid
    // targets only that bounded command and descendants it created.
    let result = unsafe { libc::kill(-(process_group as i32), libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        child.kill()
    }
}

/// Non-unix stand-in: there are no process groups to signal, so the child is all we can reach.
#[cfg(not(unix))]
pub fn kill_process_tree(
    _process_group: u32,
    child: &mut std::process::Child,
) -> std::io::Result<()> {
    child.kill()
}

/// A runner that replays canned fixtures, for deterministic core tests. Each call is matched
/// against registered `(program, args)` responses; an unmatched call returns a failed
/// [`CommandOutput`] (mirroring "binary absent / command failed"). Match by exact arg vector, or
/// register a program-wide default with `Self::on_program`.
#[derive(Debug, Default)]
pub struct FakeRunner {
    exact: Vec<(String, Vec<String>, CommandOutput)>,
    by_program: Vec<(String, CommandOutput)>,
}

impl FakeRunner {
    /// An empty runner: every call fails until a fixture is registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fixture for an exact `(program, args)` invocation.
    pub fn on(mut self, program: &str, args: &[&str], stdout: &str) -> Self {
        self.exact.push((
            program.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
            CommandOutput {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandOutput {
        for (p, a, out) in &self.exact {
            if p == program && a.iter().map(String::as_str).eq(args.iter().copied()) {
                return out.clone();
            }
        }
        for (p, out) in &self.by_program {
            if p == program {
                return out.clone();
            }
        }
        CommandOutput::default()
    }
}

/// The PR URL for the current branch via `gh pr view --json url -q .url`, or `None` when gh is
/// absent / there's no PR / it fails. Run through `runner` so it's testable; `RealRunner`
/// reproduces the original CLI behaviour exactly.
pub fn pr_url(runner: &dyn CommandRunner) -> Option<String> {
    runner
        .gh(&["pr", "view", "--json", "url", "-q", ".url"])
        .ok_stdout()
}

/// `gh <issue|pr> view <num> --repo <owner/repo> --json title -q .title`, or `None` on any
/// failure (gh absent, not authed, no such item). Best-effort title lookup for `bn capture`.
pub fn view_title(
    runner: &dyn CommandRunner,
    kind: &str,
    owner_repo: &str,
    num: &str,
) -> Option<String> {
    runner
        .gh(&[
            kind, "view", num, "--repo", owner_repo, "--json", "title", "-q", ".title",
        ])
        .ok_stdout()
}

/// Run `gh <args>` and decode its stdout as JSON into `T`, or `None` when the command fails (gh
/// absent / errored), emits no stdout, or the JSON doesn't match `T`. The typed companion to
/// [`pr_url`] / [`view_title`] for `gh ... --json <fields>` reads; drive it through `runner` so
/// call sites stay testable with [`FakeRunner`].
pub fn gh_json<T: serde::de::DeserializeOwned>(
    runner: &dyn CommandRunner,
    args: &[&str],
) -> Option<T> {
    let stdout = runner.gh(args).ok_stdout()?;
    serde_json::from_str::<T>(&stdout).ok()
}
