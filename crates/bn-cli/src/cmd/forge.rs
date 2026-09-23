//! `bn forge` — local bridge from Forge claimable tasks into bn dispatch.

use crate::*;
use bn_core::forge_local::{
    load_profile, ForgeAgentProfile, ForgeBnTask, ForgeClaimActivityResponse,
    ForgeListBnTasksResponse, ForgeProfileError, ForgeSelectionError,
};
use serde_json::json;
use serde_json::Value;

const COMMAND_LIST: &str = "forge-list";
const COMMAND_CLAIM: &str = "forge-claim";
const COMMAND_DISPATCH: &str = "forge-dispatch";
const COMMAND_TICK: &str = "forge-tick";
/// Upper bound on the task page a tick reads before filtering. Forge's bn queue is family-sized,
/// so this is a safety valve against an unbounded response, not a real paging strategy.
const TICK_TASK_LIMIT: usize = 200;

pub(crate) fn run(cfg: &Config, action: ForgeCmd, json: bool) {
    let result = run_with_client(cfg, action, json, &mut StdioForgeMcpClient::default());
    match result {
        Ok(outcome) => emit_success(outcome, json),
        Err(error) => fail_json(json, error.command(), error.code(), error.message()),
    }
}

/// `forge-tick` — the unattended poll that keeps every Forge agent profile on this machine moving:
/// release claims whose agent is gone, heartbeat the ones still working, and take on new work up to
/// the per-agent cap.
///
/// A machine with no Forge profile is not a Forge agent, so it exits clean rather than failing the
/// cron on every non-agent host.
pub(crate) fn job_forge_tick(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let agents = bn_core::forge_local::installed_agents();
    if agents.is_empty() {
        println!("forge-tick: no Forge agent profile on this machine; nothing to do");
        return 0;
    }
    // Each task is routed to its own repo; this is only the fallback for an unmapped goal.
    let repo = std::env::var("BN_FORGE_TICK_REPO").ok();
    let max_active = std::env::var("BN_FORGE_TICK_MAX_ACTIVE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let mut rc = 0;
    for agent in agents {
        let action = ForgeCmd::Tick {
            agent: agent.clone(),
            repo: repo.clone(),
            agent_harness: "copilot".into(),
            base: std::env::var("BN_FORGE_TICK_BASE").ok(),
            skill: "forge-task".into(),
            max_active,
            dry_run: false,
            lease_seconds: 900,
        };
        match run_with_client(cfg, action, false, &mut StdioForgeMcpClient::default()) {
            Ok(outcome) => emit_success(outcome, false),
            Err(error) => {
                eprintln!("forge-tick {agent}: {}", error.message());
                rc = 1;
            }
        }
    }
    rc
}

fn run_with_client<C: ForgeMcpClient>(
    cfg: &Config,
    action: ForgeCmd,
    json_output: bool,
    client: &mut C,
) -> Result<ForgeOutcome, ForgeRunError> {
    match action {
        ForgeCmd::List { agent, task, limit } => {
            let profile = load_profile(&agent).map_err(|error| ForgeRunError::profile(
                COMMAND_LIST,
                error,
            ))?;
            let mut tasks = client
                .list_claimable_tasks(&profile, limit)
                .map_err(|error| ForgeRunError::mcp(COMMAND_LIST, error))?;
            if let Some(task_id) = task {
                tasks.retain(|task| task.id == task_id);
                if tasks.is_empty() {
                    return Err(ForgeRunError::selection(
                        COMMAND_LIST,
                        ForgeSelectionError::NotFound(task_id),
                    ));
                }
            }
            Ok(ForgeOutcome::List { profile, tasks })
        }
        ForgeCmd::Claim {
            agent,
            task,
            lease_seconds,
        } => {
            let profile = load_profile(&agent).map_err(|error| ForgeRunError::profile(
                COMMAND_CLAIM,
                error,
            ))?;
            claim_one(client, &profile, &task, lease_seconds).map(|claim| ForgeOutcome::Claim {
                profile,
                task_id: task,
                claim,
            })
        }
        ForgeCmd::Dispatch {
            agent,
            task,
            repo,
            agent_harness,
            base,
            skill,
            name,
            verify,
            lease_seconds,
        } => {
            let profile = load_profile(&agent).map_err(|error| ForgeRunError::profile(
                COMMAND_DISPATCH,
                error,
            ))?;
            let claim = claim_one(client, &profile, &task, lease_seconds)?;
            let dispatch_task = claim
                .activity
                .as_ref()
                .cloned()
                .unwrap_or_else(|| ForgeBnTask {
                    id: task.clone(),
                    goal_id: None,
                    goal_name: None,
                    goal_repo_url: None,
                    title: task.clone(),
                    description: None,
                    priority: None,
                    status: None,
                    version: None,
                    is_completed: false,
                    agent_meta: Default::default(),
                    assignees: Vec::new(),
                });
            let repo = repo
                .or_else(|| resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok().map(|ctx| ctx.repo))
                .ok_or_else(|| {
                    ForgeRunError::new(
                        COMMAND_DISPATCH,
                        "dispatch_repo_required",
                        "bn forge dispatch: pass --repo, or run inside the target repo worktree",
                    )
                })?;
            dispatch_claimed(
                &dispatch_task,
                &profile,
                &repo,
                &agent_harness,
                base.as_deref(),
                skill.as_str(),
                name.as_deref(),
                verify,
                json_output,
            )
                .map_err(|message| {
                    let blocker = format!(
                        "Forge task {} is claimed by {} but local dispatch failed: {}",
                        dispatch_task.id, profile.claimer.display_name, message
                    );
                    let note_result = record_blocker(cfg, &blocker, &dispatch_task.id);
                    let full = match note_result {
                        Ok(()) => format!("{blocker}; blocker recorded in the current branch note"),
                        Err(error) => format!("{blocker}; additionally could not record blocker: {error}"),
                    };
                    ForgeRunError::new(COMMAND_DISPATCH, "dispatch_failed_after_claim", full)
                })?;
            Ok(ForgeOutcome::Dispatch {
                profile,
                task: dispatch_task,
                claim,
                repo,
                agent_harness,
            })
        }
        ForgeCmd::Tick {
            agent,
            repo,
            agent_harness,
            base,
            skill,
            max_active,
            dry_run,
            lease_seconds,
        } => {
            let profile = load_profile(&agent)
                .map_err(|error| ForgeRunError::profile(COMMAND_TICK, error))?;
            let repo = repo.or_else(|| {
                resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default())
                    .ok()
                    .map(|ctx| ctx.repo)
            });
            run_tick(
                client,
                TickRequest {
                    profile,
                    repo,
                    agent_harness,
                    base,
                    skill,
                    max_active,
                    dry_run,
                    lease_seconds,
                    json_output,
                },
                &live_targets(cfg),
                &bn_core::issues::known_repos(cfg),
            )
        }
    }
}

