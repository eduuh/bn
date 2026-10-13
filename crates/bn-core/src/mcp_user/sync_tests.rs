//! Sync writes into files other tools own, so the guarantees that matter are about what it
//! does *not* touch, and about never registering a server that cannot start.

use super::{sync_to, Client, SecretFile, SecretPrompter, UserServer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn servers(pairs: Vec<(&str, UserServer)>) -> BTreeMap<String, UserServer> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn forge() -> UserServer {
    UserServer {
        command: "npx".into(),
        args: vec!["-y".into(), "mcp-remote".into()],
        ..Default::default()
    }
}

fn read(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Answers every prompt with a fixed value — the "operator typed it" case.
struct Always(&'static str);
impl SecretPrompter for Always {
    fn ask(&mut self, _: &str, _: &Path, _: &str, _: bool) -> Option<String> {
        Some(self.0.to_string())
    }
}

/// Declines immediately — the "no TTY, or ctrl-D" case.
struct Never;
impl SecretPrompter for Never {
    fn ask(&mut self, _: &str, _: &Path, _: &str, _: bool) -> Option<String> {
        None
    }
}

#[test]
fn a_missing_config_file_is_created_with_the_declared_server() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    let targets = [(Client::CopilotCli, cfg.clone())];

    let report = sync_to(&targets, &servers(vec![("forge", forge())]), None).unwrap();

    assert_eq!(report.files[0].action, "created");
    assert_eq!(report.servers[0].action, "wired");
    assert_eq!(report.servers[0].clients, vec!["copilot-cli"]);
    assert_eq!(read(&cfg)["mcpServers"]["forge"]["command"], "npx");
}

#[test]
fn remote_http_servers_are_registered_only_in_the_selected_clients() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude.json");
    let copilot = tmp.path().join("copilot.json");
    let context7 = UserServer {
        url: Some("https://mcp.context7.com/mcp".into()),
        clients: Some(vec!["claude-code".into(), "copilot-cli".into()]),
        ..Default::default()
    };

    let report = sync_to(
        &[
            (Client::ClaudeCode, claude.clone()),
            (Client::CopilotCli, copilot.clone()),
        ],
        &servers(vec![("context7", context7)]),
        None,
    )
    .unwrap();

    assert!(report.missing_commands.is_empty());
    assert_eq!(
        read(&claude)["mcpServers"]["context7"],
        serde_json::json!({
            "type": "http",
            "url": "https://mcp.context7.com/mcp"
        })
    );
    assert_eq!(
        read(&copilot)["mcpServers"]["context7"],
        serde_json::json!({
            "type": "http",
            "url": "https://mcp.context7.com/mcp",
            "tools": ["*"]
        })
    );
}

#[test]
fn servers_bn_does_not_declare_and_keys_it_does_not_own_are_preserved() {
    // The whole file belongs to the client, not to bn: install.sh's own `bn` entry and any
    // hand-added server must survive a sync, or this command would quietly delete them.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    std::fs::write(
        &cfg,
        r#"{"mcpServers":{"bn":{"command":"bn"}},"theme":"dark"}"#,
    )
    .unwrap();

    sync_to(
        &[(Client::CopilotCli, cfg.clone())],
        &servers(vec![("forge", forge())]),
        None,
    )
    .unwrap();

    let v = read(&cfg);
    assert_eq!(v["mcpServers"]["bn"]["command"], "bn");
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["mcpServers"]["forge"]["command"], "npx");
}

#[test]
fn a_declared_server_is_overwritten_because_config_toml_is_the_source_of_truth() {
    // This is the deliberate difference from `mcp::merge_extras`, which never clobbers. Here
    // an edit in the synced config.toml is *meant* to reach every machine.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    std::fs::write(&cfg, r#"{"mcpServers":{"forge":{"command":"stale"}}}"#).unwrap();

    sync_to(
        &[(Client::CopilotCli, cfg.clone())],
        &servers(vec![("forge", forge())]),
        None,
    )
    .unwrap();

    assert_eq!(read(&cfg)["mcpServers"]["forge"]["command"], "npx");
}

#[test]
fn re_running_changes_nothing_so_install_sh_can_call_it_every_time() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    let targets = [(Client::CopilotCli, cfg)];
    let decl = servers(vec![("forge", forge())]);

    sync_to(&targets, &decl, None).unwrap();
    let second = sync_to(&targets, &decl, None).unwrap();

    assert_eq!(second.files[0].action, "unchanged");
}

