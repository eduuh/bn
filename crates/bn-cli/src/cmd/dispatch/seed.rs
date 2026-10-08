//! Seed construction for a dispatched agent — turning a workflow skill plus the dispatch's own
//! context (fresh worktree, plan-approval gate, scout framing, verify gate) into the single prompt
//! the harness starts on.
//!
//! Split out of `dispatch.rs` under the file-size ratchet, which asks for a split rather than
//! growth. The seam is a real one: everything here is *text assembly* from Markdown prompts, with
//! no git, tmux, lease or process work — which is also why it is the half `bn agent run` can reuse
//! to seed a worktree with something other than a skill.
//!
//! `use super::*` is deliberate: these functions were written against `dispatch.rs`'s private
//! helpers and constants, and a child module can see an ancestor's private items, so the move
//! needed no widening of visibility beyond the entry points the parent calls back into.

use super::*;

fn render_workflow_body(skill: &bn_core::skills::Skill, title: &str) -> Result<String, String> {
    let mut args = serde_json::Map::new();
    for argument in &skill.arguments {
        let value = match argument.name.as_str() {
            "question" | "symptom" | "topic" | "task" => title,
            "continuous" => "true",
            _ => continue,
        };
        args.insert(
            argument.name.clone(),
            serde_json::Value::String(value.to_string()),
        );
    }
    skill
        .render_checked(Some(&args))
        .map_err(|error| format!("render workflow '{}': {error}", skill.name))
}

/// The concrete plan-review-gate protocol (#349) appended to a `bn dispatch --plan` ship seed. It
/// is the authoritative final instruction: the agent writes a plan, registers it with `bn review
/// submit` (raising a pending plan-review for `repo`/`branch` under session `sid`), and PAUSES —
/// polling `bn review status <id>` — implementing and opening a PR after low-risk agent consensus,
/// or after explicit human approval for high-risk/no-consensus plans. `verify` folds the `bn
/// verify` gate into the post-approval implement step.
pub(crate) fn plan_gate_seed(
    cfg: &Config,
    base_dir: &std::path::Path,
    ctx: PlanGateContext<'_>,
) -> Result<String, String> {
    let verification_requirement = if ctx.verify {
        bn_core::skills::render_required(cfg, base_dir, "dispatch-verify-gate", None)
            .map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    let mut args = serde_json::Map::new();
    for (name, value) in [
        ("repo", ctx.repo.to_string()),
        ("branch", ctx.branch.to_string()),
        ("session_id", ctx.sid.to_string()),
        ("reviewers", ctx.reviewers.to_string()),
        ("quorum", ctx.quorum.to_string()),
        ("plan_file", ctx.plan_file.unwrap_or("plan.md").to_string()),
        ("verification_requirement", verification_requirement),
    ] {
        args.insert(name.into(), serde_json::Value::String(value));
    }
    bn_core::skills::render_required(cfg, base_dir, "dispatch-plan-review-gate", Some(&args))
        .map(|rendered| format!("\n\n{rendered}"))
        .map_err(|error| error.to_string())
}

pub(crate) struct PlanGateContext<'a> {
    pub(crate) repo: &'a str,
    pub(crate) branch: &'a str,
    pub(crate) sid: &'a str,
    pub(crate) verify: bool,
    pub(crate) reviewers: usize,
    pub(crate) quorum: usize,
    pub(crate) plan_file: Option<&'a str>,
}

pub(super) fn append_plan_gate(
    cfg: &Config,
    base_dir: &std::path::Path,
    seed: String,
    plan_gate: bool,
    entering_investigate: bool,
    ctx: PlanGateContext<'_>,
) -> Result<String, String> {
    if plan_gate && !entering_investigate {
        Ok(format!("{seed}{}", plan_gate_seed(cfg, base_dir, ctx)?))
    } else {
        Ok(seed)
    }
}

/// The workflow skill a dispatch seeds its agent with (#321): an explicit `--skill` wins, else
/// the task type picks — a scout runs `investigate` (read-only findings), a ship runs `ship-solo`
/// (scope, build, and review the change end to end in one session via bounded subagents). `debug`
/// is the explicit pick for a repro-first bugfix.
///
/// The default ship arm is the ordinary `bn dispatch` entry: with the multi-session pipeline off
/// (the default; opt in with `$BN_DISPATCH_PIPELINE=1`), a skill-less non-scout agent dispatch is
/// *not* `pipeline_investigate`, so it lands here and runs `ship-solo`. With the pipeline flag on, that
/// same dispatch routes to the Investigate stage instead and this default only applies to explicit
/// `--skill` lanes (e.g. the critic's execute re-dispatch).
///
/// Deliberately **not** the same mapping as [`bn_core::pipeline::stage::Stage::default_skill`],
/// which answers a different question. This one is "what does an ad-hoc `bn dispatch` run?" — a
/// ship gets `ship-solo`, a self-contained investigate→implement→review lane. That one is "what
/// does the nine-stage pipeline run at this stage?" — `execute` picks up an *already approved*
/// plan. Both are correct; folding them together would silently change what dispatch launches.
pub(crate) fn dispatch_skill_name(explicit: Option<&str>, scout: bool) -> &str {
    explicit.unwrap_or(if scout { "investigate" } else { "ship-solo" })
}

