//! `bn critic` — the fresh-context critic stage (#649 Phase 3, per design r17 §1).
//!
//! `bn critic dispatch` cuts a **fresh worktree off the PR head** with a **plan-free note**
//! and launches a `--fresh` critic lane seeded (via [`bn_core::pipeline::critic::critic_packet`])
//! with ONLY the diff + done-when criteria + PR url — so the critic starts with zero prior
//! turns and cannot read the approved plan. `bn critic verdict` drives the loop-back: PASS →
//! shippable; FAIL → a `task_type="critique"` attention item + re-enter execution, bounded by
//! `--max-iterations` (default 3), then a `task_type="planning"` human-steer escalation.
//!
//! Everything stateful here composes existing primitives (the dispatch launcher, the attention
//! event log's key idempotency, the note store); the pure decision logic lives in bn-core.

use crate::*;
use bn_core::pipeline::critic::{
    critic_packet, critique_title, next_after_verdict, parse_iteration, CriticNext, Verdict,
};

/// The attention-item key for a branch's critique findings — one live record per lane, refreshed
/// (not stacked) on each FAIL via `add_new`'s key idempotency (r17 §1.4).
fn critique_key(repo: &str, branch: &str) -> String {
    format!("critique:{repo}/{branch}")
}

/// The 1-based FAIL round for the next critique item: the prior live critique item's iteration + 1
/// (0 when there is none), read store-free from that item's title (`critique_title`).
fn next_iteration(key: &str) -> u32 {
    let today = today();
    bn_core::attention::open_items(&today)
        .into_iter()
        .find(|i| i.key.as_deref() == Some(key))
        .map(|i| parse_iteration(&i.title))
        .unwrap_or(0)
        + 1
}