#[test]
fn a_config_file_that_is_not_json_is_refused_rather_than_replaced() {
    // VS Code's mcp.json may legally hold JSONC comments. Overwriting a hand-written config
    // would be far worse than skipping it, so this is an error, never a clobber.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp.json");
    std::fs::write(&cfg, "{ // a comment\n }").unwrap();

    let err = sync_to(
        &[(Client::VsCode, cfg.clone())],
        &servers(vec![("forge", forge())]),
        None,
    )
    .unwrap_err();

    assert!(err.contains("not valid JSON"), "{err}");
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "{ // a comment\n }");
}

#[test]
fn clients_narrows_which_configs_a_server_reaches() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("copilot.json");
    let code = tmp.path().join("claude.json");
    let only_cli = UserServer {
        clients: Some(vec!["copilot-cli".into()]),
        ..forge()
    };

    sync_to(
        &[
            (Client::CopilotCli, cli.clone()),
            (Client::ClaudeCode, code.clone()),
        ],
        &servers(vec![("forge", only_cli)]),
        None,
    )
    .unwrap();

    assert!(cli.exists());
    assert!(
        !code.exists(),
        "an excluded client must not even get a file"
    );
}

#[test]
fn a_server_whose_secret_is_missing_is_not_registered_at_all() {
    // The point of the gate: a client that spawns a server with no credential shows a
    // permanently failing server, which is worse and harder to diagnose than an absent one.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    let secret = tmp.path().join("nested/creds.json");
    let s = UserServer {
        secret_file: Some(SecretFile {
            path: secret.to_string_lossy().to_string(),
            fields: vec!["client_id".into(), "client_secret".into()],
            hidden: vec!["client_secret".into()],
        }),
        ..forge()
    };

    let report = sync_to(
        &[(Client::CopilotCli, cfg.clone())],
        &servers(vec![("forge", s)]),
        Some(&mut Never),
    )
    .unwrap();

    assert_eq!(report.servers[0].action, "skipped-no-secret");
    assert_eq!(report.secrets[0].action, "missing");
    assert!(
        !secret.exists(),
        "a declined prompt must leave no partial credential"
    );
    assert!(!cfg.exists());
}

#[test]
fn answering_the_prompt_provisions_the_file_and_registers_the_server() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("mcp-config.json");
    let secret = tmp.path().join("nested/creds.json");
    let s = UserServer {
        secret_file: Some(SecretFile {
            path: secret.to_string_lossy().to_string(),
            fields: vec!["client_id".into(), "client_secret".into()],
            hidden: vec!["client_secret".into()],
        }),
        ..forge()
    };

    let report = sync_to(
        &[(Client::CopilotCli, cfg)],
        &servers(vec![("forge", s)]),
        Some(&mut Always("v")),
    )
    .unwrap();

    assert_eq!(report.secrets[0].action, "created");
    assert_eq!(report.servers[0].action, "wired");
    let creds = read(&secret);
    assert_eq!(creds["client_id"], "v");
    assert_eq!(creds["client_secret"], "v");
}

#[cfg(unix)]
#[test]
fn a_provisioned_credential_is_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let secret = tmp.path().join("creds.json");
    let s = UserServer {
        secret_file: Some(SecretFile {
            path: secret.to_string_lossy().to_string(),
            fields: vec!["client_secret".into()],
            hidden: vec!["client_secret".into()],
        }),
        ..forge()
    };

    sync_to(
        &[(Client::CopilotCli, tmp.path().join("c.json"))],
        &servers(vec![("forge", s)]),
        Some(&mut Always("v")),
    )
    .unwrap();

    let mode = std::fs::metadata(&secret).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "got {:o}", mode & 0o777);
}

#[test]
fn an_existing_credential_is_never_rewritten() {
    // Re-provisioning would overwrite a hand-edited or rotated credential on every install.
    let tmp = tempfile::tempdir().unwrap();
    let secret = tmp.path().join("creds.json");
    std::fs::write(&secret, r#"{"client_id":"mine"}"#).unwrap();
    let s = UserServer {
        secret_file: Some(SecretFile {
            path: secret.to_string_lossy().to_string(),
            fields: vec!["client_id".into()],
            hidden: vec![],
        }),
        ..forge()
    };

    let report = sync_to(
        &[(Client::CopilotCli, tmp.path().join("c.json"))],
        &servers(vec![("forge", s)]),
        Some(&mut Always("overwritten")),
    )
    .unwrap();

    assert_eq!(report.secrets[0].action, "present");
    assert_eq!(read(&secret)["client_id"], "mine");
}

#[test]
fn no_targets_still_reports_the_declared_servers() {
    // A machine with no MCP client installed is not an error; the run is simply a no-op.
    let report = sync_to(
        &[] as &[(Client, PathBuf)],
        &servers(vec![("forge", forge())]),
        None,
    )
    .unwrap();
    assert_eq!(report.servers[0].action, "no-client");
    assert!(report.files.is_empty());
}
