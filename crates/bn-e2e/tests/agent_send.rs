#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The agent-send lane: `TmuxTransport` delivering a message to a fake agent pane on a sandboxed
//! tmux server.
//!
//! The transport is driven in-process rather than through `bn agent send`, because what is under
//! test is the transport's own contract — paste, settle, exactly one CR, and stop at the first
//! failure — and `bn agent send` deliberately layers a harness-specific confirm-and-retry loop on
//! top that would resend the CR to a pane that never shows a spinner. The process env is pointed
//! at the sandbox socket through `EnvGuard`, and the socket is checked before any key is sent.
//!
//! Run with: `cargo test -p bn-e2e --test agent_send -- --ignored --test-threads=1`

use bn_core::agent::{Key, TmuxTransport};
use bn_core::test_support::{env_lock, EnvGuard};
use bn_core::tmux::TmuxError;
use bn_e2e::{proof, Sandbox};
use std::path::Path;
use std::time::Duration;

macro_rules! require_tmux {
    () => {
        if !proof::tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return Ok(());
        }
    };
}

/// Start the fake agent: a shell that appends every line it reads to `log`, one line per CR it
/// receives. A message submitted once is exactly one line; a second CR would add an empty one.
fn start_fake_agent(sb: &Sandbox, log: &Path) -> anyhow::Result<()> {
    let script = format!(
        "while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done",
        log.display()
    );
    // A second session keeps the server alive when the agent's is killed, so a vanished pane
    // reads as a missing target rather than as no server at all.
    for args in [
        vec!["new-session", "-d", "-s", "keepalive"],
        vec![
            "new-session",
            "-d",
            "-s",
            "agent",
            "-n",
            "main",
            "sh",
            "-c",
            &script,
        ],
    ] {
        let out = sb.tmux(&args)?;
        anyhow::ensure!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Point this process's tmux calls at the sandbox server, and prove it before returning: the
/// transport under test sends keys, and a key sent to the developer's server is real damage.
fn sandbox_tmux_env(sb: &Sandbox) -> anyhow::Result<[EnvGuard; 3]> {
    let guards = [
        EnvGuard::remove("TMUX"),
        EnvGuard::remove("TMUX_PANE"),
        EnvGuard::set("TMUX_TMPDIR", sb.path("tmux")),
    ];
    let socket = bn_core::tmux::tmux_try(&["list-sessions", "-F", "#{socket_path}"])
        .map_err(|e| anyhow::anyhow!("sandbox server unreachable in-process: {e}"))?;
    let sandbox_dir = sb.path("tmux").canonicalize()?;
    anyhow::ensure!(
        socket
            .lines()
            .all(|s| Path::new(s).starts_with(&sandbox_dir)),
        "in-process tmux reached {socket:?}, not the sandbox under {}",
        sandbox_dir.display()
    );
    Ok(guards)
}

/// A pasted message arrives whole and is submitted exactly once.
#[test]
#[ignore = "e2e: drives a sandboxed tmux server"]
fn a_message_reaches_the_agent_pane_with_one_submit() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;
    let log = sb.path("agent.log");
    start_fake_agent(&sb, &log)?;
    {
        let _lock = env_lock();
        let _env = sandbox_tmux_env(&sb)?;
        let transport = TmuxTransport::new("agent:main");
        // A leading `-` checks the `--` guard on set-buffer too.
        let msg = "-x hello from bn";
        transport.try_write_keys(&[Key::Text(msg.to_string()), Key::Submit])?;

        sb.wait_for("the agent to log the line", Duration::from_secs(5), |_| {
            std::fs::read_to_string(&log).is_ok_and(|s| s.contains(msg))
        })?;
        // Give a stray second CR time to land before counting lines.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            std::fs::read_to_string(&log)?,
            format!("{msg}\n"),
            "the message should arrive once, followed by exactly one Enter"
        );
    }
    sb.assert_host_untouched()?;
    Ok(())
}

/// A pane that is gone is an error, and nothing is left staged or submitted.
#[test]
#[ignore = "e2e: drives a sandboxed tmux server"]
fn a_vanished_pane_returns_an_error_and_sends_no_enter() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;
    let log = sb.path("agent.log");
    start_fake_agent(&sb, &log)?;
    {
        let _lock = env_lock();
        let _env = sandbox_tmux_env(&sb)?;
        // Resolved while the pane exists, then the pane goes: the window closing mid-steer.
        let transport = TmuxTransport::new("agent:main");
        let killed = sb.tmux(&["kill-session", "-t", "=agent"])?;
        anyhow::ensure!(killed.status.success(), "kill-session failed");

        let err = transport
            .try_write_keys(&[Key::Text("lost".to_string()), Key::Submit])
            .expect_err("sending to a vanished pane must fail");
        assert!(
            matches!(err, TmuxError::NoTarget(_)),
            "a missing pane should classify as NoTarget, got {err:?}"
        );
        assert!(
            !err.is_unreachable(),
            "a missing pane is routine, not a reason to warn"
        );
        let buffers = sb.tmux(&["list-buffers", "-F", "#{buffer_name}"])?;
        assert_eq!(
            String::from_utf8_lossy(&buffers.stdout).trim(),
            "",
            "a failed paste should not leave the message staged in a buffer"
        );
    }
    assert!(!log.exists(), "nothing should have reached the agent's log");
    sb.assert_host_untouched()?;
    Ok(())
}
