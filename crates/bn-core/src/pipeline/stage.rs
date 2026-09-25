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

    /// The workflow skill this stage dispatches by default — the *intent* of the stage, as
    /// opposed to [`skill_for_note`], which reports what a given worktree actually ran.
    ///
    /// `None` is a real answer, not a gap: `pickup` leases a worktree, `consensus` is reviewer
    /// votes, `validate` is `bn verify`, and `ship` hands off to the orchestrator. None of those
    /// are a harness following a workflow, and naming a skill for them would be a decoration that
    /// reads as a fact.
    ///
    /// Distinct from `bn dispatch`'s `dispatch_skill_name`, which answers "what does an ad-hoc
    /// dispatch run?" (a ship gets `plan-work`, which writes its own plan first). This answers
    /// "what does the nine-stage pipeline run here?", where `execute` picks up a plan that
    /// `consensus` already approved. Both are right; they are not the same question.
    pub fn default_skill(self) -> Option<&'static str> {
        match self {
            Stage::Investigate => Some("investigate"),
            Stage::Plan => Some("plan-investigate"),
            Stage::Execute => Some("execute"),
            Stage::Critique => Some("critique-fresh"),
            Stage::Learn => Some("evolve-skills"),
            Stage::Pickup | Stage::Consensus | Stage::Validate | Stage::Ship => None,
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
    /// The workflow skill this worktree's agent is actually running ([`skill_for_note`]).
    /// `None` from [`board`], which reads only the event log; filled by [`board_with_skills`],
    /// which also reads the notes.
    #[serde(default)]
    pub skill: Option<String>,
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

/// The workflow skill a worktree's agent is **actually** running, read back from the `skill: <name>
/// — …` decision `bn dispatch` writes into the note at launch.
///
/// Reality, as opposed to [`Stage::default_skill`]'s intent: an explicit `bn dispatch --skill debug`
/// puts `debug` here while the stage still reads `execute`, and showing the stage default in that
/// case would be a confident lie about what the agent is doing.
///
/// `None` when the note carries no such decision — a hand-made branch, or a dispatch from before
/// the line was recorded.
pub fn skill_for_note(note: &crate::model::Note) -> Option<String> {
    note.front.decisions.iter().find_map(|d| {
        let rest = d.text.trim().strip_prefix("skill:")?;
        // The recorded line is `skill: <name> — <why>`. Cut at the em-dash, or at a spaced ASCII
        // hyphen — never a bare `-`, which lives *inside* half the skill names (`plan-work`,
        // `critique-fresh`, `auto-review-pr`) and would truncate them to their first word.
        let name = rest
            .split_once('—')
            .or_else(|| rest.split_once(" - "))
            .map(|(name, _)| name)
            .unwrap_or(rest)
            .trim()
            .to_string();
        (!name.is_empty()).then_some(name)
    })
}

/// The progress note a stage transition leaves on its task.
pub fn stage_note(stage: Stage, repo: &str, branch: &str) -> String {
    format!(
        "stage {}/{}: {stage} ({repo}:{branch})",
        stage.position(),
        Stage::ALL.len()
    )
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
///
/// `cfg` is threaded in rather than loaded here: every caller already holds one, and the
/// per-stage gate policy makes the config needed on *every* transition, not just gated ones.
pub fn transition(
    cfg: &crate::resolver::Config,
    note_path: &Path,
    stage: Stage,
) -> Result<gate::GateDecision, crate::store::StoreError> {
    let _lock = crate::store::NoteLock::acquire_required(note_path)?;
    let mut note = crate::store::read(note_path)?;

    let decision = if gate::is_smart_gated(stage) {
        gate::check_entry(
            cfg,
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
    // Beside the event, not inside `record`: the event writer stays a pure append, and only a
    // transition that actually happened is reported to the task's store.
    crate::task_store::note_governing(
        cfg,
        &note.front.repo,
        &note.front.branch,
        &stage_note(stage, &note.front.repo, &note.front.branch),
    );
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
            skill: None,
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.ts));
    out
}

/// [`board`], with each row's actual workflow skill filled in from its branch note.
///
/// A separate function rather than a change to `board` because the two read different things:
/// `board` is a fold over the event log and touches no filesystem beyond it, while this reads one
/// note per row. Callers that only want stage counts should keep paying the cheaper price.
pub fn board_with_skills(cfg: &crate::resolver::Config) -> Vec<BoardEntry> {
    board()
        .into_iter()
        .map(|mut entry| {
            let ctx = crate::resolver::Resolved {
                repo: entry.repo.clone(),
                branch: entry.branch.clone(),
            };
            entry.skill = crate::store::read(&cfg.note_file(&ctx))
                .ok()
                .and_then(|note| skill_for_note(&note));
            entry
        })
        .collect()
}
