//! Launching a pipeline's sub-agents: the sub-investigator fan-out and the planner handoff.
//!
//! Both put an agent in a worktree and hand it a seed, which is a different job from deciding
//! *whether* a stage should advance — that stays in `pipeline.rs`. Split out because the ratchet
//! caps that file, and this is the half that grows whenever a harness gains a flag.
use crate::*;
use bn_core::pipeline::investigate::plan_packet;
use super::plan_agent::append_plan_gate_seed;

/// Build the `--disable-mcp-server <name>` pass-through for each of a repo's project MCP servers
/// (`.mcp.json` / `.github/mcp.json`), after the launcher's own `--` boundary — so a dispatched
/// Copilot agent never wedges at boot on a project MCP that fails to connect or wants interactive
/// auth (#384). Empty when there's nothing to skip, so the launch command is unchanged for the
/// common bn-only / no-MCP repo. Restored as a local helper: `dispatch.rs`'s equivalent folded
/// into `agent_model_suffix` (#798) once dispatch always threads a pinned model, but the two
/// pipeline sub-agent launchers below have no model to pin and only ever needed the MCP suffix.
fn copilot_mcp_disable_suffix(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut suffix = String::from(" --");
    for name in names {
        suffix.push_str(" --disable-mcp-server ");
        suffix.push_str(&wt::shq(name));
    }
    suffix
}

fn pipeline_copilot_command(session_id: &str, seed_file: &std::path::Path) -> String {
    format!(
        "{} copilot --fresh --yolo --session {session_id} --seed-file {}",
        agent_launcher_bin(),
        wt::shq(&seed_file.to_string_lossy())
    )
}

/// Where the task being staged is actually checked out, for the two launchers that have to hand a
/// real cwd to a fresh agent.
///
/// `<worktree_dir>/<repo>/<branch>` is only the convention, and a *dispatched* task — the common
/// case for a pipeline stage — lives in a warm pool slot (`_pool/<n>`) that the convention can
/// never name. Reconstructing the path therefore made both handoffs silently no-op for every
/// pooled task: the launcher's `is_dir()` guard failed, and the caller reported "not launched"
/// with a success exit. Ask git first (it knows any location) and keep the conventional layout
/// only as the last resort for a repo git cannot enumerate.
pub(crate) fn stage_worktree(cfg: &Config, ctx: &Resolved) -> Option<std::path::PathBuf> {
    bn_core::lifecycle::worktree_path(cfg, &ctx.repo, &ctx.branch).or_else(|| {
        Some(cfg.worktree_dir.join(&ctx.repo).join(&ctx.branch))
            .filter(|p: &std::path::PathBuf| p.is_dir())
    })
}

pub(crate) fn launch_sub_investigator(
    cfg: &Config,
    ctx: &Resolved,
    token: &str,
    goal: &str,
    question: &str,
    quiet: bool,
) -> bool {
    let Some(parent_wt) = stage_worktree(cfg, ctx) else {
        return false;
    };
    let mut prompt_args = serde_json::Map::new();
    for (name, value) in [
        ("goal", goal),
        ("question", question),
        ("repo", ctx.repo.as_str()),
        ("branch", ctx.branch.as_str()),
        ("token", token),
    ] {
        prompt_args.insert(name.into(), serde_json::Value::String(value.to_string()));
    }
    let seed = match bn_core::skills::render_required(
        cfg,
        &parent_wt,
        "pipeline-sub-investigator",
        Some(&prompt_args),
    ) {
        Ok(seed) => seed,
        Err(error) => {
            eprintln!("bn pipeline investigate: {error}");
            return false;
        }
    };
    let Some(head) = crate::git::git_capture(&["rev-parse", "HEAD"], Some(&parent_wt), true) else {
        return false;
    };
    let bare = cfg.bare_dir().join(format!("{}.git", ctx.repo));
    let wt_path = cfg
        .worktree_dir
        .join(&ctx.repo)
        .join(format!(".agent-{}-{token}", ctx.branch));
    if wt_path.exists() {
        return false;
    }
    let bare_s = bare.to_string_lossy().into_owned();
    let wt_s = wt_path.to_string_lossy().into_owned();
    if !crate::git::git_run_quiet(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "--detach",
        &wt_s,
        &head,
    ]) {
        return false;
    }
    let sid = gen_uuid();
    let seed_file = wt::prompt_to_file("investigate-sub", &ctx.repo, token, &seed);
    let mut cmd = match seed_file {
        Some(file) => pipeline_copilot_command(&sid, &file),
        None => {
            let _ =
                crate::git::git_ok(&["--git-dir", &bare_s, "worktree", "remove", "--force", &wt_s]);
            return false;
        }
    };
    cmd.push_str(&copilot_mcp_disable_suffix(
        &bn_core::mcp::copilot_project_servers(&wt_path),
    ));
    cmd.push_str(&format!(
        "; status=$?; cd /; git --git-dir {} worktree remove --force {}; exit $status",
        wt::shq(&bare_s),
        wt::shq(&wt_s)
    ));
    let window = format!("{}-{token}", ctx.branch);
    let tmux_target = format!("{}:{window}", ctx.repo);
    let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &ctx.repo,
        branch: &ctx.branch,
        worktree: &wt_path,
        session_id: &sid,
        harness: "copilot",
        kind: bn_core::agent::AgentLeaseKind::PipelineInvestigate,
        tmux_target: &tmux_target,
        replace: false,
        in_place: false,
    }) {
        Ok(reservation) => reservation,
        Err(error) => {
            eprintln!("bn pipeline investigate: refusing sub-agent launch — {error}");
            let _ =
                crate::git::git_ok(&["--git-dir", &bare_s, "worktree", "remove", "--force", &wt_s]);
            return false;
        }
    };
    let _ = commands::add(
        cfg,
        ctx,
        "collab",
        &format!("agent-session: copilot {sid}"),
        Some(&format!("agent-session: copilot {sid}")),
        &today(),
    );
    bn_core::cost::attribute(
        &sid,
        bn_core::cost::WorkClass::Research,
        &ctx.repo,
        &ctx.branch,
    );
    let launched = if quiet {
        wt::open_worktree_window_cmd_quiet(
            &ctx.repo,
            &window,
            &wt_path,
            false,
            Some(&cmd),
            true,
            None,
        )
    } else {
        wt::open_worktree_window_cmd(&ctx.repo, &window, &wt_path, false, Some(&cmd), true, None)
    };
    if launched {
        agent_lease::mark_window(reservation.lease());
        reservation.commit();
    } else {
        let _ = reservation.rollback();
        let _ = crate::git::git_ok(&["--git-dir", &bare_s, "worktree", "remove", "--force", &wt_s]);
    }
    launched
}

