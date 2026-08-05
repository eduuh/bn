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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir;

    fn queue(dir: &tempfile::TempDir) -> WakeQueue {
        WakeQueue::at(dir.path().join("queue.json"), dir.path().join("signal"))
    }

    fn with_queue(test: impl FnOnce(&WakeQueue)) {
        with_state_dir(|| {
            let dir = tempfile::tempdir().unwrap();
            test(&queue(&dir));
        });
    }

    #[test]
    fn classifier_ignores_progress_and_routes_actionable_transitions() {
        assert!(ActionableWake::classify(
            "p1",
            1,
            FleetTransition::Progress,
            "working",
            WakeOwner::None
        )
        .is_none());

        let wake = ActionableWake::classify(
            "b1",
            2,
            FleetTransition::Blocked,
            "needs input",
            WakeOwner::Controller("captain".into()),
        )
        .unwrap();
        assert_eq!(
            wake.pending_targets,
            BTreeSet::from([
                WakeTarget::Watch,
                WakeTarget::Controller {
                    id: "captain".into()
                }
            ])
        );
    }

    #[test]
    fn publish_deduplicates_and_acknowledges_targets_independently() {
        with_queue(|queue| {
            let wake = ActionableWake::classify(
                "done-1",
                10,
                FleetTransition::Done,
                "finished",
                WakeOwner::Orchestrate("bn".into()),
            )
            .unwrap();

            assert!(queue.publish(wake.clone()).unwrap());
            assert!(!queue.publish(wake.clone()).unwrap());
            assert_eq!(queue.pending(&WakeTarget::Watch).unwrap().len(), 1);
            let orchestrate = WakeTarget::Orchestrate { repo: "bn".into() };
            assert_eq!(queue.pending(&orchestrate).unwrap().len(), 1);

            let ids = BTreeSet::from(["done-1".to_string()]);
            assert_eq!(queue.acknowledge(&WakeTarget::Watch, &ids).unwrap(), 1);
            assert!(queue.pending(&WakeTarget::Watch).unwrap().is_empty());
            assert_eq!(queue.pending(&orchestrate).unwrap().len(), 1);
            assert!(!queue.publish(wake).unwrap());
            assert!(queue.pending(&WakeTarget::Watch).unwrap().is_empty());
            assert_eq!(queue.acknowledge(&orchestrate, &ids).unwrap(), 1);
            assert!(queue.pending(&orchestrate).unwrap().is_empty());
            assert!(!queue
                .publish(
                    ActionableWake::classify(
                        "done-1",
                        11,
                        FleetTransition::Done,
                        "finished",
                        WakeOwner::Orchestrate("bn".into()),
                    )
                    .unwrap()
                )
                .unwrap());
        });
    }

    #[test]
    fn target_isolation_keeps_other_consumers_pending() {
        with_queue(|queue| {
            for repo in ["alpha", "beta"] {
                queue
                    .publish(
                        ActionableWake::classify(
                            format!("{repo}-failed"),
                            20,
                            FleetTransition::Failed,
                            "failed",
                            WakeOwner::Orchestrate(repo.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap();
            }

            let alpha = WakeTarget::Orchestrate {
                repo: "alpha".into(),
            };
            let beta = WakeTarget::Orchestrate {
                repo: "beta".into(),
            };
            assert_eq!(queue.pending(&alpha).unwrap().len(), 1);
            assert_eq!(queue.pending(&beta).unwrap().len(), 1);
            queue
                .acknowledge(&alpha, &BTreeSet::from(["alpha-failed".into()]))
                .unwrap();
            assert!(queue.pending(&alpha).unwrap().is_empty());
            assert_eq!(queue.pending(&beta).unwrap().len(), 1);
        });
    }

    #[test]
    fn publish_advances_signal_only_for_new_delivery() {
        with_queue(|queue| {
            let wake = ActionableWake::classify(
                "wait-1",
                30,
                FleetTransition::Waiting,
                "waiting",
                WakeOwner::None,
            )
            .unwrap();
            queue.publish(wake.clone()).unwrap();
            assert_eq!(std::fs::read_to_string(queue.signal_path()).unwrap(), "1");
            queue.publish(wake).unwrap();
            assert_eq!(std::fs::read_to_string(queue.signal_path()).unwrap(), "1");
        });
    }

    #[test]
    fn away_presence_adds_an_isolated_afk_delivery() {
        with_queue(|queue| {
            crate::afk::start(100).unwrap();
            let wake = ActionableWake::classify(
                "blocked-1",
                110,
                FleetTransition::Blocked,
                "needs input",
                WakeOwner::None,
            )
            .unwrap();

            queue.publish(wake).unwrap();
            let ids = BTreeSet::from(["blocked-1".to_string()]);
            assert_eq!(queue.pending(&WakeTarget::Watch).unwrap().len(), 1);
            assert_eq!(queue.pending(&WakeTarget::Afk).unwrap().len(), 1);
            queue.acknowledge(&WakeTarget::Watch, &ids).unwrap();
            assert!(queue.pending(&WakeTarget::Watch).unwrap().is_empty());
            assert_eq!(queue.pending(&WakeTarget::Afk).unwrap().len(), 1);
        });
    }

    #[test]
    fn afk_can_receive_an_event_seen_before_away_mode() {
        with_queue(|queue| {
            let wake = ActionableWake::classify(
                "attention-1",
                90,
                FleetTransition::Attention,
                "review needed",
                WakeOwner::None,
            )
            .unwrap();
            queue.publish(wake.clone()).unwrap();
            queue
                .acknowledge(
                    &WakeTarget::Watch,
                    &BTreeSet::from(["attention-1".to_string()]),
                )
                .unwrap();

            crate::afk::start(100).unwrap();
            assert!(queue.publish(wake).unwrap());
            assert!(queue.pending(&WakeTarget::Watch).unwrap().is_empty());
            assert_eq!(queue.pending(&WakeTarget::Afk).unwrap().len(), 1);
        });
    }

    #[test]
    fn a_new_away_block_resets_only_the_prior_afk_acknowledgement() {
        with_queue(|queue| {
            crate::afk::start(100).unwrap();
            let wake = ActionableWake::classify(
                "blocked-1",
                110,
                FleetTransition::Blocked,
                "needs input",
                WakeOwner::Orchestrate("bn".into()),
            )
            .unwrap();
            queue.publish(wake.clone()).unwrap();
            crate::afk::end(120).unwrap();
            let ids = BTreeSet::from(["blocked-1".to_string()]);
            queue.acknowledge(&WakeTarget::Afk, &ids).unwrap();

            crate::afk::start(200).unwrap();
            assert!(queue.publish(wake).unwrap());
            assert_eq!(queue.pending(&WakeTarget::Afk).unwrap().len(), 1);
            assert_eq!(queue.pending(&WakeTarget::Watch).unwrap().len(), 1);
            assert_eq!(
                queue
                    .pending(&WakeTarget::Orchestrate { repo: "bn".into() })
                    .unwrap()
                    .len(),
                1
            );
        });
    }
}
