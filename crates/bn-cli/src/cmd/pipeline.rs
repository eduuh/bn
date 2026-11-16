//! `bn pipeline` — a task's place in the pipeline, two complementary ways.
//!
//! **Bare `bn pipeline` (read-only inference, #657):** infers which **phase** a task sits in —
//! Plan → Execute → Critique → Done — from existing notes, plan reviews, attention, verify/PR
//! markers, and live agents. `--all` lists the fleet; `--repo/--branch` targets another worktree.
//! It emits `phase`/`phases`, never `stage` (#994): both vocabularies contain `"execute"` and
//! `"plan"`, so the field name is the only thing telling a consumer which one it holds.
//!
//! **Subcommands (explicit state machine, PIPE-A / EPIC #746):** `set <stage>` / `advance`
//! record the worktree's stage through the observable pipeline
//! (`pickup → investigate → plan → consensus → execute → validate → critique → ship → learn`);
//! `status` shows the recorded stage + history; `board` lists every worktree's recorded stage;
//! `badge` is the compact `glyph stage` line the tmux status row renders. The pure state machine
//! lives in `bn_core::pipeline::stage`; this module is only the CLI glue + I/O.

use crate::*;

#[path = "pipeline/launch.rs"]
mod launch;
mod route_view;
use launch::{launch_plan_agent, launch_sub_investigator};
use bn_core::pipeline::investigate::{next_after_investigation, InvestigationNext, MAX_FANOUT};
use bn_core::pipeline::stage::{self, BoardEntry, Stage};
use bn_core::pipeline::status::{infer, PipelineView, PlanReview, TaskSignals};
use bn_core::plan_review::{Review, ReviewStatus};

mod status_render;
mod board;
mod plan_gate;
use status_render::{render_pipeline, render_pipeline_list, terminal_width};
mod target;
pub(crate) use route_view::emit_pipeline_route;
pub(crate) use board::{emit_pipeline_badge, emit_pipeline_board};
use board::fmt_ts;
use target::{current_stage, resolve_target};

pub(crate) fn emit_pipeline(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
    all: bool,
    json: bool,
) {
    let reviews = bn_core::plan_review::list();
    let attn = bn_core::attention::open_items(&today());
    let live = agent_live::collect(cfg, 300, 900);

    let views = if all {
        rollups::list(cfg)
            .iter()
            .map(|entry| pipeline_view(cfg, &entry.repo, &entry.branch, &reviews, &attn, &live))
            .collect::<Vec<_>>()
    } else {
        let ctx = target_ctx_or_exit(
            cfg,
            repo,
            branch,
            "bn pipeline",
            "bn pipeline: not in a worktree — pass --repo and --branch, or use --all",
        );
        vec![pipeline_view(cfg, &ctx.repo, &ctx.branch, &reviews, &attn, &live)]
    };

    if json {
        println!(
            "{}",
            Envelope::success(
                "pipeline",
                serde_json::json!({
                    "all": all,
                    "tasks": views,
                }),
            )
            .to_json()
        );
    } else if all {
        println!("{}", render_pipeline_list(&views));
    } else if let Some(view) = views.first() {
        println!("{}", render_pipeline(view, terminal_width()));
    }
}

fn pipeline_view(
    cfg: &Config,
    repo: &str,
    branch: &str,
    reviews: &[Review],
    attn: &[bn_core::attention::AttnItem],
    live: &[agent_live::LiveView],
) -> PipelineView {
    let task = bn_core::pipeline::gate::governing_task(cfg, repo, branch);
    let doc = task.as_ref().and_then(|task| bn_core::task_doc::read(cfg, task));
    let normalized_branch = bn_core::plan_review::sanitize_branch(branch);
    let matching_attn = attn.iter().filter(|item| {
        item.repo
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case(repo))
            && item.branch.as_deref().is_some_and(|b| {
                bn_core::plan_review::sanitize_branch(b).eq_ignore_ascii_case(&normalized_branch)
            })
    });
    let critic_failed = matching_attn
        .clone()
        .any(|item| item.task_type() == "critique");
    let agent_state = live
        .iter()
        .find(|row| {
            row.repo.eq_ignore_ascii_case(repo)
                && bn_core::plan_review::sanitize_branch(&row.branch)
                    .eq_ignore_ascii_case(&normalized_branch)
        })
        .map(|row| row.state.clone())
        .filter(|state| !state.is_empty());
    let critic_branch = format!("critic-{normalized_branch}");
    let critic_active = live.iter().any(|row| {
        row.repo.eq_ignore_ascii_case(repo)
            && bn_core::plan_review::sanitize_branch(&row.branch)
                .eq_ignore_ascii_case(&critic_branch)
            && !matches!(row.state.as_str(), "done" | "failed")
    });
    let open = task.as_ref().is_some_and(|task| task.is_open(&today()));
    let next_todo = task.as_ref().filter(|_| open)
        .map(|task| format!("{} {}", task.id().unwrap_or_default(), task.summary()));

    infer(&TaskSignals {
        repo: repo.to_string(),
        branch: branch.to_string(),
        goal: task.as_ref().map(|task| task.summary()),
        note_closed: task.as_ref().is_some_and(|task| task.done),
        has_done_when: doc.as_ref().is_some_and(|doc| doc.has_done_when()),
        has_todos: task.is_some(),
        open_todos: usize::from(open),
        next_todo,
        blocker: matching_attn.clone().next().map(|item| item.title.clone()),
        pr_url: task.as_ref().and_then(|task| task.meta("pr").map(str::to_string)),
        verify_passed: doc.as_ref().is_some_and(|doc| latest_verify_passed(&doc.text)),
        plan_review: latest_plan_review(reviews, repo, branch),
        critic_active,
        critic_failed,
        critic_passed: doc.as_ref().is_some_and(|doc| doc.text.lines().any(|line|
            line.trim_start_matches("- ").to_ascii_lowercase().starts_with("critique: pass"))),
        agent_state,
    })
}

