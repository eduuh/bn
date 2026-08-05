//! Shared subprocess bridge for invoking the installed `bn` CLI from sibling interfaces.
//!
//! The web and MCP surfaces deliberately compose real CLI subcommands for operations that remain
//! behind the CLI boundary. This module keeps binary resolution, argv shaping, output capture, and
//! JSON-envelope discovery identical across those callers.

use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Captured output from `bn --json <args>`.
#[derive(Debug)]
pub struct RunOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// The last JSON-parseable stdout line, after any human-readable leading output.
    pub json: Option<Value>,
}

#[derive(Debug)]
pub struct RunError {
    pub bin: PathBuf,
    pub source: std::io::Error,
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.bin.display(), self.source)
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Run `bn --json <args>` from an optional working directory.
pub fn run(args: &[&str], cwd: Option<&Path>) -> Result<RunOutput, RunError> {
    run_with_env(args, cwd, &[])
}

/// Run `bn --json <args>` with extra environment variables set on the child (else identical to
/// [`run`]). Used where a sibling surface must declare a dispatch's target session
/// (`BN_WORKER_REPO`) so the CLI's cross-repo-session guard treats it as that repo's own session
/// rather than a stray hop from the server's own worktree.
pub fn run_with_env(
    args: &[&str],
    cwd: Option<&Path>,
    envs: &[(&str, &str)],
) -> Result<RunOutput, RunError> {
    let bin = resolve_bn_bin();
    let mut cmd = Command::new(&bin);
    cmd.arg("--json").args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (key, val) in envs {
        cmd.env(key, val);
    }

    let out = cmd.output().map_err(|source| RunError { bin, source })?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let json = last_json_value(&stdout);

    Ok(RunOutput {
        success: out.status.success(),
        stdout,
        stderr,
        json,
    })
}

/// Spawn `bn <args>` detached and reap it off the caller's request path.
pub fn spawn_detached(args: &[&str]) -> std::io::Result<u32> {
    let mut child = Command::new(resolve_bn_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// Resolve `bn` from `$BN_BIN`, beside the current executable, or finally from `PATH`.
fn resolve_bn_bin() -> PathBuf {
    if let Ok(path) = std::env::var("BN_BIN") {
        let path = PathBuf::from(path);
        if is_executable(&path) {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("bn");
            if is_executable(&sibling) {
                return sibling;
            }
        }
    }
    PathBuf::from("bn")
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn last_json_value(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
}
