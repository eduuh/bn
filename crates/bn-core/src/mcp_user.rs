//! User-scoped MCP server registration, declared once and applied on every machine.
//!
//! [`crate::mcp`] wires *project*-scoped configs that get committed with a repo, and its [`EXTRAS`]
//! catalog is deliberately a closed, vetted set: those entries land in files other people read, so
//! they must stay machine-independent and reviewable. That is the wrong shape for a personal
//! server — an authenticated remote endpoint, a private gateway — which belongs to *you* rather
//! than to any one repo, and which needs to exist in every client on every machine you sit at.
//!
//! install.sh already registers bn (and nvim-mcp) at user scope for all four clients, but only
//! those two, hardcoded in shell. This module makes that registration data-driven: declare a
//! server under `[mcp.servers.<name>]` in the shared `<notes_root>/config.toml` — which already
//! syncs between machines — and `bn mcp sync-user` writes it into whichever of the four client
//! configs exist here.
//!
//! Two things make a *personal* server different from a curated one, and both are handled here:
//!
//! - **Machine-local paths.** `~` and `$HOME` are expanded at write time, so one declaration is
//!   portable across a Mac (`/Users/you`) and a Linux box (`/home/you`). Clients spawn servers
//!   without a shell, so an unexpanded `~` would reach the server process literally.
//! - **Secrets.** A credential must never enter config.toml, because that file is a synced git
//!   repo. Instead a server may declare a [`SecretFile`]: a path plus the field names it holds.
//!   Sync prompts for those fields once per machine and writes the file `0600`; thereafter it is
//!   left alone. The config carries only the *shape* of the secret, never its value.
//!
//! Unlike [`crate::mcp::merge_extras`], which never clobbers an existing entry, sync **overwrites**
//! the entry under each declared name: config.toml is the source of truth, and an edit there is
//! expected to reach every machine. Servers bn does not know about are left untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::mcp::WrittenFile;

/// One user-scoped MCP server, as declared in `[mcp.servers.<name>]`.
///
/// `command`/`args`/`env` describe how to spawn it; the remaining fields are optional policy.
/// `deny_unknown_fields` turns a typo into a loud config error instead of a server that silently
/// never appears — the failure mode is otherwise invisible until an agent asks for a missing tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserServer {
    /// The executable to spawn. Tilde/`$HOME`-expanded, then PATH-checked (a miss is reported,
    /// not fatal — `npx`-style runners are often installed after the fact).
    pub command: String,
    /// Arguments, each tilde/`$HOME`-expanded.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the server process, values tilde/`$HOME`-expanded. Put configuration
    /// here, never credentials — this table lives in the synced config.toml. Use `secret_file`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The Copilot-CLI tool allow-list. Defaults to `["*"]`; only the `copilot-cli` client reads
    /// it, matching how install.sh registers nvim-mcp.
    pub tools: Option<Vec<String>>,
    /// Restrict this server to a subset of [`Client::ALL`] by id. Absent = every client present
    /// on the machine.
    pub clients: Option<Vec<String>>,
    /// A credential file this server needs, provisioned interactively rather than from config.
    pub secret_file: Option<SecretFile>,
}

/// A credential file a server needs but config.toml must never contain.
///
/// Sync writes `path` as a flat JSON object of `fields` — the shape OAuth client-info files take —
/// with mode `0600`, and only when the file is absent. An existing file is never read, rewritten,
/// or validated: re-running sync on a provisioned machine is a no-op, and a hand-edited file
/// stays hand-edited.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFile {
    /// Where the file lives, tilde/`$HOME`-expanded. Keep it outside any git repo.
    pub path: String,
    /// The JSON keys to ask for, in prompt order.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Which of `fields` are secret, so the prompt disables terminal echo for them.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// Where one MCP client keeps its user-scope server map, and what entry shape it expects.
///
/// The four clients are genuinely four products with four config files and three entry shapes;
/// this mirrors the list install.sh already maintains, so both paths register to the same places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// Claude Code — `~/.claude.json`, key `mcpServers`, `{type:"stdio", …}`.
    ClaudeCode,
    /// Claude Desktop — its own config file and no `type` field.
    ClaudeDesktop,
    /// The VS Code Copilot extension — a `mcp.json` per user profile, key `servers`.
    VsCode,
    /// The GitHub Copilot CLI — `~/.copilot/mcp-config.json`, key `mcpServers`, and the only
    /// client that reads a `tools` allow-list.
    CopilotCli,
}

