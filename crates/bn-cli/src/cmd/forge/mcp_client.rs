//! stdio transport for the Forge MCP server: spawn, handshake, and tool calls.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Default)]
pub(super) struct StdioForgeMcpClient {
    session: Option<McpSession>,
}

impl StdioForgeMcpClient {
    fn session(&mut self, profile: &ForgeAgentProfile) -> Result<&mut McpSession, ForgeMcpError> {
        if self.session.is_none() {
            self.session = Some(McpSession::start(profile)?);
        }
        self.session
            .as_mut()
            .ok_or_else(|| ForgeMcpError::transport("mcp_session_missing", "bn forge: MCP session was not initialized"))
    }
}

impl ForgeMcpClient for StdioForgeMcpClient {
    fn list_claimable_tasks(
        &mut self,
        profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError> {
        let value = self.session(profile)?.call_tool(
            "list_bn_tasks",
            json!({
                "claimable": true,
                "includeCompletedSinceDays": 0,
            }),
        )?;
        let mut tasks = parse_tool_json::<ForgeListBnTasksResponse>(value)?.items;
        // Forge lists every agent's claimable work; take only what is assigned to this agent.
        tasks.retain(|task| task.is_assigned_to(profile));
        tasks.truncate(limit);
        Ok(tasks)
    }

    fn claim_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        lease_seconds: u64,
    ) -> Result<ForgeClaimActivityResponse, ForgeMcpError> {
        let value = self.session(profile)?.call_tool(
            "claim_activity",
            json!({
                "id": task_id,
                "claimer": profile.claimer.id,
                "leaseSeconds": lease_seconds,
            }),
        )?;
        parse_tool_json(value)
    }

    fn list_all_tasks(
        &mut self,
        profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError> {
        let value = self.session(profile)?.call_tool(
            "list_bn_tasks",
            json!({
                "claimable": false,
                "includeCompletedSinceDays": 0,
            }),
        )?;
        let mut tasks = parse_tool_json::<ForgeListBnTasksResponse>(value)?.items;
        tasks.truncate(limit);
        Ok(tasks)
    }

    fn release_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        reason: &str,
    ) -> Result<(), ForgeMcpError> {
        self.session(profile)?.call_tool(
            "release_activity",
            json!({
                "id": task_id,
                "claimer": profile.claimer.id,
                "reason": reason,
            }),
        )?;
        Ok(())
    }

    fn heartbeat_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        lease_seconds: u64,
    ) -> Result<(), ForgeMcpError> {
        self.session(profile)?.call_tool(
            "heartbeat_activity",
            json!({
                "id": task_id,
                "claimer": profile.claimer.id,
                "leaseSeconds": lease_seconds,
            }),
        )?;
        Ok(())
    }
}

impl Drop for StdioForgeMcpClient {
    fn drop(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.stop();
        }
    }
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Result<Value, String>>,
    stderr_rx: mpsc::Receiver<String>,
    next_id: u64,
}

