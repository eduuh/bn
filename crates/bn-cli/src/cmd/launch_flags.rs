//! The flags a dispatch hands through to the agent harness.
//!
//! Two guards live here, and both exist because the same failure happened twice: a value bn had
//! configured was one the harness would not accept, it reached the command line anyway, and the agent
//! died at startup on "model does not exist" — a failure that looks like a bn bug from every angle
//! except the command line nobody printed.
//!
//! So neither model nor effort is ever passed on trust. Each goes through the harness's own predicate
//! in [`bn_core::mcp::Harness`], and a value it rejects is **omitted** rather than sent: the agent
//! then boots on the harness's default, which is worse than the intent and infinitely better than
//! not booting. The predicates are shared with `bn config set-stage` and the Planning UI, so what the
//! launcher will accept and what the operator is allowed to configure cannot drift apart.
use crate::*;

/// Build the harness pass-through for a resolved pipeline model. The single leading `--` ends the
/// launcher's own options; model/effort flags and any Copilot MCP suppressions then reach the
/// harness directly. Every value is shell-quoted for the tmux command line.
pub(crate) fn agent_model_suffix(
    agent_kind: &str,
    model: &bn_core::resolver::PipelineModel,
    disabled_mcps: &[String],
) -> String {
    let mut suffix = String::from(" --");
    // Only pin `--model` when the configured id is one this harness actually accepts; otherwise
    // omit it and let the harness fall back to its own default. bn stores dotted catalog ids
    // (e.g. claude-opus-4.8) that Claude Code rejects, which parked every dispatched claude agent
    // on "model does not exist" (bn#963). Copilot accepts the dotted ids, so it is unaffected.
    if let Some(model_id) =
        bn_core::mcp::Harness::parse(agent_kind).and_then(|harness| harness.model_arg(&model.model))
    {
        suffix.push_str(" --model ");
        suffix.push_str(&shq(model_id));
    }
    // Same guard for the effort level, and for the same reason: the two harnesses accept different
    // sets (Copilot alone has `none`/`minimal`), so a level pinned for one stage could be one the
    // other rejects — and an unguarded value went straight onto the command line and killed the
    // launch. Omitting it leaves the harness on its own default, which always boots.
    if let Some(effort) = bn_core::mcp::Harness::parse(agent_kind)
        .and_then(|harness| harness.effort_arg(&model.effort))
    {
        suffix.push(' ');
        suffix.push_str(if agent_kind == "claude" {
            "--effort"
        } else {
            "--reasoning-effort"
        });
        suffix.push(' ');
        suffix.push_str(&shq(effort));
    }
    for name in disabled_mcps {
        suffix.push_str(" --disable-mcp-server ");
        suffix.push_str(&shq(name));
    }
    suffix
}
