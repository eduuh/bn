//! The process-write bridge: run `bn --json <fixed args>` and return its [`Envelope`] (design
//! §2.3(b)).
//!
//! Side-effecting spawns (tmux, git, gh, launchd) are reached through the shared
//! [`bn_core::bn_subprocess`] bridge. This is a deliberate boundary choice, not a doctrine
//! requirement: issue #428 dropped the "side-effecting spawns stay behind the CLI" constraint and
//! moved the shared tmux wrappers to [`bn_core::tmux`], so a follow-up may migrate the write paths
//! where direct reuse reads cleaner; the subprocess bridge is kept where a real `bn` subcommand is
//! genuinely the right boundary. Two hardening rules make this safe to expose over HTTP:
//!
//! 1. **Fixed allowlist.** Callers (`crate::actions`) build the argv from a closed set of
//!    subcommand templates; only validated fields (repo, item, harness, message, target) are
//!    interpolated. There is no path from an HTTP body to an arbitrary `bn` subcommand.
//! 2. **Args, never a shell.** Every field is a distinct `argv` element passed straight to
//!    `Command`, so an `item`/`message` containing shell metacharacters is inert — no `sh -c`,
//!    no interpolation, no injection.

use bn_core::render::Envelope;
use serde_json::{json, Value};
use std::path::Path;

/// The outcome of a `bn --json` invocation: whether the process exited 0, and the parsed (or
/// synthesized) response envelope as a JSON value ready to return to the HTTP client.
pub struct RunResult {
    pub ok: bool,
    pub envelope: Value,
}

/// Run `bn --json <args>` (optionally from `cwd`) and return its envelope. Blocking — call from a
/// `tokio::task::spawn_blocking` so the async runtime isn't stalled by the subprocess.
///
/// The `--json` envelope is the LAST JSON-parseable stdout line (some subcommands print a human line
/// first). When a command emits no envelope at all (e.g. `wt merge --all`, which is human-only), a
/// stand-in envelope is synthesized from the exit status and output tail so the HTTP contract still
/// holds.
pub fn run(args: &[&str], cwd: Option<&Path>) -> RunResult {
    run_env(args, cwd, &[])
}

/// Like [`run`], but sets extra environment variables on the child. Used for the cross-repo
/// dispatch actions, which declare `BN_WORKER_REPO=<target>` so the CLI's cross-repo-session guard
/// treats the spawn as the target repo's own session — the dashboard/server runs from its own
/// worktree, which would otherwise look like a stray cross-repo hop and be blocked.
pub fn run_env(args: &[&str], cwd: Option<&Path>, envs: &[(&str, &str)]) -> RunResult {
    let command_name = args.first().copied().unwrap_or("bn").to_string();

    let out = match bn_core::bn_subprocess::run_with_env(args, cwd, envs) {
        Ok(o) => o,
        Err(e) => {
            return RunResult {
                ok: false,
                envelope: serde_json::to_value(Envelope::failure(
                    &command_name,
                    "spawn_failed",
                    format!("failed to run {e}"),
                ))
                .unwrap_or_else(|_| json!({"ok": false, "command": command_name, "error": {"code":"spawn_failed"}})),
            };
        }
    };

    let ok = out.success;

    // Prefer the command's own envelope: the last JSON-parseable stdout line.
    if let Some(env) = out
        .json
        .filter(|value| value.get("ok").is_some() && value.get("command").is_some())
    {
        return RunResult { ok, envelope: env };
    }

    // No envelope (human-only command) — synthesize one from exit + output so callers still get the
    // {ok, command, data, error} shape.
    RunResult {
        ok,
        envelope: synthesize_envelope(ok, &command_name, &out.stdout, &out.stderr),
    }
}

/// Spawn `bn <args>` detached — fire-and-forget for a LONG-RUNNING command whose completion the HTTP
/// request must NOT block on (e.g. `wt pool warm`, which cuts a checkout + runs `build.sh` per slot).
/// Stdio is nulled and the child is reaped in a background thread, so the process runs to completion
/// independently while the handler returns a "started" ack. Returns the child's pid once launched,
/// or an error if the spawn itself fails.
pub fn spawn_detached(args: &[&str]) -> std::io::Result<u32> {
    bn_core::bn_subprocess::spawn_detached(args)
}

/// Build a stand-in `{ok, command, data, error}` envelope for a command that emitted none (e.g.
/// `wt merge --all`, which is human-only), from its exit status and output tail.
fn synthesize_envelope(ok: bool, command: &str, stdout: &str, stderr: &str) -> Value {
    if ok {
        json!({
            "ok": true,
            "command": command,
            "data": { "output": tail(stdout, 4000) },
            "error": null,
        })
    } else {
        let detail = if stderr.trim().is_empty() {
            tail(stdout, 2000)
        } else {
            tail(stderr, 2000)
        };
        json!({
            "ok": false,
            "command": command,
            "data": null,
            "error": { "code": "command_failed", "message": detail },
        })
    }
}

/// Keep only the last `max` chars of `s` (trimmed), so a synthesized envelope carries a useful tail
/// of a long build/merge log without ballooning the response.
fn tail(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let start = s.len() - max;
    // Snap to a char boundary so the slice is valid UTF-8.
    let start = (start..s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    format!("…{}", &s[start..])
}


