//! `bn forge-agent` and the `forge-agent` job: work Forge activities assigned to an agent member.
//!
//! One tick, under a machine-wide lock so two overlapping ticks never both claim:
//! 1. **Reconcile** every run this node holds against `bn agent live`: heartbeat a running agent;
//!    report `completed` once it ran `bn agent done`; report `blocked` when its window is gone or it
//!    overran `max_runtime_mins`. A finished or dead run's window is killed with `bn agent kill`.
//!    A heartbeat that comes back `lost` means another node owns the task now, so ours is killed.
//! 2. **Claim** one task if there is capacity: ask Forge for claimable tasks, pick the first whose
//!    repo is cloned here, claim it (Forge's conditional update makes exactly one node win), and
//!    `bn dispatch` an agent onto it. A dispatch that fails releases the claim straight away.
//!
//! The rules live in [`bn_core::forge_agent`]; this file is the IO around them. Forge is reached
//! through `bn-mcp --forge-call`, so this binary needs no HTTP client.

use crate::*;
use bn_core::forge_agent::{self as fa, Endpoint, LiveView, Run, RunAction, Runs};
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Subcommand, Debug)]
pub(crate) enum ForgeAgentCmd {
    /// Show whether this machine is opted in, which Forge server it uses, and the runs it holds.
    Status,
    /// Run one tick now: reconcile held runs, then claim and dispatch at most one task.
    RunOnce,
    /// Give a task back to Forge without finishing it, and kill its local agent if one is running.
    Release {
        /// The Forge activity id.
        id: String,
    },
}

/// `bn forge-agent <sub>`.
pub(crate) fn emit_forge_agent(cfg: &Config, action: ForgeAgentCmd, json: bool) -> i32 {
    match action {
        ForgeAgentCmd::Status => status(cfg, json),
        ForgeAgentCmd::RunOnce => tick(cfg, true),
        ForgeAgentCmd::Release { id } => release(cfg, &id, json),
    }
}

/// The `forge-agent` job, run from the five-minute tick. Silent and 0 when not opted in.
pub(crate) fn job_forge_agent(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    tick(cfg, false)
}

fn state_path() -> PathBuf {
    bn_core::resolver::state_dir()
        .join("forge-agent")
        .join("runs.json")
}

