#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The workflow lane: `bn dispatch` driving a real (sandboxed) tmux server with a fake agent.
//!
//! This is the test the harness exists for. `bn dispatch` is the whole tmux workflow in one
//! command — resolve the item, lease a worktree pool slot, branch off the base, seed the branch
//! note, render the seed prompt, open a tmux window and start an agent in it — and none of it was
//! covered, because running it meant creating worktrees and tmux windows on the developer's own
//! machine, with a real agent burning real tokens.
//!
//! Every assertion here is about the *sandbox's* tmux server. The developer's server is checked
//! at teardown ([`Sandbox::assert_host_untouched`]) and must be untouched.
//!
//! Run with: `cargo test -p bn-e2e --test tmux_workflow -- --ignored --test-threads=1`

use bn_e2e::{proof, shims::Behavior, Sandbox};
use std::time::Duration;

macro_rules! require_tmux {
    () => {
        if !proof::tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return Ok(());
        }
    };
}

/// Script both harnesses identically.
///
/// `--agent claude` is a *preference*, not a guarantee: dispatch resolves the final harness as
/// pin > stage default > copilot and then degrades an unavailable one to copilot (dispatch.rs
/// ~1444). Pinning the behaviour of both keeps this test about the tmux workflow rather than about
/// which harness the capability gate happened to pick.
fn script_both_harnesses(sb: &Sandbox, behavior: &Behavior) -> anyhow::Result<()> {
    sb.fake_agent("claude", behavior)?;
    sb.fake_agent("copilot", behavior)?;
    Ok(())
}

/// The happy path: dispatch creates a worktree, a branch, and a tmux window running the agent.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn dispatch_opens_a_tmux_window_running_the_agent() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;
    sb.seed_bare_repo("repo")?;
    // Long-running, so the window is still alive when the assertions run.
    script_both_harnesses(&sb, &Behavior::Hang)?;

    let run = sb.bn(["dispatch", "add a readme", "--agent", "claude", "--yolo"])?;
    run.expect_success()?;

    // The window bn reports is the window that exists — on the sandbox server.
    let window = "repo:dispatch-add-a-readme";
    assert!(
        run.output().contains(window),
        "dispatch should name the window it opened, got:\n{}",
        run.output()
    );
    assert_eq!(
        sb.tmux_sessions()?,
        vec!["repo".to_string()],
        "dispatch should open exactly one session, named for the repo"
    );
    assert_eq!(sb.tmux_windows()?, vec![window.to_string()]);

    // The agent really started inside the pane — not merely that a window was created. The fake
    // agent prints this banner, so seeing it proves PATH resolution reached the shim.
    sb.wait_for(
        "the agent to start in its pane",
        Duration::from_secs(20),
        |sb| {
            sb.capture_pane(window)
                .is_ok_and(|pane| pane.contains("[fake-agent] running"))
        },
    )?;

    // And bn invoked it with a seeded prompt, in the leased worktree — the part that decides
    // whether a dispatched agent actually knows what it was dispatched to do.
    let worktree = sb.path("projects/worktree/repo/_pool/2");
    let launched = sb
        .shim_calls()
        .into_iter()
        .find(|(bin, _, _)| bin == "claude" || bin == "copilot")
        .ok_or_else(|| anyhow::anyhow!("no agent harness was invoked: {:?}", sb.shim_calls()))?;
    assert_eq!(
        launched.1,
        worktree.to_string_lossy(),
        "the agent should run in the leased worktree"
    );
    assert!(
        launched.2.contains("add a readme"),
        "the seed prompt should carry the dispatched goal, got: {}",
        launched.2
    );

    // The worktree is real and on the dispatch branch.
    assert!(
        worktree.is_dir(),
        "expected a leased pool slot at {}",
        worktree.display()
    );
    let branch = sb.git_in(&worktree, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    assert_eq!(
        branch.expect_success()?.stdout.trim(),
        "dispatch/add-a-readme"
    );

    sb.assert_host_untouched()?;
    Ok(())
}

/// A crashing agent leaves a dead pane bn can still inspect.
///
/// The failure path matters more than the happy one: `reap` and the liveness probes decide what to
/// do with a window from what the pane looks like after the agent is gone, and that state was
/// previously only reachable by crashing a real agent on the developer's machine. tmux reports the
/// exit status in a dead pane, so the harness can assert on the agent's exit code end to end.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_crashed_agent_leaves_a_dead_pane_carrying_its_exit_status() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;
    sb.seed_bare_repo("repo")?;
    script_both_harnesses(&sb, &Behavior::Fail(42))?;

    sb.bn([
        "dispatch",
        "handle the error",
        "--agent",
        "claude",
        "--yolo",
    ])?
    .expect_success()?;

    let window = "repo:dispatch-handle-the-error";
    sb.wait_for("the agent's pane to die", Duration::from_secs(20), |sb| {
        sb.capture_pane(window)
            .is_ok_and(|pane| pane.contains("Pane is dead"))
    })?;

    let pane = sb.capture_pane(window)?;
    assert!(
        pane.contains("status 42"),
        "the dead pane should carry the agent's exit status, got: {pane}"
    );

    sb.assert_host_untouched()?;
    Ok(())
}

/// An unscripted agent fails loudly instead of quietly passing.
///
/// The default shim exits 97 precisely so a test that launches a harness it never configured is a
/// visible failure rather than a green run that proved nothing.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn an_unscripted_agent_is_a_loud_failure_not_a_silent_pass() -> anyhow::Result<()> {
    require_tmux!();
    let sb = Sandbox::new()?;
    sb.seed_bare_repo("repo")?;
    // Deliberately no fake_agent call.

    sb.bn(["dispatch", "unscripted", "--agent", "claude", "--yolo"])?
        .expect_success()?;

    sb.wait_for(
        "the default shim to refuse",
        Duration::from_secs(20),
        |sb| {
            sb.capture_pane("repo:dispatch-unscripted")
                .is_ok_and(|pane| pane.contains("status 97"))
        },
    )?;

    sb.assert_host_untouched()?;
    Ok(())
}

/// Two sandboxes run at once without seeing each other — the property that makes the lane
/// parallelisable, and proof that isolation is per-sandbox rather than merely per-machine.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn concurrent_sandboxes_get_separate_tmux_servers_and_ports() -> anyhow::Result<()> {
    require_tmux!();
    let a = Sandbox::new()?;
    let b = Sandbox::new()?;

    assert_ne!(a.port(), b.port(), "each sandbox owns a distinct web port");

    a.tmux(&["new-session", "-d", "-s", "only-in-a"])?;
    b.tmux(&["new-session", "-d", "-s", "only-in-b"])?;

    assert_eq!(a.tmux_sessions()?, vec!["only-in-a".to_string()]);
    assert_eq!(b.tmux_sessions()?, vec!["only-in-b".to_string()]);

    a.assert_host_untouched()?;
    Ok(())
}