impl Client {
    pub const ALL: &'static [Client] = &[
        Client::ClaudeCode,
        Client::ClaudeDesktop,
        Client::VsCode,
        Client::CopilotCli,
    ];

    /// The stable id used in `[mcp.servers.<name>].clients`.
    pub fn id(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::VsCode => "vscode",
            Client::CopilotCli => "copilot-cli",
        }
    }

    pub fn parse(s: &str) -> Option<Client> {
        Client::ALL.iter().copied().find(|c| c.id() == s)
    }

    /// The JSON key holding the server map.
    fn key(self) -> &'static str {
        match self {
            Client::VsCode => "servers",
            _ => "mcpServers",
        }
    }

    /// Render one server entry in this client's shape.
    fn entry(self, s: &Resolved) -> Value {
        let mut v = match self {
            // Claude Desktop predates the `type` discriminator and rejects nothing, but the other
            // two stdio clients want it stated.
            Client::ClaudeDesktop => json!({ "command": s.command, "args": s.args }),
            Client::CopilotCli => json!({
                "type": "local",
                "command": s.command,
                "args": s.args,
                "tools": s.tools,
            }),
            _ => json!({ "type": "stdio", "command": s.command, "args": s.args }),
        };
        if !s.env.is_empty() {
            let env: Map<String, Value> = s
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            v["env"] = Value::Object(env);
        }
        v
    }
}

/// The config files to write on *this* machine.
///
/// Only paths whose parent directory already exists are returned: a machine without VS Code should
/// not grow a VS Code profile because bn synced a server into it. `~/.copilot` is the one
/// exception install.sh also makes — the Copilot CLI creates it lazily, so the presence of either
/// the directory or the binary counts.
pub fn targets() -> Vec<(Client, PathBuf)> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();

    out.push((Client::ClaudeCode, home.join(".claude.json")));

    for dir in [
        home.join("Library/Application Support/Claude"),
        home.join(".config/Claude"),
    ] {
        if dir.is_dir() {
            out.push((
                Client::ClaudeDesktop,
                dir.join("claude_desktop_config.json"),
            ));
            break;
        }
    }

    for rel in [
        "Library/Application Support/Code/User",
        "Library/Application Support/Code - Insiders/User",
        ".config/Code/User",
        ".config/Code - Insiders/User",
        ".vscode-server/data/User",
        ".vscode-server-insiders/data/User",
    ] {
        let dir = home.join(rel);
        if dir.is_dir() {
            out.push((Client::VsCode, dir.join("mcp.json")));
        }
    }

    let copilot = home.join(".copilot");
    let path_var = std::env::var("PATH").unwrap_or_default();
    if copilot.is_dir() || crate::mcp::command_on_path("copilot", &path_var) {
        out.push((Client::CopilotCli, copilot.join("mcp-config.json")));
    }

    out
}

/// A [`UserServer`] with every machine-local path expanded and defaults applied — what actually
/// gets written.
#[derive(Debug)]
struct Resolved {
    name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    tools: Vec<String>,
    clients: Option<Vec<Client>>,
    secret_file: Option<(PathBuf, SecretFile)>,
}

/// Expand a leading `~` and any `$HOME` / `${HOME}` against the real home directory.
///
/// MCP clients spawn servers directly, with no shell between them, so an unexpanded `~` arrives at
/// the server as a literal character and the path silently does not exist. Expanding at write time
/// is what lets one declaration in the synced config.toml serve a Mac and a Linux box, whose home
/// directories differ.
///
/// `~` expands only as a path *prefix*, matching the shell: a bare `~` or a leading `~/`. It is a
/// legal character elsewhere (URLs, `~user`), so rewriting it mid-string would corrupt values that
/// were already correct. `$HOME` has no such ambiguity and expands anywhere — which is what to
/// reach for inside a composed argument like mcp-remote's `@<path>`. [`check_expanded`] makes the
/// difference loud instead of leaving a half-expanded path to fail at server start.
pub fn expand(s: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return s.to_string();
    };
    let home = home.to_string_lossy().to_string();
    let s = if s == "~" {
        home.clone()
    } else if let Some(rest) = s.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        s.to_string()
    };
    s.replace("${HOME}", &home).replace("$HOME", &home)
}

