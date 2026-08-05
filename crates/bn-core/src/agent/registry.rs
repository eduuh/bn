//! Cross-repo session registry projected from the existing launch and telemetry logs.

use super::{AgentHealth, AgentSession, LiveAgent};
use crate::controller::{ControllerRecord, ControllerStatus};
use crate::cost::{canonical_session_id, LaunchRecord, WorkClass};
use crate::resolver::{Config, Resolved};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Live,
    Idle,
    Done,
    Failed,
    Gone,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Live => "live",
            SessionStatus::Idle => "idle",
            SessionStatus::Done => "done",
            SessionStatus::Failed => "failed",
            SessionStatus::Gone => "gone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub task_name: String,
    pub harness: String,
    pub repo: String,
    pub branch: String,
    pub worktree: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveSessionError {
    NotFound(String),
    NoWorktreeSession(String),
    Ambiguous(String, Vec<String>),
}

impl std::fmt::Display for ResolveSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveSessionError::NotFound(query) => {
                write!(f, "no session matched '{query}'")
            }
            ResolveSessionError::NoWorktreeSession(worktree) => {
                write!(f, "no resumable session recorded for worktree '{worktree}'")
            }
            ResolveSessionError::Ambiguous(query, ids) => write!(
                f,
                "session name '{query}' is ambiguous; matching ids: {}",
                ids.join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSelector<'a> {
    NameOrId(&'a str),
    LatestForWorktree(&'a str),
}

fn recency_key(row: &SessionRecord) -> (i64, i64, String) {
    (
        row.updated_at,
        row.created_at,
        canonical_session_id(&row.session_id),
    )
}

/// Resolve a named session or the latest resumable harness session for one worktree.
pub fn resolve_session<'a>(
    rows: &'a [SessionRecord],
    selector: SessionSelector<'_>,
) -> Result<&'a SessionRecord, ResolveSessionError> {
    let query = match selector {
        SessionSelector::NameOrId(query) => query,
        SessionSelector::LatestForWorktree(worktree) => {
            return rows
                .iter()
                .filter(|row| {
                    super::same_dir(Path::new(&row.worktree), Path::new(worktree))
                        && matches!(row.harness.as_str(), "copilot" | "claude")
                })
                .max_by_key(|row| recency_key(row))
                .ok_or_else(|| ResolveSessionError::NoWorktreeSession(worktree.to_string()));
        }
    };
    let query = query.trim();
    let canonical = canonical_session_id(query);
    if let Some(row) = rows
        .iter()
        .find(|row| canonical_session_id(&row.session_id) == canonical)
    {
        return Ok(row);
    }
    let mut matches = rows
        .iter()
        .filter(|row| {
            canonical_session_id(&row.session_id).starts_with(&canonical)
                || row.task_name.eq_ignore_ascii_case(query)
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
    match matches.as_slice() {
        [] => Err(ResolveSessionError::NotFound(query.to_string())),
        [row] => Ok(*row),
        _ => Err(ResolveSessionError::Ambiguous(
            query.to_string(),
            matches.iter().map(|row| row.session_id.clone()).collect(),
        )),
    }
}

/// Choose a readable task name from an explicit override or a resolved title/goal.
pub fn derive_task_name(explicit: Option<&str>, source: &str) -> String {
    let raw = explicit.unwrap_or(source).trim();
    let raw = raw.strip_prefix("dispatch/").unwrap_or(raw);
    let slug_like = !raw.chars().any(char::is_whitespace)
        && raw.chars().any(|ch| matches!(ch, '-' | '_' | '/'));
    let mut name = if slug_like {
        raw.split(['-', '_', '/'])
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if name.is_empty() {
        name = "Agent session".to_string();
    }
    if slug_like {
        if let Some(first) = name.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
    }
    name
}

/// Stable, readable tmux window name. The short key suffix prevents two same-named tasks in one
/// repo from reusing each other's windows while keeping the task name as the primary label.
pub fn task_window_name(task_name: &str, key: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let clean = task_name
        .replace([':', '\t', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clean = if clean.is_empty() {
        "Agent session"
    } else {
        clean.as_str()
    };
    format!(
        "{} · {:06x}",
        clean.chars().take(60).collect::<String>(),
        hash & 0xff_ffff
    )
}

fn fallback_task_name(branch: &str) -> String {
    derive_task_name(None, branch.rsplit('/').next().unwrap_or(branch))
}

fn completed_status(session: &AgentSession) -> SessionStatus {
    if session.outcome.eq_ignore_ascii_case("failed") {
        SessionStatus::Failed
    } else {
        SessionStatus::Done
    }
}

/// Fold durable launch records and completed telemetry into one row per canonical session id.
pub fn fold_sessions(launches: &[LaunchRecord], completed: &[AgentSession]) -> Vec<SessionRecord> {
    let mut rows: BTreeMap<String, SessionRecord> = BTreeMap::new();
    for launch in launches {
        let key = canonical_session_id(&launch.session_id);
        let task_name = if launch.task_name.trim().is_empty() {
            fallback_task_name(&launch.branch)
        } else {
            launch.task_name.clone()
        };
        match rows.get_mut(&key) {
            Some(row) => {
                row.task_name = task_name;
                if !launch.harness.is_empty() {
                    row.harness = launch.harness.clone();
                }
                row.repo = launch.repo.clone();
                row.branch = launch.branch.clone();
                if !launch.worktree.is_empty() {
                    row.worktree = launch.worktree.clone();
                }
                row.controller_id = launch.controller_id.clone();
                row.created_at = row.created_at.min(launch.launched_at);
                row.updated_at = row.updated_at.max(launch.launched_at);
            }
            None => {
                rows.insert(
                    key,
                    SessionRecord {
                        task_name,
                        harness: launch.harness.clone(),
                        repo: launch.repo.clone(),
                        branch: launch.branch.clone(),
                        worktree: launch.worktree.clone(),
                        session_id: launch.session_id.clone(),
                        controller_id: launch.controller_id.clone(),
                        status: SessionStatus::Gone,
                        pr_url: None,
                        created_at: launch.launched_at,
                        updated_at: launch.launched_at,
                    },
                );
            }
        }
    }
    for session in completed {
        let key = canonical_session_id(&session.session_id);
        let row = rows.entry(key).or_insert_with(|| SessionRecord {
            task_name: fallback_task_name(&session.branch),
            harness: session.harness.clone(),
            repo: session.repo.clone(),
            branch: session.branch.clone(),
            worktree: String::new(),
            session_id: session.session_id.clone(),
            controller_id: None,
            status: completed_status(session),
            pr_url: None,
            created_at: session.started_ts,
            updated_at: session.ended_ts,
        });
        if row.harness.is_empty() {
            row.harness = session.harness.clone();
        }
        if row.repo.is_empty() {
            row.repo = session.repo.clone();
        }
        if row.branch.is_empty() {
            row.branch = session.branch.clone();
        }
        row.created_at = row.created_at.min(session.started_ts);
        row.updated_at = row.updated_at.max(session.ended_ts);
        row.status = completed_status(session);
    }
    let mut out = rows.into_values().collect::<Vec<_>>();
    out.sort_by_key(|row| std::cmp::Reverse((row.updated_at, row.created_at)));
    out
}

pub fn overlay_live(rows: &mut [SessionRecord], live: &[LiveAgent]) {
    let by_id = live
        .iter()
        .map(|agent| (canonical_session_id(&agent.session_id), agent))
        .collect::<BTreeMap<_, _>>();
    for row in rows {
        let Some(agent) = by_id.get(&canonical_session_id(&row.session_id)) else {
            continue;
        };
        row.status = match agent.health {
            AgentHealth::Starting | AgentHealth::Active => SessionStatus::Live,
            AgentHealth::Idle | AgentHealth::Stuck => SessionStatus::Idle,
        };
        row.updated_at = row.updated_at.max(agent.last_activity_ts);
        if row.harness.is_empty() {
            row.harness = agent.harness.clone();
        }
    }
}

pub fn overlay_controllers(rows: &mut [SessionRecord], controllers: &[ControllerRecord]) {
    let by_id = controllers
        .iter()
        .map(|controller| (canonical_session_id(&controller.session_id), controller))
        .collect::<BTreeMap<_, _>>();
    for row in rows {
        let Some(controller) = by_id.get(&canonical_session_id(&row.session_id)) else {
            continue;
        };
        row.status = match controller.status {
            ControllerStatus::Starting | ControllerStatus::Running => SessionStatus::Live,
            ControllerStatus::Stopped => SessionStatus::Done,
            ControllerStatus::Gone => SessionStatus::Gone,
        };
        if let Some(stopped_at) = controller.stopped_at {
            row.updated_at = row.updated_at.max(stopped_at);
        }
    }
}

/// Read the durable launch, completion, and controller stores and project the current cross-repo
/// registry. Live transcript health overlays the persisted rows; no parallel registry file exists.
pub fn session_registry(cfg: &Config) -> std::io::Result<Vec<SessionRecord>> {
    let controllers =
        crate::controller::load_registry(&crate::controller::registry_path())?.controllers;
    let mut launches = crate::cost::launch_records();
    for controller in &controllers {
        if launches.iter().any(|launch| {
            canonical_session_id(&launch.session_id) == canonical_session_id(&controller.session_id)
        }) {
            continue;
        }
        launches.push(LaunchRecord {
            session_id: controller.session_id.clone(),
            task_name: controller.task_name.clone(),
            harness: controller.harness.clone(),
            repo: controller.repo.clone(),
            branch: controller.branch.clone(),
            worktree: controller.cwd.clone(),
            work_class: WorkClass::Orchestration,
            launched_at: controller.started_at,
            controller_id: Some(controller.controller_id.clone()),
        });
    }

    let completed = super::sessions_all(cfg);
    let mut rows = fold_sessions(&launches, &completed);
    overlay_controllers(&mut rows, &controllers);
    let live = super::supervise(cfg, super::DEFAULT_IDLE_MINS, super::DEFAULT_STUCK_MINS);
    overlay_live(&mut rows, &live.agents);
    let worktree_paths = crate::lifecycle::live_worktree_paths(&cfg.bare_dir());
    for row in &mut rows {
        if row.worktree.is_empty() && !row.repo.is_empty() && !row.branch.is_empty() {
            let dashed = row.branch.replace('/', "-");
            // Prefer git's live answer — a branch dispatched into a pool slot (`_pool/N`) is NOT at
            // the conventional `<worktree_dir>/<repo>/<branch>`, and resuming needs the real path.
            row.worktree = worktree_paths
                .get(&row.repo.to_lowercase())
                .and_then(|by_branch| by_branch.get(&dashed))
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    cfg.worktree_dir
                        .join(&row.repo)
                        .join(&dashed)
                        .to_string_lossy()
                        .into_owned()
                });
        }
        let note_ctx = Resolved {
            repo: row.repo.clone(),
            branch: row.branch.replace('/', "-"),
        };
        row.pr_url = crate::commands::read_pr(cfg, &note_ctx);
    }
    rows.sort_by_key(|row| std::cmp::Reverse((row.updated_at, row.created_at)));
    Ok(rows)
}