fn latest_plan_review(reviews: &[Review], repo: &str, branch: &str) -> Option<PlanReview> {
    let normalized = bn_core::plan_review::sanitize_branch(branch);
    reviews
        .iter()
        .find(|review| {
            review.repo.eq_ignore_ascii_case(repo)
                && bn_core::plan_review::sanitize_branch(&review.branch)
                    .eq_ignore_ascii_case(&normalized)
        })
        .map(|review| match review.status {
            ReviewStatus::Pending => PlanReview::Pending {
                id: review.id.clone(),
            },
            ReviewStatus::Approved => PlanReview::Approved {
                id: review.id.clone(),
            },
            ReviewStatus::ChangesRequested => PlanReview::ChangesRequested {
                id: review.id.clone(),
            },
            ReviewStatus::Rejected => PlanReview::Rejected {
                id: review.id.clone(),
            },
        })
}

fn latest_verify_passed(body: &str) -> bool {
    body.lines()
        .rev()
        .find(|line| line.contains("verify ") && (line.contains("PASS") || line.contains("FAIL")))
        .is_some_and(|line| line.contains("PASS"))
}

/// `bn pipeline status` — the current stage + transition history for the target worktree.
pub(crate) fn emit_pipeline_status(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo: Option<&str>,
    branch: Option<&str>,
    as_json: bool,
) {
    let ctx = resolve_target(ctx, repo, branch);
    let history = stage::history(&ctx.repo, &ctx.branch);
    let (current, recorded) = current_stage(cfg, &ctx);

    if as_json {
        println!(
            "{}",
            Envelope::success(
                "pipeline",
                serde_json::json!({
                    "repo": ctx.repo,
                    "branch": ctx.branch,
                    "stage": current.as_str(),
                    "position": current.position(),
                    "recorded": recorded,
                    "history": history,
                }),
            )
            .to_json()
        );
        return;
    }

    println!(
        "{} {}/{} — stage {} {}/9{}",
        current.glyph(),
        ctx.repo,
        ctx.branch,
        current,
        current.position(),
        if recorded {
            ""
        } else {
            " (default — none recorded)"
        }
    );
    for t in &history {
        println!("  {} {}", fmt_ts(t.ts), t.stage);
    }
}

/// `bn pipeline set <stage>` — transition the target worktree to an explicit stage.
pub(crate) fn emit_pipeline_set(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo: Option<&str>,
    branch: Option<&str>,
    stage_str: &str,
    as_json: bool,
) {
    let ctx = resolve_target(ctx, repo, branch);
    let stage = match stage_str.parse::<Stage>() {
        Ok(s) => s,
        Err(e) => fail(&format!("bn pipeline set: {e}")),
    };
    transition(cfg, &ctx, stage, as_json);
}

/// `bn pipeline advance` — move the target worktree to the next stage in the flow.
pub(crate) fn emit_pipeline_advance(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo: Option<&str>,
    branch: Option<&str>,
    as_json: bool,
) {
    let ctx = resolve_target(ctx, repo, branch);
    let (current, _) = current_stage(cfg, &ctx);
    let Some(next) = current.next() else {
        fail(&format!(
            "bn pipeline advance: {}/{} is already at the terminal stage ({current})",
            ctx.repo, ctx.branch
        ));
    };
    transition(cfg, &ctx, next, as_json);
}

