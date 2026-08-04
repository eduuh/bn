//! Self-describing info agents (`agents/<name>.md`).
//!
//! One `info-gather` cron replaces the fleet of per-info timers: every 15 minutes it loads these
//! markdown agents and runs the ones that are **due** (their `on_calendar` fired since their last
//! successful run). Agents run headlessly, like every other cron — no tmux window is involved.
//! An agent can also declare `trigger: startup` so `bn startup` fires it once when the tmux server
//! boots. The gather itself still runs via `bn run <agent.name>`, so the `.md` is metadata +
//! schedule + description, never a reimplementation of the existing native/staff/script job.
//!
//! Parsing routes through [`crate::store::split_frontmatter`] (the crate's single frontmatter
//! parser) and the "due" check composes [`crate::staff_schedule::previous_occurrence`] over the
//! same `OnCalendar` subset the cron generator understands — no second schedule parser. Discovery
//! mirrors [`crate::skills::discover`] / [`crate::resolver::agent_dirs`]: branch → repo → global,
//! first tier wins by name.

use crate::events::CronRunDetail;
use crate::resolver::{self, Config, JobScope, Resolved};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// An explicit trigger declared in an agent's frontmatter. Cron eligibility is derived from the
/// presence of `on_calendar` rather than this list, so `startup` is the one trigger that adds
/// behaviour: fire once when the tmux server boots (in addition to any schedule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// Fire on the `on_calendar` schedule (the implicit default when a schedule is present).
    Cron,
    /// Fire once when the tmux server starts, via `bn startup`.
    Startup,
}

/// A `trigger:` field that accepts either a single value (`trigger: startup`) or a list
/// (`trigger: [cron, startup]`), so the manifest reads naturally either way.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum TriggerSpec {
    One(Trigger),
    Many(Vec<Trigger>),
}

impl TriggerSpec {
    fn into_vec(self) -> Vec<Trigger> {
        match self {
            TriggerSpec::One(t) => vec![t],
            TriggerSpec::Many(v) => v,
        }
    }
}

/// A roster / MCP name must stay filename-safe so frontmatter cannot escape config dirs.
pub(crate) fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A reusable skill path may contain `/`, but never an absolute path or `..`.
pub(crate) fn safe_skill(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && !s.starts_with('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
}

fn validate_agent_frontmatter(
    front: &AgentFrontmatter,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    if !safe_name(name) {
        return Err(format!(
            "{}: agent name '{}' must be alphanumeric/-/_",
            path.display(),
            name
        ));
    }
    if front.gather.as_deref().is_some_and(|g| g.trim().is_empty()) {
        return Err(format!(
            "{}: agent '{}': gather command is empty",
            path.display(),
            name
        ));
    }
    for s in &front.sinks {
        if !crate::staff::SINKS.contains(&s.as_str()) {
            return Err(format!(
                "{}: agent '{}': unknown sink '{}' (allowed: {})",
                path.display(),
                name,
                s,
                crate::staff::SINKS.join(" | ")
            ));
        }
    }
    for sk in &front.skills {
        if !safe_skill(sk) {
            return Err(format!(
                "{}: agent '{}': unsafe skill path '{}'",
                path.display(),
                name,
                sk
            ));
        }
    }
    for m in &front.mcps {
        if !safe_name(m) {
            return Err(format!(
                "{}: agent '{}': mcp name '{}' must be alphanumeric/-/_",
                path.display(),
                name,
                m
            ));
        }
    }
    Ok(())
}

/// Raw frontmatter as declared in `agents/<name>.md`, before normalization into [`AgentDef`].
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    #[serde(default)]
    on_calendar: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    network: bool,
    #[serde(default)]
    trigger: Option<TriggerSpec>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    gather: Option<String>,
    #[serde(default)]
    sinks: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcps: Vec<String>,
    #[serde(default)]
    link_domains: Vec<String>,
    #[serde(default)]
    harness: Option<String>,
    // --- specialist fields (a specialist is an agent with `repo:` set) ---
    /// Repo this specialist is gated to. Presence marks the agent as a review specialist that
    /// compiles to native Copilot/Claude custom agents rather than a cron/staff gather agent.
    #[serde(default)]
    repo: Option<String>,
    /// bn reference docs whose content is inlined into the compiled native agent body.
    #[serde(default)]
    references: Vec<String>,
    /// ADO repo the `bn agents bootstrap` mining run harvests review comments from.
    #[serde(default)]
    source_repo: Option<String>,
    /// Native worlds to compile into (`copilot`, `claude`). Empty = both.
    #[serde(default)]
    worlds: Vec<String>,
    /// Native tool aliases granted to the compiled Copilot agent
    /// (`read`/`edit`/`search`/`execute`/`agent`/`web`/`todo`, or `*`).
    #[serde(default)]
    tools: Vec<String>,
    /// Model override written into the compiled Copilot agent frontmatter.
    #[serde(default)]
    model: Option<String>,
}

