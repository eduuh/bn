//! The Forge MCP endpoint, spoken over streamable HTTP, and the tool surface bn uses from it.
//!
//! [`McpHttp`] is protocol only: JSON-RPC 2.0 over POST, `initialize` + `notifications/initialized`
//! once, the `Mcp-Session-Id` the server hands back replayed on every later call, and responses
//! read as either `application/json` or a `text/event-stream` whose `data:` lines carry the
//! JSON-RPC message. A session the server has forgotten (HTTP 404) is re-initialised once.
//!
//! [`ForgeApi`] names the Forge tools the task store needs, with typed arguments and results, so
//! [`super::store::ForgeTaskStore`] never touches JSON-RPC and its tests run against a fake.
//! [`HttpForgeApi`] is the real implementation over [`McpHttp`].
//!
//! The key is sent only as `Authorization: Bearer …`. It is never part of an error message, and
//! nothing here logs.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

/// The MCP protocol revision bn asks for. The server answers with the one it speaks, and that
/// answer is what later requests carry.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Per-request ceiling. The store runs inside CLI commands and the tick, so a hung endpoint must
/// fail rather than wedge them.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The server's `initialize` answer, reduced to what `bn forge status` shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone)]
struct Session {
    id: Option<String>,
    protocol_version: String,
}

/// A blocking MCP client for one endpoint and one key.
pub struct McpHttp {
    url: String,
    token: String,
    agent: ureq::Agent,
    session: Mutex<Option<Session>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for McpHttp {
    /// Never prints the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHttp")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// One HTTP exchange, reduced to what the protocol layer reads.
struct Reply {
    status: u16,
    content_type: String,
    session_id: Option<String>,
    body: String,
}

impl McpHttp {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> McpHttp {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        McpHttp {
            url: url.into(),
            token: token.into(),
            agent,
            session: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Open (or reopen) the session and return what the server says about itself.
    pub fn initialize(&self) -> io::Result<ServerInfo> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "bn", "version": env!("CARGO_PKG_VERSION") },
            },
        });
        let reply = self.post(&msg, None)?;
        check_status(&reply, "initialize")?;
        let result = parse_rpc_reply(&reply.content_type, &reply.body, id, "initialize")?;
        let info = ServerInfo {
            name: str_at(&result, &["serverInfo", "name"]),
            version: str_at(&result, &["serverInfo", "version"]),
            protocol_version: result
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION)
                .to_string(),
        };
        let session = Session {
            id: reply.session_id,
            protocol_version: info.protocol_version.clone(),
        };
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let ack = self.post(&note, Some(&session))?;
        check_status(&ack, "notifications/initialized")?;
        *self.session.lock().unwrap_or_else(|p| p.into_inner()) = Some(session);
        Ok(info)
    }

    /// Call tool `name` with `args` and return its result: the JSON in `content[0].text` (or the
    /// raw text when it is not JSON). A tool that reports `isError` is an `Err` carrying its text.
    pub fn call_tool(&self, name: &str, args: Value) -> io::Result<Value> {
        let params = json!({ "name": name, "arguments": args });
        let result = self.request("tools/call", params, name)?;
        tool_result(name, &result)
    }

    /// One JSON-RPC request inside the session, initialising first when there is none and once
    /// more when the server says the session is gone.
    fn request(&self, method: &str, params: Value, what: &str) -> io::Result<Value> {
        for attempt in 0..2 {
            let session = match self.current_session() {
                Some(s) => s,
                None => {
                    self.initialize()?;
                    self.current_session()
                        .ok_or_else(|| io::Error::other("Forge MCP: no session after initialize"))?
                }
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            let reply = self.post(&msg, Some(&session))?;
            if reply.status == 404 && session.id.is_some() && attempt == 0 {
                *self.session.lock().unwrap_or_else(|p| p.into_inner()) = None;
                continue;
            }
            check_status(&reply, what)?;
            return parse_rpc_reply(&reply.content_type, &reply.body, id, what);
        }
        Err(io::Error::other(format!(
            "Forge MCP {what}: the server kept dropping the session"
        )))
    }

    fn current_session(&self) -> Option<Session> {
        self.session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn post(&self, msg: &Value, session: Option<&Session>) -> io::Result<Reply> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", format!("Bearer {}", self.token));
        if let Some(s) = session {
            if let Some(id) = &s.id {
                req = req.header("Mcp-Session-Id", id);
            }
            req = req.header("MCP-Protocol-Version", &s.protocol_version);
        }
        let mut resp = req
            .send(msg.to_string())
            .map_err(|e| io::Error::other(format!("Forge MCP {}: {e}", self.url)))?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header("content-type").unwrap_or_default();
        let session_id = header("mcp-session-id");
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| io::Error::other(format!("Forge MCP {}: reading reply: {e}", self.url)))?;
        Ok(Reply {
            status,
            content_type,
            session_id,
            body,
        })
    }
}

