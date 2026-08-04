//! Per-repo MCP wiring driven by the `bn mcp init` CLI command. Writes the committable counterpart to install.sh's global registration: one or more
//! project-scoped MCP configs (a harness like `copilot` covers two editors that read different
//! files) plus a harness instruction block. Every write is idempotent — JSON merges into any
//! existing config, and the instruction block lives between markers so re-running replaces just
//! that block.
//!
//! Beyond bn's own server, a small curated set of planning/productivity extras (#323) can be
//! wired alongside it — see [`EXTRAS`]. Each extra runs through `npx -y` / `uvx`, so the runner
//! fetches (and caches) the package on first launch: install is idempotent and the config stays
//! committable — no machine-local paths. bn's job is to verify the *runner* is on PATH and hand
//! back the exact install command when it isn't (`bn mcp init` warns, `bn doctor` nags).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::resolver;

const BEGIN: &str = "<!-- BEGIN bn (managed by `bn mcp init`) -->";
const END: &str = "<!-- END bn -->";

const MCP_SESSION_TOOLS: &[&str] = &["bn_context", "bn_add", "bn_done", "bn_cat"];

/// The agent-facing instructions dropped into the harness's memory file. Mirrors the MCP
/// server's own `with_instructions`, framed for an editor that has the scoped bn tools available.
const INSTRUCTIONS: &str = "\
## Branch notes (bn)

This repo's branch-note tools are served by a per-worktree stdio launcher (`bn mcp serve`) that
your MCP client spawns automatically. They are scoped to the current worktree's branch note — the
launcher derives its (repo, branch) from your working directory, so nothing needs to be running
first. Keep this branch's note current as you work — don't wait to be asked:

- `bn_context` — compact session-start orientation (goal, counts, top todos, blockers, PR,
  next-step hints) in one call. Call it first.
- `bn_add` — record a todo / blocker / decision / research / collab / ask as it comes up.
- `bn_done` — close a todo the moment it's finished.
- `bn_cat` — read only the current branch note.

This is a deliberately slim tool surface for efficiency. The single-purpose tools (`bn_goal`,
`bn_pr`, `bn_status`, `bn_sync`, `bn_handoff_read`, `bn_handoff_write`) are not advertised over
MCP — use the `bn` CLI (`bn goal`, `bn pr`, `bn status`, `bn sync`, `bn handoff …`) when needed.

The MCP surface intentionally cannot target another repo or branch and does not expose
cross-branch rollups, search, daily goals, reviews, metrics, attention queues, cron status, or
agent fleet/session mining. Use the `bn` CLI explicitly for those broader workflows.

Fleet orchestration is driven natively from the `bn` CLI — no tmux dashboard required. Drive and
monitor agents across repos with `bn orchestrate` (`status`, `supervise [--all-repos]`,
`scale`, `add`, `--queue`, `ask` / `answers`, `backlog --all-repos`) alongside `bn next` and
`bn doctor`.

Timing matters: every mutation is timestamped as it happens, so mark `bn_done` when the work
actually happens — not batched at session end — and only record what genuinely occurred.

Prompts are reusable workflows in `config/prompts/`. In Claude Code they are served as MCP prompts
(`/bn:<name>`); in Copilot CLI they are installed as native skills referred to by their name (not
`/mcp.bn.<name>` — Copilot does not surface MCP prompts). Every effective skill is served, so a
prompt exists for each workflow the tiers define; use the CLI for cross-branch workflows, and note
that some prompts (PR review, orchestration) describe steps whose commands live only in the CLI.
Reference skills sit in `config/skills/` and are referred to by name from a prompt's body
rather than invoked directly.";

/// One curated planning/productivity MCP server (#323). `command` is always a package *runner*
/// (`npx` / `uvx`) so the server installs itself on first launch; `runner_fix` is the exact
/// one-line command to run when that runner is missing from PATH.
#[derive(Debug)]
pub struct Extra {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub runner_fix: &'static str,
}

const NPX_FIX: &str = "install Node.js to get npx — e.g. `brew install node`";
const UVX_FIX: &str = "install uv to get uvx — e.g. `brew install uv`";

/// The curated set — deliberately minimal and vetted: structured planning, cross-session memory,
/// and time/date awareness. Opt in via `bn mcp init --with-extras` or `[mcp] extras` in
/// config.toml.
pub const EXTRAS: &[Extra] = &[
    Extra {
        name: "sequential-thinking",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-sequential-thinking"],
        runner_fix: NPX_FIX,
    },
    Extra {
        name: "memory",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
        runner_fix: NPX_FIX,
    },
    Extra {
        name: "time",
        command: "uvx",
        args: &["mcp-server-time"],
        runner_fix: UVX_FIX,
    },
];