/// Reject a value that still holds a `~/` after expansion.
///
/// The trap this closes: mcp-remote takes its client-info file as `@<path>`, so `@~/creds.json` is
/// the obvious thing to write — and `~` does not expand there, because it is not a path prefix. The
/// result is a server that registers cleanly and then fails at every launch on a file whose name
/// contains a literal tilde. Erroring here, with the fix in the message, keeps that in the terminal
/// that can act on it rather than in a client's server log.
fn check_expanded(name: &str, what: &str, value: &str) -> Result<(), String> {
    if value.contains("~/") {
        return Err(format!(
            "[mcp.servers.{name}] {what} still contains '~/' after expansion: {value}\n\
             `~` only expands as a path prefix (like the shell). Inside a composed argument use \
             $HOME instead, e.g. `@$HOME/.config/creds.json`."
        ));
    }
    Ok(())
}

fn resolve(name: &str, s: &UserServer) -> Result<Resolved, String> {
    let clients = match &s.clients {
        None => None,
        Some(ids) => {
            let mut v = Vec::with_capacity(ids.len());
            for id in ids {
                v.push(Client::parse(id).ok_or_else(|| {
                    let known: Vec<&str> = Client::ALL.iter().map(|c| c.id()).collect();
                    format!(
                        "[mcp.servers.{name}] unknown client '{id}' — known: {}",
                        known.join(", ")
                    )
                })?);
            }
            Some(v)
        }
    };
    if s.command.trim().is_empty() {
        return Err(format!("[mcp.servers.{name}] needs a non-empty `command`"));
    }
    let secret_file = match &s.secret_file {
        None => None,
        Some(sf) => {
            if sf.fields.is_empty() {
                return Err(format!(
                    "[mcp.servers.{name}.secret_file] needs `fields` — the JSON keys to prompt for"
                ));
            }
            if let Some(bad) = sf.hidden.iter().find(|h| !sf.fields.contains(h)) {
                return Err(format!(
                    "[mcp.servers.{name}.secret_file] `hidden` names '{bad}', which is not in `fields`"
                ));
            }
            let path = expand(&sf.path);
            check_expanded(name, "`secret_file.path`", &path)?;
            Some((PathBuf::from(path), sf.clone()))
        }
    };
    let command = expand(&s.command);
    check_expanded(name, "`command`", &command)?;
    let mut args = Vec::with_capacity(s.args.len());
    for a in &s.args {
        let a = expand(a);
        check_expanded(name, "an argument", &a)?;
        args.push(a);
    }
    let mut env = BTreeMap::new();
    for (k, v) in &s.env {
        let v = expand(v);
        check_expanded(name, &format!("`env.{k}`"), &v)?;
        env.insert(k.clone(), v);
    }
    Ok(Resolved {
        name: name.to_string(),
        command,
        args,
        env,
        tools: s.tools.clone().unwrap_or_else(|| vec!["*".to_string()]),
        clients,
        secret_file,
    })
}

/// Asked for one field of a [`SecretFile`] that is missing on this machine.
///
/// Sync stays free of terminal I/O so it remains testable and usable from a non-interactive
/// caller; the CLI supplies the TTY implementation. Returning `None` (no TTY, EOF, empty input)
/// aborts provisioning that file and is reported as `missing`, never as a half-written credential.
pub trait SecretPrompter {
    fn ask(&mut self, server: &str, path: &Path, field: &str, hidden: bool) -> Option<String>;
}

/// How one declared server landed.
#[derive(Debug)]
pub struct ServerStatus {
    pub name: String,
    /// `wired` (registered in the listed clients) | `no-client` (no matching client on this
    /// machine) | `skipped-no-secret` (its credential is not provisioned).
    pub action: &'static str,
    pub clients: Vec<&'static str>,
}

/// How one declared [`SecretFile`] landed.
#[derive(Debug)]
pub struct SecretStatus {
    pub server: String,
    pub path: PathBuf,
    /// `present` (already there) | `created` | `missing` (not provisioned — no TTY or cancelled).
    pub action: &'static str,
}

#[derive(Debug)]
pub struct SyncReport {
    pub files: Vec<WrittenFile>,
    pub servers: Vec<ServerStatus>,
    pub secrets: Vec<SecretStatus>,
    /// Declared `command`s PATH cannot resolve. Reported, not fatal: a runner is often installed
    /// after the config that needs it.
    pub missing_commands: Vec<String>,
}

impl SyncReport {
    pub fn to_json(&self) -> Value {
        json!({
            "files": self.files.iter().map(|w| json!({
                "path": w.path.display().to_string(),
                "action": w.action,
            })).collect::<Vec<_>>(),
            "servers": self.servers.iter().map(|s| json!({
                "name": s.name,
                "action": s.action,
                "clients": s.clients,
            })).collect::<Vec<_>>(),
            "secrets": self.secrets.iter().map(|s| json!({
                "server": s.server,
                "path": s.path.display().to_string(),
                "action": s.action,
            })).collect::<Vec<_>>(),
            "missing_commands": self.missing_commands,
        })
    }
}

