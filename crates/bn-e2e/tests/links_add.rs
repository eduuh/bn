#![allow(
    clippy::panic_in_result_fn,
    reason = "assertions report test failures; Result propagates sandbox setup errors"
)]

//! Preserve `links add` behavior when its CLI arguments are grouped for the handler.
//! Run with: `cargo test -p bn-e2e --test links_add -- --ignored --test-threads=1`

use bn_e2e::Sandbox;
use serde_json::{json, Value};

fn sandbox() -> anyhow::Result<Sandbox> {
    let sb = Sandbox::new()?;
    std::fs::write(
        sb.path("home/.bn/config/config.toml"),
        "[links]\ntags = [\"knowledge\", \"code\"]\n",
    )?;
    Ok(sb)
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn every_add_option_reaches_the_saved_link() -> anyhow::Result<()> {
    let sb = sandbox()?;

    let run = sb.bn([
        "links",
        "add",
        "https://example.test/guide",
        "--category",
        "knowledge",
        "--name",
        "Project guide",
        "--tags",
        "code,unlisted",
        "--repo",
        "",
        "--ttl",
        "2w",
        "--json",
    ])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["url"], "https://example.test/guide");
    assert_eq!(value["data"]["name"], "Project guide");
    assert_eq!(
        value["data"]["tags"],
        json!(["knowledge", "code", "unlisted"])
    );
    assert_eq!(value["data"]["repo"], Value::Null);
    assert_eq!(value["data"]["expires"], "2026-01-29");
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn omitted_options_still_inherit_the_repo_without_expiry() -> anyhow::Result<()> {
    let sb = sandbox()?;

    let run = sb.bn([
        "links",
        "add",
        "https://example.test/guide",
        "--category",
        "knowledge",
        "--json",
    ])?;
    run.expect_success()?;
    let value: Value = serde_json::from_str(&run.stdout)?;

    assert_eq!(value["data"]["repo"], "repo");
    assert_eq!(value["data"]["tags"], json!(["knowledge"]));
    assert_eq!(value["data"]["expires"], Value::Null);
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn invalid_ttl_fails_before_saving_a_link() -> anyhow::Result<()> {
    let sb = sandbox()?;

    let run = sb.bn([
        "links",
        "add",
        "https://example.test/guide",
        "--category",
        "knowledge",
        "--ttl",
        "invalid",
        "--json",
    ])?;
    let value: Value = serde_json::from_str(&run.stdout)?;
    let listed = sb.bn(["links", "list", "--json"])?;
    listed.expect_success()?;
    let store: Value = serde_json::from_str(&listed.stdout)?;

    assert_eq!(run.code, Some(1));
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "usage");
    assert_eq!(store["data"]["items"], json!([]));
    Ok(())
}

#[test]
#[ignore = "e2e: needs a built bn binary"]
fn the_category_flag_remains_required() -> anyhow::Result<()> {
    let sb = sandbox()?;

    let run = sb.bn(["links", "add", "https://example.test/guide"])?;

    assert_eq!(run.code, Some(2));
    assert!(run.output().contains("--category"));
    Ok(())
}