pub(crate) fn launch_plan_agent(
    cfg: &Config,
    ctx: &Resolved,
    goal: &str,
    findings: &[String],
    quiet: bool,
) -> bool {
    let Some(wt_path) = stage_worktree(cfg, ctx) else {
        return false;
    };
    let sid = gen_uuid();
    let window = plan_window_name(&ctx.branch, &sid);
    let tmux_target = format!("{}:{window}", ctx.repo);
    let plan_path = bn_core::plan::init_agent(cfg, ctx, &sid)
        .unwrap_or_else(|error| fail(&format!("bn pipeline plan: cannot create plan draft: {error}")));
    let mut plan_args = serde_json::Map::new();
    plan_args.insert("topic".into(), serde_json::Value::String(goal.to_string()));
    let workflow = bn_core::skills::render_required(cfg, &wt_path, "plan", Some(&plan_args))
        .unwrap_or_else(|error| fail(&format!("bn pipeline plan: {error}")));
    let packet = plan_packet(goal, findings);
    let plan_path_s = plan_path.display().to_string();
    let mut envelope_args = serde_json::Map::new();
    envelope_args.insert(
        "workflow_body".into(),
        serde_json::Value::String(workflow),
    );
    envelope_args.insert("packet".into(), serde_json::Value::String(packet));
    envelope_args.insert(
        "plan_file".into(),
        serde_json::Value::String(plan_path_s.clone()),
    );
    let mut seed = bn_core::skills::render_required(
        cfg,
        &wt_path,
        "pipeline-plan-envelope",
        Some(&envelope_args),
    )
    .unwrap_or_else(|error| fail(&format!("bn pipeline plan: {error}")));
    if let Ok(note) = bn_core::store::read(&cfg.note_file(ctx)) {
        append_plan_gate_seed(cfg, ctx, &wt_path, &sid, &plan_path_s, &note, &mut seed);
    }
    let Some(seed_file) = wt::prompt_to_file("pipeline-plan", &ctx.repo, &ctx.branch, &seed) else {
        return false;
    };
    let mut cmd = pipeline_copilot_command(&sid, &seed_file);
    cmd.push_str(&copilot_mcp_disable_suffix(
        &bn_core::mcp::copilot_project_servers(&wt_path),
    ));
    // The investigator issues this handoff from inside the window already holding the worktree's
    // dispatch lease, so the plan launch would otherwise refuse *itself* — `investigate finish` is
    // that agent saying it is done, so its lease goes with it. `Ours` means this window is the
    // checkout's sole occupant; a live agent in any other window still refuses below.
    if matches!(
        agent_lease::worktree_holder(&wt_path),
        agent_lease::WorktreeHolder::Ours
    ) {
        agent_lease::release_own(&wt_path);
    }
    let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &ctx.repo,
        branch: &ctx.branch,
        worktree: &wt_path,
        session_id: &sid,
        harness: "copilot",
        kind: bn_core::agent::AgentLeaseKind::PipelinePlan,
        tmux_target: &tmux_target,
        replace: false,
        in_place: false,
    }) {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("bn pipeline plan: refusing agent launch — {error}");
            return false;
        }
    };
    let _ = commands::add(
        cfg,
        ctx,
        "collab",
        &format!("agent-session: copilot {sid}"),
        Some(&format!("agent-session: copilot {sid}")),
        &today(),
    );
    bn_core::cost::attribute(
        &sid,
        bn_core::cost::WorkClass::Research,
        &ctx.repo,
        &ctx.branch,
    );
    let launched = if quiet {
        wt::open_worktree_window_cmd_quiet(
            &ctx.repo,
            &window,
            &wt_path,
            false,
            Some(&cmd),
            true,
            None,
        )
    } else {
        wt::open_worktree_window_cmd(&ctx.repo, &window, &wt_path, false, Some(&cmd), true, None)
    };
    if !launched {
        if let Err(error) = reservation.rollback() {
            eprintln!("bn pipeline plan: launch failed; {error}");
        }
    } else {
        agent_lease::mark_window(reservation.lease());
        reservation.commit();
        let _ = wt::open_plan_split(&ctx.repo, &window, &plan_path);
    }
    launched
}

pub(crate) fn plan_window_name(branch: &str, session_id: &str) -> String {
    format!(
        "{branch}-plan-{}",
        session_id.get(..8).unwrap_or(session_id)
    )
}
