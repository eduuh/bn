//! Proof of tmux isolation — the guard that makes this harness safe to run on a developer's box.
//!
//! Isolation here rests on removing `TMUX`/`TMUX_PANE` and pinning `TMUX_TMPDIR`+`HOME`. That is
//! only three variables, but getting it wrong is silent and destructive: most of bn's tmux surface
//! runs on the *default* socket (`agent_lease`, `reap`, `agent_live`, `jump`, `connect`) and calls
//! `kill-window` on windows it matched by enumerating `list-windows -a` — i.e. every window on the
//! server, including ones the developer is using.
//!
//! The observed failure, verified before this harness was written:
//!
//! ```text
//! TMUX_TMPDIR=$SB/tmux HOME=$SB/home  tmux new-session -d -s X   # with $TMUX inherited
//!   → $SB/tmux is EMPTY; session X landed on the developer's real server
//! ```
//!
//! `$TMUX` carries the socket path, and tmux honours it over `TMUX_TMPDIR`. Running the suite from
//! inside a tmux session — the normal way to work — is therefore exactly the case that leaks.
//!
//! So the harness does not *assume* the contract holds; it demonstrates it at construction with a
//! canary session, and refuses to hand out a sandbox if the canary is visible on the host. And it
//! snapshots the host's sessions so a test that destroys one fails the suite loudly instead of
//! being discovered hours later.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::process::Command;

/// Sessions on the *host's* default tmux server, as the ambient environment sees it.
///
/// Deliberately inherits the real environment (including `$TMUX`): this is the server the harness
/// must never touch, and the ambient environment is precisely how a leaking call would reach it.
/// `None` means no reachable host server — no tmux installed, or none running — which is not an
/// error; it just means there is nothing to protect.
pub fn host_sessions() -> Option<Vec<String>> {
    let out = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

/// Whether tmux can be executed at all. Lanes skip rather than fail when it is absent.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run tmux inside the sandbox: `env` applied, [`super::env::REMOVED`] stripped.
///
/// Refuses `kill-server` outright — that one verb must go through [`kill_server`], which checks
/// the environment can only reach a sandbox. Everything else tmux can do is either read-only or
/// *targeted* (`kill-session -t <name>`, `kill-window -t <id>`): bounded to something the caller
/// named, and at worst a nuisance. `kill-server` is the only call whose blast radius is "every
/// session on whichever server this happens to reach", and the only one whose damage cannot be
/// undone — so it is the only one worth making unreachable by accident. Routing it through a
/// single checked door means a future test cannot re-open the hole by calling this directly.
pub fn sandbox_tmux(env: &BTreeMap<String, String>, args: &[&str]) -> Result<std::process::Output> {
    if args.first().is_some_and(|verb| *verb == "kill-server") {
        bail!(
            "refusing `tmux kill-server` via sandbox_tmux: use proof::kill_server, which verifies \
             the environment cannot reach the host's server"
        );
    }
    let mut cmd = Command::new("tmux");
    cmd.args(args);
    for key in super::env::REMOVED {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    Ok(cmd.output()?)
}

/// Kill a sandbox's tmux server, refusing when the environment cannot be shown to target one.
///
/// `kill-server` destroys every session on whichever server it reaches, so it is the one call in
/// this harness that must never be aimed by an unchecked environment. Two conditions are
/// re-verified at the call site itself, cheaply, rather than trusted from construction time:
/// `TMUX_TMPDIR` must be set (or the socket falls back to the shared `/tmp` default), and `TMUX`
/// must be absent from the map (a `TMUX` entry is applied *after* [`super::env::REMOVED`] strips
/// the inherited one, so it would win and redirect the kill at the developer's live server).
///
/// Belt-and-braces with `Sandbox`'s `isolation_proven` gate: that flag records that the canary
/// passed, this checks the environment has not been mutated since.
pub fn kill_server(env: &BTreeMap<String, String>) -> Result<()> {
    if !env.contains_key("TMUX_TMPDIR") {
        bail!(
            "refusing `tmux kill-server`: the environment has no TMUX_TMPDIR, so the socket \
               would resolve to the shared default — the developer's own server"
        );
    }
    if env.contains_key("TMUX") {
        bail!(
            "refusing `tmux kill-server`: the environment sets TMUX, which tmux honours over \
               TMUX_TMPDIR, so the kill would reach the server named by $TMUX"
        );
    }
    // Past the checks, so bypass the chokepoint deliberately — this IS the sanctioned door.
    let mut cmd = Command::new("tmux");
    cmd.arg("kill-server");
    for key in super::env::REMOVED {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()?;
    Ok(())
}

/// Demonstrate that the sandbox tmux server is unreachable from the host, and vice versa.
///
/// Creates a uniquely-named canary session inside the sandbox, then checks three things:
/// the canary is visible on the sandbox socket, it is **not** visible on the host's default
/// socket, and it is not among the sessions the host had before we started. Any failure is fatal:
/// the caller must not proceed to run tests that call `kill-window`.
pub fn assert_isolated(env: &BTreeMap<String, String>, canary: &str) -> Result<()> {
    let created = sandbox_tmux(env, &["new-session", "-d", "-s", canary])?;
    if !created.status.success() {
        bail!(
            "sandbox tmux refused to start a canary session: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        );
    }

    let inside = sandbox_tmux(env, &["list-sessions", "-F", "#{session_name}"])?;
    let inside = String::from_utf8_lossy(&inside.stdout);
    let visible_inside = inside.lines().any(|s| s == canary);

    // The critical check. If the canary shows up here, TMUX_TMPDIR was bypassed and every
    // subsequent `kill-window` in this suite would target the developer's live windows.
    let leaked = host_sessions()
        .unwrap_or_default()
        .iter()
        .any(|s| s == canary);

    let _ = sandbox_tmux(env, &["kill-session", "-t", canary]);

    if leaked {
        bail!(
            "tmux isolation FAILED: canary session '{canary}' is visible on the host's default \
             tmux server. Refusing to run — tests in this lane call kill-window. This means the \
             sandbox environment did not take effect (check that TMUX and TMUX_PANE are removed; \
             TMUX_TMPDIR alone is not sufficient when $TMUX is set)."
        );
    }
    if !visible_inside {
        bail!(
            "tmux isolation is inconclusive: canary session '{canary}' was created but is not \
             listed on the sandbox socket. Refusing to run."
        );
    }
    Ok(())
}

/// Confirm the host's sessions are exactly as they were when the sandbox was created.
///
/// Called at teardown. A test that killed one of the developer's windows shows up here as a
/// missing session — which is the whole point: the damage is reported, not silently absorbed.
pub fn assert_host_untouched(before: &Option<Vec<String>>) -> Result<()> {
    let (Some(before), Some(after)) = (before, host_sessions()) else {
        return Ok(());
    };
    let lost: Vec<&String> = before.iter().filter(|s| !after.contains(s)).collect();
    if !lost.is_empty() {
        bail!(
            "the host's tmux sessions changed while the sandbox ran — these disappeared: {lost:?}. \
             A sandboxed test reached the developer's real tmux server."
        );
    }
    Ok(())
}
