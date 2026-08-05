//! The **opt-in** orchestrator MCP server (`bn-mcp --orchestrate`) — the cross-worktree surface the
//! "friend" drives. It is deliberately SEPARATE from the scoped [`crate::server::BnServer`]: that
//! server is locked to one worktree so an agent can't touch unrelated repos, and this one is its
//! escape hatch, registered only in the orchestrator's own environment.
//!
//! Every tool composes the real `bn` CLI (resolved binary path, never a bare PATH guess) rather than
//! re-implementing dispatch/fleet/attention logic — so there is one source of truth (the CLI) and
//! bn-core stays tmux-free. The tools are thin: resolve `bn`, run a subcommand with `--json`, hand
//! the envelope back.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct BnOrchestratorServer;

impl Default for BnOrchestratorServer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpinUpArgs {
    /// Target repo by name (a bare repo under the bare root). Works from anywhere.
    repo: String,
    /// The goal / item to work on — becomes the new branch note's Goal and the agent's seed. May be
    /// free text, or an issue/PR URL (its title is resolved).
    goal: String,
    /// Branch to create (default: derived from the goal as `dispatch/<slug>`). For attention
    /// fanout, pass a unique branch (e.g. `dispatch/<slug>-<attn-id>`) so items can't collide.
    #[serde(default)]
    branch: Option<String>,
    /// Base branch to fork from (default: main).
    #[serde(default)]
    base: Option<String>,
    /// Agent to launch in the worktree window: `copilot` (default) or `claude`. Empty/absent →
    /// copilot. Pass a literal `none` to open a plain shell window with no agent.
    #[serde(default)]
    agent: Option<String>,
    /// Scout task: the agent investigates / reproduces / audits and writes findings to the branch
    /// note — it changes no code (vs the default "ship" task, which delivers a change). Use for
    /// "investigate X and report back". Default false (ship).
    #[serde(default)]
    scout: bool,
    /// Workflow skill to seed the agent with (see the bn skill prompts): its rendered body becomes
    /// the agent's process, ending with a `bn reflect "skill:<name> — …"` line. Defaults per task
    /// type — `plan-work` (ship) / `investigate` (scout); pass `debug` for a repro-first bugfix.
    #[serde(default)]
    skill: Option<String>,
    /// Gate "done" on verification: the ship agent must run `bn verify` (the repo's build/test/lint gate)
    /// and only report done once it passes. Ignored for scout. Default false.
    #[serde(default)]
    verify: bool,
    /// Trusted-repo autonomy: launch the agent without approval prompts so it runs clean instead of
    /// wedging on first-run trust/MCP prompts. Only for repos the user trusts. Default false.
    #[serde(default)]
    yolo: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    /// The agent's tmux window, as `repo:branch` (the `target` from `list_agents`).
    target: String,
    /// The message to send.
    msg: String,
    /// Fire-and-forget for a LONG task (implement + PR): run the turn detached and return now; the
    /// agent reports back with `bn agent done`. Use this for anything that will take minutes — a
    /// blocking send would time out. Default false (waits for the reply, good for quick questions).
    #[serde(default)]
    detach: bool,
}

#[tool_router]
impl BnOrchestratorServer {
    pub fn new() -> Self {
        Self
    }

    /// Run `bn --json <args...>` and return the parsed JSON envelope. Errors carry the CLI's stderr.
    fn run_bn(&self, args: &[&str]) -> Result<Value, ErrorData> {
        self.run_bn_env(args, &[])
    }

    /// Like [`Self::run_bn`], but sets extra env on the child. `spin_up_work` uses it to declare
    /// `BN_WORKER_REPO=<target>` so the CLI's cross-repo-session guard treats a spin-up as the
    /// target repo's own session (this server runs from its own worktree, so it would otherwise
    /// look like a stray cross-repo hop and be blocked).
    fn run_bn_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<Value, ErrorData> {
        let out = bn_core::bn_subprocess::run_with_env(args, None, envs)
            .map_err(|e| ErrorData::internal_error(format!("failed to run {e}"), None))?;
        if !out.success {
            let stderr = out.stderr.trim();
            let stdout = out.stdout.trim();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            return Err(ErrorData::internal_error(
                format!("bn {}: {detail}", args.join(" ")),
                None,
            ));
        }
        // The `--json` envelope is the LAST line, but some subcommands (e.g. dispatch's tmux
        // window-open) print a human line to stdout first. Take the last JSON-parseable line so we
        // tolerate that leading noise instead of choking on it.
        out.json.ok_or_else(|| {
            ErrorData::internal_error(
                format!("bn {} returned no JSON envelope", args.join(" ")),
                None,
            )
        })
    }

