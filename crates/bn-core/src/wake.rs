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
    /// Written by the removed `bn afk`; kept so older queue files still parse, and stripped on load.
    Afk,
    Controller {
        id: String,
    },
    Orchestrate {
        repo: String,
    },
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
            Ok(body) => {
                let mut state: Self = serde_json::from_str(&body)
                    .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?;
                // Nothing reads the away-mode digest any more: drop its target, and any wake that
                // was waiting for nothing else, so it cannot sit in the queue forever.
                for item in &mut state.items {
                    item.pending_targets.remove(&WakeTarget::Afk);
                }
                state.items.retain(|item| !item.pending_targets.is_empty());
                Ok(state)
            }
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
    pub fn publish(&self, wake: ActionableWake) -> std::io::Result<bool> {
        let _lock = FileLock::acquire_required(&self.queue_path)?;
        let mut state = WakeState::load(&self.queue_path)?;
        let changed = if let Some(existing) = state
            .items
            .iter_mut()
            .find(|item| item.event_id == wake.event_id)
        {
            let old_len = existing.pending_targets.len();
            existing.pending_targets.extend(
                wake.pending_targets
                    .into_iter()
                    .filter(|target| !existing.acknowledged_targets.contains(target)),
            );
            existing.pending_targets.len() != old_len
        } else if state.seen_event_ids.contains(&wake.event_id) {
            false
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wake(id: &str, targets: &[WakeTarget]) -> ActionableWake {
        ActionableWake {
            event_id: id.to_string(),
            ts: 1,
            transition: FleetTransition::Waiting,
            summary: id.to_string(),
            pending_targets: targets.iter().cloned().collect(),
            acknowledged_targets: BTreeSet::new(),
        }
    }

    /// A queue written while away mode existed still parses, and a wake that only the away digest
    /// was waiting for is dropped instead of sitting in the queue forever.
    #[test]
    fn old_away_mode_targets_are_stripped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake-queue.json");
        let old = WakeState {
            items: vec![
                wake("afk-only", &[WakeTarget::Afk]),
                wake("both", &[WakeTarget::Watch, WakeTarget::Afk]),
            ],
            ..WakeState::default()
        };
        std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();

        let loaded = WakeState::load(&path).unwrap();
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].event_id, "both");
        assert_eq!(
            loaded.items[0].pending_targets,
            BTreeSet::from([WakeTarget::Watch])
        );
    }
}
