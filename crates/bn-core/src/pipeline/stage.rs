//! The per-worktree pipeline stage state machine (PIPE-A, EPIC #746).
//!
//! A dispatched task flows through explicit, observable stages:
//! `pickup → investigate → plan → consensus → execute → validate → critique → ship → learn`.
//! The [`Stage`] enum + its ordering are **pure** (unit-tested below). The current stage is durable
//! in branch-note frontmatter, while every transition is also appended to [`crate::events`] as a
//! `pipeline_stage` event keyed by `(repo, branch)`. Legacy notes fall back to their latest event,
//! or to [`Stage::DEFAULT`] when neither source exists.

use super::gate;
use crate::events;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map};
use std::path::Path;

/// The event `kind` under which stage transitions are logged.
pub const EVENT_KIND: &str = "pipeline_stage";

/// One pipeline stage. Ordered pickup→learn so [`Stage::next`] and comparisons follow the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Task leased into a fresh, uniquely-branched worktree.
    Pickup,
    /// Research the problem + codebase; write findings for the planner.
    Investigate,
    /// Consume findings, propose the fix + done-when; submit for review.
    Plan,
    /// Reviewer agents vote; quorum advances, high-risk/no-consensus pulls in a human.
    Consensus,
    /// Implement exactly the agreed plan.
    Execute,
    /// `bn verify` — build + test + lint (re-verified after any rebase).
    Validate,
    /// Fresh-context adversarial diff review vs the acceptance criteria.
    Critique,
    /// Open / mark-ready the PR; the orchestrator lands it on merge.
    Ship,
    /// Fold the outcome + reflections back (`bn ingest` / `evolve-skills`).
    Learn,
}

impl Stage {
    /// Every stage in flow order.
    pub const ALL: [Stage; 9] = [
        Stage::Pickup,
        Stage::Investigate,
        Stage::Plan,
        Stage::Consensus,
        Stage::Execute,
        Stage::Validate,
        Stage::Critique,
        Stage::Ship,
        Stage::Learn,
    ];

    /// The stage a worktree with no recorded transition folds to — the flow's entry point.
    pub const DEFAULT: Stage = Stage::Pickup;

