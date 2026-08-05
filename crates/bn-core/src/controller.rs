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
use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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

pub fn read_worker_events(controller_id: &str) -> Vec<ControllerWorkerEvent> {
    read_worker_events_at(&worker_event_log_path(), controller_id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerLogObservation {
    pub len: u64,
    pub modified_nanos: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerEventCursor {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub observed_log: Option<ControllerLogObservation>,
    #[serde(default)]
    pub seen_event_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerEventBatch {
    pub events: Vec<ControllerWorkerEvent>,
    pub cursor: ControllerEventCursor,
}

pub fn worker_event_log_path() -> PathBuf {
    crate::resolver::state_dir().join("controller-events.jsonl")
}

pub fn event_cursor_path(controller_id: &str) -> PathBuf {
    crate::resolver::state_dir()
        .join("controller-cursors")
        .join(format!("{controller_id}.json"))
}

pub fn load_event_cursor(path: &Path) -> std::io::Result<ControllerEventCursor> {
    match std::fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body)
            .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ControllerEventCursor::default()),
        Err(error) => Err(error),
    }
}

fn complete_prefix(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&[], |last_newline| &bytes[..=last_newline])
}

fn log_observation(metadata: &std::fs::Metadata) -> ControllerLogObservation {
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64);
    ControllerLogObservation {
        len: metadata.len(),
        modified_nanos,
    }
}

fn cursor_start(
    file: &mut std::fs::File,
    file_len: u64,
    requested_offset: u64,
) -> std::io::Result<u64> {
    if requested_offset == 0 || file_len < requested_offset {
        return Ok(0);
    }
    file.seek(SeekFrom::Start(requested_offset - 1))?;
    let mut boundary = [0_u8; 1];
    match file.read_exact(&mut boundary) {
        Ok(()) if boundary[0] == b'\n' => Ok(requested_offset),
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(0),
        Err(error) => Err(error),
    }
}

pub fn read_worker_events_from_cursor(
    path: &Path,
    controller_id: &str,
    cursor: &ControllerEventCursor,
) -> std::io::Result<ControllerEventBatch> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let mut next = cursor.clone();
            next.offset = 0;
            next.observed_log = None;
            return Ok(ControllerEventBatch {
                events: Vec::new(),
                cursor: next,
            });
        }
        Err(error) => return Err(error),
    };
    let observation = log_observation(&metadata);
    if cursor.observed_log.as_ref() == Some(&observation) {
        return Ok(ControllerEventBatch {
            events: Vec::new(),
            cursor: cursor.clone(),
        });
    }

    let mut file = std::fs::File::open(path)?;
    let start = cursor_start(&mut file, metadata.len(), cursor.offset)?;
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let complete = complete_prefix(&tail);

    let mut next = cursor.clone();
    let mut emitted = BTreeSet::new();
    let events = complete
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<Event>(line).ok())
        .filter_map(|event| ControllerWorkerEvent::from_event(&event))
        .filter(|event| event.controller_id == controller_id)
        .filter(|event| {
            !next.seen_event_ids.contains(&event.event_id) && emitted.insert(event.event_id.clone())
        })
        .collect::<Vec<_>>();

    next.seen_event_ids
        .extend(events.iter().map(|event| event.event_id.clone()));
    next.offset = start.saturating_add(complete.len() as u64);
    next.observed_log = Some(observation);
    Ok(ControllerEventBatch {
        events,
        cursor: next,
    })
}

pub fn consume_worker_events_at(
    log_path: &Path,
    cursor_path: &Path,
    controller_id: &str,
) -> std::io::Result<Vec<ControllerWorkerEvent>> {
    consume_worker_events_at_with(log_path, cursor_path, controller_id, |_| Ok(()))
}

pub fn consume_worker_events_at_with(
    log_path: &Path,
    cursor_path: &Path,
    controller_id: &str,
    before_commit: impl FnOnce(&[ControllerWorkerEvent]) -> std::io::Result<()>,
) -> std::io::Result<Vec<ControllerWorkerEvent>> {
    let _lock = FileLock::acquire_required(cursor_path)?;
    let cursor = match load_event_cursor(cursor_path) {
        Ok(cursor) => cursor,
        Err(error) if error.kind() == ErrorKind::InvalidData => ControllerEventCursor::default(),
        Err(error) => return Err(error),
    };
    let batch = read_worker_events_from_cursor(log_path, controller_id, &cursor)?;
    before_commit(&batch.events)?;
    if batch.cursor != cursor {
        crate::store::save_json_atomic(cursor_path, &batch.cursor)?;
    }
    Ok(batch.events)
}