fn str_at(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for key in path {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return String::new(),
        }
    }
    cur.as_str().unwrap_or_default().to_string()
}

/// Map a non-2xx reply to an error. 401/403 name `bn forge login`, because a rejected key is the
/// one failure a person can fix from here.
fn check_status(reply: &Reply, what: &str) -> io::Result<()> {
    match reply.status {
        200..=299 => Ok(()),
        401 | 403 => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Forge rejected the key (HTTP {}) on {what}: run `bn forge login` for a new one",
                reply.status
            ),
        )),
        status => {
            let snippet: String = reply.body.chars().take(200).collect();
            Err(io::Error::other(format!(
                "Forge MCP {what}: HTTP {status} {}",
                snippet.trim()
            )))
        }
    }
}

/// The JSON-RPC `result` answering request `id`, from either a JSON body or an SSE stream.
pub(crate) fn parse_rpc_reply(
    content_type: &str,
    body: &str,
    id: u64,
    what: &str,
) -> io::Result<Value> {
    let messages: Vec<Value> = if content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
    {
        sse_data(body)
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    } else {
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Array(batch)) => batch,
            Ok(v) => vec![v],
            Err(e) => {
                return Err(io::Error::other(format!(
                    "Forge MCP {what}: reply is not JSON ({e})"
                )))
            }
        }
    };
    let answer = messages
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_u64) == Some(id))
        .ok_or_else(|| io::Error::other(format!("Forge MCP {what}: no reply to request {id}")))?;
    if let Some(err) = answer.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(io::Error::other(format!("Forge MCP {what}: {message}")));
    }
    answer
        .get("result")
        .cloned()
        .ok_or_else(|| io::Error::other(format!("Forge MCP {what}: reply has no result")))
}

/// The `data` payload of each event in an SSE body: consecutive `data:` lines joined by `\n`,
/// events split on blank lines.
pub(crate) fn sse_data(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !cur.is_empty() {
                events.push(cur.join("\n"));
                cur.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            cur.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !cur.is_empty() {
        events.push(cur.join("\n"));
    }
    events
}

/// A `tools/call` result as a value: `isError` becomes an `Err`, `content[0].text` is parsed as
/// JSON when it is JSON, and `structuredContent` stands in when there is no text content.
pub(crate) fn tool_result(name: &str, result: &Value) -> io::Result<Value> {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.iter().find_map(|item| item.get("text")))
        .and_then(Value::as_str);
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(io::Error::other(format!(
            "Forge {name}: {}",
            text.unwrap_or("the tool reported an error")
        )));
    }
    match text {
        Some(t) => Ok(serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string()))),
        None => Ok(result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null)),
    }
}

// ---------------------------------------------------------------------------------------------
// The typed tool surface.
// ---------------------------------------------------------------------------------------------

/// A JSON `null` reads as the type's default, the same as an omitted key. Forge omits nulls, but a
/// store that must not break on the other spelling costs one attribute.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A member assigned to an activity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Assignee {
    #[serde(deserialize_with = "null_default")]
    pub user_id: String,
    pub display_name: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub is_agent: bool,
}

/// One row of `list_bn_tasks`: an activity under a projects goal, or assigned to an agent. The
/// same shape comes back from `claim_activity` and `assign_activity`. Forge omits null fields, so
/// every optional field is an `Option` and every other field has a default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BnTaskRow {
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub goal_id: String,
    #[serde(deserialize_with = "null_default")]
    pub goal_name: String,
    pub goal_repo_url: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    pub description: Option<String>,
    /// `None`, `Low`, `Medium`, `High` or `Urgent`.
    #[serde(deserialize_with = "null_default")]
    pub priority: String,
    pub due_date: Option<String>,
    /// `Active`, `Blocked` or `Abandoned`.
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    #[serde(deserialize_with = "null_default")]
    pub is_completed: bool,
    pub completed_at: Option<String>,
    pub started_at: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub created_at: String,
    #[serde(deserialize_with = "null_default")]
    pub version: i64,
    #[serde(deserialize_with = "null_default")]
    pub assignees: Vec<Assignee>,
    #[serde(deserialize_with = "null_default")]
    pub agent_meta: BTreeMap<String, String>,
}