impl McpSession {
    fn start(profile: &ForgeAgentProfile) -> Result<Self, ForgeMcpError> {
        let launch = McpLaunch::resolve(profile)?;
        let mut child = ProcessCommand::new(&launch.command)
            .args(&launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                ForgeMcpError::transport(
                    "mcp_spawn_failed",
                    format!("bn forge: could not start Forge MCP client '{}': {error}", launch.command),
                )
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            ForgeMcpError::transport("mcp_stdio_unavailable", "bn forge: MCP stdin unavailable")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ForgeMcpError::transport("mcp_stdio_unavailable", "bn forge: MCP stdout unavailable")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ForgeMcpError::transport("mcp_stdio_unavailable", "bn forge: MCP stderr unavailable")
        })?;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = read_mcp_message(&mut reader);
                let done = message.is_err();
                if tx.send(message).is_err() || done {
                    break;
                }
            }
        });

        let (stderr_tx, stderr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = String::new();
            let _ = reader.read_to_string(&mut buf);
            let _ = stderr_tx.send(buf);
        });

        let mut session = Self {
            child,
            stdin,
            rx,
            stderr_rx,
            next_id: 1,
        };
        let _ = session.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "bn-forge",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )?;
        session.notify("notifications/initialized", json!({}))?;
        Ok(session)
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, ForgeMcpError> {
        let response = self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )?;
        if response
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(ForgeMcpError::transport(
                "mcp_tool_error",
                format!("bn forge: Forge MCP tool '{name}' returned an error"),
            ));
        }
        tool_result_json(response).ok_or_else(|| {
            ForgeMcpError::transport(
                "mcp_response_invalid",
                format!("bn forge: Forge MCP tool '{name}' did not return JSON content"),
            )
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, ForgeMcpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            let message = self.read_next()?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown MCP error");
                return Err(ForgeMcpError::transport(
                    "mcp_request_failed",
                    format!("bn forge: MCP request '{method}' failed: {message}"),
                ));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ForgeMcpError> {
        self.write_json(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_json(&mut self, value: Value) -> Result<(), ForgeMcpError> {
        let body = serde_json::to_vec(&value).map_err(|error| {
            ForgeMcpError::transport(
                "mcp_encode_failed",
                format!("bn forge: could not encode MCP request: {error}"),
            )
        })?;
        self.stdin
            .write_all(&body)
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|error| {
                ForgeMcpError::transport(
                    "mcp_write_failed",
                    format!("bn forge: could not write MCP request: {error}"),
                )
            })
    }

    fn read_next(&mut self) -> Result<Value, ForgeMcpError> {
        match self.rx.recv_timeout(Duration::from_secs(45)) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(ForgeMcpError::transport(
                "mcp_read_failed",
                format!("bn forge: could not read MCP response: {error}"),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let stderr = self.stderr_rx.try_recv().unwrap_or_default();
                let detail = if stderr.trim().is_empty() {
                    "no response before timeout".to_string()
                } else {
                    format!("no response before timeout; MCP stderr: {}", oneline(&stderr, 300))
                };
                Err(ForgeMcpError::transport(
                    "mcp_timeout",
                    format!("bn forge: Forge MCP client timed out ({detail})"),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let stderr = self.stderr_rx.try_recv().unwrap_or_default();
                Err(ForgeMcpError::transport(
                    "mcp_exited",
                    format!("bn forge: Forge MCP client exited: {}", oneline(&stderr, 300)),
                ))
            }
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct McpLaunch {
    command: String,
    args: Vec<String>,
}

impl McpLaunch {
    fn resolve(profile: &ForgeAgentProfile) -> Result<Self, ForgeMcpError> {
        if let Some(launch) = copilot_registered_forge_server() {
            return Ok(launch);
        }
        let endpoint = profile
            .forge_mcp
            .endpoint
            .as_deref()
            .ok_or_else(|| ForgeMcpError::metadata_unavailable(profile))?;
        Ok(Self {
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "mcp-remote@latest".to_string(),
                endpoint.to_string(),
                "--protocol".to_string(),
                "auto".to_string(),
            ],
        })
    }
}

fn copilot_registered_forge_server() -> Option<McpLaunch> {
    let path = dirs::home_dir()?.join(".copilot/mcp-config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    let server = root
        .get("mcpServers")?
        .get("forge")
        .filter(|server| server.get("type").and_then(Value::as_str) == Some("local"))?;
    let command = server.get("command")?.as_str()?.to_string();
    let args = server
        .get("args")?
        .as_array()?
        .iter()
        .map(|arg| arg.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(McpLaunch { command, args })
}

pub(super) fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Value, String> {
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Err("EOF".to_string());
    }
    serde_json::from_str(line.trim_end()).map_err(|error| error.to_string())
}

pub(super) fn tool_result_json(value: Value) -> Option<Value> {
    let content = value.get("content")?.as_array()?;
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            let text = item.get("text")?.as_str()?;
            if let Ok(json) = serde_json::from_str(text) {
                return Some(json);
            }
        }
        if item.get("type").and_then(Value::as_str) == Some("json") {
            if let Some(json) = item.get("json") {
                return Some(json.clone());
            }
        }
    }
    None
}

fn parse_tool_json<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ForgeMcpError> {
    serde_json::from_value(value).map_err(|error| {
        ForgeMcpError::transport(
            "mcp_response_invalid",
            format!("bn forge: Forge MCP response did not match expected shape: {error}"),
        )
    })
}

fn oneline(text: &str, max: usize) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    bn_core::render::oneline(line, max)
}
