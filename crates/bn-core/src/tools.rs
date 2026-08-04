//! Managed "tools" (services) subsystem (issue #653): declaratively deploy and keep long-running
//! local developer tools alive. The first opt-in pilot tool is an **n8n** Docker instance running
//! persistently when enabled.
//!
//! A [`ToolSpec`] declares a docker-backed service (image, ports, volumes, env, restart policy,
//! health URL). The lifecycle operations — deploy, status, reconcile, down — are all driven
//! through the injectable [`CommandRunner`] seam so they are fully unit-testable with
//! [`crate::runner::FakeRunner`] instead of shelling out to a real docker daemon.
//!
//! Built-in specs (currently just `n8n`) ship with bn but are opt-in via `[tools].builtins` /
//! `$BN_TOOLS_BUILTINS`; user-declared tools are persisted as TOML at `<config_dir>/tools.toml`
//! and merged over the enabled built-ins by name.

use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_TIMEOUT: Duration = Duration::from_secs(45);

/// A host↔container port mapping (`-p host:container`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMap {
    /// Host-side port.
    pub host: u16,
    /// Container-side port.
    pub container: u16,
}

/// A volume mount (`-v source:target`). `source` is a named volume or an absolute host path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMap {
    /// Named volume or host path.
    pub source: String,
    /// Mount point inside the container.
    pub target: String,
}

/// A declarative, docker-backed managed tool. Serializable to/from `tools.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Stable tool name (also the container name).
    pub name: String,
    /// Container image reference.
    pub image: String,
    /// Published ports.
    #[serde(default)]
    pub ports: Vec<PortMap>,
    /// Volume mounts.
    #[serde(default)]
    pub volumes: Vec<VolumeMap>,
    /// Environment variables passed with `-e KEY=VALUE`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Docker restart policy (default `always`).
    #[serde(default = "default_restart")]
    pub restart: String,
    /// Optional HTTP healthcheck URL probed via `curl` (a 2xx/3xx means healthy).
    #[serde(default)]
    pub health_url: Option<String>,
}

fn default_restart() -> String {
    "always".to_string()
}

/// The built-in n8n pilot spec (issue #653): a personal persistent workflow-automation server.
/// It is not active unless `[tools].builtins` / `$BN_TOOLS_BUILTINS` includes `n8n`; alternatively,
/// a user can opt in by declaring their own `[[tool]] name = "n8n"` in `tools.toml`.
pub fn n8n_spec() -> ToolSpec {
    let mut env = BTreeMap::new();
    env.insert("TZ".to_string(), "Africa/Nairobi".to_string());
    ToolSpec {
        name: "n8n".to_string(),
        image: "docker.n8n.io/n8nio/n8n".to_string(),
        ports: vec![PortMap {
            host: 5678,
            container: 5678,
        }],
        volumes: vec![VolumeMap {
            source: "n8n_data".to_string(),
            target: "/home/node/.n8n".to_string(),
        }],
        env,
        restart: "always".to_string(),
        health_url: Some("http://localhost:5678/healthz".to_string()),
    }
}

/// Every enabled built-in tool spec that ships with bn.
///
/// Built-ins are an explicit per-machine allowlist (`[tools].builtins`, env:
/// `$BN_TOOLS_BUILTINS`) and default empty so personal tools never reconcile on work machines by
/// accident.
pub fn builtin_specs() -> Vec<ToolSpec> {
    builtin_specs_for(&crate::resolver::ToolsConfig::load().builtins)
}

fn builtin_specs_for(enabled: &[String]) -> Vec<ToolSpec> {
    let wants = |name: &str| enabled.iter().any(|enabled| enabled == name);
    if wants("n8n") {
        vec![n8n_spec()]
    } else {
        Vec::new()
    }
}

/// The `docker run -d` argument vector for a spec (excluding the leading `docker` program).
/// Deterministic ordering so it can be asserted in tests: `run -d --name <n> --restart <r>
/// [-p h:c]… [-v s:t]… [-e K=V]… <image>`.
pub fn docker_run_args(spec: &ToolSpec) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        spec.name.clone(),
        "--restart".to_string(),
        spec.restart.clone(),
    ];
    for p in &spec.ports {
        args.push("-p".to_string());
        args.push(format!("{}:{}", p.host, p.container));
    }
    for v in &spec.volumes {
        args.push("-v".to_string());
        args.push(format!("{}:{}", v.source, v.target));
    }
    // BTreeMap iterates in key order → deterministic.
    for (k, val) in &spec.env {
        args.push("-e".to_string());
        args.push(format!("{k}={val}"));
    }
    args.push(spec.image.clone());
    args
}