/// Write every declared server into each client config present on this machine.
///
/// Provisions any declared [`SecretFile`] first: a server whose credential could not be written is
/// **not** registered, because a client that spawns it would fail on every launch and surface as a
/// permanently broken server rather than an absent one. Everything else is idempotent — an
/// unchanged file is not rewritten, so this is safe to run from install.sh on every update.
pub fn sync(
    servers: &BTreeMap<String, UserServer>,
    prompter: Option<&mut dyn SecretPrompter>,
) -> Result<SyncReport, String> {
    sync_to(&targets(), servers, prompter)
}

/// [`sync`] against an explicit target list.
///
/// Split out so the interesting behaviour — secret gating, per-client filtering, idempotence — is
/// testable against a temp directory. [`sync`] itself would write to the real `~/.claude.json`.
pub fn sync_to(
    targets: &[(Client, PathBuf)],
    servers: &BTreeMap<String, UserServer>,
    mut prompter: Option<&mut dyn SecretPrompter>,
) -> Result<SyncReport, String> {
    let resolved: Vec<Resolved> = servers
        .iter()
        .map(|(name, s)| resolve(name, s))
        .collect::<Result<_, _>>()?;

    let mut secrets = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    for r in &resolved {
        let Some((path, sf)) = &r.secret_file else {
            continue;
        };
        if path.exists() {
            secrets.push(SecretStatus {
                server: r.name.clone(),
                path: path.clone(),
                action: "present",
            });
            continue;
        }
        let created = match prompter.as_deref_mut() {
            Some(p) => provision(&r.name, path, sf, p)?,
            None => false,
        };
        if !created {
            blocked.push(r.name.clone());
        }
        secrets.push(SecretStatus {
            server: r.name.clone(),
            path: path.clone(),
            action: if created { "created" } else { "missing" },
        });
    }

    let mut files = Vec::new();
    let mut wired: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();

    for (client, path) in targets {
        let applicable: Vec<&Resolved> = resolved
            .iter()
            .filter(|r| !blocked.contains(&r.name))
            .filter(|r| r.clients.as_ref().is_none_or(|cs| cs.contains(client)))
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let written = merge_into(path, client.key(), &applicable, *client)?;
        for r in &applicable {
            wired.entry(r.name.clone()).or_default().push(client.id());
        }
        files.push(written);
    }

    let path_var = std::env::var("PATH").unwrap_or_default();
    let mut missing_commands: Vec<String> = Vec::new();
    for r in &resolved {
        if !crate::mcp::command_on_path(&r.command, &path_var)
            && !missing_commands.contains(&r.command)
        {
            missing_commands.push(r.command.clone());
        }
    }

    let server_status = resolved
        .iter()
        .map(|r| {
            let clients = wired.remove(&r.name).unwrap_or_default();
            ServerStatus {
                name: r.name.clone(),
                action: if blocked.contains(&r.name) {
                    "skipped-no-secret"
                } else if clients.is_empty() {
                    "no-client"
                } else {
                    "wired"
                },
                clients,
            }
        })
        .collect();

    Ok(SyncReport {
        files,
        servers: server_status,
        secrets,
        missing_commands,
    })
}

