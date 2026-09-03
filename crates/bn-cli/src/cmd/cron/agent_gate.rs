//! Everything that can stop a named agent before it runs, in one place.
//!
//! Three refusals, and each answers a different question: *may this thing run at all* (a
//! specialist is installable, not runnable), *may it run here* (its repository is not on this
//! machine), and *is it switched on* (an opt-in agent nobody has opted into).
//!
//! They live together, and outside `run_job_inner`, because the last of them used to be seven
//! copies of the same check scattered through seven agent-named native jobs — which is exactly
//! what made an agent something Rust had to know about rather than something a manifest declares.
use crate::*;
use bn_core::agents::AgentDef;

/// What a refusal costs the caller: the exit code to return, having already said why.
pub(super) struct Refusal(pub i32);

/// Check `agent` against everything that could stop it. `None` means go ahead.
///
/// Exit codes carry the distinction the board reads: `2` is "you asked for something that cannot
/// work", `0` is "correct to do nothing right now" — a repo that lives on another machine, an
/// agent switched off on purpose. Only the second is recorded, because only the second would
/// otherwise show as a scheduled run that never fired.
pub(super) fn refuse(cfg: &Config, agent: &AgentDef) -> Option<Refusal> {
    if let Err(error) = agent.ensure_runnable() {
        eprintln!("bn run: {error}");
        return Some(Refusal(2));
    }
    if !agent.repo_available_here(cfg) {
        let repo = agent.requires_repo.as_deref().unwrap_or_default();
        println!(
            "bn run: skipping '{}' — repository '{repo}' is not registered and available on '{}'",
            agent.name,
            cfg.machine()
        );
        return Some(Refusal(0));
    }
    if let Some(reason) = agent.off_reason(&resolver::JobConfig::load().agents_enabled) {
        println!("{}: {reason}", agent.name);
        bn_core::staff::record_off(&agent.name, &today(), &reason);
        return Some(Refusal(0));
    }
    None
}
