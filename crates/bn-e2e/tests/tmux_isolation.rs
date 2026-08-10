#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The safety lane: proof that a sandboxed bn cannot see or touch the developer's tmux server.
//!
//! These tests come first because every other tmux test depends on them. bn's fleet surface —
//! `agent_lease`, `reap`, `agent_live`, `jump`, `connect` — runs on the *default* tmux socket and
//! calls `kill-window` on windows it matched by enumerating the whole server. If isolation is
//! broken, the rest of the suite does not fail; it closes the developer's windows.
//!
//! Run with: `cargo test -p bn-e2e -- --ignored --test-threads=1`
//! (`#[ignore]` keeps them out of the plain `cargo test --workspace` unit lane, which has no
//! built `bn` binary to point at.)

use bn_e2e::{proof, Sandbox};

/// Skip rather than fail where tmux is absent — the harness is still useful without it.
macro_rules! require_tmux {
    () => {
        if !proof::tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return Ok(());
        }
    };
}

/// The sandbox's tmux server and the host's are mutually invisible.
///
/// `Sandbox::new` already refuses to build if the sandbox→host direction leaks; this asserts it
/// as a test in its own right, and adds the host→sandbox direction, so a regression in the
/// environment contract is reported here by name instead of as a confusing failure elsewhere.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn sandbox_and_host_tmux_servers_are_mutually_invisible() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;

    let marker = format!("bn-e2e-visible-{}", std::process::id());
    sb.tmux(&["new-session", "-d", "-s", &marker])?;

    assert!(
        sb.tmux_sessions()?.contains(&marker),
        "the sandbox server should list its own session"
    );

    // The direction that matters: nothing the sandbox creates may appear on the host.
    let host = proof::host_sessions().unwrap_or_default();
    assert!(
        !host.contains(&marker),
        "sandbox session '{marker}' leaked onto the host tmux server (host sessions: {host:?})"
    );

    // And the reverse, for free, whenever the developer has sessions open: the sandbox must not
    // be able to enumerate them. Vacuous on a bare CI box with no tmux server, which is fine —
    // there is nothing to leak there.
    if !host.is_empty() {
        let inside = sb.tmux_sessions()?;
        let leaked: Vec<&String> = host.iter().filter(|s| inside.contains(s)).collect();
        assert!(
            leaked.is_empty(),
            "the sandbox can see the host's tmux sessions: {leaked:?}"
        );
    }

    sb.assert_host_untouched()?;
    Ok(())
}

/// `bn fleet` enumerates tmux across every session on the default socket. In a sandbox it must
/// report nothing, even on a machine with a live fleet.
///
/// This is the containment test for the whole default-socket surface: `bn fleet` is the read-only
/// member of the same family as `reap`'s `kill-window`, so if it is contained, so are they.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn bn_fleet_in_a_sandbox_cannot_see_the_hosts_agents() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;

    let run = sb.bn(["fleet"])?;
    let out = run.output();

    assert!(
        out.contains("No live agents"),
        "a fresh sandbox should have no fleet, got:\n{out}"
    );
    // Nothing from the real machine may appear. The developer's worktrees live under their real
    // home; every sandbox path lives under the sandbox root.
    assert!(
        !out.contains("/Users/") && !out.contains("/home/"),
        "bn fleet leaked host paths into a sandbox run:\n{out}"
    );

    sb.assert_host_untouched()?;
    Ok(())
}

/// The isolation guard detects a real leak — it is not merely always-passing.
///
/// A safety check that cannot fail is worse than none, because it is believed. This reproduces the
/// exact leak the contract exists to prevent: `$TMUX` carries the socket path, and tmux honours it
/// over `TMUX_TMPDIR`, so a suite run from inside a tmux session drives the developer's live
/// server despite a pinned `TMUX_TMPDIR`. Injecting `TMUX` back into the sandbox environment (it
/// is applied after [`bn_e2e::env::REMOVED`] is stripped, so it wins) must make the guard refuse.
///
/// The canary briefly lands on the host server; `assert_isolated` kills it on the way out.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn the_isolation_guard_refuses_when_tmux_leaks_to_the_host() -> anyhow::Result<()> {
    require_tmux!();
    let Some(host_socket) = host_socket_path() else {
        eprintln!("skipping: no host tmux server to leak into");
        return Ok(());
    };
    let before = proof::host_sessions().unwrap_or_default();

    let root = tempfile::tempdir()?;
    let mut env = bn_e2e::env::contract(root.path(), 7999, 0, "2026-01-15");
    std::fs::create_dir_all(root.path().join("tmux"))?;
    // Re-introduce the leak the contract removes.
    env.insert("TMUX".into(), format!("{host_socket},1,0"));

    let canary = format!("bn-e2e-leakcheck-{}", std::process::id());
    let verdict = proof::assert_isolated(&env, &canary);

    assert!(
        verdict.is_err(),
        "the guard passed despite $TMUX pointing at the host socket — it cannot detect a leak"
    );
    let message = verdict.unwrap_err().to_string();
    assert!(
        message.contains("isolation FAILED"),
        "expected an isolation failure, got: {message}"
    );

    // The guard's own cleanup must have removed the canary from the host.
    let after = proof::host_sessions().unwrap_or_default();
    assert!(
        !after.contains(&canary),
        "the leak-check canary was left behind on the host server"
    );
    assert!(
        before.iter().all(|s| after.contains(s)),
        "the leak check disturbed the host's sessions: before {before:?}, after {after:?}"
    );
    Ok(())
}