    #[tool(
        name = "list_attention",
        description = "The open attention list across all repos, each item annotated with a triage class: \"review\" (a finished agent's completion — source `agent`; review/merge its result, don't re-dispatch), \"auto\" (fully routable to a worktree — has both repo and branch — safe to spin up unattended), or \"confirm\" (no repo/branch, or a judgment call — ask first). Use this to decide what to spin_up_work autonomously, what to surface, and which agents just finished."
    )]
    async fn list_attention(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Two lanes since #242: `attn list` is the actionable open set (completions filtered
        // out); the finished-agent completions this tool classifies as "review" live in the
        // feed's review inbox. Read both so a done agent still surfaces here.
        let env = self.run_bn(&["attn", "list"])?;
        let mut items = env
            .get("data")
            .and_then(|d| d.get("items"))
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        let feed = self.run_bn(&["feed"])?;
        items.extend(
            feed.get("data")
                .and_then(|d| d.get("completed"))
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default(),
        );
        let classify = |it: &Value| -> &'static str {
            let has = |k: &str| {
                it.get(k)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
            };
            let source = it.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let title = it.get("title").and_then(|v| v.as_str()).unwrap_or("");
            // A finished agent's completion push (source `agent`, "done: …") is NOT new work to
            // spin up — it's a result to review/merge. Classify it apart so the buddy never
            // re-dispatches a done item.
            if source == "agent" || title.starts_with("done:") {
                "review"
            } else if has("repo") && has("branch") {
                // Auto only when the item routes to a specific worktree — the unambiguous case
                // (e.g. an auto-review PR). Anything without repo+branch needs a human to place it.
                "auto"
            } else {
                "confirm"
            }
        };
        let annotated: Vec<Value> = items
            .iter()
            .map(|it| {
                let mut o = it.clone();
                if let Some(map) = o.as_object_mut() {
                    map.insert("triage".into(), json!(classify(it)));
                }
                o
            })
            .collect();
        let count = |cls: &str| {
            annotated
                .iter()
                .filter(|it| it.get("triage").and_then(|v| v.as_str()) == Some(cls))
                .count()
        };
        Ok(CallToolResult::structured(json!({
            "items": annotated,
            "auto": count("auto"),
            "confirm": count("confirm"),
            "review": count("review"),
        })))
    }

    #[tool(
        name = "list_agents",
        description = "The joined live agent fleet across all worktrees and both harnesses (Copilot + Claude): one row per repo:branch with its tmux state (working/waiting/done), health (active/idle/stuck), and whether a window exists. This is \"what are all my agents doing right now\"."
    )]
    async fn list_agents(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let env = self.run_bn(&["agent", "live"])?;
        Ok(CallToolResult::structured(
            env.get("data").cloned().unwrap_or(json!({})),
        ))
    }

    #[tool(
        name = "spin_up_work",
        description = "Spin up isolated work on any repo from anywhere: create a worktree off the base branch, seed its branch note with the goal, open a window in the repo's tmux session, and launch an agent (copilot default; claude also). Each call is fully isolated — its own worktree, tmux window, and note. Re-calling the same branch is idempotent (won't clobber a live note)."
    )]
    async fn spin_up_work(
        &self,
        Parameters(a): Parameters<SpinUpArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Build argv defensively against flag smuggling: option values use the attached `--k=v`
        // form (so a value starting with `-` is taken literally, never parsed as a flag), and the
        // free-text `goal` is placed after a `--` separator so it can never be read as an option —
        // even if the caller passes goal = "--no-tmux".
        let mut args: Vec<String> = vec!["dispatch".into(), format!("--repo={}", a.repo)];
        if let Some(b) = a.branch.as_deref().filter(|s| !s.is_empty()) {
            args.push(format!("--branch={b}"));
        }
        if let Some(b) = a.base.as_deref().filter(|s| !s.is_empty()) {
            args.push(format!("--base={b}"));
        }
        match a.agent.as_deref().map(str::trim) {
            Some("none") => { /* plain shell window: pass no --agent */ }
            Some(k) if !k.is_empty() => args.push(format!("--agent={k}")),
            _ => args.push("--agent=copilot".into()), // default
        }
        if a.scout {
            args.push("--scout".into());
        }
        if let Some(s) = a.skill.as_deref().filter(|s| !s.is_empty()) {
            args.push(format!("--skill={s}"));
        }
        if a.verify {
            args.push("--verify".into());
        }
        if a.yolo {
            args.push("--yolo".into());
        }
        args.push("--".into());
        args.push(a.goal.clone());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        // Declare the target as this dispatch's session so the CLI's cross-repo-session guard
        // treats it as sandbox's own session (spin_up_work is the sanctioned cross-repo path).
        let env = self.run_bn_env(&argv, &[("BN_WORKER_REPO", &a.repo)])?;
        Ok(CallToolResult::structured(
            env.get("data").cloned().unwrap_or(env),
        ))
    }

    #[tool(
        name = "send_to_agent",
        description = "Steer a running agent (repo:branch from list_agents) and get its reply — reliable, works even against a live session. Validated against the live fleet first. For a LONG task (implement + open a PR), set detach:true so it runs in the background and reports back via `bn agent done` — a blocking send would time out. For a quick question, leave detach false to get the reply."
    )]
    async fn send_to_agent(
        &self,
        Parameters(a): Parameters<SendArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Validate the target against the live fleet before sending (never send blind).
        let fleet = self.run_bn(&["agent", "live"])?;
        let live = fleet
            .get("data")
            .and_then(|d| d.get("agents"))
            .and_then(|a| a.as_array())
            .map(|agents| {
                agents.iter().any(|ag| {
                    ag.get("target").and_then(|t| t.as_str()) == Some(a.target.as_str())
                        && ag.get("has_window").and_then(|w| w.as_bool()) == Some(true)
                })
            })
            .unwrap_or(false);
        if !live {
            return Err(ErrorData::invalid_params(
                format!(
                    "no live agent window for target '{}' — check list_agents",
                    a.target
                ),
                None,
            ));
        }
        // `--` before the positionals so a target/msg starting with `-` can't smuggle a flag.
        let mut args: Vec<&str> = vec!["agent", "send"];
        if a.detach {
            args.push("--detach");
        }
        args.extend(["--", &a.target, &a.msg]);
        let env = self.run_bn(&args)?;
        Ok(CallToolResult::structured(
            env.get("data").cloned().unwrap_or(env),
        ))
    }

    #[tool(
        name = "plan_day",
        description = "The ranked cross-repo action queue (bn next): failing crons, attention items, PRs awaiting your review, your stale PRs, today's goals, and blockers — most-pressing first. The \"what's up / what to do next today\" read the buddy leads with."
    )]
    async fn plan_day(
        &self,
        Parameters(_a): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let env = self.run_bn(&["next"])?;
        Ok(CallToolResult::structured(
            env.get("data").cloned().unwrap_or(json!({})),
        ))
    }
}

#[tool_handler]
impl ServerHandler for BnOrchestratorServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "bn-orchestrator",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "bn orchestrator — the opt-in CROSS-WORKTREE surface (separate from the scoped \
                 per-worktree bn server). Manage the whole desk from anywhere: plan_day for the \
                 ranked next-action queue; list_attention for the attention list (each item tagged \
                 auto vs confirm — spin up the auto ones unattended, ask about the confirm ones); \
                 spin_up_work to create an isolated worktree+tmux window+seeded agent on any repo; \
                 list_agents for the live fleet (both harnesses) and send_to_agent to steer one. \
                 Every spun-up agent is fully isolated and keeps its own bn note."
                    .to_string(),
            )
    }
}
