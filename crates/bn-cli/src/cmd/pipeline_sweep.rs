//! `bn pipeline sweep` — the IO half of [`bn_core::pipeline::sweep`].
//!
//! Reads each task's stage entries out of the backlog, asks the pure decision what is due, and
//! enqueues it. The interesting rules live in bn-core and are unit-tested there; this resolves the
//! per-stage profile, pins the worktree, and reports.
//!
//! # Every stage shares one worktree
//!
//! Stages are separate *entries*, never separate checkouts. The findings the investigate stage
//! writes, the plan artifact the plan stage produces and the branch note they both live in **are**
//! the hand-off between stages, so a fresh worktree per stage would throw all three away and each
//! stage would start from nothing. Every entry therefore pins the task's `repo:branch`, and the
//! first worktree-bearing stage is what creates that branch — `bn dispatch --task` binds it back onto
//! the task, so the stages that follow find it.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind};
use bn_core::pipeline::stage::Stage;
use bn_core::pipeline::sweep::{self, Advance, StageState};
use bn_core::queue::enqueue::enqueue;
use bn_core::queue::kind::{ExecSpec, Requirements};

/// What one task's sweep did, for the report and the `--json` envelope.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Swept {
    pub(crate) task: String,
    pub(crate) repo: String,
    /// The kebab-case outcome: `queued` / `in-flight` / `held` / `blocked` / `complete` / `skipped`.
    pub(crate) outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) entry: Option<String>,
    /// Why, when the outcome needs one — a held stage or a skipped task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// The stages configured to wait for a human, for this repo.
///
/// Resolved through the same [`profile`](bn_core::pipeline::profile) layering every launcher uses, so
/// `[pipeline.stages.plan] gate = "hold"` means here what it means there.
fn held_stages(cfg: &Config, repo: &str, branch: &str) -> Vec<Stage> {
    sweep::SWEPT
        .iter()
        .copied()
        .filter(|&stage| {
            bn_core::pipeline::profile::resolve(
                cfg,
                bn_core::resolver::RepoBranch { repo, branch },
                stage,
                &Default::default(),
            )
            .is_held()
        })
        .collect()
}

/// Whether a plan review for this worktree has been approved.
///
/// The gate's release condition. `plan_review` is already the record the agent writes and the human
/// decides on, so the sweep reads it rather than introducing a second notion of approval that could
/// disagree with the one on the Reviews page.
fn plan_approved(repo: &str, branch: &str) -> bool {
    bn_core::plan_review::list()
        .iter()
        .filter(|r| r.repo == repo && r.branch == branch)
        .any(|r| r.status == bn_core::plan_review::ReviewStatus::Approved)
}

/// Read one task's swept-stage states out of the backlog.
fn stage_states(bl: &backlog::Backlog, task_id: &str) -> Vec<StageState> {
    sweep::SWEPT
        .iter()
        .map(|&stage| StageState {
            stage,
            status: bl
                .for_source(&sweep::stage_source(task_id, stage))
                .map(|e| e.status),
        })
        .collect()
}

/// Enqueue one stage for a task. Returns the entry id.
fn enqueue_stage(
    cfg: &Config,
    task: &bn_core::todo::Task,
    task_id: &str,
    repo: &str,
    branch: Option<&str>,
    stage: Stage,
    now: i64,
) -> Option<String> {
    let profile = bn_core::pipeline::profile::resolve(
        cfg,
        bn_core::resolver::RepoBranch {
            repo,
            branch: branch.unwrap_or_default(),
        },
        stage,
        &Default::default(),
    );
    // Config wins, then the stage's shipped workflow. A stage with neither would run a bare agent
    // with no workflow at all, which is how a "plan" stage ends up writing code.
    let skill = profile
        .skill
        .clone()
        .or_else(|| sweep::default_skill(stage).map(str::to_string));
    let spec = AddSpec {
        kind: kind_for(stage),
        priority: PRIORITY,
        source: sweep::stage_source(task_id, stage),
        task: task_id.to_string(),
        exec: match &skill {
            Some(s) => ExecSpec::Skill {
                skill: s.clone(),
                arg: None,
            },
            None => ExecSpec::Agent,
        },
        requires: Requirements {
            harness: profile.harness.iter().cloned().collect(),
            ..Default::default()
        },
        agent: bn_core::backlog::AgentSpec {
            // Pinned so every stage lands in the one worktree — see the module docs.
            branch: branch.map(str::to_string),
            name: Some(format!("{task_id}-{}", stage.as_str())),
            yolo: true,
            no_plan_review: false,
            // Declared, so the launch resolves *this* stage's model and effort rather than the one
            // dispatch would infer from the skill.
            with_main: false,
            stage: Some(stage),
        },
        ..Default::default()
    };
    let subject = format!("{}: {}", stage.as_str(), task.summary());
    match enqueue(&backlog::backlog_path(), repo, &subject, &spec, now) {
        Ok(e) => Some(e.entry().id.clone()),
        Err(err) => {
            eprintln!("bn pipeline sweep: could not queue {task_id} {stage:?} — {err}");
            None
        }
    }
}

/// Where pipeline work sits: above the cron floor, below a human's typed `bn dispatch` (100).
/// A task already admitted to the pipeline is agreed work, not an interrupt.
const PRIORITY: i64 = 70;

/// The backlog kind for a stage — the taxonomy, which drives the lane and the timeline.
fn kind_for(stage: Stage) -> BacklogKind {
    match stage {
        // Read-only, task-bound research that writes evidence into the note.
        Stage::Investigate => BacklogKind::Investigate,
        // A plan is not a merge either, but it is not exploration — it produces the artifact the
        // review decides on, so it rides the same lane as the investigation it consumes.
        Stage::Plan => BacklogKind::Investigate,
        _ => BacklogKind::Ship,
    }
}