/// The container-runtime state of a managed tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// Container is running.
    Running,
    /// Container exists but is not running.
    Stopped,
    /// No container with this name exists.
    Absent,
}

impl RunState {
    /// Human label.
    pub fn label(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Stopped => "stopped",
            RunState::Absent => "absent",
        }
    }
}

/// The health-probe outcome for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    /// Health URL responded 2xx/3xx.
    Healthy,
    /// Health URL failed or returned an error status.
    Unhealthy,
    /// No health URL declared (nothing to probe).
    Unknown,
}

impl Health {
    /// Human label.
    pub fn label(self) -> &'static str {
        match self {
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
            Health::Unknown => "unknown",
        }
    }
}

/// A tool's resolved status: container run-state plus health-probe result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    /// Tool name.
    pub name: String,
    /// Container run-state.
    pub state: RunState,
    /// Health-probe result.
    pub health: Health,
}

/// Resolve the docker binary to use: prefer `docker`, falling back to `docker.exe` (WSL forwards
/// to Docker Desktop on Windows through the `.exe` shim).
pub fn docker_bin() -> &'static str {
    let path = std::env::var("PATH").unwrap_or_default();
    if crate::mcp::command_on_path("docker", &path) {
        "docker"
    } else if crate::mcp::command_on_path("docker.exe", &path) {
        "docker.exe"
    } else {
        // Nothing on PATH — default to `docker`; the runner surfaces the failure to the caller.
        "docker"
    }
}

/// Parse the `docker inspect -f '{{.State.Running}}'` output into a [`RunState`]. Only an explicit
/// missing-container response means absent; an indeterminate inspect must stop reconciliation.
fn parse_run_state(out: &crate::runner::CommandOutput) -> Result<RunState, String> {
    if !out.success {
        let combined = format!("{}{}", out.stderr, out.stdout);
        let lower = combined.to_lowercase();
        if lower.contains("no such object") || lower.contains("no such container") {
            return Ok(RunState::Absent);
        }
        return Err(fail_msg(out));
    }
    match out.stdout.trim() {
        "true" => Ok(RunState::Running),
        "false" => Ok(RunState::Stopped),
        other => Err(format!(
            "unexpected container state from docker inspect: {other:?}"
        )),
    }
}

/// Probe a tool's health URL with `curl`. Returns [`Health::Unknown`] when no URL is declared.
pub fn probe_health<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> Health {
    let Some(url) = spec.health_url.as_deref() else {
        return Health::Unknown;
    };
    // `-f` fails (non-zero) on HTTP >= 400; `-s -S` quiet but keep errors; short timeouts so a
    // hung service doesn't stall status. Success ⇒ healthy.
    let out = runner.run(
        "curl",
        &[
            "-fsS",
            "-o",
            "/dev/null",
            "--max-time",
            "5",
            "--connect-timeout",
            "3",
            url,
        ],
    );
    if out.success {
        Health::Healthy
    } else {
        Health::Unhealthy
    }
}

/// Query the run-state of a tool's container via `docker inspect`.
pub fn run_state<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> Result<RunState, String> {
    let out = runner.run_with_timeout(
        docker_bin(),
        &["inspect", "-f", "{{.State.Running}}", &spec.name],
        INSPECT_TIMEOUT,
    );
    parse_run_state(&out)
}

/// Full status (run-state + health) for a tool.
pub fn status<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> Result<ToolStatus, String> {
    let state = run_state(runner, spec)?;
    // Only probe health when the container is actually running — an absent/stopped tool can't be
    // healthy, and probing would just hang on a refused connection.
    let health = if state == RunState::Running {
        probe_health(runner, spec)
    } else {
        match spec.health_url {
            Some(_) => Health::Unhealthy,
            None => Health::Unknown,
        }
    };
    Ok(ToolStatus {
        name: spec.name.clone(),
        state,
        health,
    })
}

