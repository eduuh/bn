//! `bn cmd` — your own commands: the thing you do over and over, written once as `<name>.sh` and
//! run with arguments by you (`bn cmd <name> ...`) or an agent (the `bn_cmd` MCP tool).
//!
//! A command is a job script in everything but intent. It resolves through the same tiers
//! (branch → project → repo → global, see [`resolver::script_tier_dirs`]) under `cmds/` instead
//! of `jobs/`, so a cron's scripts and your hand-run wrappers never share a listing. Unlike a job
//! it takes arguments, runs in the caller's directory, and is not recorded to the cron ledger.
//!
//! The script's leading comments describe it: `# description: ...` is the one line `bn cmd` and
//! agents see, `# usage: ...` says what arguments it takes.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::resolver::{self, Config, JobScope, Resolved};

/// How many leading lines are searched for the `# description:` / `# usage:` header.
const HEADER_LINES: usize = 20;

/// Captured output kept per stream for a caller that reads it back (an agent): the tail wins.
const CAPTURE_BYTES: usize = 32 * 1024;

/// One resolvable command.
#[derive(Debug, Clone, Serialize)]
pub struct CmdRef {
    pub name: String,
    pub scope: JobScope,
    pub path: PathBuf,
    /// The `# description:` line, empty when the script has none.
    pub description: String,
    /// The `# usage:` line, when the script has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

/// The `cmds/` dirs for `ctx`, highest precedence first.
pub fn dirs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<(JobScope, PathBuf)> {
    resolver::script_tier_dirs(cfg, ctx, "cmds", resolver::cmds_dir())
}

/// A command name is a plain file stem: lowercase letters, digits, `-` and `_`, not starting with
/// a separator. Keeps a name from ever being a path (`../x`) or a flag (`-x`).
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Resolve `name` to the highest-tier `<dir>/<name>.sh`.
pub fn find(cfg: &Config, ctx: Option<&Resolved>, name: &str) -> Option<CmdRef> {
    if !valid_name(name) {
        return None;
    }
    dirs(cfg, ctx).into_iter().find_map(|(scope, dir)| {
        let path = dir.join(format!("{name}.sh"));
        path.is_file().then(|| describe(name, scope, path))
    })
}

/// Every command in scope, one per name (the tier that would run it), sorted by name.
pub fn list(cfg: &Config, ctx: Option<&Resolved>) -> Vec<CmdRef> {
    let mut out: Vec<CmdRef> = Vec::new();
    for (scope, dir) in dirs(cfg, ctx) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name
                .to_string_lossy()
                .strip_suffix(".sh")
                .map(str::to_string)
            else {
                continue;
            };
            if valid_name(&name) && !out.iter().any(|c| c.name == name) {
                out.push(describe(&name, scope, entry.path()));
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn describe(name: &str, scope: JobScope, path: PathBuf) -> CmdRef {
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let (description, usage) = parse_header(&text);
    CmdRef {
        name: name.to_string(),
        scope,
        path,
        description: description.unwrap_or_default(),
        usage,
    }
}

/// `(description, usage)` from a script's leading `# description:` / `# usage:` comments.
pub fn parse_header(text: &str) -> (Option<String>, Option<String>) {
    let field = |key: &str| {
        text.lines().take(HEADER_LINES).find_map(|line| {
            let rest = line.trim().strip_prefix('#')?.trim_start();
            let value = rest.strip_prefix(key)?.strip_prefix(':')?.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    };
    (field("description"), field("usage"))
}

/// The directory `bn cmd new --scope <scope>` writes into, or `None` when that tier does not exist
/// for `ctx` (branch on `main`, project/repo/branch outside a worktree). Global means your
/// private, synced `<personal_root>/cmds`, never the shipped dir.
pub fn scope_dir(cfg: &Config, ctx: Option<&Resolved>, scope: JobScope) -> Option<PathBuf> {
    if scope == JobScope::Global {
        return Some(cfg.personal_root.join("cmds"));
    }
    dirs(cfg, ctx)
        .into_iter()
        .find_map(|(s, dir)| (s == scope).then_some(dir))
}

/// Write a starter `<dir>/<name>.sh`, refusing to overwrite one that exists.
pub fn scaffold(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    if !valid_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{name}' is not a command name (lowercase letters, digits, - and _)"),
        ));
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.sh"));
    let body = format!(
        "#!/bin/sh\n\
         # description: TODO one line saying what `{name}` does (shown by `bn cmd` and to agents)\n\
         # usage: {name} [args...]\n\
         #\n\
         # Run with `bn cmd {name} [args...]`. It runs in the caller's directory; $BN_CMD_REPO and\n\
         # $BN_CMD_BRANCH name the worktree it was called from (empty outside one), and\n\
         # $BN_CMD_LIB is the shipped shell helpers dir (`. \"$BN_CMD_LIB/<file>.sh\"`).\n\
         set -eu\n\
         \n\
         echo \"{name}: $*\"\n"
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::io::Write::write_all(&mut file, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(path)
}

/// The process for running `cmd` with `args` from `cwd`. Stdio is left to the caller.
pub fn command(
    cmd: &CmdRef,
    args: &[String],
    cwd: &Path,
    ctx: Option<&Resolved>,
) -> std::process::Command {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg(&cmd.path)
        .args(args)
        .current_dir(cwd)
        .env("PATH", job_path())
        .env("BN_CMD_NAME", &cmd.name)
        .env("BN_CMD_REPO", ctx.map(|c| c.repo.as_str()).unwrap_or(""))
        .env(
            "BN_CMD_BRANCH",
            ctx.map(|c| c.branch.as_str()).unwrap_or(""),
        )
        .env("BN_CMD_LIB", resolver::jobs_dir().join("lib"));
    command
}

/// The PATH every process bn spawns to run the user's own scripts gets.
///
/// The leading entry is this executable's own directory, and it is load-bearing: on a stock install
/// that is the *only* place `bn` exists (`~/.bn/bin`), and a scheduler shell — launchd, systemd,
/// cron — inherits none of the profile that normally puts it on PATH. A hand-rolled
/// `~/.local/bin:~/.bin:$PATH` left every `run:` command that called `bn` exiting 127 with
/// `/bin/sh: bn: command not found`.
///
/// Job scripts, `run:` commands, staff gathers, harness dispatches, the `verify`/`build` gates and
/// `bn cmd` all take their PATH from here. Re-deriving the prelude at a call site is what produced
/// that 127, so there is one of these.
pub fn job_path() -> String {
    let home = dirs::home_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
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

/// What a captured run produced.
#[derive(Debug, Clone, Serialize)]
pub struct Captured {
    /// The exit code; `None` when killed at the timeout or by a signal.
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `command` with no stdin, capture both streams (last [`CAPTURE_BYTES`] of each), and kill it
/// at `timeout`. A script that leaves a background child holding its stdout open (a tmux server, a
/// daemon) still returns once the script itself exits: the readers are not waited on past that.
pub fn run_captured(
    mut command: std::process::Command,
    timeout: Duration,
) -> std::io::Result<Captured> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let out = spawn_reader(child.stdout.take());
    let err = spawn_reader(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let (code, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status.code(), false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let grace = Duration::from_millis(500);
    Ok(Captured {
        code,
        timed_out,
        stdout: out.recv_timeout(grace).unwrap_or_default(),
        stderr: err.recv_timeout(grace).unwrap_or_default(),
    })
}

fn spawn_reader(stream: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut stream) = stream {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stream.read_to_end(&mut buf);
            let start = buf.len().saturating_sub(CAPTURE_BYTES);
            let _ = tx.send(String::from_utf8_lossy(&buf[start..]).into_owned());
        });
    } else {
        let _ = tx.send(String::new());
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_file_stems_never_paths_or_flags() {
        assert!(valid_name("window"));
        assert!(valid_name("open-pr_2"));
        for bad in ["", "-x", "../x", "a/b", "Window", "a.sh", "_x"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn header_reads_description_and_usage_from_leading_comments() {
        let text = "#!/bin/sh\n# description: Open a tmux window\n#usage: window [cmd]\necho\n";
        assert_eq!(
            parse_header(text),
            (
                Some("Open a tmux window".to_string()),
                Some("window [cmd]".to_string())
            )
        );
        assert_eq!(parse_header("#!/bin/sh\necho hi\n"), (None, None));
    }

    #[test]
    fn scaffold_writes_a_runnable_script_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = scaffold(dir.path(), "hello").unwrap();
        let (description, usage) = parse_header(&std::fs::read_to_string(&path).unwrap());
        assert!(description.unwrap().contains("hello"));
        assert_eq!(usage.as_deref(), Some("hello [args...]"));
        assert!(scaffold(dir.path(), "hello").is_err(), "never overwrites");
        assert!(scaffold(dir.path(), "../evil").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn captured_run_passes_args_and_kills_at_timeout() {
        let mut ok = std::process::Command::new("/bin/sh");
        ok.args(["-c", "echo \"$1\"; echo err >&2; exit 3", "sh", "a b"]);
        let out = run_captured(ok, Duration::from_secs(10)).unwrap();
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (Some(3), "a b\n", "err\n")
        );

        let mut slow = std::process::Command::new("/bin/sh");
        slow.args(["-c", "sleep 5"]);
        let out = run_captured(slow, Duration::from_millis(100)).unwrap();
        assert!(out.timed_out && out.code.is_none());
    }
}