/// Resolve requested extra names against the curated catalog. Unknown names are an error (the
/// set is vetted on purpose) listing what's available.
pub fn resolve_extras(names: &[String]) -> Result<Vec<&'static Extra>, String> {
    names
        .iter()
        .map(|n| {
            EXTRAS.iter().find(|e| e.name == n.as_str()).ok_or_else(|| {
                let known: Vec<&str> = EXTRAS.iter().map(|e| e.name).collect();
                format!(
                    "unknown MCP extra '{n}' — curated set: {}",
                    known.join(", ")
                )
            })
        })
        .collect()
}

/// True when `cmd` resolves against `path_var` (a `PATH`-style list): either an absolute/relative
/// path that exists, or a name found executable in one of the PATH dirs. Pure over `path_var` so
/// it's testable without touching the process env.
pub fn command_on_path(cmd: &str, path_var: &str) -> bool {
    let p = Path::new(cmd);
    if p.components().count() > 1 {
        return is_executable(p);
    }
    std::env::split_paths(path_var).any(|dir| {
        let cand = dir.join(cmd);
        // Windows resolves commands via PATHEXT (`npx` → `npx.cmd`), so a bare-name hit counts.
        is_executable(&cand) || cfg!(windows) && cand.with_extension("cmd").is_file()
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Workspace MCP-config files the **GitHub Copilot CLI** honors, relative to the repo root. The
/// CLI merges these with the user config (`~/.copilot/mcp-config.json`) at startup; we read only
/// the repo-local ones so suppression stays scoped to the dispatched-into project. Both use the
/// `mcpServers` key. (`.vscode/mcp.json` is the VS Code extension's file — the CLI ignores it.)
const COPILOT_WORKSPACE_MCP_FILES: &[&str] = &[".mcp.json", ".github/mcp.json"];

/// The names of the **project-scoped** MCP servers the GitHub Copilot CLI would load for the repo
/// rooted at `worktree`, with bn's own server excluded.
///
/// `bn dispatch` feeds each name to `copilot --disable-mcp-server <name>` so an *unattended* agent
/// boots past a failing or auth-requiring repo MCP instead of hanging on it at startup (#384). An
/// unattended agent can't complete an interactive auth flow, so a project server that blocks the
/// MCP handshake wedges the whole session before it ever reaches its prompt. bn's own server
/// (the conventional name `bn`, or the legacy `bn-mcp` command) is kept — it's the local
/// `bn serve` HTTP endpoint and the source of the branch-note tools the dispatched workflow drives.
///
/// Best-effort and side-effect free: an unreadable or malformed config file is skipped (a dispatch
/// must never fail because a repo's MCP JSON is broken). Returns a de-duplicated, sorted list.
pub fn copilot_project_servers(worktree: &Path) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for rel in COPILOT_WORKSPACE_MCP_FILES {
        let Ok(text) = std::fs::read_to_string(worktree.join(rel)) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            collect_non_bn_servers(&value, &mut names);
        }
    }
    names.into_iter().collect()
}

/// Insert the non-bn server names under a config's `mcpServers` map into `out`. Split out so the
/// name-selection rule is unit-testable without touching the filesystem.
fn collect_non_bn_servers(root: &Value, out: &mut std::collections::BTreeSet<String>) {
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return;
    };
    for (name, entry) in servers {
        if !is_bn_server(name, entry) {
            out.insert(name.clone());
        }
    }
}

/// Whether a configured server entry is bn's own — matched by the conventional server name or,
/// robustly, by the launched command's basename (`bn-mcp`), so a repo that renames the server
/// still keeps it instead of suppressing the branch-note tools. bn's HTTP entry (#640) is always
/// written under the name `bn`, so the name check covers it; a generic "url ends in /mcp" test is
/// deliberately avoided since many third-party servers share that path and must stay suppressed.
fn is_bn_server(name: &str, entry: &Value) -> bool {
    if name == "bn" || name == "bn-mcp" {
        return true;
    }
    entry
        .get("command")
        .and_then(Value::as_str)
        .and_then(|c| Path::new(c).file_name())
        .and_then(|f| f.to_str())
        == Some("bn-mcp")
}

#[derive(Clone, Copy)]
pub enum Harness {
    Claude,
    Copilot,
}

