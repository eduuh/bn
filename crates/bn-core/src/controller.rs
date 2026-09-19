//! Controller-agent state, worker ownership, and durable request queues.
//!
//! Controller lifecycle metadata is a small atomically-replaced registry. Requests deliberately
//! reuse [`crate::backlog`]: each controller is represented by the synthetic backlog repo
//! `controller:<controller_id>`, preserving the existing append-only lease lifecycle.

use crate::backlog::{self, AddSpec, BacklogEntry, BacklogStatus};
use crate::cost::LaunchRecord;
use crate::events::{self, Event};
use crate::store::FileLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const CONTROLLER_WORKER_EVENT_KIND: &str = "controller_worker_event";
pub const CONTROLLER_HEARTBEAT_STALE_AFTER_SECS: i64 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerHeartbeat {
    pub session_id: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndGuardInput<'a> {
    pub controller_running: bool,
    pub session_id: &'a str,
    pub open_requests: usize,
    pub live_workers: usize,
    pub heartbeat: Option<&'a ControllerHeartbeat>,
    pub now: i64,
    pub stop_hook_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndDecision {
    Allow,
    Block { reason: String },
}

pub fn heartbeat_path(controller_id: &str) -> PathBuf {
    crate::resolver::state_dir()
        .join("controller-heartbeats")
        .join(format!("{controller_id}.json"))
}

pub fn load_heartbeat(path: &Path) -> std::io::Result<Option<ControllerHeartbeat>> {
    match std::fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body)
            .map(Some)
            .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save_heartbeat(path: &Path, session_id: &str, updated_at: i64) -> std::io::Result<()> {
    crate::store::save_json_atomic(
        path,
        &ControllerHeartbeat {
            session_id: session_id.to_string(),
            updated_at,
        },
    )
}

pub fn turn_end_decision(input: &TurnEndGuardInput<'_>) -> TurnEndDecision {
    if input.stop_hook_active || !input.controller_running {
        return TurnEndDecision::Allow;
    }
    let live_work = input.open_requests > 0 || input.live_workers > 0;
    if !live_work {
        return TurnEndDecision::Allow;
    }
    let heartbeat_fresh = input.heartbeat.is_some_and(|heartbeat| {
        heartbeat.session_id == input.session_id
            && input.now.saturating_sub(heartbeat.updated_at)
                <= CONTROLLER_HEARTBEAT_STALE_AFTER_SECS
    });
    if heartbeat_fresh {
        return TurnEndDecision::Allow;
    }
    TurnEndDecision::Block {
        reason: format!(
            "Controller work is still live ({} open request(s), {} owned worker(s)), but its \
             watcher heartbeat is missing or stale. Reconcile the live work now, ensure \
             `bn control watch` is running, and continue coordination before ending the turn.",
            input.open_requests, input.live_workers
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControllerWorkerEventType {
    Progress,
    Blocked,
    PrOpened,
    Done,
    Failed,
}

impl ControllerWorkerEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Blocked => "blocked",
            Self::PrOpened => "pr-opened",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerWorkerEvent {
    pub kind: String,
    pub ts: i64,
    pub controller_id: String,
    pub worker_session_id: String,
    pub repo: String,
    pub branch: String,
    pub event_type: ControllerWorkerEventType,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub event_id: String,
}

impl ControllerWorkerEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        controller_id: impl Into<String>,
        worker_session_id: impl Into<String>,
        repo: impl Into<String>,
        branch: impl Into<String>,
        event_type: ControllerWorkerEventType,
        summary: impl Into<String>,
        url: Option<String>,
        ts: i64,
    ) -> Self {
        let controller_id = controller_id.into();
        let worker_session_id = worker_session_id.into();
        let repo = repo.into();
        let branch = branch.into();
        let summary = summary.into();
        let event_id = stable_worker_event_id(
            &controller_id,
            &worker_session_id,
            &repo,
            &branch,
            event_type,
            &summary,
            url.as_deref(),
        );
        Self {
            kind: CONTROLLER_WORKER_EVENT_KIND.into(),
            ts,
            controller_id,
            worker_session_id,
            repo,
            branch,
            event_type,
            summary,
            url,
            event_id,
        }
    }

    pub fn from_event(event: &Event) -> Option<Self> {
        if event.kind != CONTROLLER_WORKER_EVENT_KIND {
            return None;
        }
        let string_field = |name: &str| {
            event
                .fields
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let event_type = serde_json::from_value(event.fields.get("event_type")?.clone()).ok()?;
        Some(Self {
            kind: event.kind.clone(),
            ts: event.ts,
            controller_id: string_field("controller_id")?,
            worker_session_id: string_field("worker_session_id")?,
            repo: string_field("repo")?,
            branch: string_field("branch")?,
            event_type,
            summary: string_field("summary")?,
            url: string_field("url"),
            event_id: string_field("event_id")?,
        })
    }

    fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        fields.insert(
            "controller_id".into(),
            Value::String(self.controller_id.clone()),
        );
        fields.insert(
            "worker_session_id".into(),
            Value::String(self.worker_session_id.clone()),
        );
        fields.insert("repo".into(), Value::String(self.repo.clone()));
        fields.insert("branch".into(), Value::String(self.branch.clone()));
        fields.insert(
            "event_type".into(),
            Value::String(self.event_type.as_str().to_string()),
        );
        fields.insert("summary".into(), Value::String(self.summary.clone()));
        if let Some(url) = &self.url {
            fields.insert("url".into(), Value::String(url.clone()));
        }
        fields.insert("event_id".into(), Value::String(self.event_id.clone()));
        fields
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[allow(clippy::too_many_arguments)]
pub fn stable_worker_event_id(
    controller_id: &str,
    worker_session_id: &str,
    repo: &str,
    branch: &str,
    event_type: ControllerWorkerEventType,
    summary: &str,
    url: Option<&str>,
) -> String {
    let key = format!(
        "{controller_id}\u{1f}{worker_session_id}\u{1f}{repo}\u{1f}{branch}\u{1f}{}\u{1f}{summary}\u{1f}{}",
        event_type.as_str(),
        url.unwrap_or_default()
    );
    format!("cwe-{:016x}", fnv1a64(key.as_bytes()))
}

pub fn append_worker_event_at(path: &Path, event: &ControllerWorkerEvent) -> std::io::Result<()> {
    if event.kind != CONTROLLER_WORKER_EVENT_KIND {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "controller worker event has the wrong kind",
        ));
    }
    let _lock = if event.event_type == ControllerWorkerEventType::Blocked {
        let lock = FileLock::acquire_required(path)?;
        if read_worker_events_at(path, &event.controller_id)
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            return Ok(());
        }
        Some(lock)
    } else {
        None
    };
    events::append_event_at(path, CONTROLLER_WORKER_EVENT_KIND, event.ts, event.fields())
}

pub fn append_worker_event(event: &ControllerWorkerEvent) -> std::io::Result<()> {
    append_worker_event_at(&worker_event_log_path(), event)?;
    if let Some(wake) = actionable_worker_wake(event) {
        crate::wake::WakeQueue::default().publish(wake)?;
    }
    Ok(())
}

fn actionable_worker_wake(event: &ControllerWorkerEvent) -> Option<crate::wake::ActionableWake> {
    let transition = match event.event_type {
        ControllerWorkerEventType::Progress => crate::wake::FleetTransition::Progress,
        ControllerWorkerEventType::Blocked => crate::wake::FleetTransition::Blocked,
        ControllerWorkerEventType::PrOpened => crate::wake::FleetTransition::PrOpened,
        ControllerWorkerEventType::Done => crate::wake::FleetTransition::Done,
        ControllerWorkerEventType::Failed => crate::wake::FleetTransition::Failed,
    };
    crate::wake::ActionableWake::classify(
        event.event_id.clone(),
        event.ts,
        transition,
        format!(
            "{} {}:{} — {}",
            event.event_type.as_str(),
            event.repo,
            event.branch,
            event.summary
        ),
        crate::wake::WakeOwner::Controller(event.controller_id.clone()),
    )
}

pub fn read_worker_events_at(path: &Path, controller_id: &str) -> Vec<ControllerWorkerEvent> {
    events::read_events_at(path)
        .iter()
        .filter_map(ControllerWorkerEvent::from_event)
        .filter(|event| event.controller_id == controller_id)
        .collect()
}
pub fn worker_event_log_path() -> PathBuf {
    crate::resolver::state_dir().join("controller-events.jsonl")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControllerStatus {
    Starting,
    Running,
    Stopped,
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerRecord {
    pub controller_id: String,
    pub session_id: String,
    #[serde(default)]
    pub task_name: String,
    pub harness: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    pub target: String,
    pub cwd: String,
    pub status: ControllerStatus,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerRegistry {
    #[serde(default)]
    pub controllers: Vec<ControllerRecord>,
}

impl ControllerRegistry {
    pub fn upsert(&mut self, controller: ControllerRecord) {
        match self
            .controllers
            .iter_mut()
            .find(|c| c.controller_id == controller.controller_id)
        {
            Some(slot) => *slot = controller,
            None => self.controllers.push(controller),
        }
    }

    pub fn get(&self, controller_id: &str) -> Option<&ControllerRecord> {
        self.controllers
            .iter()
            .find(|c| c.controller_id == controller_id)
    }

    pub fn stop(&mut self, controller_id: &str, now: i64) -> bool {
        let Some(controller) = self
            .controllers
            .iter_mut()
            .find(|c| c.controller_id == controller_id)
        else {
            return false;
        };
        controller.status = ControllerStatus::Stopped;
        controller.stopped_at = Some(now);
        true
    }
}

pub fn registry_path() -> PathBuf {
    crate::resolver::state_dir().join("controllers.json")
}

pub fn load_registry(path: &Path) -> std::io::Result<ControllerRegistry> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ControllerRegistry::default());
        }
        Err(error) => return Err(error),
    };
    serde_json::from_str(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub fn update_registry<T>(
    path: &Path,
    f: impl FnOnce(&mut ControllerRegistry) -> T,
) -> std::io::Result<T> {
    let _lock = FileLock::acquire_required(path)?;
    let mut registry = load_registry(path)?;
    let out = f(&mut registry);
    crate::store::save_json_atomic(path, &registry)?;
    Ok(out)
}

pub fn queue_key(controller_id: &str) -> String {
    format!("controller:{controller_id}")
}

pub fn enqueue(
    path: &Path,
    controller_id: &str,
    requests: &[String],
    now: i64,
) -> std::io::Result<Vec<String>> {
    let spec = AddSpec {
        source: queue_key(controller_id),
        ..AddSpec::default()
    };
    let added = backlog::add(path, &queue_key(controller_id), requests, &spec, now)?;
    // The store is append-only; trim terminal and superseded lines once they dominate. A failed
    // trim loses nothing, so it never fails the add.
    let _ = backlog::compact_if_needed(path);
    Ok(added)
}

pub fn lease_next(
    path: &Path,
    controller_id: &str,
    now: i64,
) -> std::io::Result<Option<BacklogEntry>> {
    Ok(backlog::lease(path, &queue_key(controller_id), 1, now)?
        .into_iter()
        .next())
}

pub fn drain(
    path: &Path,
    controller_id: &str,
    request_id: &str,
    succeeded: bool,
    now: i64,
) -> std::io::Result<bool> {
    let status = if succeeded {
        BacklogStatus::Done
    } else {
        BacklogStatus::Failed
    };
    backlog::mark_leased(path, &queue_key(controller_id), request_id, status, now)
}

pub fn requeue_leased(path: &Path, controller_id: &str, now: i64) -> std::io::Result<usize> {
    let key = queue_key(controller_id);
    let backlog = backlog::load(path);
    let ids: Vec<String> = backlog
        .list_view(&key, Some(BacklogStatus::Leased))
        .into_iter()
        .map(|entry| entry.id.clone())
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    backlog::release_many(path, &key, &refs, now)
}

pub fn queue_entries(path: &Path, controller_id: &str) -> Vec<BacklogEntry> {
    backlog::load(path)
        .list_view(&queue_key(controller_id), None)
        .into_iter()
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedWorker {
    pub session_id: String,
    pub repo: String,
    pub branch: String,
    pub launched_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerOwnership {
    pub controller: ControllerRecord,
    pub workers: Vec<OwnedWorker>,
}

pub fn join_ownership(
    controllers: &[ControllerRecord],
    launches: &[LaunchRecord],
) -> Vec<ControllerOwnership> {
    controllers
        .iter()
        .cloned()
        .map(|controller| {
            let workers = launches
                .iter()
                .filter(|launch| {
                    launch.controller_id.as_deref() == Some(controller.controller_id.as_str())
                })
                .map(|launch| OwnedWorker {
                    session_id: launch.session_id.clone(),
                    repo: launch.repo.clone(),
                    branch: launch.branch.clone(),
                    launched_at: launch.launched_at,
                })
                .collect();
            ControllerOwnership {
                controller,
                workers,
            }
        })
        .collect()
}