/// One `bn forge tick` invocation, gathered so the loop below reads as policy rather than argument
/// plumbing.
struct TickRequest {
    profile: ForgeAgentProfile,
    /// Fallback for a task whose goal routes nowhere (see [`bn_core::forge_local::route_repo`]).
    repo: Option<String>,
    agent_harness: String,
    base: Option<String>,
    skill: String,
    max_active: usize,
    dry_run: bool,
    lease_seconds: u64,
    json_output: bool,
}

/// Targets of agents that are still doing something, lowercased as `bn agent live` reports them.
///
/// `done` is deliberately excluded: an agent that finished without closing its Forge task is the
/// exact failure this poller exists to clean up, so its claim must look abandoned, not live.
fn live_targets(cfg: &Config) -> Vec<String> {
    agent_live::collect(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    )
        .into_iter()
        .filter(|view| view.state != "done")
        .map(|view| view.target)
        .collect()
}

/// Whether some live agent is working this task, matched on the id prefix dispatch puts in the
/// branch (and therefore the target), since the full uuid is truncated there.
fn task_has_live_agent(live: &[String], task_id: &str) -> bool {
    let needle = short_id(task_id).to_lowercase();
    !needle.is_empty() && live.iter().any(|target| target.contains(&needle))
}

fn run_tick<C: ForgeMcpClient>(
    client: &mut C,
    req: TickRequest,
    live: &[String],
    known_repos: &[String],
) -> Result<ForgeOutcome, ForgeRunError> {
    let claimer = req.profile.claimer.id.clone();
    let all = client
        .list_all_tasks(&req.profile, TICK_TASK_LIMIT)
        .map_err(|error| ForgeRunError::mcp(COMMAND_TICK, error))?;

    let mut held = Vec::new();
    let mut released = Vec::new();
    let mut errors = Vec::new();
    // Pass 1 — settle what this agent already holds, so capacity reflects reality before any new
    // work is taken on.
    for task in all
        .iter()
        .filter(|task| !task.is_completed && task.agent_meta.claimer.as_deref() == Some(claimer.as_str()))
    {
        if task_has_live_agent(live, &task.id) {
            if !req.dry_run {
                if let Err(error) =
                    client.heartbeat_activity(&req.profile, &task.id, req.lease_seconds)
                {
                    errors.push(format!("heartbeat {}: {}", task.id, error.message));
                }
            }
            held.push(task.id.clone());
        } else {
            let reason = format!(
                "bn forge tick: no live {claimer} agent for this claim; releasing so it can be picked up again"
            );
            if !req.dry_run {
                if let Err(error) = client.release_activity(&req.profile, &task.id, &reason) {
                    errors.push(format!("release {}: {}", task.id, error.message));
                    continue;
                }
            }
            released.push(task.id.clone());
        }
    }

    // Pass 2 — fill whatever capacity is left from the claimable queue.
    let mut dispatched = Vec::new();
    let mut unrouted = Vec::new();
    let capacity = req.max_active.saturating_sub(held.len());
    if capacity > 0 {
        let claimable = client
            .list_claimable_tasks(&req.profile, TICK_TASK_LIMIT)
            .map_err(|error| ForgeRunError::mcp(COMMAND_TICK, error))?;
        let mut routed = Vec::new();
        for task in claimable {
            match bn_core::forge_local::route_repo(
                &task,
                &req.profile,
                known_repos,
                req.repo.as_deref(),
            ) {
                Ok(repo) => routed.push((task, repo)),
                // Never claim what cannot be routed: the claim would only sit until reaped.
                Err(reason) => unrouted.push(format!("{}: {reason}", task.id)),
            }
        }
        for (task, repo) in routed.into_iter().take(capacity) {
            if req.dry_run {
                dispatched.push(task.id);
                continue;
            }
            let claim = match claim_one(client, &req.profile, &task.id, req.lease_seconds) {
                Ok(claim) => claim,
                Err(error) => {
                    errors.push(format!("claim {}: {}", task.id, error.message()));
                    continue;
                }
            };
            let task = claim.activity.clone().unwrap_or(task);
            match dispatch_claimed(
                &task,
                &req.profile,
                &repo,
                &req.agent_harness,
                req.base.as_deref(),
                req.skill.as_str(),
                None,
                false,
                req.json_output,
            ) {
                Ok(()) => dispatched.push(task.id),
                Err(message) => {
                    // A claim we cannot dispatch is the very state this poller reaps, so hand it
                    // straight back instead of letting the next tick discover it as abandoned.
                    let reason = format!("bn forge tick: dispatch failed after claim: {message}");
                    if let Err(error) = client.release_activity(&req.profile, &task.id, &reason) {
                        errors.push(format!(
                            "release-after-failed-dispatch {}: {}",
                            task.id, error.message
                        ));
                    }
                    errors.push(format!("dispatch {}: {message}", task.id));
                }
            }
        }
    }

    Ok(ForgeOutcome::Tick {
        profile: req.profile,
        held,
        released,
        dispatched,
        unrouted,
        errors,
        dry_run: req.dry_run,
    })
}