/// The outcome of a deploy/reconcile action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployOutcome {
    /// A fresh container was created via `docker run`.
    Created,
    /// An existing stopped container was started.
    Started,
    /// The container was already running; nothing to do.
    AlreadyRunning,
    /// The action failed; carries the captured stderr/stdout for reporting.
    Failed(String),
}

/// Bring a tool up: create it if absent, start it if stopped, no-op if already running.
/// Idempotent, so it doubles as the reconcile primitive.
pub fn deploy<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> DeployOutcome {
    let docker = docker_bin();
    match run_state(runner, spec) {
        Ok(RunState::Running) => DeployOutcome::AlreadyRunning,
        Ok(RunState::Stopped) => {
            let out = runner.run_with_timeout(docker, &["start", &spec.name], START_TIMEOUT);
            if out.success {
                DeployOutcome::Started
            } else {
                DeployOutcome::Failed(fail_msg(&out))
            }
        }
        Ok(RunState::Absent) => {
            let args = docker_run_args(spec);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = runner.run_with_timeout(docker, &refs, RUN_TIMEOUT);
            if out.success {
                DeployOutcome::Created
            } else {
                DeployOutcome::Failed(fail_msg(&out))
            }
        }
        Err(e) => DeployOutcome::Failed(e),
    }
}

/// Reconcile a tool to the "up" desired state — an alias for [`deploy`] read as the keep-alive
/// primitive the cron loop calls.
pub fn reconcile<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> DeployOutcome {
    deploy(runner, spec)
}

/// Tear a tool down: `docker rm -f <name>` (removes even a running container). Returns `true` on
/// success or when the container was already absent.
pub fn down<R: CommandRunner>(runner: &R, spec: &ToolSpec) -> Result<(), String> {
    let out = runner.run(docker_bin(), &["rm", "-f", &spec.name]);
    if out.success {
        Ok(())
    } else {
        // Already-absent is not an error for an idempotent teardown.
        let combined = format!("{}{}", out.stdout, out.stderr).to_lowercase();
        if combined.contains("no such container") || combined.contains("no such object") {
            Ok(())
        } else {
            Err(fail_msg(&out))
        }
    }
}

fn fail_msg(out: &crate::runner::CommandOutput) -> String {
    let s = out.stderr.trim();
    if !s.is_empty() {
        return s.to_string();
    }
    let s = out.stdout.trim();
    if !s.is_empty() {
        s.to_string()
    } else {
        "command failed".to_string()
    }
}

/// Path to the user tool registry, `<config_dir>/tools.toml`.
pub fn registry_path() -> PathBuf {
    crate::resolver::config_dir().join("tools.toml")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default, rename = "tool")]
    tools: Vec<ToolSpec>,
}

/// Load user-declared tools from `tools.toml` (empty when the file is missing/unreadable).
pub fn load_user_specs(path: &Path) -> Vec<ToolSpec> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<RegistryFile>(&raw)
        .map(|f| f.tools)
        .unwrap_or_default()
}

/// Persist user-declared tools to `tools.toml`, creating the parent directory as needed.
pub fn save_user_specs(path: &Path, tools: &[ToolSpec]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = RegistryFile {
        tools: tools.to_vec(),
    };
    let body = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, body).map_err(|e| e.to_string())
}

/// The effective tool set: enabled built-ins overlaid by any user tools of the same name, plus any
/// user-only tools. Sorted by name for stable output. With no `[tools].builtins` / env opt-in and
/// no `tools.toml`, this is empty.
pub fn resolve_specs(user: Vec<ToolSpec>) -> Vec<ToolSpec> {
    resolve_specs_with_builtins(user, builtin_specs())
}

fn resolve_specs_with_builtins(user: Vec<ToolSpec>, builtins: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut by_name: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for spec in builtins {
        by_name.insert(spec.name.clone(), spec);
    }
    for spec in user {
        by_name.insert(spec.name.clone(), spec);
    }
    by_name.into_values().collect()
}

/// All effective specs, reading the on-disk registry and the built-in allowlist.
///
/// On machines that have not opted in to built-ins and have no `tools.toml`, this returns an empty
/// list, so `tools-reconcile` is a clean no-op.
pub fn all_specs() -> Vec<ToolSpec> {
    resolve_specs(load_user_specs(&registry_path()))
}

/// Find one effective spec by name.
pub fn find_spec(name: &str) -> Option<ToolSpec> {
    all_specs().into_iter().find(|s| s.name == name)
}


