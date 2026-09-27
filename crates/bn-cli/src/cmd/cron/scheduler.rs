use super::run_job;
use crate::*;

/// Run one self-describing agent **headlessly** through the named execution boundary. The
/// scheduler and startup paths only decide eligibility; resolution, gates, and ledger ownership all
/// remain in [`run_job`].
///
/// Returns true when the agent ran — or was intentionally skipped (not due at `now`, or a prior run
/// still in flight) — and false when the command could not be launched.
pub(crate) fn run_agent(
    cfg: &Config,
    ctx: Option<&Resolved>,
    agent: &bn_core::agents::AgentDef,
) -> bool {
    let now = bn_core::util::now_epoch_secs();
    if !agent.launchable(bn_core::events::last_run(&agent.name).as_ref(), now) {
        return true;
    }
    if bn_core::events::running_jobs().contains(&agent.name) {
        return true;
    }
    run_job(cfg, ctx, &agent.name) == 0
}

/// Every scheduled non-specialist agent due at `now`, using its own last-attempt and retry policy.
pub(crate) fn due_agents(
    cfg: &Config,
    ctx: Option<&Resolved>,
    now: i64,
) -> Result<Vec<bn_core::agents::AgentDef>, bn_core::agents::AgentCatalogError> {
    let catalog = bn_core::agents::discover(cfg, ctx)?;
    let last_runs = bn_core::events::last_runs();
    Ok(catalog
        .into_iter()
        .filter(|agent| agent.is_cron() && !agent.is_specialist())
        .filter(|agent| agent.repo_available_here(cfg))
        .filter(|agent| agent.due(last_runs.get(&agent.name), now))
        .collect())
}

/// One pass of the common minute-level agent scheduler. Catalog failures fail this job loudly;
/// individual agent failures remain on each agent's own run row and never block later siblings.
pub(crate) fn job_agent_scheduler(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let now = bn_core::util::now_epoch_secs();
    let agents = match due_agents(cfg, ctx, now) {
        Ok(agents) => agents,
        Err(error) => {
            native_job_line(&format!("agent-scheduler: {error}"), true);
            return 2;
        }
    };
    for agent in agents {
        run_agent(cfg, ctx, &agent);
    }
    0
}

/// The tmux server PID (`tmux display-message -p '#{pid}'`) — the stable boot key for the startup
/// dedup. `None` when no server is running (or tmux is absent).
fn tmux_server_pid() -> Option<String> {
    proc::tmux(&["display-message", "-p", "#{pid}"])
}

/// Claim the once-per-boot startup fire for tmux server `pid`.
pub(crate) fn claim_startup(state_dir: &std::path::Path, pid: &str) -> bool {
    let sentinel = state_dir.join(format!("startup-{pid}"));
    if sentinel.exists() {
        return false;
    }
    let _ = std::fs::create_dir_all(state_dir);
    std::fs::write(&sentinel, b"").is_ok()
}

/// Fire every `trigger: startup` agent once per tmux server boot.
pub(crate) fn emit_startup(cfg: &Config, ctx: Option<&Resolved>) {
    let Some(pid) = tmux_server_pid() else {
        return;
    };
    let catalog = bn_core::agents::discover(cfg, ctx)
        .unwrap_or_else(|error| fail(&format!("bn startup: {error}")));
    if !claim_startup(&resolver::state_dir(), &pid) {
        return;
    }
    for agent in catalog {
        if agent.is_startup() {
            run_agent(cfg, ctx, &agent);
        }
    }
}
