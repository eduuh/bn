//! Local Forge-agent profile and task contracts.
//!
//! This module is deliberately pure: it reads/parses local profile data and models Forge task
//! shapes, but never opens a network connection, writes notes, or starts dispatch.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

const SECRET_FIELD_FRAGMENTS: &[&str] = &[
    "api_key",
    "apikey",
    "family_key",
    "familykey",
    "famkey",
    "bearertoken",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "token",
    "secret",
    "password",
    "authorization",
    "credential",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeAgentProfile {
    pub agent: String,
    pub path: PathBuf,
    pub claimer: ForgeClaimer,
    pub forge_mcp: ForgeMcpProfile,
    /// Forge goal name → local bn repo, for tasks whose goal carries no repo URL.
    pub repos: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeClaimer {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeMcpProfile {
    pub server: Option<String>,
    pub endpoint: Option<String>,
    pub oauth_profile: Option<String>,
    pub oauth_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeProfileError {
    Missing { path: PathBuf },
    Read { path: PathBuf, message: String },
    Malformed { path: PathBuf, message: String },
    Invalid { path: PathBuf, message: String },
    UnsupportedSecretField { path: PathBuf, field: String },
}

impl ForgeProfileError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "profile_missing",
            Self::Read { .. } => "profile_unreadable",
            Self::Malformed { .. } => "profile_malformed",
            Self::Invalid { .. } => "profile_invalid",
            Self::UnsupportedSecretField { .. } => "unsupported_api_key_auth",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Missing { path } => format!(
                "bn forge: missing Forge agent profile {}",
                path.display()
            ),
            Self::Read { path, message } => {
                format!("bn forge: could not read profile {}: {message}", path.display())
            }
            Self::Malformed { path, message } => {
                format!("bn forge: malformed profile {}: {message}", path.display())
            }
            Self::Invalid { path, message } => {
                format!("bn forge: invalid profile {}: {message}", path.display())
            }
            Self::UnsupportedSecretField { path, field } => format!(
                "bn forge: profile {} uses unsupported secret/API-key authentication field '{field}'; Forge MCP is OAuth-only",
                path.display()
            ),
        }
    }
}

impl fmt::Display for ForgeProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ForgeProfileError {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawForgeAgentProfile {
    agent_name: Option<String>,
    claimer: Option<RawClaimer>,
    forge_mcp: Option<RawForgeMcpProfile>,
    forge_mcp_url: Option<String>,
    #[serde(default)]
    repos: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawClaimer {
    String(String),
    Object(RawForgeClaimer),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawForgeClaimer {
    id: Option<String>,
    display_name: Option<String>,
    display: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawForgeMcpProfile {
    server: Option<String>,
    endpoint: Option<String>,
    oauth_profile: Option<String>,
    oauth_session: Option<String>,
}

pub fn profiles_dir() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.extend([".config", "wira360", "agents"]);
    path
}

/// Names of the Forge agent profiles installed on this machine, sorted for a stable tick order.
///
/// Empty is the normal state on a machine that is not a Forge agent, so callers treat it as
/// "nothing to do" rather than an error.
pub fn installed_agents() -> Vec<String> {
    let mut agents: Vec<String> = std::fs::read_dir(profiles_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect();
    agents.sort();
    agents
}

pub fn default_profile_path(agent: &str) -> PathBuf {
    let mut path = profiles_dir();
    path.push(format!("{}.json", normalize_agent_name(agent)));
    path
}

pub fn load_profile(agent: &str) -> Result<ForgeAgentProfile, ForgeProfileError> {
    let path = default_profile_path(agent);
    load_profile_from_path(agent, &path)
}

pub fn load_profile_from_path(
    agent: &str,
    path: &Path,
) -> Result<ForgeAgentProfile, ForgeProfileError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ForgeProfileError::Missing {
                path: path.to_path_buf(),
            }
        } else {
            ForgeProfileError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        }
    })?;
    parse_profile(agent, path, &text)
}

pub fn parse_profile(
    agent: &str,
    path: &Path,
    text: &str,
) -> Result<ForgeAgentProfile, ForgeProfileError> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| ForgeProfileError::Malformed {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if let Some(field) = first_secret_field(&value) {
        return Err(ForgeProfileError::UnsupportedSecretField {
            path: path.to_path_buf(),
            field,
        });
    }
    let raw: RawForgeAgentProfile =
        serde_json::from_value(value).map_err(|error| ForgeProfileError::Malformed {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    validate_profile(agent, path, raw)
}

fn validate_profile(
    agent: &str,
    path: &Path,
    raw: RawForgeAgentProfile,
) -> Result<ForgeAgentProfile, ForgeProfileError> {
    let claimer = raw
        .claimer
        .ok_or_else(|| invalid(path, "missing claimer"))?;
    let claimer = validate_claimer(path, claimer)?;

    let forge_mcp = raw.forge_mcp.unwrap_or(RawForgeMcpProfile {
        server: Some("forge".to_string()),
        endpoint: raw.forge_mcp_url,
        oauth_profile: Some(raw.agent_name.unwrap_or_else(|| agent.to_string())),
        oauth_session: None,
    });
    let server = non_empty(forge_mcp.server);
    let endpoint = non_empty(forge_mcp.endpoint);
    if server.is_none() && endpoint.is_none() {
        return Err(invalid(
            path,
            "missing forgeMcp.server or forgeMcp.endpoint",
        ));
    }
    let oauth_profile = non_empty(forge_mcp.oauth_profile);
    let oauth_session = non_empty(forge_mcp.oauth_session);
    if oauth_profile.is_none() && oauth_session.is_none() {
        return Err(invalid(
            path,
            "missing forgeMcp.oauthProfile or forgeMcp.oauthSession",
        ));
    }

    Ok(ForgeAgentProfile {
        agent: agent.to_string(),
        path: path.to_path_buf(),
        claimer,
        forge_mcp: ForgeMcpProfile {
            server,
            endpoint,
            oauth_profile,
            oauth_session,
        },
        repos: raw.repos,
    })
}

/// The local repo a task should be worked in, or why it cannot be routed.
///
/// In order: the goal's repo URL when it names a repo this machine knows, then the profile's
/// `repos` map keyed by goal name (case-insensitive), then `fallback`. A task that matches none is
/// not claimed: dispatching it into an arbitrary repo would do the work in the wrong place.
pub fn route_repo(
    task: &ForgeBnTask,
    profile: &ForgeAgentProfile,
    known: &[String],
    fallback: Option<&str>,
) -> Result<String, String> {
    if let Some(name) = task
        .goal_repo_url
        .as_deref()
        .and_then(crate::issues::repo_name_from_url)
    {
        if known.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            return Ok(name);
        }
    }
    if let Some(goal) = task.goal_name.as_deref() {
        if let Some((_, repo)) = profile
            .repos
            .iter()
            .find(|(g, _)| g.eq_ignore_ascii_case(goal))
        {
            return Ok(repo.clone());
        }
    }
    if let Some(repo) = fallback.filter(|r| !r.trim().is_empty()) {
        return Ok(repo.to_string());
    }
    Err(format!(
        "no repo for goal {:?}: add it to \"repos\" in {}",
        task.goal_name.as_deref().unwrap_or("(none)"),
        profile.path.display()
    ))
}

fn validate_claimer(path: &Path, raw: RawClaimer) -> Result<ForgeClaimer, ForgeProfileError> {
    match raw {
        RawClaimer::String(value) => {
            let value = non_empty(Some(value)).ok_or_else(|| invalid(path, "missing claimer"))?;
            Ok(ForgeClaimer {
                id: value.clone(),
                display_name: value,
            })
        }
        RawClaimer::Object(claimer) => {
            let id = non_empty(claimer.id).ok_or_else(|| invalid(path, "missing claimer.id"))?;
            let display_name = non_empty(claimer.display_name)
                .or_else(|| non_empty(claimer.display))
                .or_else(|| non_empty(claimer.name))
                .ok_or_else(|| invalid(path, "missing claimer.displayName"))?;
            Ok(ForgeClaimer { id, display_name })
        }
    }
}

pub fn normalize_agent_name(agent: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in agent.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn invalid(path: &Path, message: &str) -> ForgeProfileError {
    ForgeProfileError::Invalid {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_secret_field(value: &Value) -> Option<String> {
    first_secret_field_at(value, "$")
}

fn first_secret_field_at(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let field_path = format!("{path}.{key}");
                if is_secret_field(key) {
                    return Some(field_path);
                }
                if let Some(found) = first_secret_field_at(child, &field_path) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, child)| first_secret_field_at(child, &format!("{path}[{index}]"))),
        _ => None,
    }
}

fn is_secret_field(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect();
    SECRET_FIELD_FRAGMENTS.iter().any(|fragment| {
        let fragment = fragment.replace(['_', '-'], "");
        normalized.contains(&fragment)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeBnTask {
    pub id: String,
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default)]
    pub goal_name: Option<String>,
    #[serde(default)]
    pub goal_repo_url: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub is_completed: bool,
    #[serde(default)]
    pub agent_meta: ForgeAgentMeta,
    #[serde(default)]
    pub assignees: Vec<ForgeAssignee>,
}

/// A member a task is assigned to. `list_bn_tasks(claimable=true)` returns every agent's claimable
/// work, so a poller must pick out its own by this list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeAssignee {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_agent: bool,
}

impl ForgeBnTask {
    /// Whether this task is assigned to the profile's Forge agent member. The member is matched by
    /// name, case-insensitively, against the profile name and the claimer's display name.
    pub fn is_assigned_to(&self, profile: &ForgeAgentProfile) -> bool {
        let names = [
            normalize_agent_name(&profile.agent),
            normalize_agent_name(&profile.claimer.display_name),
        ];
        self.assignees.iter().any(|assignee| {
            assignee.is_agent
                && assignee
                    .display_name
                    .as_deref()
                    .is_some_and(|name| names.contains(&normalize_agent_name(name)))
        })
    }
}

/// Forge-side bookkeeping on a task. `claimer` is the string a claiming agent writes, and the only
/// way a poller can tell its own in-flight work from another agent's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeAgentMeta {
    #[serde(default)]
    pub claimer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeListBnTasksResponse {
    #[serde(default)]
    pub items: Vec<ForgeBnTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeClaimActivityResponse {
    pub claimed: bool,
    #[serde(default)]
    pub activity: Option<ForgeBnTask>,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

pub fn select_task<'a>(
    tasks: &'a [ForgeBnTask],
    task_id: Option<&str>,
) -> Result<&'a ForgeBnTask, ForgeSelectionError> {
    match task_id {
        Some(id) => tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or_else(|| ForgeSelectionError::NotFound(id.to_string())),
        None if tasks.len() == 1 => tasks
            .first()
            .ok_or_else(|| ForgeSelectionError::NotFound(String::new())),
        None => Err(ForgeSelectionError::Ambiguous(
            tasks.iter().map(|task| task.id.clone()).collect(),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeSelectionError {
    NotFound(String),
    Ambiguous(Vec<String>),
}

impl ForgeSelectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "task_not_found",
            Self::Ambiguous(_) => "ambiguous_task",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotFound(id) if id.is_empty() => "bn forge: no claimable task found".to_string(),
            Self::NotFound(id) => format!("bn forge: task '{id}' is not claimable"),
            Self::Ambiguous(ids) => format!(
                "bn forge: multiple claimable tasks match; pass --task with one of: {}",
                ids.join(", ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ForgeAgentProfile, ForgeProfileError> {
        parse_profile("mac-agent", Path::new("/tmp/mac-agent.json"), text)
    }

    #[test]
    fn validates_minimal_profile() {
        let profile = parse(
            r#"{
                "claimer": { "id": "bn@mac", "displayName": "Mac Agent" },
                "forgeMcp": { "server": "forge", "oauthProfile": "default" }
            }"#,
        )
        .unwrap();
        assert_eq!(profile.claimer.id, "bn@mac");
        assert_eq!(profile.claimer.display_name, "Mac Agent");
        assert_eq!(profile.forge_mcp.server.as_deref(), Some("forge"));
        assert_eq!(profile.forge_mcp.oauth_profile.as_deref(), Some("default"));
    }

    #[test]
    fn validates_setup_script_profile_shape() {
        let profile = parse(
            r#"{
                "agentName": "Contabo-agent",
                "claimer": "Contabo-agent",
                "forgeMcpUrl": "https://api.wira360.com/mcp"
            }"#,
        )
        .unwrap();
        assert_eq!(profile.claimer.id, "Contabo-agent");
        assert_eq!(profile.claimer.display_name, "Contabo-agent");
        assert_eq!(profile.forge_mcp.server.as_deref(), Some("forge"));
        assert_eq!(
            profile.forge_mcp.endpoint.as_deref(),
            Some("https://api.wira360.com/mcp")
        );
        assert_eq!(
            profile.forge_mcp.oauth_profile.as_deref(),
            Some("Contabo-agent")
        );
    }

    #[test]
    fn normalizes_profile_file_name_like_setup_script() {
        assert_eq!(normalize_agent_name("Contabo-agent"), "contabo-agent");
        assert_eq!(normalize_agent_name("My Forge Agent"), "my-forge-agent");
    }

    #[test]
    fn rejects_secret_fields_without_leaking_values() {
        let err = parse(
            r#"{
                "claimer": { "id": "bn@mac", "displayName": "Mac Agent" },
                "forgeMcp": { "server": "forge", "oauthProfile": "default", "apiKey": "sentinel-secret-value" }
            }"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), "unsupported_api_key_auth");
        let msg = err.message();
        assert!(msg.contains("$.forgeMcp.apiKey"));
        assert!(!msg.contains("sentinel-secret-value"));
        assert!(msg.contains("OAuth-only"));
    }

    #[test]
    fn requires_claimer_and_oauth_mcp_refs() {
        let err = parse(
            r#"{
                "claimer": { "id": "bn@mac", "displayName": "Mac Agent" },
                "forgeMcp": { "server": "forge" }
            }"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), "profile_invalid");
        assert!(err.message().contains("oauthProfile"));
    }

    #[test]
    fn refuses_to_guess_between_multiple_tasks() {
        let tasks = vec![
            ForgeBnTask {
                id: "a".into(),
                goal_id: None,
                goal_name: None,
                goal_repo_url: None,
                title: "A".into(),
                description: None,
                priority: None,
                status: None,
                version: None,
                is_completed: false,
                agent_meta: ForgeAgentMeta::default(),
                assignees: Vec::new(),
            },
            ForgeBnTask {
                id: "b".into(),
                goal_id: None,
                goal_name: None,
                goal_repo_url: None,
                title: "B".into(),
                description: None,
                priority: None,
                status: None,
                version: None,
                is_completed: false,
                agent_meta: ForgeAgentMeta::default(),
                assignees: Vec::new(),
            },
        ];
        let err = select_task(&tasks, None).unwrap_err();
        assert_eq!(err.code(), "ambiguous_task");
        assert!(err.message().contains("a, b"));
    }

    #[test]
    fn claimable_task_is_ours_only_when_assigned_to_this_agent() {
        let profile = parse_profile(
            "contabo-agent",
            Path::new("/tmp/contabo-agent.json"),
            r#"{
                "agentName": "Contabo-agent",
                "claimer": "Contabo-agent",
                "forgeMcpUrl": "https://api.wira360.com/mcp"
            }"#,
        )
        .unwrap();
        let assigned = |name: &str, is_agent: bool| ForgeBnTask {
            id: "a".into(),
            goal_id: None,
            goal_name: None,
            goal_repo_url: None,
            title: "A".into(),
            description: None,
            priority: None,
            status: None,
            version: None,
            is_completed: false,
            agent_meta: ForgeAgentMeta::default(),
            assignees: vec![ForgeAssignee {
                user_id: Some("u".into()),
                display_name: Some(name.into()),
                is_agent,
            }],
        };
        // Forge spells the member "Contabo-Agent"; the profile "Contabo-agent".
        assert!(assigned("Contabo-Agent", true).is_assigned_to(&profile));
        assert!(!assigned("mac-agent", true).is_assigned_to(&profile));
        // A person who happens to share the name is not the agent.
        assert!(!assigned("Contabo-Agent", false).is_assigned_to(&profile));
        let mut unassigned = assigned("x", true);
        unassigned.assignees.clear();
        assert!(!unassigned.is_assigned_to(&profile));
    }

    fn routing_profile() -> ForgeAgentProfile {
        parse_profile(
            "mac-agent",
            Path::new("/p/mac-agent.json"),
            r#"{"claimer":"mac-agent","forgeMcpUrl":"https://f/mcp","repos":{"Homelab":"kube-homelab"}}"#,
        )
        .unwrap()
    }

    fn goal_task(goal: Option<&str>, url: Option<&str>) -> ForgeBnTask {
        serde_json::from_value(serde_json::json!({
            "id": "t", "title": "x", "goalName": goal, "goalRepoUrl": url
        }))
        .unwrap()
    }

    #[test]
    fn route_repo_prefers_a_known_goal_repo_url_then_the_goal_map_then_the_fallback() {
        let p = routing_profile();
        let known = vec!["wira360".to_string(), "kube-homelab".to_string()];
        let url = goal_task(Some("Homelab"), Some("git@github.com:eduuh/wira360.git"));
        assert_eq!(route_repo(&url, &p, &known, None).unwrap(), "wira360");
        let mapped = goal_task(Some("homelab"), None);
        assert_eq!(
            route_repo(&mapped, &p, &known, None).unwrap(),
            "kube-homelab"
        );
        let unknown_url = goal_task(Some("Homelab"), Some("https://github.com/x/elsewhere"));
        assert_eq!(
            route_repo(&unknown_url, &p, &known, None).unwrap(),
            "kube-homelab"
        );
        let other = goal_task(Some("Garden"), None);
        assert_eq!(route_repo(&other, &p, &known, Some("bn")).unwrap(), "bn");
        assert!(route_repo(&other, &p, &known, None)
            .unwrap_err()
            .contains("Garden"));
    }
}