/// Prompt for each field and write the credential file `0600`. Returns `false` when the operator
/// declined or there was no input — nothing is written in that case, so a cancelled run leaves no
/// half-formed credential behind for a later run to mistake for a provisioned one.
fn provision(
    server: &str,
    path: &Path,
    sf: &SecretFile,
    prompter: &mut dyn SecretPrompter,
) -> Result<bool, String> {
    let mut obj = Map::new();
    for field in &sf.fields {
        let hidden = sf.hidden.contains(field);
        let Some(value) = prompter.ask(server, path, field, hidden) else {
            return Ok(false);
        };
        obj.insert(field.clone(), Value::String(value));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    out.push('\n');
    std::fs::write(path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
    restrict(path)?;
    Ok(true)
}

/// Narrow a freshly written credential file to owner-only.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod 600 {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Merge `servers` into `<server_key>` of a client config, preserving every other key and every
/// server bn did not declare.
///
/// A file that is absent or blank starts from `{}`. A file that is not valid JSON is an error
/// rather than a clobber: VS Code's `mcp.json` may legally contain JSONC comments, and silently
/// replacing a hand-written config would be far worse than skipping it.
fn merge_into(
    path: &Path,
    server_key: &str,
    servers: &[&Resolved],
    client: Client,
) -> Result<WrittenFile, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => None,
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut root: Value = match &existing {
        Some(text) => serde_json::from_str(text).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}) — fix or remove it first",
                path.display()
            )
        })?,
        None => Value::Object(Map::new()),
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    let map = obj
        .entry(server_key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{}.{server_key} is not a JSON object", path.display()))?;

    for r in servers {
        map.insert(r.name.clone(), client.entry(r));
    }

    let mut out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    out.push('\n');

    let action = match &existing {
        Some(text) if *text == out => "unchanged",
        Some(_) => "updated",
        None => "created",
    };
    if action != "unchanged" {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    Ok(WrittenFile {
        path: path.to_path_buf(),
        action,
    })
}

#[cfg(test)]
mod expand_tests {
    //! Path expansion is the difference between one portable declaration and a per-machine one.
    //! Clients spawn servers with no shell between them, so anything left unexpanded arrives
    //! literally and the path simply does not exist — silently, at server-start time.
    use super::expand;

    fn home() -> String {
        dirs::home_dir().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        assert_eq!(
            expand("~/.copilot/x.json"),
            format!("{}/.copilot/x.json", home())
        );
        assert_eq!(expand("~"), home());
    }

    #[test]
    fn both_home_spellings_expand_anywhere_in_the_string() {
        // mcp-remote takes `@<path>`, so the marker is not always at position 0 — an expansion
        // anchored to the start would leave exactly that case broken.
        assert_eq!(expand("@$HOME/c.json"), format!("@{}/c.json", home()));
        assert_eq!(expand("@${HOME}/c.json"), format!("@{}/c.json", home()));
    }

    #[test]
    fn a_tilde_that_is_not_a_path_prefix_is_left_alone() {
        // `~` is a legal character in a URL fragment and an argument value; only the home
        // shorthand (`~` alone or `~/`) may be rewritten.
        assert_eq!(expand("https://x/a~b"), "https://x/a~b");
        assert_eq!(expand("~user/x"), "~user/x");
    }

    #[test]
    fn a_plain_argument_survives_untouched() {
        assert_eq!(expand("--protocol"), "--protocol");
        assert_eq!(expand(r#"{"scope":"mcp"}"#), r#"{"scope":"mcp"}"#);
    }
}

#[cfg(test)]
mod resolve_tests {
    //! Config is hand-edited and its failures are invisible at runtime: a mistyped client id or a
    //! malformed secret declaration would otherwise produce a server that never appears, with no
    //! message anywhere. Every one of these is therefore an error at sync time, not a shrug.
    use super::{resolve, SecretFile, UserServer};

    fn server(command: &str) -> UserServer {
        UserServer {
            command: command.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn tools_default_to_everything_and_clients_to_all_of_them() {
        let r = resolve("forge", &server("npx")).unwrap();
        assert_eq!(r.tools, vec!["*".to_string()]);
        assert!(
            r.clients.is_none(),
            "absent `clients` must mean every client"
        );
    }

    #[test]
    fn an_unknown_client_id_names_the_ones_that_exist() {
        let s = UserServer {
            clients: Some(vec!["vscode".into(), "emacs".into()]),
            ..server("npx")
        };
        let err = resolve("forge", &s).unwrap_err();
        assert!(err.contains("unknown client 'emacs'"), "{err}");
        assert!(
            err.contains("copilot-cli"),
            "the message must list the valid ids: {err}"
        );
    }

    #[test]
    fn an_empty_command_is_rejected_rather_than_written() {
        // A blank command produces a client entry that fails on every launch; catching it here
        // keeps the failure in the terminal that can fix it.
        assert!(resolve("forge", &server("   ")).is_err());
    }

    #[test]
    fn a_secret_file_must_say_which_fields_it_holds() {
        let s = UserServer {
            secret_file: Some(SecretFile {
                path: "~/x.json".into(),
                fields: vec![],
                hidden: vec![],
            }),
            ..server("npx")
        };
        let err = resolve("forge", &s).unwrap_err();
        assert!(err.contains("needs `fields`"), "{err}");
    }

    #[test]
    fn hiding_a_field_that_is_not_prompted_for_is_a_typo_not_a_no_op() {
        // Silently ignoring it is the dangerous reading: `hidden = ["secret"]` against a field
        // actually named `client_secret` would echo the credential to the screen.
        let s = UserServer {
            secret_file: Some(SecretFile {
                path: "~/x.json".into(),
                fields: vec!["client_id".into(), "client_secret".into()],
                hidden: vec!["secret".into()],
            }),
            ..server("npx")
        };
        let err = resolve("forge", &s).unwrap_err();
        assert!(err.contains("not in `fields`"), "{err}");
    }

    #[test]
    fn a_tilde_that_could_not_expand_is_an_error_not_a_broken_server() {
        // The exact trap: mcp-remote takes `@<path>`, so `@~/creds.json` is the natural thing to
        // write — and `~` does not expand there. Unguarded, the server registers fine and then
        // fails at every launch on a filename containing a literal tilde.
        let s = UserServer {
            args: vec!["--static-oauth-client-info".into(), "@~/creds.json".into()],
            ..server("npx")
        };
        let err = resolve("forge", &s).unwrap_err();
        assert!(err.contains("still contains '~/'"), "{err}");
        assert!(
            err.contains("$HOME"),
            "the message must name the fix: {err}"
        );
    }

    #[test]
    fn the_same_argument_written_with_home_resolves_cleanly() {
        let s = UserServer {
            args: vec![
                "--static-oauth-client-info".into(),
                "@$HOME/creds.json".into(),
            ],
            ..server("npx")
        };
        let r = resolve("forge", &s).unwrap();
        assert!(!r.args[1].contains('~'));
        assert!(r.args[1].starts_with('@'));
    }

    #[test]
    fn the_secret_path_is_expanded_like_every_other_path() {
        let s = UserServer {
            secret_file: Some(SecretFile {
                path: "~/.copilot/servers/c.json".into(),
                fields: vec!["client_id".into()],
                hidden: vec![],
            }),
            ..server("npx")
        };
        let r = resolve("forge", &s).unwrap();
        let (path, _) = r.secret_file.unwrap();
        assert!(path.is_absolute(), "got {}", path.display());
    }
}

#[cfg(test)]
mod entry_tests {
    //! The four clients are four products with three entry shapes. Getting one wrong produces a
    //! config the client parses but ignores, which looks identical to bn never having run.
    use super::{resolve, Client, UserServer};

    fn resolved() -> super::Resolved {
        resolve(
            "forge",
            &UserServer {
                command: "npx".into(),
                args: vec!["-y".into(), "mcp-remote".into()],
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn only_the_copilot_cli_gets_a_tools_allow_list() {
        let r = resolved();
        assert_eq!(Client::CopilotCli.entry(&r)["type"], "local");
        assert_eq!(Client::CopilotCli.entry(&r)["tools"][0], "*");
        assert!(Client::ClaudeCode.entry(&r).get("tools").is_none());
        assert!(Client::VsCode.entry(&r).get("tools").is_none());
    }

    #[test]
    fn claude_desktop_takes_no_type_discriminator_but_the_stdio_clients_do() {
        let r = resolved();
        assert!(Client::ClaudeDesktop.entry(&r).get("type").is_none());
        assert_eq!(Client::ClaudeCode.entry(&r)["type"], "stdio");
        assert_eq!(Client::VsCode.entry(&r)["type"], "stdio");
    }

    #[test]
    fn the_server_map_key_differs_between_vs_code_and_everyone_else() {
        assert_eq!(Client::VsCode.key(), "servers");
        for c in [
            Client::ClaudeCode,
            Client::ClaudeDesktop,
            Client::CopilotCli,
        ] {
            assert_eq!(c.key(), "mcpServers");
        }
    }

    #[test]
    fn env_is_emitted_only_when_there_is_some() {
        let r = resolved();
        assert!(Client::ClaudeCode.entry(&r).get("env").is_none());

        let with_env = resolve(
            "forge",
            &UserServer {
                command: "npx".into(),
                env: [("FORGE_REGION".to_string(), "eu".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            Client::ClaudeCode.entry(&with_env)["env"]["FORGE_REGION"],
            "eu"
        );
    }

    #[test]
    fn every_client_id_round_trips_through_parse() {
        // `clients = [...]` in config.toml is matched against these ids; a rename that misses one
        // side would reject a config that was correct yesterday.
        for c in Client::ALL {
            assert_eq!(Client::parse(c.id()), Some(*c));
        }
        assert_eq!(Client::parse("nope"), None);
    }
}

#[cfg(test)]
mod sync_tests {
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
}