/// Record the transition and report it (JSON envelope or a one-line human message).
fn transition(cfg: &Config, ctx: &Resolved, stage: Stage, as_json: bool) {
    let decision = match stage::transition(cfg, ctx, stage) {
        Ok(decision) => decision,
        Err(e) => fail(&format!("bn pipeline: failed to transition stage: {e}")),
    };
    // The SMART gate (#998) refused: nothing was written, so reporting the stage as recorded
    // would be a plain lie. The task has been stamped and an attention item raised.
    if !decision.admits() {
        let why = decision.refusal_line().unwrap_or_default();
        fail_json(as_json, "pipeline", "smart-gate", &format!("bn pipeline: {why}"));
    }
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "pipeline",
                serde_json::json!({
                    "repo": ctx.repo,
                    "branch": ctx.branch,
                    "stage": stage.as_str(),
                    "position": stage.position(),
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "{} {}/{} → stage {} ({}/9)",
            stage.glyph(),
            ctx.repo,
            ctx.branch,
            stage,
            stage.position()
        );
    }
}

/// Record unknowns the fan-out cap would not fund, as research items on the note.
///
/// A capped unknown must not simply disappear: the plan stage reads the note's research to decide
/// what it knows, and a note that silently lost three questions reads as a complete investigation.
/// The text doubles as the dedupe key, so re-running the finish step does not stack duplicates.
fn record_deferred(cfg: &Config, ctx: &Resolved, deferred: &[String]) {
    for question in deferred {
        let text = format!("deferred unknown (fan-out capped at {MAX_FANOUT}): {question}");
        bn_core::task_doc::append_context(cfg, ctx, "Investigation", &text)
            .unwrap_or_else(|error| fail(&format!("cannot record deferred question: {error}")));
    }
}

fn raise_investigation_attention(cfg: &Config, ctx: &Resolved, reason: &str) {
    let key = format!("investigate:{}/{}", ctx.repo, ctx.branch);
    let title = format!("Planning input needed for {}/{}", ctx.repo, ctx.branch);
    let _ = bn_core::attention::add_new(
        &bn_core::attention::NewAttn {
            source: "pipeline-investigate",
            title: &title,
            severity: "urgent",
            repo: Some(&ctx.repo),
            branch: Some(&ctx.branch),
            key: Some(&key),
            detail: Some(reason),
            task_type: Some("planning"),
            ..Default::default()
        },
        &today(),
    );
    bn_core::task_doc::append_context(cfg, ctx, "Open questions", reason)
        .unwrap_or_else(|error| fail(&format!("cannot record investigation blocker: {error}")));
}

fn enter_plan(
    cfg: &Config,
    ctx: &Resolved,
    goal: &str,
    findings: &[String],
    no_tmux: bool,
    as_json: bool,
) {
    // Entering `plan` is not a gated transition, so this only fails on real I/O trouble.
    if let Err(e) = stage::transition(cfg, ctx, Stage::Plan) {
        fail(&format!(
            "bn pipeline investigate: failed to advance to plan: {e}"
        ));
    }
    let launched = !no_tmux && launch_plan_agent(cfg, ctx, goal, findings, as_json);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "pipeline_investigate",
                serde_json::json!({
                    "repo": ctx.repo,
                    "branch": ctx.branch,
                    "outcome": "plan",
                    "findings": findings.len(),
                    "plan_agent_launched": launched,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "{} {}/{} → stage plan (3/9) — {} findings, {}",
            Stage::Plan.glyph(),
            ctx.repo,
            ctx.branch,
            findings.len(),
            if launched {
                "fresh planner launched"
            } else {
                "planner not launched"
            }
        );
    }
}