/// The seed for a skill-driven dispatch (#321): wraps the skill's rendered workflow body with
/// dispatch context (fresh worktree, plan-approval gate unless --yolo, scout read-only framing),
/// maps the workflow's MCP tool names onto the CLI for harnesses without the bn MCP server, and
/// closes with outcome ingestion, completion reporting, and the `bn reflect "skill:<name> — …"`
/// line whose text the next run of that skill sees under "Recent lessons". Self-contained: the caller appends nothing.
pub(super) struct SkillSeedContext<'a> {
    pub(super) title: &'a str,
    pub(super) url: Option<&'a str>,
    pub(super) scout: bool,
    pub(super) yolo: bool,
    pub(super) verify: bool,
    /// A plan-review gate will be appended to this seed. The gate then owns the plan file and
    /// the completion protocol, so the envelope suppresses its bootstrap/verification/completion
    /// blocks instead of issuing a second, contradicting copy.
    pub(super) plan_gate: bool,
}

pub(super) fn skill_seed_prompt(
    cfg: &Config,
    base_dir: &std::path::Path,
    ctx: SkillSeedContext<'_>,
    skill: &bn_core::skills::Skill,
) -> Result<String, String> {
    let lessons = bn_core::learnings::recent_lessons(cfg, &bn_core::util::today());
    let body = render_workflow_body(skill, ctx.title)?
        + &bn_core::learnings::lessons_section(&lessons, &skill.name);
    let name = &skill.name;
    let cli_map = bn_core::skills::render_required(cfg, base_dir, "dispatch-cli-map", None)
        .map_err(|error| error.to_string())?;
    let bootstrap = if !ctx.scout && name == "plan-work" && !ctx.plan_gate {
        bn_core::skills::render_required(cfg, base_dir, "dispatch-plan-work-bootstrap", None)
            .map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    // The verify gate renders at most once per seed: the plan-review gate carries it when one is
    // appended, and `plan-work`/`execute`/`ship-solo` already gate their finish step on `bn verify`
    // (rebase re-run included), so only other workflows need the explicit clause.
    let verification = if ctx.verify
        && !ctx.scout
        && !ctx.plan_gate
        && name != "plan-work"
        && name != "execute"
        && name != "ship-solo"
    {
        bn_core::skills::render_required(cfg, base_dir, "dispatch-verify-gate", None)
            .map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    let mut args = serde_json::Map::new();
    for (key, value) in [
        ("goal", ctx.title.to_string()),
        (
            "source",
            ctx.url.unwrap_or("(none; use the branch note)").to_string(),
        ),
        ("workflow_name", name.to_string()),
        ("workflow_body", body),
        ("cli_map", cli_map),
    ] {
        args.insert(key.into(), serde_json::Value::String(value));
    }
    if ctx.scout {
        return bn_core::skills::render_required(cfg, base_dir, "dispatch-scout", Some(&args))
            .map_err(|error| error.to_string());
    }
    let autonomy_name = if name == "ship-solo" {
        // Neither stance fits ship-solo: interactive forces a plan proposal it has no stage for,
        // and unattended forbids the `ask_user` that is its whole human-gate. Its own stance is
        // autonomous-but-ask-on-ambiguity.
        "dispatch-autonomy-solo"
    } else if ctx.yolo {
        "dispatch-autonomy-unattended"
    } else {
        "dispatch-autonomy-interactive"
    };
    let autonomy = bn_core::skills::render_required(cfg, base_dir, autonomy_name, None)
        .map_err(|error| error.to_string())?;
    // With a plan gate appended, the gate's approved-branch is the completion protocol; an empty
    // slot here keeps the seed to a single copy.
    let completion = if ctx.plan_gate {
        String::new()
    } else {
        let mut completion_args = serde_json::Map::new();
        completion_args.insert(
            "workflow_name".into(),
            serde_json::Value::String(name.to_string()),
        );
        bn_core::skills::render_required(
            cfg,
            base_dir,
            "dispatch-completion",
            Some(&completion_args),
        )
        .map_err(|error| error.to_string())?
    };
    args.insert("autonomy".into(), serde_json::Value::String(autonomy));
    args.insert("bootstrap".into(), serde_json::Value::String(bootstrap));
    args.insert(
        "verification".into(),
        serde_json::Value::String(verification),
    );
    args.insert("completion".into(), serde_json::Value::String(completion));
    bn_core::skills::render_required(cfg, base_dir, "dispatch-ship", Some(&args))
        .map_err(|error| error.to_string())
}

pub(super) fn pipeline_investigate_seed(
    cfg: &Config,
    base_dir: &std::path::Path,
    title: &str,
    url: Option<&str>,
    skill: &bn_core::skills::Skill,
) -> Result<String, String> {
    let body = render_workflow_body(skill, title)?;
    let cli_map = bn_core::skills::render_required(cfg, base_dir, "dispatch-cli-map", None)
        .map_err(|error| error.to_string())?;
    let mut args = serde_json::Map::new();
    args.insert("goal".into(), serde_json::Value::String(title.to_string()));
    args.insert(
        "source".into(),
        serde_json::Value::String(url.unwrap_or("(none; use the branch note)").to_string()),
    );
    args.insert("workflow_body".into(), serde_json::Value::String(body));
    args.insert("cli_map".into(), serde_json::Value::String(cli_map));
    bn_core::skills::render_required(cfg, base_dir, "pipeline-investigate-envelope", Some(&args))
        .map_err(|error| error.to_string())
}
