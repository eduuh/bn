#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The `--json` contract, checked against the commands that claim it.
//!
//! `bn schema` publishes a hand-written registry, and a hand-written registry is exactly the kind
//! of thing that drifts: a field gets renamed, the doc keeps the old name, and a script built on
//! the doc breaks with no warning from either side. So the registry is not trusted here — every
//! documented command is *run*, in a hermetic sandbox, and its real envelope is compared against
//! what the registry promised.
//!
//! Run with: `cargo test -p bn-e2e --test json_schema -- --ignored --test-threads=1`

use bn_e2e::Sandbox;
use serde_json::Value;

/// The documented field names for one command, as `bn schema <tag> --json` reports them.
///
/// A registry entry reads `name: type — meaning`; only the part before the first colon is the key,
/// and a nested key (`repos[].repo`) documents a member of an array of objects.
fn documented_keys(sb: &Sandbox, tag: &str) -> anyhow::Result<Vec<String>> {
    let run = sb.bn(["schema", tag, "--json"])?;
    run.expect_success()?;
    let v: Value = serde_json::from_str(&run.stdout)?;
    Ok(v["data"]["fields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.as_str())
                .filter_map(|f| f.split(':').next())
                .map(|k| k.trim().to_string())
                .collect()
        })
        .unwrap_or_default())
}

/// Every key actually present in `data`, flattened the way the registry writes them: a top-level
/// key is `name`, and a key inside an array of objects is `name[].inner`.
fn actual_keys(data: &Value, prefix: &str, out: &mut Vec<String>) {
    let Some(map) = data.as_object() else { return };
    for (k, v) in map {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        out.push(path.clone());
        if let Some(first) = v.as_array().and_then(|a| a.first()) {
            actual_keys(first, &format!("{path}[]"), out);
        }
    }
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn the_envelope_carries_its_contract_version() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let run = sb.bn(["goal", "--json"])?;
    run.expect_success()?;
    let v: Value = serde_json::from_str(&run.stdout)?;
    assert_eq!(
        v["v"].as_u64(),
        Some(u64::from(bn_core::render::ENVELOPE_VERSION)),
        "every envelope must carry `v`: {}",
        run.stdout
    );
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["command"].as_str(), Some("goal"));
    assert!(v.get("data").is_some() && v.get("error").is_some());
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn a_failure_envelope_carries_the_version_too() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    // An unknown schema tag is the cheapest failure that goes through `fail_json`.
    let run = sb.bn(["schema", "definitely-not-a-command", "--json"])?;
    let v: Value = serde_json::from_str(&run.stdout)?;
    assert_eq!(
        v["v"].as_u64(),
        Some(u64::from(bn_core::render::ENVELOPE_VERSION))
    );
    assert_eq!(v["ok"].as_bool(), Some(false));
    assert_eq!(v["error"]["code"].as_str(), Some("unknown_command"));
    assert!(v["data"].is_null(), "a failure carries no data");
    Ok(())
}

/// The registry's own listing must agree with the per-command entries it points at.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn every_listed_command_has_a_resolvable_entry() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    let run = sb.bn(["schema", "--json"])?;
    run.expect_success()?;
    let v: Value = serde_json::from_str(&run.stdout)?;
    let listed = v["data"]["commands"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!listed.is_empty(), "the registry documents nothing");
    for c in &listed {
        let tag = c["command"].as_str().unwrap_or_default();
        assert!(
            !documented_keys(&sb, tag)?.is_empty(),
            "{tag} is listed but documents no fields"
        );
    }
    Ok(())
}

/// The drift guard: what a command emits must be what the registry says it emits.
///
/// Only the commands that answer identically in an empty sandbox are checked here — `bn repos`
/// and friends need a populated fleet, and a test that has to build one would be testing the
/// fixture, not the contract.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn documented_shapes_match_what_the_commands_emit() -> anyhow::Result<()> {
    let sb = Sandbox::new()?;
    sb.bn(["goal", "a sandbox goal"])?.expect_success()?;

    for (tag, argv) in [
        ("goal", vec!["goal"]),
        ("repos", vec!["repos", "list"]),
        ("findings-sources", vec!["findings", "sources"]),
    ] {
        let mut args = argv.clone();
        args.push("--json");
        let run = sb.bn(&args)?;
        run.expect_success()?;
        let v: Value = serde_json::from_str(&run.stdout)?;

        let mut actual = Vec::new();
        actual_keys(&v["data"], "", &mut actual);
        let documented = documented_keys(&sb, tag)?;

        for key in &actual {
            assert!(
                documented.contains(key),
                "`{tag}` emits `data.{key}`, which `bn schema {tag}` does not document.\n\
                 Document it, or stop emitting it.\nemitted: {actual:?}\ndocumented: {documented:?}"
            );
        }
    }
    Ok(())
}