/// Whether `model` is an id the Claude Code CLI accepts as `--model`: a tier alias
/// (`opus`/`sonnet`/`haiku`/`fable`) or an Anthropic id in native *dashed* form
/// (`claude-opus-4-8`). bn's canonical *dotted* ids (`claude-opus-4.8`) and any non-Anthropic id
/// (`gpt-5.4`) are rejected — Claude Code parks on "model does not exist" for them (bn#963). The
/// dashed-vs-dotted split is the same digit-separator convention `normalize_model_id` folds.
fn is_native_claude_model(model: &str) -> bool {
    const ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];
    let lower = model.trim().to_ascii_lowercase();
    ALIASES.contains(&lower.as_str())
        || (lower.starts_with("claude") && !has_dotted_version(&lower))
}

/// True when `s` carries a `<digit>.<digit>` version separator — bn's canonical dotted form, which
/// the Claude CLI can't parse (it wants the dashed `claude-opus-4-8`, not `claude-opus-4.8`).
fn has_dotted_version(s: &str) -> bool {
    let bytes = s.as_bytes();
    s.char_indices().any(|(i, ch)| {
        ch == '.'
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
    })
}

impl Harness {
    pub fn parse(s: &str) -> Option<Harness> {
        match s.to_lowercase().as_str() {
            "claude" | "claude-code" | "cc" => Some(Harness::Claude),
            "copilot" | "vscode" | "code" => Some(Harness::Copilot),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Copilot => "copilot",
        }
    }

    /// The `--model` value to hand this harness for the configured id `model`, or `None` when that
    /// id is not a valid *native* id for the harness — the caller then omits `--model` entirely and
    /// lets the harness fall back to its own default (bn#963). The **Copilot CLI** is a multi-model
    /// gateway that accepts bn's dotted catalog ids (`gpt-5.4`, `claude-sonnet-4.6`, `auto`), so any
    /// non-empty id passes through unchanged. **Claude Code** accepts only its native *dashed* ids
    /// (`claude-opus-4-8`) or tier aliases (`opus`/`sonnet`/…); bn's dotted ids (`claude-opus-4.8`)
    /// and cross-provider ids (`gpt-5.4`) are omitted so a dispatched claude agent still boots.
    pub fn model_arg(self, model: &str) -> Option<&str> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        match self {
            Harness::Copilot => Some(model),
            Harness::Claude => is_native_claude_model(model).then_some(model),
        }
    }

    /// The per-harness "yolo" bypass flags that skip the interactive tool/path approval prompts, so
    /// an unattended/dispatched agent runs without blocking on a permission picker. Single source of
    /// truth: every launch site (interactive, `-p --resume` steer, staff run, review lane) equips
    /// the harness through here so the bypass flag set can never drift between call sites.
    ///
    /// SECURITY: these grant a blanket tool/path bypass and must only be applied to trusted,
    /// self-authored runs — never to a launch that ingests untrusted content (e.g. a review of an
    /// external PR), where a prompt-injected file could otherwise run arbitrary commands. The gate
    /// stays with the caller; this only defines *what* the bypass is.
    pub fn yolo_args(self) -> &'static [&'static str] {
        match self {
            Harness::Copilot => &["--allow-all-tools", "--allow-all-paths"],
            Harness::Claude => &["--dangerously-skip-permissions"],
        }
    }

    /// The MCP-config files to write — one per editor that reads a different file or expects a
    /// different server shape — plus the shared instruction-file path, all relative to the repo
    /// root. `copilot` covers both Copilots, which read different files: the VS Code extension
    /// reads `.vscode/mcp.json` (`servers`); the GitHub Copilot CLI reads `.github/mcp.json`
    /// (`mcpServers`) and ignores the VS Code file. The bn server entry is the per-worktree stdio
    /// launcher `bn mcp serve` for every target — it pins its `(repo, branch)` to the client's cwd;
    /// the concrete entry shape is resolved in [`wire`] per [`Flavor`].
    fn targets(self) -> (Vec<ServerTarget>, &'static str) {
        match self {
            Harness::Claude => (
                vec![ServerTarget::new(".mcp.json", "mcpServers", Flavor::Stdio)],
                "CLAUDE.md",
            ),
            Harness::Copilot => (
                vec![
                    // VS Code Copilot extension (in-editor agent mode).
                    ServerTarget::new(".vscode/mcp.json", "servers", Flavor::Stdio),
                    // GitHub Copilot CLI — workspace config it actually reads; wants the tools list.
                    ServerTarget::new(".github/mcp.json", "mcpServers", Flavor::Local),
                ],
                ".github/copilot-instructions.md",
            ),
        }
    }
}

