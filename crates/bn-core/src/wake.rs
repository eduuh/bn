//! Durable actionable wake delivery for fleet supervision.
//!
//! Producers classify transitions before enqueueing them. Consumers acknowledge only their own
//! target, so one transition can independently wake the human-facing watcher and an owning
//! controller or orchestrator without either consumer racing the other.

use crate::store::{self, FileLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const SEEN_EVENT_CAP: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WakeTarget {
    Watch,
    Afk,
    Controller { id: String },
    Orchestrate { repo: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOwner {
    None,
    Controller(String),
    Orchestrate(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FleetTransition {
    Progress,
    Waiting,
    Blocked,
    PrOpened,
    Done,
    Failed,
    Stuck,
    Attention,
}

impl FleetTransition {
    pub fn is_actionable(self) -> bool {
        !matches!(self, Self::Progress)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionableWake {
    pub event_id: String,
    pub ts: i64,
    pub transition: FleetTransition,
    pub summary: String,
    pub pending_targets: BTreeSet<WakeTarget>,
    #[serde(default)]
    pub acknowledged_targets: BTreeSet<WakeTarget>,
}

impl ActionableWake {
    pub fn classify(
        event_id: impl Into<String>,
        ts: i64,
        transition: FleetTransition,
        summary: impl Into<String>,
        owner: WakeOwner,
    ) -> Option<Self> {
        if !transition.is_actionable() {
            return None;
        }
        let mut pending_targets = BTreeSet::from([WakeTarget::Watch]);
        match owner {
            WakeOwner::None => {}
            WakeOwner::Controller(id) => {
                pending_targets.insert(WakeTarget::Controller { id });
            }
            WakeOwner::Orchestrate(repo) => {
                pending_targets.insert(WakeTarget::Orchestrate { repo });
            }
        }
        Some(Self {
            event_id: event_id.into(),
            ts,
            transition,
            summary: summary.into(),
            pending_targets,
            acknowledged_targets: BTreeSet::new(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct WakeState {
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    items: Vec<ActionableWake>,
    #[serde(default)]
    seen_event_ids: Vec<String>,
}

impl WakeState {
    fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(body) => serde_json::from_str(&body)
                .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        store::save_json_atomic(path, self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeQueue {
    queue_path: PathBuf,
    signal_path: PathBuf,
}

impl Default for WakeQueue {
    fn default() -> Self {
        let state = crate::resolver::state_dir();
        Self::at(
            state.join("wake-queue.json"),
            state.join("wake-queue.signal"),
        )
    }
}

impl WakeQueue {
    pub fn at(queue_path: PathBuf, signal_path: PathBuf) -> Self {
        Self {
            queue_path,
            signal_path,
        }
    }

    pub fn queue_path(&self) -> &Path {
        &self.queue_path
    }

    pub fn signal_path(&self) -> &Path {
        &self.signal_path
    }

    /// Enqueue a classified wake. Re-publishing the same event id merges any newly introduced
    /// targets rather than creating duplicate deliveries.
    pub fn publish(&self, mut wake: ActionableWake) -> std::io::Result<bool> {
        if crate::afk::is_active(wake.ts) {
            wake.pending_targets.insert(WakeTarget::Afk);
        }
        let _lock = FileLock::acquire_required(&self.queue_path)?;
        let mut state = WakeState::load(&self.queue_path)?;
        let changed = if let Some(existing) = state
            .items
            .iter_mut()
            .find(|item| item.event_id == wake.event_id)
        {
            if wake.pending_targets.contains(&WakeTarget::Afk) {
                existing.acknowledged_targets.remove(&WakeTarget::Afk);
            }
            let old_len = existing.pending_targets.len();
            existing.pending_targets.extend(
                wake.pending_targets
                    .into_iter()
                    .filter(|target| !existing.acknowledged_targets.contains(target)),
            );
            existing.pending_targets.len() != old_len
        } else if state.seen_event_ids.contains(&wake.event_id) {
            if wake.pending_targets.contains(&WakeTarget::Afk) {
                wake.pending_targets = BTreeSet::from([WakeTarget::Afk]);
                wake.acknowledged_targets.clear();
                state.items.push(wake);
                true
            } else {
                false
            }
        } else {
            state.seen_event_ids.push(wake.event_id.clone());
            let excess = state.seen_event_ids.len().saturating_sub(SEEN_EVENT_CAP);
            if excess > 0 {
                state.seen_event_ids.drain(..excess);
            }
            state.items.push(wake);
            true
        };
        if changed {
            state.generation = state.generation.wrapping_add(1);
            state.save(&self.queue_path)?;
            store::write_atomic(&self.signal_path, state.generation.to_string().as_bytes())?;
        }
        Ok(changed)
    }

    pub fn pending(&self, target: &WakeTarget) -> std::io::Result<Vec<ActionableWake>> {
        let state = WakeState::load(&self.queue_path)?;
        Ok(state
            .items
            .into_iter()
            .filter(|item| item.pending_targets.contains(target))
            .collect())
    }

    /// Acknowledge deliveries for one consumer. Items remain durable until every target has
    /// acknowledged them.
    pub fn acknowledge(
        &self,
        target: &WakeTarget,
        event_ids: &BTreeSet<String>,
    ) -> std::io::Result<usize> {
        let _lock = FileLock::acquire_required(&self.queue_path)?;
        let mut state = WakeState::load(&self.queue_path)?;
        let mut acknowledged = 0;
        for item in &mut state.items {
            if event_ids.contains(&item.event_id) && item.pending_targets.remove(target) {
                item.acknowledged_targets.insert(target.clone());
                acknowledged += 1;
            }
        }
        if acknowledged > 0 {
            state.items.retain(|item| !item.pending_targets.is_empty());
            state.save(&self.queue_path)?;
        }
        Ok(acknowledged)
    }
}