/// Complete the primary investigate stage, fanning out remaining unknowns when needed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_pipeline_investigate_finish(
    cfg: &Config,
    current_ctx: Option<&Resolved>,
    repo: Option<&str>,
    branch: Option<&str>,
    unknowns: &[String],
    blocked: Option<&str>,
    no_tmux: bool,
    as_json: bool,
) {
    let ctx = resolve_target(current_ctx, repo, branch);
    let (goal, findings) = investigation_context(cfg, &ctx);

    // The cap is the pipeline's, not the investigating agent's: it lists what it wants looked at,
    // and this decides how many of those are worth a worktree each.
    match next_after_investigation(&findings, unknowns, blocked, MAX_FANOUT) {
        InvestigationNext::Plan { findings, deferred } => {
            record_deferred(cfg, &ctx, &deferred);
            enter_plan(cfg, &ctx, &goal, &findings, no_tmux, as_json);
        }
        InvestigationNext::Escalate { reason } => {
            raise_investigation_attention(cfg, &ctx, &reason);
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "pipeline_investigate",
                        serde_json::json!({
                            "repo": ctx.repo,
                            "branch": ctx.branch,
                            "outcome": "planning-attention",
                            "reason": reason,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!("investigate: paused — planning attention raised: {reason}");
            }
        }
        InvestigationNext::FanOut {
            findings,
            unknowns,
            deferred,
        } => {
            let existing = pending_fanout_tokens(&ctx);
            if !existing.is_empty() {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success(
                            "pipeline_investigate",
                            serde_json::json!({
                                "repo": ctx.repo,
                                "branch": ctx.branch,
                                "outcome": "fan-out-pending",
                                "tokens": existing,
                                "launched": 0,
                            }),
                        )
                        .to_json()
                    );
                } else {
                    println!(
                        "investigate: {} sub-investigators are already pending",
                        existing.len()
                    );
                }
                return;
            }
            record_deferred(cfg, &ctx, &deferred);
            let run = format!("f{}", bn_core::util::now_epoch_secs());
            let mut launched = 0usize;
            let mut tokens = Vec::new();
            for (index, question) in unknowns.iter().enumerate() {
                let token = format!("{run}-{}", index + 1);
                bn_core::pipeline::execution::record_fanout(&ctx.repo, &ctx.branch, &token, "pending")
                    .unwrap_or_else(|error| fail(&format!("cannot record fanout: {error}")));
                if !no_tmux && launch_sub_investigator(cfg, &ctx, &token, &goal, question, as_json)
                {
                    launched += 1;
                }
                tokens.push(token);
            }
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "pipeline_investigate",
                        serde_json::json!({
                            "repo": ctx.repo,
                            "branch": ctx.branch,
                            "outcome": "fan-out",
                            "findings": findings.len(),
                            "unknowns": unknowns,
                            "tokens": tokens,
                            "launched": launched,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "investigate: fan-out {} unknowns — {launched} fresh sub-investigators launched",
                    unknowns.len()
                );
            }
        }
    }
}

fn pending_fanout_tokens(ctx: &Resolved) -> Vec<String> {
    bn_core::pipeline::execution::fanouts(&ctx.repo, &ctx.branch)
        .into_iter().filter(|(_, status)| status == "pending").map(|(token, _)| token)
        .collect()
}

fn all_fanouts_complete(ctx: &Resolved) -> bool {
    let fanouts = bn_core::pipeline::execution::fanouts(&ctx.repo, &ctx.branch);
    !fanouts.is_empty() && fanouts.values().all(|status| status == "done")
}

fn investigation_context(cfg: &Config, ctx: &Resolved) -> (String, Vec<String>) {
    let task = bn_core::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch)
        .unwrap_or_else(|| fail("pipeline investigation requires a governing task"));
    let findings = bn_core::task_doc::read(cfg, &task)
        .and_then(|doc| doc.section("Investigation"))
        .map(|text| vec![text]).unwrap_or_default();
    (task.summary(), findings)
}

/// Merge one focused sub-investigator's findings and advance once the whole fan-out completes.
pub(crate) fn emit_pipeline_investigate_contribute(
    cfg: &Config,
    repo: &str,
    branch: &str,
    token: &str,
    findings: &[String],
    blocked: Option<&str>,
    as_json: bool,
) {
    let ctx = Resolved {
        repo: repo.to_string(),
        branch: branch.replace('/', "-"),
    };
    if token.rsplit_once('-').is_none() {
        fail("bn pipeline investigate contribute: invalid --token");
    }
    if findings.iter().all(|finding| finding.trim().is_empty())
        && blocked.is_none_or(|reason| reason.trim().is_empty())
    {
        fail("bn pipeline investigate contribute: pass --finding or --blocked");
    }

    for finding in findings {
        if finding.trim().is_empty() {
            continue;
        }
        bn_core::task_doc::append_context(cfg, &ctx, "Investigation", finding)
            .unwrap_or_else(|error| fail(&format!("cannot record finding: {error}")));
    }
    let status = if let Some(reason) = blocked.map(str::trim).filter(|reason| !reason.is_empty()) {
        raise_investigation_attention(cfg, &ctx, reason);
        "blocked"
    } else {
        "done"
    };
    bn_core::pipeline::execution::record_fanout(&ctx.repo, &ctx.branch, token, status)
        .unwrap_or_else(|error| fail(&format!("cannot record fanout: {error}")));
    let completed = status == "done" && all_fanouts_complete(&ctx);
    if completed {
        let (goal, findings) = investigation_context(cfg, &ctx);
        enter_plan(cfg, &ctx, &goal, &findings, false, as_json);
    } else if as_json {
        println!(
            "{}",
            Envelope::success(
                "pipeline_investigate_contribute",
                serde_json::json!({
                    "repo": repo,
                    "branch": branch,
                    "token": token,
                    "status": status,
                    "fanout_complete": false,
                }),
            )
            .to_json()
        );
    } else {
        println!("investigate: contribution {token} recorded ({status})");
    }
}
