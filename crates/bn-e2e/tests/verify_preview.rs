#![allow(
    clippy::panic_in_result_fn,
    reason = "assertions report test failures; Result propagates sandbox setup errors"
)]

//! Verification preflight through the real CLI, including its no-write contract.
//! Run with: `cargo test -p bn-e2e --test verify_preview -- --ignored --test-threads=1`

use bn_e2e::Sandbox;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn configure_command(sb: &Sandbox, command: &str) -> anyhow::Result<()> {
    std::fs::write(
        sb.path("home/.bn/config/config.toml"),
        format!(
            "[repo.repo]\nverify = {}\n",
            serde_json::to_string(command)?
        ),
    )?;
    Ok(())
}

fn job(sb: &Sandbox, name: &str, script: &str) -> anyhow::Result<PathBuf> {
    let dir = sb.path("home/.bn/config/jobs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.sh"));
    std::fs::write(&path, script)?;
    Ok(path)
}

fn files(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            result.insert(path.clone(), Vec::new());
            result.extend(files(&path)?);
        } else {
            result.insert(path.clone(), std::fs::read(path)?);
        }
    }
    Ok(result)
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn configured_command_preview_does_not_execute_or_write_verification_state() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let command = "printf 'must not run' > executed; exit 7";
    configure_command(&sb, command)?;
    sb.bn(["goal", "--json"])?.expect_success()?;
    let notes = files(&sb.path("notes"))?;
    let state = files(&sb.path("state"))?;

    let run = sb.bn(["verify", "--dry-run", "--json"])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;
    let cwd = value["data"]["cwd"].as_str().expect("preview cwd");

    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "verify");
    assert_eq!(value["data"]["dry_run"], true);
    assert_eq!(value["data"]["job"], command);
    assert_eq!(
        std::fs::canonicalize(cwd)?,
        std::fs::canonicalize(sb.repo())?
    );
    assert_eq!(
        value["data"]["steps"],
        json!([{"kind": "command", "label": command, "command": command}])
    );
    assert!(!sb.repo().join("executed").exists());
    assert_eq!(files(&sb.path("notes"))?, notes);
    assert_eq!(files(&sb.path("state"))?, state);
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn conventional_jobs_are_previewed_in_execution_order() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let lint = job(&sb, "lint", "touch lint-ran")?;
    let build = job(&sb, "build", "touch build-ran")?;
    let test = job(&sb, "test", "touch test-ran")?;

    let run = sb.bn(["verify", "--dry-run", "--json"])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(value["data"]["job"], "build+test+lint");
    assert_eq!(
        value["data"]["steps"],
        json!([
            {"kind": "script", "label": "build", "path": build},
            {"kind": "script", "label": "test", "path": test},
            {"kind": "script", "label": "lint", "path": lint}
        ])
    );
    assert!(!sb.repo().join("build-ran").exists());
    assert!(!sb.repo().join("test-ran").exists());
    assert!(!sb.repo().join("lint-ran").exists());
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn configured_command_takes_precedence_over_conventional_jobs() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    configure_command(&sb, "native-check --all")?;
    job(&sb, "build", "exit 1")?;
    job(&sb, "test", "exit 1")?;

    let run = sb.bn(["verify", "--dry-run", "--json"])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(
        value["data"]["steps"],
        json!([{"kind": "command", "label": "native-check --all", "command": "native-check --all"}])
    );
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn explicit_job_takes_precedence_over_configured_command() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    configure_command(&sb, "touch native-ran")?;
    let path = job(&sb, "lint", "touch lint-ran")?;

    let run = sb.bn(["verify", "--dry-run", "--job", "lint", "--json"])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(value["data"]["job"], "lint");
    assert_eq!(
        value["data"]["steps"],
        json!([{"kind": "script", "label": "lint", "path": path}])
    );
    assert!(!sb.repo().join("native-ran").exists());
    assert!(!sb.repo().join("lint-ran").exists());
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn plain_preview_names_the_directory_and_script_without_claiming_a_pass() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let path = job(&sb, "test", "touch test-ran")?;

    let run = sb.bn(["verify", "--dry-run"])?;
    run.expect_success()?;

    assert!(run.stdout.contains("DRY RUN"));
    assert!(run.stdout.contains(sb.repo().to_str().unwrap()));
    assert!(run.stdout.contains(path.to_str().unwrap()));
    assert!(!run.stdout.contains("PASS"));
    assert!(!sb.repo().join("test-ran").exists());
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn missing_explicit_job_fails_instead_of_previewing_the_configured_command() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    configure_command(&sb, "touch native-ran")?;

    let run = sb.bn(["verify", "--dry-run", "--job", "missing"])?;

    assert_eq!(run.code, Some(1));
    assert!(run.output().contains("no 'missing' job"));
    assert!(!sb.repo().join("native-ran").exists());
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn an_unconfigured_gate_is_not_a_successful_preview() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;

    let run = sb.bn(["verify", "--dry-run"])?;

    assert_eq!(run.code, Some(1));
    assert!(run.output().contains("no build, test, or lint job"));
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn preview_cannot_be_used_to_capture_proof_or_preview_the_pre_land_gate() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;

    let proof = sb.bn(["verify", "--dry-run", "--for", "w1"])?;
    let gate = sb.bn(["gate", "--dry-run"])?;

    assert_eq!(proof.code, Some(2));
    assert!(proof.output().contains("cannot be used with"));
    assert_eq!(gate.code, Some(2));
    assert!(gate.output().contains("unexpected argument '--dry-run'"));
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn ordinary_verification_still_runs_and_returns_its_existing_result_shape() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    configure_command(&sb, "touch executed; exit 7")?;

    let run = sb.bn(["verify", "--json"])?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(run.code, Some(7));
    assert_eq!(value["data"]["pass"], false);
    assert_eq!(value["data"]["code"], 7);
    assert!(value["data"].get("proof").is_some());
    assert!(value["data"].get("dry_run").is_none());
    assert!(sb.repo().join("executed").exists());
    Ok(())
}
