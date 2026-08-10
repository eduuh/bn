//! PATH shims: fake `claude` / `copilot` / `gh` / service-manager binaries.
//!
//! bn resolves agent harnesses by bare name — `Command::new("claude")` in
//! `bn-cli/src/cmd/agent/steer.rs` and `bn-cli/src/cmd/staff.rs`, and as a bare word inside the
//! command line handed to `tmux new-session`. Both resolve through `PATH`, so prepending a shim
//! directory is enough to replace the real agent with a scripted one. That is what makes the tmux
//! workflow testable at all: admission → launch → liveness → reap can be driven deterministically,
//! with no tokens spent and no real agent loose in a worktree.
//!
//! `systemctl` / `launchctl` are shimmed to no-ops for the same reason the sandbox pins `HOME`:
//! a code path that reaches them would otherwise register a real service on the developer's
//! machine, and the failure would outlive the test run.

use anyhow::Result;
use std::path::Path;

/// What a fake agent should do when bn launches it.
#[derive(Debug, Clone)]
pub enum Behavior {
    /// Announce itself, record the call, exit 0 immediately.
    Succeed,
    /// Exit with a specific non-zero code — the "agent crashed" path.
    Fail(i32),
    /// Run until killed. The liveness/reap path: bn should see a live window and a live process.
    Hang,
    /// Stay alive for N seconds, then exit 0 — a bounded run that finishes on its own.
    ExitAfter(u32),
    /// A raw `sh` body, for cases the presets do not cover. `$CALLS` is in scope.
    Script(String),
}

impl Behavior {
    fn body(&self) -> String {
        match self {
            Behavior::Succeed => "echo \"[fake-agent] done\"\nexit 0\n".into(),
            Behavior::Fail(code) => {
                format!("echo \"[fake-agent] failing\" >&2\nexit {code}\n")
            }
            // `sleep infinity` is not portable (macOS sleep rejects it); a bounded loop is.
            Behavior::Hang => {
                "echo \"[fake-agent] running\"\nwhile true; do sleep 1; done\n".into()
            }
            Behavior::ExitAfter(secs) => {
                format!("echo \"[fake-agent] running\"\nsleep {secs}\nexit 0\n")
            }
            Behavior::Script(body) => body.clone(),
        }
    }
}

/// Write an executable `sh` shim named `name` into `dir`.
///
/// Every shim appends its argv to `calls` before doing anything else, so a test can assert on how
/// bn invoked it — which flags, which prompt, which working directory — not merely that it ran.
pub fn write(dir: &Path, calls: &Path, name: &str, behavior: &Behavior) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\n\
         # Fake `{name}` installed by the bn-e2e sandbox. The real binary must never run here.\n\
         CALLS='{calls}'\n\
         # Flatten newlines: a dispatched seed prompt is multi-paragraph, and an unflattened argv\n\
         # would split one call across many log lines — silently truncating what a test can assert\n\
         # on to the prompt's first line.\n\
         printf '%s\\t%s\\t%s\\n' \"{name}\" \"$PWD\" \"$(printf '%s' \"$*\" | tr '\\n' ' ')\" >> \"$CALLS\"\n\
         {body}",
        name = name,
        calls = calls.display(),
        body = behavior.body()
    );
    std::fs::write(&path, script)?;
    make_executable(&path)?;
    Ok(())
}

/// Install the default shim set: agent harnesses that fail loudly if a test forgot to script them,
/// and no-op service managers.
///
/// The agent default is [`Behavior::Fail`] rather than [`Behavior::Succeed`] on purpose. A test
/// that launches an agent it never configured has a hole in it, and a silently-succeeding stub
/// would hide that; a non-zero exit surfaces it.
pub fn install_defaults(dir: &Path, calls: &Path) -> Result<()> {
    for agent in ["claude", "copilot"] {
        write(dir, calls, agent, &Behavior::Fail(97))?;
    }
    // Service managers and the network CLI: recorded, never effective.
    for noop in ["systemctl", "launchctl", "crontab", "gh"] {
        write(dir, calls, noop, &Behavior::Succeed)?;
    }
    Ok(())
}

/// Recorded shim invocations as `(binary, cwd, args)` rows.
pub fn calls(path: &Path) -> Vec<(String, String, String)> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            Some((
                parts.next()?.to_string(),
                parts.next()?.to_string(),
                parts.next().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}