pub fn consume_worker_events(controller_id: &str) -> std::io::Result<Vec<ControllerWorkerEvent>> {
    consume_worker_events_at(
        &worker_event_log_path(),
        &event_cursor_path(controller_id),
        controller_id,
    )
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
    backlog::add(path, &queue_key(controller_id), requests, &spec, now)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn controller(id: &str) -> ControllerRecord {
        ControllerRecord {
            controller_id: id.into(),
            session_id: format!("session-{id}"),
            task_name: format!("Controller {id}"),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "main".into(),
            target: format!("planning:control-{id}"),
            cwd: "/tmp/repo".into(),
            status: ControllerStatus::Running,
            started_at: 10,
            stopped_at: None,
        }
    }

    fn guard_input<'a>(heartbeat: Option<&'a ControllerHeartbeat>) -> TurnEndGuardInput<'a> {
        TurnEndGuardInput {
            controller_running: true,
            session_id: "session-alpha",
            open_requests: 1,
            live_workers: 0,
            heartbeat,
            now: 100,
            stop_hook_active: false,
        }
    }

    #[test]
    fn heartbeat_round_trips_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat.json");

        assert_eq!(load_heartbeat(&path).unwrap(), None);
        save_heartbeat(&path, "session-alpha", 42).unwrap();
        assert_eq!(
            load_heartbeat(&path).unwrap(),
            Some(ControllerHeartbeat {
                session_id: "session-alpha".into(),
                updated_at: 42,
            })
        );
    }

    #[test]
    fn turn_end_allows_without_live_work_or_with_fresh_matching_heartbeat() {
        let heartbeat = ControllerHeartbeat {
            session_id: "session-alpha".into(),
            updated_at: 95,
        };
        assert_eq!(
            turn_end_decision(&guard_input(Some(&heartbeat))),
            TurnEndDecision::Allow
        );

        let mut input = guard_input(None);
        input.open_requests = 0;
        assert_eq!(turn_end_decision(&input), TurnEndDecision::Allow);
    }

    #[test]
    fn turn_end_blocks_live_work_when_heartbeat_is_absent_stale_or_from_old_session() {
        assert!(matches!(
            turn_end_decision(&guard_input(None)),
            TurnEndDecision::Block { .. }
        ));

        let stale = ControllerHeartbeat {
            session_id: "session-alpha".into(),
            updated_at: 94,
        };
        assert!(matches!(
            turn_end_decision(&guard_input(Some(&stale))),
            TurnEndDecision::Block { .. }
        ));

        let old_session = ControllerHeartbeat {
            session_id: "session-old".into(),
            updated_at: 100,
        };
        assert!(matches!(
            turn_end_decision(&guard_input(Some(&old_session))),
            TurnEndDecision::Block { .. }
        ));
    }

    #[test]
    fn turn_end_forced_follow_up_is_bounded() {
        let mut input = guard_input(None);
        input.stop_hook_active = true;
        assert_eq!(turn_end_decision(&input), TurnEndDecision::Allow);
    }

    #[test]
    fn ownership_join_groups_workers_by_controller_id() {
        let launches = vec![
            LaunchRecord {
                session_id: "worker-a".into(),
                task_name: "Worker A".into(),
                harness: "copilot".into(),
                repo: "bn".into(),
                branch: "dispatch/a".into(),
                worktree: "/tmp/a".into(),
                work_class: crate::cost::WorkClass::Coding,
                launched_at: 20,
                controller_id: Some("alpha".into()),
            },
            LaunchRecord {
                session_id: "worker-b".into(),
                task_name: "Worker B".into(),
                harness: "copilot".into(),
                repo: "bn".into(),
                branch: "dispatch/b".into(),
                worktree: "/tmp/b".into(),
                work_class: crate::cost::WorkClass::Coding,
                launched_at: 21,
                controller_id: Some("beta".into()),
            },
            LaunchRecord {
                session_id: "worker-free".into(),
                task_name: "Worker free".into(),
                harness: "copilot".into(),
                repo: "bn".into(),
                branch: "dispatch/free".into(),
                worktree: "/tmp/free".into(),
                work_class: crate::cost::WorkClass::Coding,
                launched_at: 22,
                controller_id: None,
            },
        ];

        let joined = join_ownership(&[controller("alpha"), controller("beta")], &launches);

        assert_eq!(joined.len(), 2);
        assert_eq!(joined[0].workers.len(), 1);
        assert_eq!(joined[0].workers[0].session_id, "worker-a");
        assert_eq!(joined[1].workers.len(), 1);
        assert_eq!(joined[1].workers[0].session_id, "worker-b");
    }

    #[test]
    fn controller_queue_enqueues_leases_and_drains() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backlog.jsonl");
        let requests = vec!["ship issue 1".to_string(), "triage issue 2".to_string()];

        let ids = enqueue(&path, "alpha", &requests, 10).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(queue_entries(&path, "alpha").len(), 2);
        assert!(queue_entries(&path, "beta").is_empty());
        assert!(!drain(&path, "alpha", &ids[0], true, 15).unwrap());

        let leased = lease_next(&path, "alpha", 20).unwrap().unwrap();
        assert_eq!(leased.status, BacklogStatus::Leased);
        assert_eq!(requeue_leased(&path, "alpha", 25).unwrap(), 1);
        let leased = lease_next(&path, "alpha", 26).unwrap().unwrap();
        assert_eq!(leased.item, "ship issue 1");
        assert!(drain(&path, "alpha", &leased.id, true, 30).unwrap());

        let entries = queue_entries(&path, "alpha");
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == leased.id)
                .unwrap()
                .status,
            BacklogStatus::Done
        );
        assert_eq!(
            lease_next(&path, "alpha", 40).unwrap().unwrap().item,
            "triage issue 2"
        );
    }

    fn worker_event(
        event_type: ControllerWorkerEventType,
        summary: &str,
        ts: i64,
    ) -> ControllerWorkerEvent {
        ControllerWorkerEvent::new(
            "alpha",
            "worker-1",
            "bn",
            "dispatch/example",
            event_type,
            summary,
            None,
            ts,
        )
    }

    #[test]
    fn worker_event_replay_preserves_order_and_filters_controller() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        append_worker_event_at(
            &log,
            &worker_event(ControllerWorkerEventType::Progress, "started", 10),
        )
        .unwrap();
        append_worker_event_at(
            &log,
            &ControllerWorkerEvent::new(
                "beta",
                "worker-2",
                "bn",
                "dispatch/other",
                ControllerWorkerEventType::Done,
                "other controller",
                None,
                11,
            ),
        )
        .unwrap();
        append_worker_event_at(
            &log,
            &worker_event(ControllerWorkerEventType::Done, "finished", 12),
        )
        .unwrap();

        let replay = read_worker_events_at(&log, "alpha");
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].summary, "started");
        assert_eq!(replay[1].summary, "finished");
    }

    #[test]
    fn worker_wake_ignores_progress_and_carries_delivery_context() {
        assert!(actionable_worker_wake(&worker_event(
            ControllerWorkerEventType::Progress,
            "started",
            10
        ))
        .is_none());
        let wake = actionable_worker_wake(&worker_event(
            ControllerWorkerEventType::Blocked,
            "needs input",
            11,
        ))
        .unwrap();
        assert_eq!(wake.summary, "blocked bn:dispatch/example — needs input");
        assert!(wake
            .pending_targets
            .contains(&crate::wake::WakeTarget::Controller { id: "alpha".into() }));
    }

    #[test]
    fn duplicate_blocked_event_is_appended_and_consumed_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        let event = worker_event(ControllerWorkerEventType::Blocked, "lane is stuck", 10);
        append_worker_event_at(&log, &event).unwrap();
        append_worker_event_at(&log, &event).unwrap();

        assert_eq!(read_worker_events_at(&log, "alpha"), vec![event.clone()]);
        let consumed = consume_worker_events_at(&log, &cursor, "alpha").unwrap();
        assert_eq!(consumed, vec![event]);
        assert!(consume_worker_events_at(&log, &cursor, "alpha")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cursor_restart_reads_only_new_events() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        append_worker_event_at(
            &log,
            &worker_event(ControllerWorkerEventType::Progress, "started", 10),
        )
        .unwrap();
        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha")
                .unwrap()
                .len(),
            1
        );

        append_worker_event_at(
            &log,
            &worker_event(ControllerWorkerEventType::PrOpened, "PR opened", 11),
        )
        .unwrap();
        let after_restart = consume_worker_events_at(&log, &cursor, "alpha").unwrap();
        assert_eq!(after_restart.len(), 1);
        assert_eq!(
            after_restart[0].event_type,
            ControllerWorkerEventType::PrOpened
        );
    }

    #[test]
    fn cursor_seeks_to_the_stored_newline_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        append_worker_event_at(
            &log,
            &worker_event(ControllerWorkerEventType::Progress, "started", 10),
        )
        .unwrap();
        let first_event_end = std::fs::metadata(&log).unwrap().len();
        let fresh = worker_event(ControllerWorkerEventType::Done, "finished", 11);
        append_worker_event_at(&log, &fresh).unwrap();

        let cursor = ControllerEventCursor {
            offset: first_event_end,
            ..ControllerEventCursor::default()
        };
        let batch = read_worker_events_from_cursor(&log, "alpha", &cursor).unwrap();

        assert_eq!(batch.events, vec![fresh]);
        assert_eq!(batch.cursor.offset, std::fs::metadata(&log).unwrap().len());
    }

    #[test]
    fn unchanged_log_metadata_skips_cursor_read() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let event = worker_event(ControllerWorkerEventType::Done, "finished", 10);
        append_worker_event_at(&log, &event).unwrap();
        let observed =
            read_worker_events_from_cursor(&log, "alpha", &ControllerEventCursor::default())
                .unwrap()
                .cursor;
        let unchanged = ControllerEventCursor {
            offset: 0,
            seen_event_ids: BTreeSet::new(),
            ..observed
        };

        let batch = read_worker_events_from_cursor(&log, "alpha", &unchanged).unwrap();

        assert!(batch.events.is_empty());
        assert_eq!(batch.cursor, unchanged);
    }

    #[test]
    fn cursor_recovers_after_log_truncation_without_replaying_seen_ids() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        let old = worker_event(ControllerWorkerEventType::Progress, "started", 10);
        append_worker_event_at(&log, &old).unwrap();
        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha").unwrap(),
            vec![old.clone()]
        );

        std::fs::write(&log, []).unwrap();
        append_worker_event_at(&log, &old).unwrap();
        let fresh = worker_event(ControllerWorkerEventType::Done, "finished", 11);
        append_worker_event_at(&log, &fresh).unwrap();

        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha").unwrap(),
            vec![fresh]
        );
    }

    #[test]
    fn malformed_lines_are_skipped_and_cursor_advances() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        {
            let mut file = std::fs::File::create(&log).unwrap();
            writeln!(file, "{{malformed").unwrap();
        }
        let valid = worker_event(ControllerWorkerEventType::Blocked, "needs input", 10);
        append_worker_event_at(&log, &valid).unwrap();

        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha").unwrap(),
            vec![valid]
        );
        assert!(consume_worker_events_at(&log, &cursor, "alpha")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn corrupt_cursor_recovers_by_replaying_the_durable_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        let valid = worker_event(ControllerWorkerEventType::Done, "finished", 10);
        append_worker_event_at(&log, &valid).unwrap();
        std::fs::write(&cursor, "{invalid").unwrap();

        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha").unwrap(),
            vec![valid]
        );
        load_event_cursor(&cursor).unwrap();
    }

    #[test]
    fn cursor_is_not_committed_when_delivery_fails() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let cursor = dir.path().join("alpha.cursor.json");
        let valid = worker_event(ControllerWorkerEventType::Done, "finished", 10);
        append_worker_event_at(&log, &valid).unwrap();

        let error = consume_worker_events_at_with(&log, &cursor, "alpha", |_| {
            Err(std::io::Error::other("wake failed"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(!cursor.exists());
        assert_eq!(
            consume_worker_events_at(&log, &cursor, "alpha").unwrap(),
            vec![valid]
        );
    }
}