/// A member from `get_family_members`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FamilyMemberRow {
    #[serde(deserialize_with = "null_default")]
    pub user_id: String,
    #[serde(deserialize_with = "null_default")]
    pub display_name: String,
    /// `owner`, `admin` or `member`.
    #[serde(deserialize_with = "null_default")]
    pub role: String,
    #[serde(deserialize_with = "null_default")]
    pub is_agent: bool,
}

/// A goal from `get_goals`. `repoUrl` arrives normalised (http(s), no trailing `/` or `.git`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoalRow {
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    pub repo_url: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    pub parent_goal_id: Option<String>,
}

/// `create_activity` arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewActivity {
    pub goal_id: String,
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub priority: Option<String>,
}

/// `update_activity` arguments: only the fields that changed are `Some`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityPatch {
    pub title: Option<String>,
    pub due_date: Option<String>,
    pub clear_due_date: bool,
    pub priority: Option<String>,
}

impl ActivityPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.due_date.is_none()
            && !self.clear_due_date
            && self.priority.is_none()
    }
}

/// `set_activity_status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityStatus {
    /// Reopened, not started.
    Active,
    /// In progress (`StartedAt` set).
    Started,
    Completed,
}

impl ActivityStatus {
    pub fn word(self) -> &'static str {
        match self {
            ActivityStatus::Active => "active",
            ActivityStatus::Started => "started",
            ActivityStatus::Completed => "completed",
        }
    }
}

/// `claim_activity`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Claimed; the activity as it now stands.
    Claimed(Box<BnTaskRow>),
    /// `already_claimed`, `not_agent_assigned`, `completed`, or whatever else the server says.
    Refused(String),
}

/// `set_activity_meta`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaOutcome {
    /// Written; the activity is now at `version`.
    Ok { version: i64 },
    /// `expectedVersion` was stale; the activity is at `version`. Nothing was written.
    VersionConflict { version: i64 },
}

/// The Forge tools the task store and `bn forge` use.
pub trait ForgeApi: Send + Sync {
    fn list_bn_tasks(
        &self,
        include_completed_since_days: Option<u32>,
    ) -> io::Result<Vec<BnTaskRow>>;
    /// Returns the new activity's id.
    fn create_activity(&self, new: &NewActivity) -> io::Result<String>;
    fn update_activity(&self, id: &str, patch: &ActivityPatch) -> io::Result<()>;
    fn set_activity_status(&self, id: &str, status: ActivityStatus) -> io::Result<()>;
    fn set_activity_meta(
        &self,
        id: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
        expected_version: Option<i64>,
    ) -> io::Result<MetaOutcome>;
    fn claim_activity(&self, id: &str, claimer: &str) -> io::Result<ClaimOutcome>;
    /// Replace the assignees; returns the updated row.
    fn assign_activity(&self, id: &str, user_ids: &[String]) -> io::Result<BnTaskRow>;
    fn create_comment(&self, activity_id: &str, text: &str) -> io::Result<()>;
    fn get_goals(&self) -> io::Result<Vec<GoalRow>>;
    fn get_family_members(&self) -> io::Result<Vec<FamilyMemberRow>>;
}

/// [`ForgeApi`] over a live [`McpHttp`] session.
#[derive(Debug)]
pub struct HttpForgeApi {
    mcp: McpHttp,
}

impl HttpForgeApi {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> HttpForgeApi {
        HttpForgeApi {
            mcp: McpHttp::new(url, token),
        }
    }

    /// The protocol client, for `initialize` in `bn forge status`.
    pub fn mcp(&self) -> &McpHttp {
        &self.mcp
    }
}

fn decode<T: serde::de::DeserializeOwned>(tool: &str, v: Value) -> io::Result<T> {
    serde_json::from_value(v)
        .map_err(|e| io::Error::other(format!("Forge {tool}: unexpected result shape ({e})")))
}

/// `true`/`false` at `key`, treating absence as `false`.
fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn decode_meta_outcome(v: &Value) -> io::Result<MetaOutcome> {
    let version = v.get("version").and_then(Value::as_i64).unwrap_or(0);
    if flag(v, "ok") {
        return Ok(MetaOutcome::Ok { version });
    }
    match v.get("reason").and_then(Value::as_str) {
        Some("version_conflict") => Ok(MetaOutcome::VersionConflict { version }),
        other => Err(io::Error::other(format!(
            "Forge set_activity_meta: refused ({})",
            other.unwrap_or("no reason given")
        ))),
    }
}