/// A resolved agent definition: its metadata, the tier it was discovered in, and its body text.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// The `bn run <name>` job this agent gathers with.
    pub name: String,
    /// systemd `OnCalendar` schedule; present for a cron agent, absent for a startup-only one.
    pub on_calendar: Option<String>,
    /// The shell command this agent runs, headlessly, when due. Declared in frontmatter as `run:`;
    /// defaults to `bn run <name>` so a file that names an existing job still runs. This is the
    /// whole contract — drop a `.md` with a `run:` command and it is run on schedule.
    pub run: String,
    /// The gather hits the network (informational; the schedule generator carries the real gate).
    pub network: bool,
    /// Explicit triggers declared in frontmatter (only `startup` adds behaviour — see [`Trigger`]).
    pub triggers: Vec<Trigger>,
    /// One-line description of what this agent gathers.
    pub description: Option<String>,
    /// Deterministic packet command. Presence marks this as an LLM/staff agent.
    pub gather: Option<String>,
    /// Per-agent sink allowlist for routed briefing-card items.
    pub sinks: Vec<String>,
    /// Reusable bn skills available to this agent, referred to by name (loaded natively in its
    /// harness — not invoked as slash-command prompts).
    pub skills: Vec<String>,
    /// MCP servers/tools this agent connects to.
    pub mcps: Vec<String>,
    /// Extra card-link domains allowed for this agent.
    pub link_domains: Vec<String>,
    /// Headless harness override (`copilot` / `claude`).
    pub harness: Option<String>,
    /// Repo this specialist is gated to; `Some` marks the agent as a review specialist.
    pub repo: Option<String>,
    /// bn reference docs inlined into the compiled native agent.
    pub references: Vec<String>,
    /// ADO repo the `bn agents bootstrap` mining run harvests review comments from.
    pub source_repo: Option<String>,
    /// Native worlds this specialist compiles into (`copilot`/`claude`); empty = both.
    pub worlds: Vec<String>,
    /// Native tool aliases for the compiled Copilot agent.
    pub tools: Vec<String>,
    /// Model override for the compiled Copilot agent.
    pub model: Option<String>,
    /// Markdown body after the frontmatter fence — the prompt for gather agents.
    pub body: String,
    /// The tier this definition was discovered in (branch shadows repo shadows global).
    pub scope: JobScope,
    /// Absolute path to the source `.md` file.
    pub path: PathBuf,
}

impl AgentDef {
    /// True when this agent participates in the `info-gather` cron (it has a schedule).
    pub fn is_cron(&self) -> bool {
        self.on_calendar.is_some()
    }

    /// True when this agent should also fire once on tmux-server startup (`bn startup`).
    pub fn is_startup(&self) -> bool {
        self.triggers.contains(&Trigger::Startup)
    }

    /// True when this agent runs through the LLM staff pipeline.
    pub fn is_staff(&self) -> bool {
        self.gather.is_some()
    }

    /// True when this agent is a repo-scoped review specialist — an agent with `repo:` set that
    /// compiles to native Copilot/Claude custom agents (and is skipped by every cron/staff path).
    pub fn is_specialist(&self) -> bool {
        self.repo.is_some()
    }

