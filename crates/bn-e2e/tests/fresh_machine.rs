#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The fresh-machine lane: bn on a machine nobody configured.
//!
//! A machine used to need a registered machine profile before bn would run anything on it, and a
//! missing one failed silently: cron registered nothing. On one
//! real machine that was eight days of a release job never running. This pins the opposite
//! default: a sandbox whose config names no machine still declares its crons.
//!
//! Run with: `cargo test -p bn-e2e --test fresh_machine -- --ignored --test-threads=1`

use bn_e2e::Sandbox;

/// Rewrite the sandbox config without a `[machine]` table and declare one global cron.
fn unconfigure(sb: &Sandbox) -> anyhow::Result<()> {
    std::fs::write(sb.path("home/.bn/config/config.toml"), "")?;
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
fn a_machine_with_no_profile_runs_cron() -> anyhow::Result<()> {
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

    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn cli_only_install_preserves_agent_commands_and_rejects_web_commands() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    for args in [
        vec!["--help"],
        vec!["mcp", "serve", "--help"],
        vec!["mcp", "serve", "--orchestrate", "--help"],
        vec!["verify", "--help"],
        vec!["gate", "--help"],
    ] {
        sb.bn(args)?.expect_success()?;
    }
    for args in [
        vec!["serve", "--help"],
        vec!["tmux-mcp", "--help"],
        vec!["verify", "--no-ui"],
    ] {
        let run = sb.bn(args)?;
        assert_eq!(
            run.code,
            Some(2),
            "retired web command accepted: {}",
            run.output()
        );
    }
    sb.bn(["goal", "CLI-only smoke test"])?.expect_success()?;
    let note = sb.bn(["goal", "--json"])?;
    note.expect_success()?;
    let json: serde_json::Value = serde_json::from_str(&note.stdout)?;
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["goal"], "CLI-only smoke test");
    Ok(())
}