pub(crate) fn decode_claim_outcome(v: &Value) -> io::Result<ClaimOutcome> {
    if flag(v, "claimed") {
        let row = v.get("activity").cloned().unwrap_or(Value::Null);
        Ok(ClaimOutcome::Claimed(Box::new(decode(
            "claim_activity",
            row,
        )?)))
    } else {
        Ok(ClaimOutcome::Refused(
            v.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        ))
    }
}

impl ForgeApi for HttpForgeApi {
    fn list_bn_tasks(&self, since_days: Option<u32>) -> io::Result<Vec<BnTaskRow>> {
        let mut args = json!({});
        if let Some(days) = since_days {
            args["includeCompletedSinceDays"] = json!(days);
        }
        let v = self.mcp.call_tool("list_bn_tasks", args)?;
        let items = match v {
            Value::Array(_) => v,
            other => other.get("items").cloned().unwrap_or(Value::Array(vec![])),
        };
        decode("list_bn_tasks", items)
    }

    fn create_activity(&self, new: &NewActivity) -> io::Result<String> {
        let mut args = json!({ "goalId": new.goal_id, "title": new.title });
        if let Some(d) = &new.description {
            args["description"] = json!(d);
        }
        if let Some(d) = &new.due_date {
            args["dueDate"] = json!(d);
        }
        if let Some(p) = &new.priority {
            args["priority"] = json!(p);
        }
        let v = self.mcp.call_tool("create_activity", args)?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| io::Error::other("Forge create_activity: result has no id"))
    }

    fn update_activity(&self, id: &str, patch: &ActivityPatch) -> io::Result<()> {
        let mut args = json!({ "id": id });
        if let Some(t) = &patch.title {
            args["title"] = json!(t);
        }
        if let Some(d) = &patch.due_date {
            args["dueDate"] = json!(d);
        }
        if patch.clear_due_date {
            args["clearDueDate"] = json!(true);
        }
        if let Some(p) = &patch.priority {
            args["priority"] = json!(p);
        }
        self.mcp.call_tool("update_activity", args).map(|_| ())
    }

    fn set_activity_status(&self, id: &str, status: ActivityStatus) -> io::Result<()> {
        self.mcp
            .call_tool(
                "set_activity_status",
                json!({ "id": id, "status": status.word() }),
            )
            .map(|_| ())
    }

    fn set_activity_meta(
        &self,
        id: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
        expected_version: Option<i64>,
    ) -> io::Result<MetaOutcome> {
        let mut args = json!({ "id": id });
        if !set.is_empty() {
            args["set"] = json!(set);
        }
        if !remove.is_empty() {
            args["remove"] = json!(remove);
        }
        if let Some(v) = expected_version {
            args["expectedVersion"] = json!(v);
        }
        let v = self.mcp.call_tool("set_activity_meta", args)?;
        decode_meta_outcome(&v)
    }

    fn claim_activity(&self, id: &str, claimer: &str) -> io::Result<ClaimOutcome> {
        let v = self
            .mcp
            .call_tool("claim_activity", json!({ "id": id, "claimer": claimer }))?;
        decode_claim_outcome(&v)
    }

    fn assign_activity(&self, id: &str, user_ids: &[String]) -> io::Result<BnTaskRow> {
        let v = self
            .mcp
            .call_tool("assign_activity", json!({ "id": id, "userIds": user_ids }))?;
        decode("assign_activity", v)
    }

    fn create_comment(&self, activity_id: &str, text: &str) -> io::Result<()> {
        self.mcp
            .call_tool(
                "create_comment",
                json!({ "activityId": activity_id, "text": text }),
            )
            .map(|_| ())
    }

    fn get_goals(&self) -> io::Result<Vec<GoalRow>> {
        let v = self.mcp.call_tool("get_goals", json!({}))?;
        let items = match v {
            Value::Array(_) => v,
            other => other.get("items").cloned().unwrap_or(Value::Array(vec![])),
        };
        decode("get_goals", items)
    }

    fn get_family_members(&self) -> io::Result<Vec<FamilyMemberRow>> {
        let v = self.mcp.call_tool("get_family_members", json!({}))?;
        decode("get_family_members", v)
    }
}

#[cfg(test)]
mod tests;