    /// Whether this agent is DUE now: never run, last run failed, or a scheduled occurrence has
    /// passed since the last successful run. A startup-only agent (no `on_calendar`) is never due
    /// on the cron path. `last_run` is the agent's most recent recorded run of its `bn run` job.
    pub fn due(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        match self.on_calendar.as_deref() {
            Some(cal) => due(cal, last_run, now),
            None => false,
        }
    }

    /// Whether this agent may be launched *now* by the shared window/startup dispatch path. A
    /// **scheduled** agent (has `on_calendar`) is launchable only when its schedule is [`due`]
    /// (never-run / last-failed still due, per `due()`); an **unscheduled** startup-class agent
    /// (no `on_calendar`) is always launchable, so `bn startup` still fires it on every boot. This
    /// is the single gate that keeps a weekly agent from opening its window off-schedule
    /// regardless of which path (info-gather cron, `bn startup`, …) reaches the dispatcher.
    pub fn launchable(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        self.on_calendar.is_none() || self.due(last_run, now)
    }
}

/// Whether a scheduled occurrence of `on_calendar` has passed since the last successful run. Pure
/// (no I/O) so it is unit-testable: a never-run or last-FAILED agent is always due; otherwise it
/// is due when the most recent scheduled slot at or before `now` is newer than the last run. A
/// schedule with no enumerable slot (unmodeled shape) errs toward due, matching the degrade-loud
/// contract elsewhere — better a spurious refresh than a silently stalled agent.
pub fn due(on_calendar: &str, last_run: Option<&CronRunDetail>, now: i64) -> bool {
    match last_run {
        None => return true,
        Some(r) if r.exit_code != 0 => return true,
        Some(_) => {}
    }
    let last_ts = last_run.map(|r| r.ts).unwrap_or(0);
    match crate::staff_schedule::previous_occurrence(on_calendar, now) {
        Some(slot) => slot > last_ts,
        None => true,
    }
}

/// Parse one `agents/<name>.md`. `Ok(None)` when there is no frontmatter fence; errors are
/// configuration hard-failures (invalid YAML, unsafe names, unknown sinks). `name` falls back to
/// the file stem, mirroring [`crate::skills::parse_skill`].
pub fn parse_agent(
    text: &str,
    stem: &str,
    scope: JobScope,
    path: PathBuf,
) -> Result<Option<AgentDef>, String> {
    let Some((yaml, body)) = crate::store::split_frontmatter(text) else {
        return Ok(None);
    };
    let front: AgentFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| format!("{}: {e}", path.display()))?;
    let triggers = front
        .trigger
        .clone()
        .map(TriggerSpec::into_vec)
        .unwrap_or_default();
    let name = front.name.clone().unwrap_or_else(|| stem.to_string());
    validate_agent_frontmatter(&front, &name, &path)?;
    let run = front
        .run
        .clone()
        .unwrap_or_else(|| format!("bn run {name}"));
    Ok(Some(AgentDef {
        name,
        on_calendar: front.on_calendar,
        run,
        network: front.network,
        triggers,
        description: front.description,
        gather: front.gather,
        sinks: front.sinks,
        skills: front.skills,
        mcps: front.mcps,
        link_domains: front.link_domains,
        harness: front.harness,
        repo: front.repo,
        references: front.references,
        source_repo: front.source_repo,
        worlds: front.worlds,
        tools: front.tools,
        model: front.model,
        body: body.trim_start().to_string(),
        scope,
        path,
    }))
}

fn load_agent_files(dir: &Path, scope: JobScope) -> Vec<AgentDef> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
        match parse_agent(&text, stem, scope, path.clone()) {
            Ok(Some(def)) => out.push(def),
            Ok(None) => {}
            Err(e) => eprintln!("bn: skipping malformed agent {e}"),
        }
    }
    out
}

/// Every agent across all tiers for `ctx`, folded first-tier-wins by name (a branch `agents/`
/// shadows a repo or shipped default), then sorted by name for deterministic dispatch order.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for (scope, dir) in resolver::agent_dirs(cfg, ctx) {
        for def in load_agent_files(&dir, scope) {
            if out.iter().any(|a| a.name == def.name) {
                continue;
            }
            out.push(def);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}