/// `bn pipeline sweep` — advance every task that is in the pipeline.
///
/// Idempotent and cheap to repeat: it enqueues only what the backlog says is missing, so running it
/// every tick costs two file reads when there is nothing to do.
pub(crate) fn emit_pipeline_sweep(cfg: &Config, as_json: bool) {
    let swept = sweep_tasks(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "pipeline-sweep",
                serde_json::json!({ "count": swept.len(), "tasks": swept })
            )
            .to_json()
        );
        return;
    }
    report(&swept);
}

/// The sweep itself, silent — what the tick calls. Returns what it did so a caller can render or
/// count it. Printing from here would put a JSON envelope (or a task list) into the tick's log on
/// every pass, which is how a useful log becomes one nobody reads.
pub(crate) fn sweep_tasks(cfg: &Config) -> Vec<Swept> {
    let now = bn_core::util::now_epoch_secs();
    let today = today();
    let path = crate::cmd::task::task_path(cfg);
    let Ok(file) = bn_core::todo::load(&path) else {
        return Vec::new();
    };
    let bl = backlog::load(&backlog::backlog_path());
    let mut swept: Vec<Swept> = Vec::new();

    for task in file.tasks().filter(|t| t.is_open(&today)) {
        let Some(task_id) = task.id() else { continue };
        let Some(repo) = task.meta("repo").map(str::trim).filter(|r| !r.is_empty()) else {
            continue; // Ungrounded: the enhance pass owns it until it has a repo.
        };
        // Only tasks the gate has admitted enter the pipeline. An unjudged or failing task would
        // otherwise have three agents spend a worktree each on work nobody has scoped.
        if task.meta("smart") != Some("ok") {
            continue;
        }
        let branch = task.meta("branch").map(str::trim).filter(|b| !b.is_empty());
        let states = stage_states(&bl, task_id);
        let held = held_stages(cfg, repo, branch.unwrap_or_default());
        let approved = branch.is_some_and(|b| plan_approved(repo, b));

        let row = match sweep::decide(&states, &held, approved) {
            Advance::InFlight(stage) => Swept {
                task: task_id.to_string(),
                repo: repo.to_string(),
                outcome: "in-flight",
                stage: Some(stage.as_str().to_string()),
                entry: None,
                detail: None,
            },
            Advance::Blocked(stage) => Swept {
                task: task_id.to_string(),
                repo: repo.to_string(),
                outcome: "blocked",
                stage: Some(stage.as_str().to_string()),
                entry: None,
                detail: Some(format!("the {} stage failed — needs you", stage.as_str())),
            },
            Advance::HeldForReview(stage) => Swept {
                task: task_id.to_string(),
                repo: repo.to_string(),
                outcome: "held",
                stage: Some(stage.as_str().to_string()),
                entry: None,
                detail: Some(format!(
                    "{} waits on your plan review — `bn review` to decide",
                    stage.as_str()
                )),
            },
            Advance::Complete => Swept {
                task: task_id.to_string(),
                repo: repo.to_string(),
                outcome: "complete",
                stage: None,
                entry: None,
                detail: None,
            },
            Advance::Next(stage) => {
                // A worktree-bearing stage after the first needs the branch its predecessor cut. No
                // branch here means the binding never landed, and cutting a second worktree would
                // silently strand the findings and the plan the next stage is supposed to read.
                if branch.is_none() && stage != sweep::SWEPT[0] {
                    Swept {
                        task: task_id.to_string(),
                        repo: repo.to_string(),
                        outcome: "skipped",
                        stage: Some(stage.as_str().to_string()),
                        entry: None,
                        detail: Some(
                            "no branch bound to the task — `bn task claim` it to the worktree its \
                             earlier stage used, so this stage lands there too"
                                .to_string(),
                        ),
                    }
                } else {
                    match enqueue_stage(cfg, task, task_id, repo, branch, stage, now) {
                        Some(entry) => Swept {
                            task: task_id.to_string(),
                            repo: repo.to_string(),
                            outcome: "queued",
                            stage: Some(stage.as_str().to_string()),
                            entry: Some(entry),
                            detail: None,
                        },
                        None => continue,
                    }
                }
            }
        };
        swept.push(row);
    }

    swept
}

/// The human report. Silent on a quiet sweep beyond one line, loud about anything waiting on a
/// person — a hold nobody can see is the failure mode this whole gate has to avoid.
fn report(swept: &[Swept]) {
    if swept.is_empty() {
        println!("No tasks in the pipeline — a task needs `repo:` and `smart:ok` to enter it.");
        return;
    }
    for s in swept {
        let stage = s.stage.as_deref().unwrap_or("-");
        match s.outcome {
            "queued" => println!(
                "  queued {}/{} · {} · {stage}",
                s.repo,
                s.entry.as_deref().unwrap_or("?"),
                s.task
            ),
            "in-flight" => println!("  {} · {stage} running", s.task),
            "complete" => println!("  {} · pipeline complete", s.task),
            _ => println!(
                "  {} · {stage} — {}",
                s.task,
                s.detail.as_deref().unwrap_or(s.outcome)
            ),
        }
    }
    let held = swept.iter().filter(|s| s.outcome == "held").count();
    if held > 0 {
        println!(
            "{held} task(s) held for your review — nothing else will run for them until you decide."
        );
    }
}
