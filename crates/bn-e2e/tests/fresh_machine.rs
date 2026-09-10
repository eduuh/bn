#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The fresh-machine lane: bn on a machine nobody configured.
//!
//! A machine used to need a registered machine profile before bn would run anything on it, and a
//! missing one failed silently: cron registered nothing, the queue refused, and the dashboard would
//! not start. On one real machine that was eight days of a release job never running. This pins
//! the opposite default: a sandbox whose config names no machine still declares its crons, answers
//! `bn queue`, and reports on `bn serve`.
//!
//! Run with: `cargo test -p bn-e2e --test fresh_machine -- --ignored --test-threads=1`

use bn_e2e::Sandbox;

/// Rewrite the sandbox config without a `[machine]` table, keeping only what isolates the sandbox
/// (its dashboard port), and declare one cron that has no repo to be bound to.
fn unconfigure(sb: &Sandbox) -> anyhow::Result<()> {
    std::fs::write(
        sb.path("home/.bn/config/config.toml"),
        format!("[web]\nport = {}\nbind = \"127.0.0.1\"\n", sb.port()),
    )?;
    // `persistent = false`, so no catch-up pass tries to run a job that has no script.
    std::fs::write(
        sb.path("home/.bn/config/crons.toml"),
        "[[cron]]\njob = \"fresh-machine-probe\"\non_calendar = \"*-*-* 03:00:00\"\n\
         persistent = false\ndescription = \"e2e probe\"\n",
    )?;
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_machine_with_no_profile_runs_cron_queue_and_web() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    unconfigure(&sb)?;

    let cron = sb.bn(["cron", "status"])?.output();
    assert!(
        !cron.contains("not enabled") && !cron.contains("switched off"),
        "cron refused an unconfigured machine:\n{cron}"
    );
    assert!(
        cron.contains("fresh-machine-probe"),
        "the declared cron is missing from a machine with no profile:\n{cron}"
    );

    let queue = sb.bn(["queue", "status", "--json"])?.output();
    assert!(
        !queue.contains("not_enabled"),
        "the queue refused an unconfigured machine:\n{queue}"
    );

    let web = sb.bn(["serve", "status", "--json"])?.output();
    assert!(
        !web.contains("not_enabled"),
        "the dashboard refused an unconfigured machine:\n{web}"
    );
    Ok(())
}