    /// The kebab-case identifier persisted in the event log and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Pickup => "pickup",
            Stage::Investigate => "investigate",
            Stage::Plan => "plan",
            Stage::Consensus => "consensus",
            Stage::Execute => "execute",
            Stage::Validate => "validate",
            Stage::Critique => "critique",
            Stage::Ship => "ship",
            Stage::Learn => "learn",
        }
    }

    /// A compact glyph for the tmux status row / fleet board.
    pub fn glyph(self) -> &'static str {
        match self {
            Stage::Pickup => "◦",
            Stage::Investigate => "🔍",
            Stage::Plan => "◇",
            Stage::Consensus => "🗳",
            Stage::Execute => "⚙",
            Stage::Validate => "✓",
            Stage::Critique => "⚖",
            Stage::Ship => "🚢",
            Stage::Learn => "🎓",
        }
    }

    /// The 1-based position of this stage in the flow (`pickup` == 1, `learn` == 9).
    pub fn position(self) -> usize {
        Stage::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }

    /// The next stage in the flow, or `None` at the terminal [`Stage::Learn`].
    pub fn next(self) -> Option<Stage> {
        let i = self.position(); // 1-based; ALL[i] is the successor
        Stage::ALL.get(i).copied()
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Stage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Stage::ALL
            .into_iter()
            .find(|st| st.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| {
                format!(
                    "unknown stage {s:?} — expected one of: {}",
                    Stage::ALL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// One recorded stage transition for a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub stage: Stage,
    /// Unix epoch seconds when the transition was recorded.
    pub ts: i64,
}

/// The current stage + how many transitions a worktree has been through — the `board` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardEntry {
    pub repo: String,
    pub branch: String,
    pub stage: Stage,
    /// Unix epoch seconds of the latest transition.
    pub ts: i64,
}

fn field_str(ev: &events::Event, key: &str) -> Option<String> {
    ev.fields
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Record that `(repo, branch)` entered `stage`, appending a `pipeline_stage` event stamped now.
pub fn record(repo: &str, branch: &str, stage: Stage) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("repo".into(), json!(repo));
    fields.insert("branch".into(), json!(branch));
    fields.insert("stage".into(), json!(stage.as_str()));
    events::append_at(EVENT_KIND, now_epoch_secs(), fields)
}

/// The full transition history for `(repo, branch)`, oldest first. Empty when none recorded.
pub fn history(repo: &str, branch: &str) -> Vec<Transition> {
    let mut evs: Vec<&events::Event> = Vec::new();
    let all = events::read_all();
    for ev in &all {
        if ev.kind != EVENT_KIND {
            continue;
        }
        if field_str(ev, "repo").as_deref() == Some(repo)
            && field_str(ev, "branch").as_deref() == Some(branch)
        {
            evs.push(ev);
        }
    }
    evs.sort_by_key(|e| e.ts);
    evs.into_iter()
        .filter_map(|e| {
            field_str(e, "stage")
                .and_then(|s| s.parse::<Stage>().ok())
                .map(|stage| Transition { stage, ts: e.ts })
        })
        .collect()
}

/// The latest transition event for `(repo, branch)`, or `None` if none was recorded.
pub fn current(repo: &str, branch: &str) -> Option<Stage> {
    history(repo, branch).last().map(|t| t.stage)
}

/// Read the durable current stage from a branch note, falling back to the latest transition event
/// for notes written before the `stage:` frontmatter field existed.
pub fn current_for_note(note: &crate::model::Note) -> Option<Stage> {
    note.front
        .stage
        .or_else(|| current(&note.front.repo, &note.front.branch))
}

/// Persist the durable current stage and append its transition event — **through the SMART gate**.
///
/// This is the single enforcement point for #998. Everything that starts work (`bn dispatch`,
/// `bn attn start`, `bn pipeline set`/`advance`) records its stage here, so all of them are gated
/// by construction rather than by remembering to call a checker. A refused transition writes
/// nothing: the note keeps its old stage, no event is appended, and the returned
/// [`gate::GateDecision`] carries the reasons plus the attention item that was raised to fix them.
///
/// Ungated stages and worktrees with no governing task pass straight through, so this is safe to
/// call for every transition.
pub fn transition(
    note_path: &Path,
    stage: Stage,
) -> Result<gate::GateDecision, crate::store::StoreError> {
    let _lock = crate::store::NoteLock::acquire_required(note_path)?;
    let mut note = crate::store::read(note_path)?;

    let decision = if gate::is_gated(stage) {
        // Only load the config on a gated transition — the common path stays a note write.
        let cfg = crate::resolver::Config::load();
        gate::check_entry(
            &cfg,
            &note.front.repo,
            &note.front.branch,
            stage,
            &crate::util::today(),
        )
    } else {
        gate::GateDecision::Admitted
    };
    if !decision.admits() {
        return Ok(decision);
    }

    note.front.stage = Some(stage);
    crate::store::save(&note, note_path)?;
    record(&note.front.repo, &note.front.branch, stage)?;
    Ok(decision)
}

/// Every worktree with a transition event, newest-transition first.
pub fn board() -> Vec<BoardEntry> {
    use std::collections::HashMap;
    let mut latest: HashMap<(String, String), (Stage, i64)> = HashMap::new();
    for ev in events::read_all() {
        if ev.kind != EVENT_KIND {
            continue;
        }
        let (Some(repo), Some(branch), Some(stage)) = (
            field_str(&ev, "repo"),
            field_str(&ev, "branch"),
            field_str(&ev, "stage").and_then(|s| s.parse::<Stage>().ok()),
        ) else {
            continue;
        };
        let key = (repo, branch);
        match latest.get(&key) {
            // Keep the existing only if it's strictly newer; on an equal `ts` the later-
            // appended event (file order) wins, matching the append-only log's intent.
            Some((_, ts)) if *ts > ev.ts => {}
            _ => {
                latest.insert(key, (stage, ev.ts));
            }
        }
    }
    let mut out: Vec<BoardEntry> = latest
        .into_iter()
        .map(|((repo, branch), (stage, ts))| BoardEntry {
            repo,
            branch,
            stage,
            ts,
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.ts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Note;
    use crate::resolver::RepoBranch;
    use crate::test_support::with_state_dir_path as with_state_dir;

    #[test]
    fn stage_str_round_trips() {
        for s in Stage::ALL {
            assert_eq!(s.as_str().parse::<Stage>().unwrap(), s);
            assert_eq!(s.to_string(), s.as_str());
        }
    }

    #[test]
    fn from_str_is_case_insensitive_and_trims() {
        assert_eq!("  Plan ".parse::<Stage>().unwrap(), Stage::Plan);
        assert_eq!("EXECUTE".parse::<Stage>().unwrap(), Stage::Execute);
    }

    #[test]
    fn unknown_stage_errors_with_the_valid_set() {
        let err = "deploy".parse::<Stage>().unwrap_err();
        assert!(err.contains("unknown stage"));
        assert!(err.contains("pickup"));
        assert!(err.contains("learn"));
    }

    #[test]
    fn next_follows_the_flow_and_ends_at_learn() {
        assert_eq!(Stage::Pickup.next(), Some(Stage::Investigate));
        assert_eq!(Stage::Critique.next(), Some(Stage::Ship));
        assert_eq!(Stage::Ship.next(), Some(Stage::Learn));
        assert_eq!(Stage::Learn.next(), None);
    }

    #[test]
    fn position_is_one_based_in_flow_order() {
        assert_eq!(Stage::Pickup.position(), 1);
        assert_eq!(Stage::Learn.position(), 9);
        assert!(Stage::Plan < Stage::Execute, "ordering follows the flow");
    }

    #[test]
    fn current_is_none_before_any_transition() {
        with_state_dir(|_| {
            assert_eq!(current("bn", "feat"), None);
            assert!(history("bn", "feat").is_empty());
        });
    }

    #[test]
    fn record_then_current_returns_latest() {
        with_state_dir(|_| {
            record("bn", "feat", Stage::Investigate).unwrap();
            record("bn", "feat", Stage::Plan).unwrap();
            assert_eq!(current("bn", "feat"), Some(Stage::Plan));
            let h = history("bn", "feat");
            assert_eq!(h.len(), 2);
            assert_eq!(h[0].stage, Stage::Investigate);
            assert_eq!(h[1].stage, Stage::Plan);
        });
    }

    #[test]
    fn transition_persists_frontmatter_and_appends_history() {
        with_state_dir(|_| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("note.md");
            let note = Note::new(RepoBranch::new("bn", "feat"), "2026-07-18", "testhost");
            crate::store::save(&note, &path).unwrap();

            transition(&path, Stage::Plan).unwrap();

            let saved = crate::store::read(&path).unwrap();
            assert_eq!(saved.front.stage, Some(Stage::Plan));
            assert_eq!(current_for_note(&saved), Some(Stage::Plan));
            assert_eq!(
                history("bn", "feat")
                    .into_iter()
                    .map(|entry| entry.stage)
                    .collect::<Vec<_>>(),
                vec![Stage::Plan]
            );
        });
    }

    #[test]
    fn frontmatter_stage_wins_over_legacy_event_fallback() {
        with_state_dir(|_| {
            record("bn", "feat", Stage::Investigate).unwrap();
            let mut note = Note::new(RepoBranch::new("bn", "feat"), "2026-07-18", "testhost");
            assert_eq!(current_for_note(&note), Some(Stage::Investigate));
            note.front.stage = Some(Stage::Execute);
            assert_eq!(current_for_note(&note), Some(Stage::Execute));
        });
    }

    #[test]
    fn transitions_are_scoped_per_repo_branch() {
        with_state_dir(|_| {
            record("bn", "a", Stage::Plan).unwrap();
            record("bn", "b", Stage::Ship).unwrap();
            record("other", "a", Stage::Execute).unwrap();
            assert_eq!(current("bn", "a"), Some(Stage::Plan));
            assert_eq!(current("bn", "b"), Some(Stage::Ship));
            assert_eq!(current("other", "a"), Some(Stage::Execute));
        });
    }

    #[test]
    fn full_flow_advances_a_worktree_pickup_through_learn() {
        with_state_dir(|_| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("note.md");
            let note = Note::new(RepoBranch::new("bn", "feat"), "2026-07-18", "testhost");
            crate::store::save(&note, &path).unwrap();

            // The advance engine: start at the flow entry point, then walk `next()` to the end,
            // persisting each hop through the real `transition()` (frontmatter + event log).
            let mut stage = Stage::DEFAULT;
            transition(&path, stage).unwrap();

            let mut visited = vec![stage];
            let mut expected_position = 1;
            loop {
                // Each persisted stage is observable via both the durable note and the event log.
                let saved = crate::store::read(&path).unwrap();
                assert_eq!(saved.front.stage, Some(stage), "frontmatter tracks {stage}");
                assert_eq!(current_for_note(&saved), Some(stage));
                assert_eq!(
                    current("bn", "feat"),
                    Some(stage),
                    "event log tracks {stage}"
                );
                assert_eq!(
                    stage.position(),
                    expected_position,
                    "flow position is monotonic"
                );

                match stage.next() {
                    Some(next) => {
                        stage = next;
                        transition(&path, stage).unwrap();
                        visited.push(stage);
                        expected_position += 1;
                    }
                    None => break,
                }
            }

            // Walked every stage, in flow order, ending at the terminal `learn`.
            assert_eq!(visited, Stage::ALL.to_vec(), "every stage visited in order");
            assert_eq!(stage, Stage::Learn, "flow ends at learn");
            assert_eq!(stage.next(), None, "learn is terminal");

            // The full history mirrors the flow, oldest first, one entry per stage.
            let flow: Vec<Stage> = history("bn", "feat").into_iter().map(|t| t.stage).collect();
            assert_eq!(flow, Stage::ALL.to_vec(), "history replays the whole flow");

            // The board's single row for this worktree reflects the final stage.
            let row = board()
                .into_iter()
                .find(|e| e.repo == "bn" && e.branch == "feat")
                .expect("worktree appears on the board");
            assert_eq!(row.stage, Stage::Learn, "board shows the terminal stage");
        });
    }

    #[test]
    fn board_shows_latest_per_worktree_newest_first() {
        with_state_dir(|_| {
            record("bn", "a", Stage::Pickup).unwrap();
            record("bn", "a", Stage::Investigate).unwrap();
            record("bn", "b", Stage::Execute).unwrap();
            let b = board();
            assert_eq!(b.len(), 2, "one row per (repo, branch)");
            // both share the second-resolution timestamp; assert the set, not the order
            let a = b.iter().find(|e| e.branch == "a").unwrap();
            assert_eq!(a.stage, Stage::Investigate, "latest wins for a");
            let bb = b.iter().find(|e| e.branch == "b").unwrap();
            assert_eq!(bb.stage, Stage::Execute);
        });
    }
}