fn claim_one<C: ForgeMcpClient>(
    client: &mut C,
    profile: &ForgeAgentProfile,
    task_id: &str,
    lease_seconds: u64,
) -> Result<ForgeClaimActivityResponse, ForgeRunError> {
    let claim = client
        .claim_activity(profile, task_id, lease_seconds)
        .map_err(|error| ForgeRunError::mcp(COMMAND_CLAIM, error))?;
    if claim.claimed {
        Ok(claim)
    } else {
        let reason = claim.reason.unwrap_or_else(|| "unknown".to_string());
        Err(ForgeRunError::new(
            COMMAND_CLAIM,
            "claim_failed",
            format!("bn forge claim: task '{task_id}' was not claimed ({reason})"),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_claimed(
    task: &ForgeBnTask,
    profile: &ForgeAgentProfile,
    repo: &str,
    agent: &str,
    base: Option<&str>,
    skill: &str,
    name: Option<&str>,
    verify: bool,
    json_output: bool,
) -> Result<(), String> {
    let item = forge_dispatch_item(task, profile);
    let default_name = format!("forge-{}", short_id(&task.id));
    DispatchInvocation::new(&item, repo, agent)
        .name(name.or(Some(default_name.as_str())))
        .base(base)
        .skill(Some(skill))
        .task(Some(&task.id))
        .verify(verify)
        .json(json_output)
        .launch()
}

fn forge_dispatch_item(task: &ForgeBnTask, profile: &ForgeAgentProfile) -> String {
    let mut item = format!(
        "Forge task {}: {}\n\nClaimer: {}\n\nUse the `forge-task` skill as the lifecycle wrapper. When done, update Forge task {} with a rich Markdown report and call finish_activity using claimer {}.",
        task.id, task.title, profile.claimer.id, task.id, profile.claimer.id
    );
    if let Some(description) = task.description.as_deref().filter(|text| !text.trim().is_empty()) {
        item.push_str("\n\nTask details:\n");
        item.push_str(description.trim());
    }
    if let Some(goal) = task.goal_name.as_deref().filter(|text| !text.trim().is_empty()) {
        item.push_str("\n\nForge goal: ");
        item.push_str(goal);
    }
    item
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn record_blocker(cfg: &Config, text: &str, key: &str) -> Result<(), String> {
    let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default())
        .map_err(|error| format!("could not resolve current branch note: {error:?}"))?;
    let outcome = commands::add_full(
        cfg,
        &ctx,
        "blocker",
        text,
        Some(key),
        None,
        None,
        &today(),
    )
    .map_err(|error| error.message())?;
    if !outcome.skipped {
        append_inherited_worker_event(
            cfg,
            &ctx,
            bn_core::controller::ControllerWorkerEventType::Blocked,
            text,
            None,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

trait ForgeMcpClient {
    fn list_claimable_tasks(
        &mut self,
        profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError>;

    fn claim_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        lease_seconds: u64,
    ) -> Result<ForgeClaimActivityResponse, ForgeMcpError>;

    /// Every bn task Forge knows about, claimed or not — the only view that shows what this agent
    /// is already holding, which `list_claimable_tasks` by definition excludes.
    fn list_all_tasks(
        &mut self,
        profile: &ForgeAgentProfile,
        limit: usize,
    ) -> Result<Vec<ForgeBnTask>, ForgeMcpError>;

    /// Hand a claim back to Forge so another agent can take it.
    fn release_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        reason: &str,
    ) -> Result<(), ForgeMcpError>;

    /// Extend the claim lease for work that is genuinely still running.
    fn heartbeat_activity(
        &mut self,
        profile: &ForgeAgentProfile,
        task_id: &str,
        lease_seconds: u64,
    ) -> Result<(), ForgeMcpError>;
}

#[path = "forge/mcp_client.rs"]
mod mcp_client;
use mcp_client::StdioForgeMcpClient;

#[derive(Debug, Clone)]
struct ForgeMcpError {
    code: &'static str,
    message: String,
}

impl ForgeMcpError {
    fn transport(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn metadata_unavailable(profile: &ForgeAgentProfile) -> Self {
        let target = profile
            .forge_mcp
            .server
            .as_deref()
            .or(profile.forge_mcp.endpoint.as_deref())
            .unwrap_or("forge");
        Self {
            code: "mcp_client_unavailable",
            message: format!(
                "bn forge: OAuth MCP profile '{}' targets '{target}', but this bn binary has no Forge MCP client transport yet; documented Forge tools are list_bn_tasks(claimable=true) and claim_activity(claimer,id,leaseSeconds)",
                profile.agent
            ),
        }
    }
}

#[derive(Debug)]
struct ForgeRunError {
    command: &'static str,
    code: String,
    message: String,
}

impl ForgeRunError {
    fn new(
        command: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            command,
            code: code.into(),
            message: message.into(),
        }
    }

    fn profile(command: &'static str, error: ForgeProfileError) -> Self {
        Self::new(command, error.code(), error.message())
    }

    fn mcp(command: &'static str, error: ForgeMcpError) -> Self {
        Self::new(command, error.code, error.message)
    }

    fn selection(command: &'static str, error: ForgeSelectionError) -> Self {
        Self::new(command, error.code(), error.message())
    }

    fn command(&self) -> &'static str {
        self.command
    }

    fn code(&self) -> &str {
        &self.code
    }

    fn message(&self) -> &str {
        &self.message
    }
}

#[allow(clippy::large_enum_variant)]
enum ForgeOutcome {
    List {
        profile: ForgeAgentProfile,
        tasks: Vec<ForgeBnTask>,
    },
    Claim {
        profile: ForgeAgentProfile,
        task_id: String,
        claim: ForgeClaimActivityResponse,
    },
    Dispatch {
        profile: ForgeAgentProfile,
        task: ForgeBnTask,
        claim: ForgeClaimActivityResponse,
        repo: String,
        agent_harness: String,
    },
    Tick {
        profile: ForgeAgentProfile,
        held: Vec<String>,
        released: Vec<String>,
        dispatched: Vec<String>,
        /// Claimable tasks left unclaimed because their goal maps to no local repo.
        unrouted: Vec<String>,
        errors: Vec<String>,
        dry_run: bool,
    },
}

fn emit_success(outcome: ForgeOutcome, json_output: bool) {
    match outcome {
        ForgeOutcome::Tick {
            profile,
            held,
            released,
            dispatched,
            unrouted,
            errors,
            dry_run,
        } => {
            let data = json!({
                "agent": profile.agent,
                "claimer": profile.claimer,
                "dryRun": dry_run,
                "held": held,
                "released": released,
                "dispatched": dispatched,
                "unrouted": unrouted,
                "errors": errors,
            });
            if json_output {
                println!("{}", Envelope::success(COMMAND_TICK, data).to_json());
            } else {
                let prefix = if dry_run { "would " } else { "" };
                println!(
                    "forge tick {}: {}held {}, {}released {}, {}dispatched {}",
                    profile.agent,
                    prefix,
                    held.len(),
                    prefix,
                    released.len(),
                    prefix,
                    dispatched.len()
                );
                for id in &released {
                    println!("released\t{id}");
                }
                for id in &dispatched {
                    println!("dispatched\t{id}");
                }
                for task in &unrouted {
                    println!("unrouted\t{task}");
                }
                for error in &errors {
                    eprintln!("warn\t{error}");
                }
            }
        }
        ForgeOutcome::List { profile, tasks } => {
            let data = json!({
                "agent": profile.agent,
                "claimer": profile.claimer,
                "tasks": tasks,
            });
            if json_output {
                println!("{}", Envelope::success(COMMAND_LIST, data).to_json());
            } else if let Some(tasks) = data.get("tasks").and_then(|v| v.as_array()) {
                println!("{} claimable Forge task(s) for {}", tasks.len(), profile.agent);
                for task in tasks {
                    let id = task.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let priority = task.get("priority").and_then(|v| v.as_str()).unwrap_or("None");
                    println!("{id}\t{priority}\t{title}");
                }
            }
        }
        ForgeOutcome::Claim {
            profile,
            task_id,
            claim,
        } => {
            let data = json!({
                "agent": profile.agent,
                "claimer": profile.claimer,
                "taskId": task_id,
                "leaseExpiresAt": claim.lease_expires_at,
            });
            if json_output {
                println!("{}", Envelope::success(COMMAND_CLAIM, data).to_json());
            } else {
                println!(
                    "claimed Forge task {} as {}",
                    task_id, profile.claimer.display_name
                );
            }
        }
        ForgeOutcome::Dispatch {
            profile,
            task,
            claim,
            repo,
            agent_harness,
        } => {
            let data = json!({
                "agent": profile.agent,
                "claimer": profile.claimer,
                "task": task,
                "leaseExpiresAt": claim.lease_expires_at,
                "repo": repo,
                "agentHarness": agent_harness,
            });
            if json_output {
                println!("{}", Envelope::success(COMMAND_DISPATCH, data).to_json());
            } else {
                println!(
                    "dispatched Forge task {} to {} with {}",
                    task.id, repo, agent_harness
                );
            }
        }
    }
}

#[allow(dead_code)]
fn parse_list_response(value: serde_json::Value) -> Result<Vec<ForgeBnTask>, serde_json::Error> {
    serde_json::from_value::<ForgeListBnTasksResponse>(value).map(|response| response.items)
}

#[cfg(test)]
#[path = "forge/tests.rs"]
mod tests;