/// Regression: `kill-server` refuses an environment that could reach the host.
///
/// The bug this pins down destroyed a developer's live tmux sessions. `Sandbox::new` used to build
/// the struct *before* proving isolation, so a contract that failed to strip `$TMUX` made the
/// guard return an error — which dropped the half-built sandbox — which ran `Drop` — which called
/// `tmux kill-server` with the environment the guard had just rejected. The guard refused the run
/// and then killed every session on the machine on its way out.
///
/// Two fixes, both asserted here: the check now runs before anything droppable exists, and the
/// kill itself re-verifies its environment. This test exercises the second, which is the one that
/// can be checked without endangering the host — it asserts on the refusal, and never runs tmux.
#[test]
fn kill_server_refuses_an_environment_that_could_reach_the_host() {
    let root = std::path::Path::new("/tmp/bn-e2e-killcheck");

    // A `TMUX` entry wins over the stripped inherited one and redirects the kill at the host.
    let mut leaky = bn_e2e::env::contract(root, 7999, 0, "2026-01-15");
    leaky.insert("TMUX".into(), "/private/tmp/tmux-502/default,1,0".into());
    let refused = proof::kill_server(&leaky).expect_err("must refuse a TMUX-carrying environment");
    assert!(
        refused.to_string().contains("sets TMUX"),
        "the refusal should name the cause, got: {refused}"
    );

    // Without TMUX_TMPDIR the socket falls back to the shared default — also the host's server.
    let mut rootless = bn_e2e::env::contract(root, 7999, 0, "2026-01-15");
    rootless.remove("TMUX_TMPDIR");
    let refused = proof::kill_server(&rootless).expect_err("must refuse without TMUX_TMPDIR");
    assert!(
        refused.to_string().contains("no TMUX_TMPDIR"),
        "the refusal should name the cause, got: {refused}"
    );

    // And the checked door cannot be walked around: the general-purpose runner rejects the verb
    // outright, so a future caller cannot reintroduce the bug by reaching for `sandbox_tmux`.
    let safe = bn_e2e::env::contract(root, 7999, 0, "2026-01-15");
    let refused = proof::sandbox_tmux(&safe, &["kill-server"])
        .expect_err("sandbox_tmux must not carry kill-server, even with a good environment");
    assert!(
        refused.to_string().contains("use proof::kill_server"),
        "the refusal should point at the checked door, got: {refused}"
    );
}

/// The host's default tmux socket path, or `None` when no server is running.
fn host_socket_path() -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#{socket_path}"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A fake agent replaces the real harness, and bn's own subprocess spawn resolves to it.
///
/// Without this the tmux lane would either burn tokens or fail on a missing binary. It also
/// records argv, so later tests can assert *how* bn invoked the agent, not just that it did.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_fake_agent_shim_shadows_the_real_harness() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    sb.fake_agent(
        "claude",
        &bn_e2e::shims::Behavior::Script("echo shimmed-ok\nexit 0\n".into()),
    )?;

    let out = std::process::Command::new("claude")
        .arg("--version")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", sb.path("shims").display()),
        )
        .output()?;

    assert!(
        String::from_utf8_lossy(&out.stdout).contains("shimmed-ok"),
        "the shim did not shadow the real binary"
    );
    let calls = sb.shim_calls();
    assert_eq!(calls.len(), 1, "the shim should record its invocation");
    assert_eq!(calls[0].0, "claude");
    assert!(calls[0].2.contains("--version"), "argv should be recorded");
    Ok(())
}