/// The per-worktree stdio launcher an MCP client spawns for the scoped branch-note tools:
/// `bn mcp serve`. The child (`bn-mcp`) derives its `(repo, branch)` from the launch cwd, so the
/// committed config stays branch-agnostic and needs no per-call scope headers. `bn` is resolved off
/// the client's `PATH` (install.sh puts it there); a from-source checkout can point the generated
/// entry at an absolute path.
const BN_COMMAND: &str = "bn";
const BN_SERVE_ARGS: &[&str] = &["mcp", "serve"];

/// The transport shape a config file expects — decides how an [`Extra`]'s entry is rendered and
/// whether the bn entry carries the Copilot-CLI `tools` allow-list.
#[derive(Clone, Copy)]
enum Flavor {
    /// Claude Code / VS Code Copilot: extras as `{type: "stdio", command, args}`.
    Stdio,
    /// GitHub Copilot CLI: extras as `{type: "local", command, args, tools}`; the bn entry
    /// additionally carries the curated `tools` allow-list.
    Local,
}

impl Flavor {
    fn extra_entry(self, e: &Extra) -> Value {
        match self {
            Flavor::Stdio => json!({ "type": "stdio", "command": e.command, "args": e.args }),
            Flavor::Local => json!({
                "type": "local",
                "command": e.command,
                "args": e.args,
                "tools": ["*"]
            }),
        }
    }

    /// The bn server entry: the per-worktree **stdio** launcher `bn mcp serve`, which pins its
    /// scope to the client's cwd so no per-call headers are needed. Claude / VS Code get a
    /// `{type:"stdio", command, args}` entry; the GitHub Copilot CLI gets `{type:"local", …}` and
    /// additionally pins the curated session-tools allow-list.
    fn bn_entry(self) -> Value {
        match self {
            Flavor::Stdio => json!({
                "type": "stdio",
                "command": BN_COMMAND,
                "args": BN_SERVE_ARGS,
            }),
            Flavor::Local => json!({
                "type": "local",
                "command": BN_COMMAND,
                "args": BN_SERVE_ARGS,
                "tools": MCP_SESSION_TOOLS,
            }),
        }
    }
}

/// One MCP-config file to write: its path (relative to the repo root), the JSON key holding the
/// server map, and the entry shape the bn server and extras take.
struct ServerTarget {
    rel: &'static str,
    key: &'static str,
    flavor: Flavor,
}

impl ServerTarget {
    fn new(rel: &'static str, key: &'static str, flavor: Flavor) -> ServerTarget {
        ServerTarget { rel, key, flavor }
    }
}

/// A file the command created or modified.
pub struct WrittenFile {
    pub path: PathBuf,
    pub action: &'static str, // "created" | "updated" | "unchanged"
}

/// How one requested extra landed: `wired` (inserted into at least one config file) or
/// `already-present` (every target file already had a server under that name — left untouched).
pub struct ExtraStatus {
    pub name: &'static str,
    pub action: &'static str,
}

/// A runner binary (`npx` / `uvx`) that requested extras need but PATH can't resolve, with the
/// exact one-line install command.
pub struct MissingRunner {
    pub runner: &'static str,
    pub fix: &'static str,
}

pub struct InitReport {
    pub harness: &'static str,
    pub repo_root: PathBuf,
    pub files: Vec<WrittenFile>,
    pub extras: Vec<ExtraStatus>,
    pub missing_runners: Vec<MissingRunner>,
}

