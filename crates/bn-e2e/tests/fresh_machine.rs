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

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn review_regression_plan_paths_are_plain_text_unless_json_requested() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    for args in [
        vec!["plan", "init"],
        vec!["plan", "path"],
        vec!["plan", "path", "--agent", "draft-session"],
    ] {
        let plain = sb.bn(&args)?;
        plain.expect_success()?;
        assert!(
            std::path::Path::new(plain.stdout.trim()).is_file(),
            "not a usable plan path: {}",
            plain.stdout
        );
        let mut structured_args = args;
        structured_args.push("--json");
        let structured = sb.bn(structured_args)?;
        structured.expect_success()?;
        let json: serde_json::Value = serde_json::from_str(&structured.stdout)?;
        assert_eq!(json["data"]["path"], plain.stdout.trim());
    }
    Ok(())
}

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
    sb.bn(["task", "add", "CLI-only smoke test"])?
        .expect_success()?;
    sb.bn(["task", "doc", "t1"])?.expect_success()?;
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn fresh_task_plan_and_dispatch_create_no_branch_notes() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    std::fs::write(
        sb.path("home/.bn/config/config.toml"),
        "[machine]\nrepos = [\"repo\", \"atlas\"]\n",
    )?;
    sb.seed_bare_repo("atlas")?;
    sb.bn([
        "task",
        "add",
        "Preserve context",
        "--body",
        "Original task context",
    ])?
    .expect_success()?;
    let doc = sb.bn(["task", "doc", "t1"])?;
    doc.expect_success()?;
    let path = doc.stdout.trim();
    assert!(std::fs::read_to_string(path)?.contains("Original task context"));
    sb.bn([
        "task", "claim", "t1", "--repo", "repo", "--branch", "rebound",
    ])?
    .expect_success()?;
    assert_eq!(sb.bn(["task", "doc", "t1"])?.stdout.trim(), path);
    sb.bn([
        "agent",
        "instruct",
        "repo:rebound",
        "Keep task context",
        "--no-send",
    ])?
    .expect_success()?;
    let listed = sb.bn(["agent", "instruct", "repo:rebound", "--list", "--json"])?;
    listed.expect_success()?;
    let directives: serde_json::Value = serde_json::from_str(&listed.stdout)?;
    assert_eq!(
        directives["data"]["directives"][0]["text"],
        "Keep task context"
    );
    let id = directives["data"]["directives"][0]["id"].as_str().unwrap();
    sb.bn(["agent", "instruct", "repo:rebound", "--drop", id])?
        .expect_success()?;
    sb.bn(["task", "do", "t1"])?.expect_success()?;
    sb.bn(["plan", "init"])?.expect_success()?;
    sb.bn(["plan", "status"])?.expect_success()?;
    sb.bn(["plan", "path"])?.expect_success()?;
    sb.bn([
        "dispatch",
        "Implement task context",
        "--repo",
        "atlas",
        "--branch",
        "fresh-flow",
        "--no-tmux",
    ])?
    .expect_success()?;
    let tasks = std::fs::read_to_string(sb.path("notes/tasks.txt"))?;
    assert!(
        tasks.contains("repo:atlas") && tasks.contains("branch:fresh-flow"),
        "{tasks}"
    );
    let mut pending = vec![sb.path("notes")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                assert!(!matches!(
                    entry.file_name().to_str(),
                    Some("note.md" | "note.yaml" | "handoff.md" | "handoff.json")
                ));
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn task_ingest_scopes_ado_keys_to_the_requested_repo_without_a_branch() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let args = [
        "task", "ingest", "--source", "pull-ado", "--branch", "", "--repo", "Atlas", "--json",
    ];
    let row = r#"[{"text":"Fix work item","key":"atlas:42"}]"#;
    let first = sb.bn_stdin(args, row)?;
    first.expect_success()?;
    let second = sb.bn_stdin(args, row)?;
    second.expect_success()?;
    let outcome: serde_json::Value = serde_json::from_str(&second.stdout)?;
    assert_eq!(outcome["data"]["added"], 0);
    assert_eq!(outcome["data"]["skipped"], 1);
    let tasks = std::fs::read_to_string(sb.path("notes/tasks.txt"))?;
    assert!(tasks.contains("repo:atlas"), "{tasks}");
    assert!(tasks.contains("key:pull-ado:atlas:42"), "{tasks}");
    assert!(!tasks.contains("branch:"), "{tasks}");
    Ok(())
}