/// `bn critic dispatch` — launch a fresh critic lane over the execution branch's diff.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_critic_dispatch(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    branch: &str,
    pr: Option<&str>,
    base: &str,
    agent: Option<&str>,
    no_tmux: bool,
    json: bool,
) {
    // Resolve the target repo + bare (an explicit --repo from anywhere, else the cwd's repo).
    let (repo, bare) = match wt::resolve_bare_by_name(cfg, repo_override) {
        Ok(Some((r, b))) => (r, b),
        Ok(None) => match ctx {
            Some(c) => (
                c.repo.clone(),
                cfg.bare_dir().join(format!("{}.git", c.repo)),
            ),
            None => fail_json(
                json,
                "critic",
                "no-repo",
                "bn critic: not in a repo — pass --repo <name>",
            ),
        },
        Err(e) => fail_json(json, "critic", "unknown-repo", &format!("bn critic: {e}")),
    };
    if !bare.is_dir() {
        fail_json(
            json,
            "critic",
            "no-bare-repo",
            &format!("bn critic: no bare repo at {}", bare.display()),
        );
    }
    // Harness precedence mirrors `bn dispatch`: an explicit `--agent` pin wins; else the Critique
    // stage's configured harness (`[pipeline.models.critique].harness`); else copilot — then the
    // machine capability gate degrades an unavailable harness to copilot (surfaced, never silent).
    let model = cfg.pipeline_model(&repo, bn_core::pipeline::stage::Stage::Critique);
    let agent_kind = {
        let pinned = agent
            .map(str::trim)
            .filter(|a| !a.eq_ignore_ascii_case("auto"))
            .map(|a| {
                normalize_agent(a).unwrap_or_else(|| {
                    fail_json(
                        json,
                        "critic",
                        "bad-agent",
                        "bn critic: unknown --agent (use: copilot | claude)",
                    )
                })
            });
        let stage_harness = model
            .harness
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(|h| {
                normalize_agent(h).unwrap_or_else(|| {
                    fail_json(
                        json,
                        "critic",
                        "bad-stage-harness",
                        &format!(
                            "bn critic: unknown harness '{h}' in [pipeline.models.critique] (use: copilot | claude)"
                        ),
                    )
                })
            });
        let resolved = crate::cmd::dispatch::effective_launch_harness(cfg, pinned, stage_harness);
        if let Some((from, reason)) = &resolved.fallback_from {
            if !json {
                eprintln!(
                    "bn critic: {from} unavailable ({reason}); falling back to {}",
                    resolved.harness
                );
            }
        }
        resolved.harness
    };

    // The execution lane under test — its note holds the done-when criteria (never the plan).
    let exec_sanitized = branch.replace('/', "-");
    let exec_ctx = Resolved {
        repo: repo.clone(),
        branch: exec_sanitized.clone(),
    };
    let done_when: Vec<String> = bn_core::store::read(&cfg.note_file(&exec_ctx))
        .map(|n| {
            n.front
                .done_when
                .iter()
                .map(|i| i.text.clone())
                .filter(|t| !t.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Cut the critic worktree OFF THE EXECUTION HEAD (r17 §1.3 option b): a NEW branch starting at
    // the execution branch's tip, so the critic sees the exact diff under test but on its own lane.
    let critic_branch = format!("critic/{exec_sanitized}");
    let critic_sanitized = critic_branch.replace('/', "-");
    let wt_path = cfg.worktree_dir.join(&repo).join(&critic_sanitized);
    if let Err(e) = create_worktree(&bare, &wt_path, &critic_branch, branch) {
        fail_json(
            json,
            "critic",
            "worktree-failed",
            &format!("bn critic: critic worktree creation failed: {e}"),
        );
    }

    // The diff under test: base...head, computed by the driver (not the agent). Three-dot so it's
    // the change the branch introduced over the base, matching what a PR shows.
    let range = format!("{base}...HEAD");
    let diff =
        crate::git::git_capture(&["diff", &range], Some(&wt_path), false).unwrap_or_default();

    // Seed a PLAN-FREE note: the Goal is only "critique this diff" — the `## Plan` section is never
    // copied here, so the critic literally cannot `bn --cat` the approved plan (the cleanest
    // isolation, r17 §1.3).
    let critic_ctx = Resolved {
        repo: repo.clone(),
        branch: critic_sanitized.clone(),
    };
    let _ = commands::ensure_note(cfg, &critic_ctx, &today());
    let goal = format!(
        "Critique the diff on {branch} against its done-when criteria (fresh context — no plan, no \
         author reasoning). Report the verdict with `bn critic verdict`."
    );
    let _ = commands::set_goal(cfg, &critic_ctx, &goal, &today());
    let _ = commands::add(
        cfg,
        &critic_ctx,
        "decision",
        "task: critique — independently review the diff, run tests, verify done-when; ship no code",
        Some("task:"),
        &today(),
    );

    // Launch the FRESH critic lane: brand-new harness session (zero prior turns), seeded with ONLY
    // the diff + done-when + pr via the leak-proof packet builder. No plan, execution transcript,
    // or execution session id is forwarded.
    let mut workflow_args = serde_json::Map::new();
    workflow_args.insert(
        "pr".into(),
        serde_json::Value::String(pr.unwrap_or("").to_string()),
    );
    let workflow =
        bn_core::skills::render_required(cfg, &wt_path, "critique-fresh", Some(&workflow_args))
            .unwrap_or_else(|error| fail(&format!("bn critic: {error}")));
    let packet = critic_packet(&diff, &done_when, pr);
    let verdict_commands = format!(
        "PASS: `bn critic verdict --repo {repo} --branch {branch} --pass`\n\
         FAIL: `bn critic verdict --repo {repo} --branch {branch} --fail --findings-file <path>`"
    );
    let mut envelope_args = serde_json::Map::new();
    envelope_args.insert(
        "workflow_body".into(),
        serde_json::Value::String(workflow),
    );
    envelope_args.insert("packet".into(), serde_json::Value::String(packet));
    envelope_args.insert(
        "verdict_commands".into(),
        serde_json::Value::String(verdict_commands),
    );
    let seed = bn_core::skills::render_required(
        cfg,
        &wt_path,
        "pipeline-critic-envelope",
        Some(&envelope_args),
    )
    .unwrap_or_else(|error| fail(&format!("bn critic: {error}")));
    let mut agent_launched = false;
    if !no_tmux {
        let _ = apply_folder_trust(&wt_path.to_string_lossy(), agent_kind);
        let sid = gen_uuid();
        let _ = commands::add(
            cfg,
            &critic_ctx,
            "collab",
            &format!("agent-session: {agent_kind} {sid}"),
            None,
            &today(),
        );
        bn_core::cost::attribute(
            &sid,
            bn_core::cost::WorkClass::Research,
            &repo,
            &critic_branch,
        );
        let mut cmd = match wt::prompt_to_file("critic-seed", &repo, &critic_sanitized, &seed) {
            Some(f) => format!(
                "{} {agent_kind} --yolo --fresh --session {sid} --seed-file {}",
                agent_launcher_bin(),
                wt::shq(&f.to_string_lossy())
            ),
            None => format!(
                "{} {agent_kind} --yolo --fresh --session {sid} --seed {}",
                agent_launcher_bin(),
                wt::shq(&seed)
            ),
        };
        // Suppress a repo's project MCP servers for a dispatched Copilot so a failing/auth-wanting
        // one can't wedge the critic at boot (#384); bn's own server is always kept.
        let suppressed = if agent_kind == "copilot" {
            bn_core::mcp::copilot_project_servers(&wt_path)
        } else {
            Vec::new()
        };
        // `model` (Critique stage) was resolved up front alongside the harness — reuse it.
        cmd.push_str(&agent_model_suffix(agent_kind, &model, &suppressed));
        agent_launched = wt::open_worktree_window_cmd(
            &repo,
            &critic_sanitized,
            &wt_path,
            false,
            Some(&cmd),
            true,
            None,
        );
    }

    if json {
        let env = Envelope::success(
            "critic_dispatch",
            serde_json::json!({
                "repo": repo,
                "branch": branch,
                "critic_branch": critic_branch,
                "pr": pr,
                "done_when": done_when.len(),
                "agent_launched": agent_launched,
            }),
        );
        println!("{}", env.to_json());
    } else {
        println!(
            "✓ critic lane on {critic_branch} (fresh, off {branch}) — {} done-when criteria, {}",
            done_when.len(),
            if agent_launched {
                "agent launched"
            } else {
                "no agent (--no-tmux)"
            }
        );
    }
}

/// `bn critic verdict` — record the critic's ruling and drive the loop-back (r17 §1.4).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_critic_verdict(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    branch_override: Option<&str>,
    pass: bool,
    findings: Option<&str>,
    max_iterations: u32,
    json: bool,
) {
    // Resolve the EXECUTION lane the verdict is about. From inside the critic worktree the branch
    // is `critic/<exec>`, so strip that prefix back to the execution branch; an explicit
    // --repo/--branch wins.
    let resolved = target_ctx(
        cfg,
        repo_override.map(str::to_string),
        branch_override.map(str::to_string),
    );
    let (repo, branch) = match (repo_override, branch_override, resolved.as_ref(), ctx) {
        (Some(r), Some(b), _, _) => (r.to_string(), b.to_string()),
        _ => {
            let base = resolved.as_ref().or(ctx);
            match base {
                Some(c) => {
                    let b = c
                        .branch
                        .strip_prefix("critic-")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| c.branch.clone());
                    (c.repo.clone(), b)
                }
                None => fail_json(
                    json,
                    "critic",
                    "no-target",
                    "bn critic verdict: not in a repo — pass --repo/--branch",
                ),
            }
        }
    };
    let exec_ctx = Resolved {
        repo: repo.clone(),
        branch: branch.replace('/', "-"),
    };

    let verdict = if pass { Verdict::Pass } else { Verdict::Fail };
    let key = critique_key(&repo, &branch);
    // For a FAIL, this is the round we're now on (prior + 1); for a PASS it's unused.
    let iteration = if pass { 0 } else { next_iteration(&key) };
    let next = next_after_verdict(verdict, iteration, max_iterations);

    match next {
        CriticNext::Shippable => {
            let _ = commands::add(
                cfg,
                &exec_ctx,
                "decision",
                "critique: PASS — fresh-context critic verified every done-when criterion",
                Some("critique:"),
                &today(),
            );
            // Close any open critique item — the loop is resolved.
            if let Some(item) = bn_core::attention::open_items(&today())
                .into_iter()
                .find(|i| i.key.as_deref() == Some(key.as_str()))
            {
                let _ = bn_core::attention::ack(&item.id);
            }
            emit_verdict(
                json,
                "shippable",
                &repo,
                &branch,
                iteration,
                max_iterations,
                None,
            );
        }
        CriticNext::ReExecute { iteration } => {
            let detail = findings.unwrap_or("(no findings text provided)");
            let title = critique_title(iteration, max_iterations);
            let _ = bn_core::attention::add_new(
                &bn_core::attention::NewAttn {
                    source: "critic",
                    title: &title,
                    severity: "high",
                    repo: Some(&repo),
                    branch: Some(&branch),
                    key: Some(&key),
                    detail: Some(detail),
                    task_type: Some("critique"),
                    ..Default::default()
                },
                &today(),
            );
            // Durable copy in the execution note so the re-dispatched implementer sees it on `--cat`.
            let _ = commands::add(
                cfg,
                &exec_ctx,
                "blocker",
                &format!("critic FAIL (iteration {iteration}/{max_iterations}): {detail}"),
                None,
                &today(),
            );
            // Re-enter execution: re-dispatch the implementer lane FRESH (#383 path), seeded with
            // the findings so it fixes them rather than resuming an empty prompt.
            let raw = format!(
                "Critic FAILED (iteration {iteration}/{max_iterations}). Address these findings, \
                 then re-run `bn verify`:\n{detail}"
            );
            run_dispatch(
                cfg,
                None,
                Some(&repo),
                Some("main"),
                &raw,
                None,
                Some(branch.clone()),
                false,
                Some("copilot"),
                false,
                false,
                None,
                true,
                true,
                false,
                true,
                false,
                false,
                // A critic re-dispatch is a retry of work already in flight, so a full pool means
                // "wait for the slot", not "cut another worktree".
                crate::cli::OnPoolFull::Queue,
                None,
                false,
                &[],
                json,
            );
            emit_verdict(
                json,
                "re-execute",
                &repo,
                &branch,
                iteration,
                max_iterations,
                Some(detail),
            );
        }
        CriticNext::Escalate => {
            let detail = findings.unwrap_or("(no findings text provided)");
            let title = format!(
                "Human steer needed: critic + implementer disagree on {repo}/{branch} after {max_iterations} rounds"
            );
            let esc_key = format!("critique-escalate:{repo}/{branch}");
            let _ = bn_core::attention::add_new(
                &bn_core::attention::NewAttn {
                    source: "critic",
                    title: &title,
                    severity: "high",
                    repo: Some(&repo),
                    branch: Some(&branch),
                    key: Some(&esc_key),
                    detail: Some(detail),
                    task_type: Some("planning"),
                    ..Default::default()
                },
                &today(),
            );
            emit_verdict(
                json,
                "escalate",
                &repo,
                &branch,
                iteration,
                max_iterations,
                Some(detail),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_verdict(
    json: bool,
    stage: &str,
    repo: &str,
    branch: &str,
    iteration: u32,
    max_iterations: u32,
    detail: Option<&str>,
) {
    if json {
        let env = Envelope::success(
            "critic_verdict",
            serde_json::json!({
                "stage": stage,
                "repo": repo,
                "branch": branch,
                "iteration": iteration,
                "max_iterations": max_iterations,
            }),
        );
        println!("{}", env.to_json());
        return;
    }
    match stage {
        "shippable" => println!("✓ critique PASS → {repo}/{branch} advanced to shippable"),
        "re-execute" => println!(
            "✗ critique FAIL (iteration {iteration}/{max_iterations}) → {repo}/{branch} re-entering execution\n  findings: {}",
            detail.unwrap_or("")
        ),
        "escalate" => println!(
            "⚑ critique budget exhausted ({max_iterations} rounds) → raised a planning human-steer item for {repo}/{branch}"
        ),
        _ => {}
    }
}