fn load_runs() -> Runs {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_runs(runs: &Runs) {
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(runs).unwrap_or_default();
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The file the Forge entry is read from: `BN_FORGE_AGENT_MCP_CONFIG`, else
/// `[forge_agent] mcp_config`, else `~/.claude.json`.
fn mcp_config_path(cfg: &Config) -> Option<PathBuf> {
    std::env::var("BN_FORGE_AGENT_MCP_CONFIG")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .or_else(|| cfg.forge_agent.mcp_config.clone())
        .map(|p| match p.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => PathBuf::from(p),
        })
        .or_else(|| Some(dirs::home_dir()?.join(".claude.json")))
}

fn endpoint(cfg: &Config) -> Option<Endpoint> {
    let text = std::fs::read_to_string(mcp_config_path(cfg)?).ok()?;
    fa::discover_endpoint(&text, &cfg.forge_agent)
}

/// Who this node is to Forge: `bn@<machine>`.
fn claimer() -> String {
    format!("bn@{}", bn_core::worker::node_id())
}

/// One Forge tool call through `bn-mcp --forge-call`. `Err` carries a readable reason.
fn call(ep: &Endpoint, tool: &str, arguments: Value) -> Result<Value, String> {
    let req = json!({ "url": ep.url, "token": ep.token, "tool": tool, "arguments": arguments });
    let mut child = Command::new(crate::mcp::resolve_bn_mcp())
        .arg("--forge-call")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run bn-mcp: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(req.to_string().as_bytes())
            .map_err(|e| format!("writing to bn-mcp: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("waiting for bn-mcp: {e}"))?;
    let reply: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .ok_or_else(|| format!("{tool}: bn-mcp gave no reply"))?;
    if reply["ok"].as_bool() == Some(true) {
        Ok(reply["result"].clone())
    } else {
        Err(format!(
            "{tool}: {}",
            reply["error"].as_str().unwrap_or("failed")
        ))
    }
}

/// `bn agent live`, keyed by `(repo lowercased, branch)`, with each row's kill target.
fn live_agents() -> Vec<(String, String, String, LiveView)> {
    let Ok(out) = bn_core::bn_subprocess::run(&["agent", "live"], None) else {
        return Vec::new();
    };
    out.json
        .as_ref()
        .and_then(|j| j["data"]["agents"].as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
                    let view = LiveView {
                        has_window: r["has_window"].as_bool().unwrap_or(false),
                        state: s("state"),
                    };
                    (s("repo").to_ascii_lowercase(), s("branch"), s("target"), view)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn live_for<'a>(
    live: &'a [(String, String, String, LiveView)],
    run: &Run,
) -> Option<&'a (String, String, String, LiveView)> {
    // Windows are named with slashes dashed, so accept either spelling of the branch.
    let dashed = run.branch.replace('/', "-");
    live.iter().find(|(repo, branch, _, _)| {
        repo.eq_ignore_ascii_case(&run.repo) && (*branch == run.branch || *branch == dashed)
    })
}

fn kill(target: &str) {
    let _ = bn_core::bn_subprocess::run(&["agent", "kill", target], None);
}

/// The PR recorded on a run's branch note, if any. `bn pr` reads the worktree it runs in.
fn pr_url(run: &Run) -> Option<String> {
    let wt = PathBuf::from(run.worktree.as_deref()?);
    let out = bn_core::bn_subprocess::run(&["pr", "--show"], Some(&wt)).ok()?;
    out.json.as_ref()?["data"]["url"].as_str().map(str::to_string)
}

fn tick(cfg: &Config, verbose: bool) -> i32 {
    let say = |msg: &str| {
        if verbose {
            println!("{msg}");
        }
    };
    let Some(ep) = endpoint(cfg) else {
        say("forge-agent: not opted in (no Forge MCP entry with a fam_ key, or disabled)");
        return 0;
    };

    let lock_path = state_path().with_file_name("tick.lock");
    if let Some(dir) = lock_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let lock = match bn_core::flock::open_lock_file(&lock_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("forge-agent: {e}");
            return 1;
        }
    };
    match bn_core::flock::try_lock(&lock) {
        bn_core::flock::FlockOutcome::Acquired => {}
        bn_core::flock::FlockOutcome::Held => {
            say("forge-agent: another tick is running");
            return 0;
        }
        bn_core::flock::FlockOutcome::Failed(e) => {
            eprintln!("forge-agent: lock: {e}");
            return 1;
        }
    }

    let me = claimer();
    let fc = &cfg.forge_agent;
    let mut runs = load_runs();
    let mut failed = false;

    // 1. Reconcile what we hold.
    let live = live_agents();
    let now = bn_core::util::now_epoch_secs();
    let mut kept = Vec::new();
    for run in std::mem::take(&mut runs.runs) {
        let row = live_for(&live, &run);
        let action = fa::reconcile(&run, row.map(|r| &r.3), now, fc.max_runtime_mins);
        let target = row.map_or_else(|| run.target(), |r| r.2.clone());
        match action {
            RunAction::Heartbeat => {
                let args = json!({"id": run.activity_id, "claimer": me, "leaseSeconds": fc.lease_secs});
                match call(&ep, "heartbeat_activity", args) {
                    Ok(v) if v["ok"].as_bool() == Some(true) => kept.push(run),
                    Ok(v) => {
                        say(&format!(
                            "forge-agent: lost {} ({}); stopping {target}",
                            run.activity_id,
                            fa::reply_reason(&v).unwrap_or("rejected")
                        ));
                        kill(&target);
                    }
                    // Forge unreachable: keep running and try again next tick. The lease covers
                    // a missed beat or two.
                    Err(e) => {
                        eprintln!("forge-agent: {e}");
                        failed = true;
                        kept.push(run);
                    }
                }
            }
            RunAction::Complete | RunAction::Block(_) => {
                let (outcome, summary) = match &action {
                    RunAction::Complete => (
                        "completed",
                        format!("Done by {me} on branch {}.", run.branch),
                    ),
                    RunAction::Block(why) => ("blocked", format!("{me}: {why} (branch {}).", run.branch)),
                    RunAction::Heartbeat => unreachable!(),
                };
                let mut args = json!({
                    "id": run.activity_id, "claimer": me, "outcome": outcome, "summary": summary,
                });
                if let Some(pr) = pr_url(&run) {
                    args["prUrl"] = json!(pr);
                }
                kill(&target);
                match call(&ep, "finish_activity", args) {
                    Ok(v) => say(&format!(
                        "forge-agent: {} {} → {outcome}{}",
                        run.activity_id,
                        run.title,
                        fa::reply_reason(&v)
                            .map(|r| format!(" (forge: {r})"))
                            .unwrap_or_default()
                    )),
                    // The window is already gone; keep the run so the report is retried.
                    Err(e) => {
                        eprintln!("forge-agent: {e}");
                        failed = true;
                        kept.push(run);
                    }
                }
            }
        }
    }
    runs.runs = kept;
    save_runs(&runs);

    // 2. Claim one more if there is room.
    if runs.runs.len() >= fc.max_concurrent {
        say("forge-agent: at capacity");
        return i32::from(failed);
    }
    let list = match call(
        &ep,
        "list_bn_tasks",
        json!({"includeCompletedSinceDays": 0, "claimable": true}),
    ) {
        Ok(v) => fa::parse_task_list(&v),
        Err(e) => {
            eprintln!("forge-agent: {e}");
            return 1;
        }
    };
    let repos: Vec<(String, String)> = bn_core::lifecycle::cloned_repo_dirs(cfg)
        .into_iter()
        .filter_map(|(name, dir)| Some((name, bn_core::forge::origin_url(&dir)?)))
        .collect();
    let Some((task, repo)) = fa::pick_task(&list, &runs, &repos, fc.max_concurrent) else {
        say(&format!(
            "forge-agent: nothing to claim ({} claimable, none for a repo cloned here)",
            list.len()
        ));
        return i32::from(failed);
    };

    let claim = call(
        &ep,
        "claim_activity",
        json!({"id": task.id, "claimer": me, "leaseSeconds": fc.lease_secs}),
    );
    match claim {
        Ok(v) if v["claimed"].as_bool() == Some(true) => {}
        Ok(v) => {
            say(&format!(
                "forge-agent: {} not claimed ({})",
                task.id,
                fa::reply_reason(&v).unwrap_or("rejected")
            ));
            return i32::from(failed);
        }
        Err(e) => {
            eprintln!("forge-agent: {e}");
            return 1;
        }
    }

    let branch = fa::branch_for(task);
    let brief = fa::brief(task);
    let agent_flag = match &fc.harness {
        Some(h) => format!("--agent={h}"),
        None => "--agent".to_string(),
    };
    let args = [
        "dispatch",
        brief.as_str(),
        "--repo",
        repo.as_str(),
        "--branch",
        branch.as_str(),
        "--name",
        task.title.as_str(),
        agent_flag.as_str(),
        "--yolo",
        "--no-plan-review",
    ];
    // Declared as the target repo's own dispatch so the cross-repo-session guard lets it through.
    let home = dirs::home_dir();
    let out = bn_core::bn_subprocess::run_with_env(
        &args,
        home.as_deref(),
        &[("BN_WORKER_REPO", repo.as_str())],
    );
    let data = out
        .as_ref()
        .ok()
        .and_then(|o| o.json.as_ref())
        .map(|j| j["data"].clone())
        .unwrap_or(Value::Null);
    if data["agent_launched"].as_bool() != Some(true) {
        let why = match &out {
            Err(e) => e.to_string(),
            Ok(o) => o
                .json
                .as_ref()
                .and_then(|j| j["error"]["message"].as_str())
                .or_else(|| data["reason"].as_str())
                .unwrap_or("agent did not launch")
                .to_string(),
        };
        eprintln!("forge-agent: dispatch for {} failed: {why}; releasing", task.id);
        let _ = call(
            &ep,
            "release_activity",
            json!({"id": task.id, "claimer": me, "reason": format!("dispatch failed: {why}")}),
        );
        return 1;
    }

    runs.runs.push(Run {
        activity_id: task.id.clone(),
        title: task.title.clone(),
        repo: repo.clone(),
        branch: data["branch"].as_str().unwrap_or(&branch).to_string(),
        worktree: data["worktree"].as_str().map(str::to_string),
        claimed_at: bn_core::util::now_epoch_secs(),
    });
    save_runs(&runs);
    say(&format!(
        "forge-agent: claimed {} \"{}\" → {repo}:{branch}",
        task.id, task.title
    ));
    i32::from(failed)
}

fn status(cfg: &Config, json: bool) -> i32 {
    let ep = endpoint(cfg);
    let runs = load_runs();
    if json {
        let data = json!({
            "enabled": ep.is_some(),
            "server": ep.as_ref().map(|e| e.name.clone()),
            "url": ep.as_ref().map(|e| e.url.clone()),
            "claimer": claimer(),
            "max_concurrent": cfg.forge_agent.max_concurrent,
            "runs": runs.runs,
        });
        println!("{}", Envelope::success("forge_agent", data).to_json());
        return 0;
    }
    match &ep {
        Some(e) => println!("forge-agent: on — {} ({}) as {}", e.name, e.url, claimer()),
        None => println!(
            "forge-agent: off — add a Forge MCP server with an agent key to ~/.claude.json to opt in"
        ),
    }
    if runs.runs.is_empty() {
        println!("no runs");
    }
    for r in &runs.runs {
        println!("{}  {}  {}", r.activity_id, r.target(), r.title);
    }
    0
}

fn release(cfg: &Config, id: &str, json: bool) -> i32 {
    let Some(ep) = endpoint(cfg) else {
        fail_json(json, "forge_agent", "not_enabled", "forge-agent is not opted in");
    };
    let mut runs = load_runs();
    if let Some(pos) = runs.runs.iter().position(|r| r.activity_id == id) {
        let run = runs.runs.remove(pos);
        let live = live_agents();
        let target = live_for(&live, &run).map_or_else(|| run.target(), |r| r.2.clone());
        kill(&target);
        save_runs(&runs);
    }
    match call(
        &ep,
        "release_activity",
        json!({"id": id, "claimer": claimer(), "reason": "released by hand"}),
    ) {
        Ok(v) => {
            if json {
                println!("{}", Envelope::success("forge_agent", v).to_json());
            } else {
                println!("released {id}");
            }
            0
        }
        Err(e) => fail_json(json, "forge_agent", "release_failed", &e),
    }
}