impl InitReport {
    pub fn to_json(&self) -> Value {
        json!({
            "harness": self.harness,
            "repo_root": self.repo_root.display().to_string(),
            "files": self.files.iter().map(|w| json!({
                "path": w.path.display().to_string(),
                "action": w.action,
            })).collect::<Vec<_>>(),
            "extras": self.extras.iter().map(|e| json!({
                "name": e.name,
                "action": e.action,
            })).collect::<Vec<_>>(),
            "missing_runners": self.missing_runners.iter().map(|m| json!({
                "runner": m.runner,
                "fix": m.fix,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Wire the repo containing `base_dir` for `harness`: the bn server, any requested curated
/// `extras` (see [`EXTRAS`]), and the instruction block. Errors with a human-readable message
/// when `base_dir` is not inside a git repo, an extra name is unknown, or a target file can't
/// be read/written.
pub fn init(base_dir: &Path, harness: Harness, extras: &[String]) -> Result<InitReport, String> {
    wire(base_dir, harness, extras, true)
}

/// Server configs only — no instruction block. The dispatch path uses this to hand a fresh agent
/// worktree the curated extras without dirtying a tracked CLAUDE.md / copilot-instructions.md in
/// the agent's diff.
pub fn wire_servers(
    base_dir: &Path,
    harness: Harness,
    extras: &[String],
) -> Result<InitReport, String> {
    wire(base_dir, harness, extras, false)
}

fn wire(
    base_dir: &Path,
    harness: Harness,
    extras: &[String],
    instructions: bool,
) -> Result<InitReport, String> {
    let root = resolver::repo_root(base_dir)
        .ok_or_else(|| "not in a git repo — run it from inside the repo to wire".to_string())?;
    let extras = resolve_extras(extras)?;

    let (servers, instr_rel) = harness.targets();
    let mut files = Vec::with_capacity(servers.len() + 1);
    let mut wired: Vec<&'static str> = Vec::new();
    for t in &servers {
        let entry = t.flavor.bn_entry();
        let (file, inserted) = write_server_json(&root.join(t.rel), t.key, &entry, |m| {
            merge_extras(m, &extras, t.flavor)
        })?;
        files.push(file);
        wired.extend(inserted);
    }
    if instructions {
        files.push(write_instructions(&root.join(instr_rel))?);
    }

    let extra_status = extras
        .iter()
        .map(|e| ExtraStatus {
            name: e.name,
            action: if wired.contains(&e.name) {
                "wired"
            } else {
                "already-present"
            },
        })
        .collect();

    // Runner check: `npx -y` / `uvx` self-install the package, so the runner itself is the only
    // machine dependency worth verifying. One row per missing runner, not per extra.
    let path_var = std::env::var("PATH").unwrap_or_default();
    let mut missing_runners: Vec<MissingRunner> = Vec::new();
    for e in &extras {
        if !command_on_path(e.command, &path_var)
            && !missing_runners.iter().any(|m| m.runner == e.command)
        {
            missing_runners.push(MissingRunner {
                runner: e.command,
                fix: e.runner_fix,
            });
        }
    }

    Ok(InitReport {
        harness: harness.name(),
        repo_root: root,
        files,
        extras: extra_status,
        missing_runners,
    })
}

/// Insert each extra under its name unless the server map already has one — a user's existing
/// entry (possibly customized) is never clobbered. Returns the names actually inserted.
fn merge_extras(
    servers: &mut Map<String, Value>,
    extras: &[&'static Extra],
    flavor: Flavor,
) -> Vec<&'static str> {
    let mut inserted = Vec::new();
    for e in extras {
        if !servers.contains_key(e.name) {
            servers.insert(e.name.to_string(), flavor.extra_entry(e));
            inserted.push(e.name);
        }
    }
    inserted
}

/// Merge `entry` under `<server_key>.bn` in a JSON config, preserving every other key, then let
/// `merge_more` add further servers to the same map. Creates the file (and parent dirs) when
/// absent; skips the write (action `unchanged`) when nothing changed, so re-runs — and the
/// dispatch path writing into a worktree with these files committed — stay clean.
fn write_server_json<R>(
    path: &Path,
    server_key: &str,
    entry: &Value,
    merge_more: impl FnOnce(&mut Map<String, Value>) -> R,
) -> Result<(WrittenFile, R), String> {
    let existing = match std::fs::read_to_string(path) {
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
    let servers = obj
        .entry(server_key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{}.{server_key} is not a JSON object", path.display()))?;

    servers.insert("bn".to_string(), entry.clone());
    let merged = merge_more(servers);

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

    Ok((
        WrittenFile {
            path: path.to_path_buf(),
            action,
        },
        merged,
    ))
}

/// Drop the bn instruction block into a Markdown memory file between marker comments.
/// Replaces an existing block in place; appends if the file has no block; creates the file
/// when absent. Never touches the user's surrounding prose.
fn write_instructions(path: &Path) -> Result<WrittenFile, String> {
    let block = format!("{BEGIN}\n\n{INSTRUCTIONS}\n\n{END}\n");

    let (content, action) = match std::fs::read_to_string(path) {
        Ok(existing) => match (existing.find(BEGIN), existing.find(END)) {
            (Some(start), Some(end)) if end > start => {
                let end = end + END.len();
                let after = existing[end..].trim_start_matches('\n');
                let mut s = String::from(&existing[..start]);
                s.push_str(&block);
                if !after.is_empty() {
                    s.push('\n');
                    s.push_str(after);
                }
                // Re-running with the current block already in place is a no-op — report it as
                // such instead of a noisy "updated" that didn't change anything.
                let action = if s == existing {
                    "unchanged"
                } else {
                    "updated"
                };
                (s, action)
            }
            _ => {
                let mut s = existing;
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push('\n');
                s.push_str(&block);
                (s, "updated")
            }
        },
        Err(_) => (block, "created"),
    };

    if action != "unchanged" {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(path, content).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }

    Ok(WrittenFile {
        path: path.to_path_buf(),
        action,
    })
}

