//! Characterization snapshots of [`Config::load`] and the config readers beside it
//! ([`JobConfig`], [`ToolsConfig`], the link tags, the MCP extras) — the regression gate for any
//! change to how settings resolve.
//!
//! These pin the *output*, so the layers underneath can be rewritten freely: a precedence bug is
//! silent otherwise, because the wrong default just quietly wins on one machine.
//!
//! Snapshots live in `tests/snapshots/` and are regenerated with:
//!
//! ```sh
//! BN_UPDATE_SNAPSHOTS=1 cargo test -p bn-core --test config_resolve_snapshot
//! ```
//!
//! Review the diff before committing a regeneration — an unreviewed `--update` turns this file
//! from a gate into a rubber stamp.
use bn_core::resolver::{self, Config, JobConfig, ToolsConfig};
use bn_core::test_support::EnvGuard;
use std::path::Path;

/// Resolve `toml` as the only config layer, in an isolated HOME.
///
/// `HOME` and `BN_HOME` are pinned because the defaults derive most paths from them; the hostname
/// is pinned because the machine's name and its config file key off it. Everything else is left
/// alone deliberately — the test asserts what the *file* resolves to, so an env override leaking
/// in would be a real finding, not test noise.
fn resolve_snapshot(toml: &str) -> String {
    bn_core::test_support::with_state_dir_path(|dir| {
        let cfg_path = dir.join("config.toml");
        std::fs::write(&cfg_path, toml).expect("write fixture config");

        let _home = EnvGuard::set("HOME", dir);
        let _bn_home = EnvGuard::set("BN_HOME", dir);
        let _config = EnvGuard::set("BN_CONFIG", &cfg_path);
        let _host = EnvGuard::set("BN_HOSTNAME", "snapshot-host");

        render(dir)
    })
}

/// The `Debug` rendering of every config reader, with each machine-specific path replaced by
/// `<ROOT>` so the snapshot is identical on any checkout.
fn render(root: &Path) -> String {
    let cfg = Config::load();
    let jobs = JobConfig::load();
    let tools = ToolsConfig::load();
    let links = resolver::link_tags_with_source();
    let mcp = resolver::mcp_extras();
    format!(
        "{cfg:#?}\n\n--- JobConfig\n{jobs:#?}\n\n--- ToolsConfig\n{tools:#?}\n\n--- link tags\n{links:#?}\n\n--- mcp extras\n{mcp:#?}\n"
    )
    .replace(&root.to_string_lossy().into_owned(), "<ROOT>")
}

/// Compare against the committed snapshot, or write it when `BN_UPDATE_SNAPSHOTS=1`.
#[allow(
    clippy::panic,
    reason = "a snapshot mismatch must fail loudly with the full diff hint"
)]
fn assert_snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));

    if std::env::var("BN_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing snapshot {}: {error}\nregenerate with BN_UPDATE_SNAPSHOTS=1 cargo test -p bn-core --test config_resolve_snapshot",
            path.display()
        )
    });

    if expected != actual {
        panic!(
            "config snapshot `{name}` changed.\n\
             If the change is intended, regenerate with:\n  \
             BN_UPDATE_SNAPSHOTS=1 cargo test -p bn-core --test config_resolve_snapshot\n\n\
             --- expected\n{expected}\n--- actual\n{actual}"
        );
    }
}

/// The defaults tier on its own: an empty file must still produce a fully-populated `Config`.
/// This is the snapshot that catches a default silently changing during a resolver rewrite.
#[test]
fn empty_config_resolves_to_defaults() {
    assert_snapshot("empty", &resolve_snapshot(""));
}

/// A broad, realistic file touching every section a refactor is likely to disturb: the machine
/// table, the review SLAs, work-repo classification, capture dirs, harness policy, pipeline
/// stages and a per-repo override.
#[test]
fn populated_config_resolves_every_section() {
    assert_snapshot("populated", &resolve_snapshot(POPULATED));
}

/// A real machine's `config.toml` from before settings moved into the notes tree, migrated the way
/// the first `bn` after the upgrade migrates it, must resolve exactly as it did in place. This is
/// the snapshot that proves the move lost nothing.
#[test]
fn the_live_config_resolves_the_same_after_migrating() {
    bn_core::test_support::with_state_dir_path(|dir| {
        let config_dir = dir.join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        std::fs::create_dir_all(dir.join("projects/branch-notes")).expect("notes root");
        std::fs::write(
            config_dir.join("config.toml"),
            include_str!("fixtures/config-live.toml"),
        )
        .expect("seed the old file");

        let _home = EnvGuard::set("HOME", dir);
        let _bn_home = EnvGuard::set("BN_HOME", dir);
        let _host = EnvGuard::set("BN_HOSTNAME", "snapshot-host");
        let _config = EnvGuard::remove("BN_CONFIG");
        let _config_dir = EnvGuard::remove("BN_CONFIG_DIR");
        let _notes = EnvGuard::remove("BN_NOTES_DIR");

        let report = bn_core::config_migrate::run().expect("the old file migrates");
        assert!(report.backup.exists(), "the old file is kept as a backup");
        assert!(!config_dir.join("config.toml").exists());
        assert!(
            bn_core::config_migrate::run().is_none(),
            "a second run has nothing to move"
        );

        assert_snapshot("live", &render(dir));
    })
}

const POPULATED: &str = r#"
[machine]
repos = ["bn", "arena"]
queue = true
cron = true
web = true

[harness]
default = "claude"
disabled = ["claude"]

[work]
work_repos = ["acme", "internal-tool"]

[review]
weekly_anchor = "monday"
sla_first_review_hours = 6
harness = "copilot"

[attention]
max_age_hours = 48

[capture]
screenshot_dir = "/srv/shots"
gif_dir = "/srv/gifs"

[managed_repos]
repos = ["git@github.com:eduuh/bn.git"]

[jobs.only_on]
refresh-all = ["snapshot-host"]

[pipeline.stages.investigate]
model = "gpt-5.4"
effort = "high"

[repo.bn]
verify = "cargo test"
"#;
