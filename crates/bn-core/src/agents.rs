//! Self-describing info agents (`agents/<name>.md`).
//!
//! One minute-level `agent-scheduler` loads these Markdown agents and runs the ones that are due.
//! Successful runs wait for the next occurrence; failures respect per-agent retry backoff.
//! Agents run headlessly, like every other cron — no tmux window is involved.
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
use crate::template::{self, TemplateError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};

pub use crate::template::ArgumentDef as AgentArgDef;

/// Authoring the roster — create / save / delete an `agents/<name>.md`, each validated through
/// [`parse_agent`] first. Its own file because the read side above is what the scheduler, the
/// dispatcher and every roster view use, and none of them writes.
pub mod write;

/// What a manifest may declare, and every refusal for one that declares something else.
mod validate;
pub use validate::*;

pub const DEFAULT_RETRY_AFTER_SECS: i64 = 15 * 60;

/// The explicit execution contract for an agent manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Command,
    Staff,
    Hosted,
    Specialist,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Staff => "staff",
            Self::Hosted => "hosted",
            Self::Specialist => "specialist",
        }
    }
}

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

/// Raw frontmatter as declared in `agents/<name>.md`, before normalization into [`AgentDef`].
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    #[serde(default)]
    kind: Option<AgentKind>,
    #[serde(default)]
    on_calendar: Option<String>,
    /// Also fire on the day's first bn event (`bn run <name> --if-stale`), not only on the
    /// `on_calendar` timer — the agent-manifest twin of `[[cron]] catch_up` (see crons.rs), for
    /// refreshes tied to *when you start work* (meetings, the m365 sweep).
    #[serde(default)]
    catch_up: bool,
    #[serde(default)]
    retry_after: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    network: bool,
    /// `queue: false` opts this agent out of the work queue; absent means yes.
    #[serde(default = "yes")]
    queue: bool,
    #[serde(default)]
    trigger: Option<TriggerSpec>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    gather: Option<String>,
    #[serde(default)]
    sinks: Vec<String>,
    /// This agent re-derives its whole current set of attention items every run, so a keyed item
    /// it stops reporting is resolved rather than merely stale. See [`AgentDef::replace`].
    #[serde(default)]
    replace: bool,
    /// This agent stays off until it is named in `[agents] enabled`. See [`AgentDef::opt_in`].
    #[serde(default)]
    opt_in: bool,
    /// The notes-repo directory this agent's `doc` items land in. See [`AgentDef::doc_dir`].
    #[serde(default)]
    doc_dir: Option<String>,
    /// The highest severity this agent's items may claim. See [`AgentDef::max_severity`].
    #[serde(default)]
    max_severity: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcps: Vec<String>,
    #[serde(default)]
    link_domains: Vec<String>,
    #[serde(default)]
    harness: Option<String>,
    /// Repository that must be assigned to this machine and locally available before a scheduled
    /// or manually invoked agent may run.
    #[serde(default)]
    requires_repo: Option<String>,
    /// Synthetic steering address used by a hosted agent that does not belong to a checkout.
    #[serde(default)]
    host_repo: Option<String>,
    /// Tmux window name used by a hosted agent.
    #[serde(default)]
    host_window: Option<String>,
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
    #[serde(default)]
    arguments: Vec<AgentArgDef>,
}

/// A resolved agent definition: its metadata, the tier it was discovered in, and its body text.
/// Serde default for opt-in-by-absence booleans.
fn yes() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct AgentDef {
    /// The `bn run <name>` job this agent gathers with.
    pub name: String,
    /// Explicit execution contract declared by `kind:`.
    pub kind: AgentKind,
    /// systemd `OnCalendar` schedule; present for a cron agent, absent for a startup-only one.
    pub on_calendar: Option<String>,
    /// Also fire on the day's first bn event, not only on the schedule (`catch_up:` frontmatter).
    pub catch_up: bool,
    /// Failure retry backoff in seconds. Scheduled agents default to 15 minutes.
    pub retry_after_secs: Option<i64>,
    /// An explicit custom shell command from `run:`. When absent, `bn run <name>` resolves the
    /// same-name staff, native, or script job without spawning a recursive child process.
    pub run: Option<String>,
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
    /// The per-machine notes-repo directory this agent's `doc` items are written into, relative to
    /// `<personal_root>/<machine>/` — `chronicle`, `learn`, and so on.
    ///
    /// Declared in the manifest, and *only* there: a card names the file, the manifest names the
    /// directory, so an agent's output cannot escape the archive it was given no matter what its
    /// harness returns. Required when `doc` is on the sink allowlist, and forbidden otherwise.
    pub doc_dir: Option<String>,
    /// The highest severity this agent's items may claim; anything above it is clamped down.
    ///
    /// Some agents summarize rather than discover — a briefing that folds seven sources into one
    /// card is not the thing that found the problem, and urgency belongs to whatever did. Stating
    /// that in prose does not hold: the output contract offers `urgent` to every agent, and a
    /// model handed a genuinely alarming packet will reach for it. So it is declared, and the
    /// item is clamped rather than dropped — a summarizer that over-reached still has something
    /// worth reading.
    pub max_severity: Option<String>,
    /// Whether this agent stays switched off until `[agents] enabled` names it.
    ///
    /// Declared by agents that do something consequential unattended — spend model tokens, write
    /// into the notes repo, open a session. A person should choose those deliberately, and a
    /// scheduled run that finds itself off records as *off* rather than as a miss.
    ///
    /// It lives in the manifest because it is a property of what the agent does, not of one
    /// machine's configuration: adding an agent that reviews your PRs should not also require
    /// remembering to gate it.
    pub opt_in: bool,
    /// Whether this agent's card states its **whole current set** of keyed attention items, so
    /// anything it stops reporting is resolved rather than left standing.
    ///
    /// True for an agent that re-derives its findings from the world every run (an estate patrol,
    /// a queue poll); false for one that reports events as they occur, where silence about an item
    /// means nothing. It changes the output contract the agent is given as well as how its card is
    /// routed, so it is declared once here rather than guessed per run.
    pub replace: bool,
    /// Reusable bn skills available to this agent, referred to by name (loaded natively in its
    /// harness — not invoked as slash-command prompts).
    pub skills: Vec<String>,
    /// MCP servers/tools this agent connects to.
    pub mcps: Vec<String>,
    /// Extra card-link domains allowed for this agent.
    pub link_domains: Vec<String>,
    /// Headless harness override (`copilot` / `claude`).
    pub harness: Option<String>,
    /// Repository required before this agent may run.
    pub requires_repo: Option<String>,
    /// Synthetic steering address for a hosted agent.
    pub host_repo: Option<String>,
    /// Tmux window name for a hosted agent.
    pub host_window: Option<String>,
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
    /// Declared named arguments used by the Markdown body.
    pub arguments: Vec<AgentArgDef>,
    /// Whether this agent goes through the work queue when `[queue].capture` includes `cron`.
    ///
    /// Defaults to true, so capturing is a single decision rather than one to re-make per agent;
    /// `queue: false` in frontmatter is the opt-out for anything that genuinely must run inline
    /// (a gate something else blocks on, a probe whose value is its immediacy).
    pub queue: bool,
    /// Markdown body after the frontmatter fence — the prompt for gather agents.
    pub body: String,
    /// The tier this definition was discovered in (branch shadows repo shadows global).
    pub scope: JobScope,
    /// Absolute path to the source `.md` file.
    pub path: PathBuf,
}

impl AgentDef {
    /// True when this agent participates in the common scheduler (it has a schedule).
    pub fn is_cron(&self) -> bool {
        self.on_calendar.is_some()
    }

    /// Whether this agent's declared repository is assigned to this machine and present locally.
    pub fn repo_available_here(&self, cfg: &Config) -> bool {
        self.requires_repo
            .as_deref()
            .is_none_or(|repo| cfg.repo_available_here(repo))
    }

    /// Whether this agent may run here, given the machine's `[agents] enabled` list.
    ///
    /// An agent that is not opt-in always may. One that is must be named — matched
    /// case-insensitively, since the list is hand-written config and `Staff-Mentor` plainly means
    /// the same agent.
    pub fn switched_on(&self, enabled: &[String]) -> bool {
        !self.opt_in
            || enabled
                .iter()
                .any(|name| name.trim().eq_ignore_ascii_case(&self.name))
    }

    /// Why this agent will do nothing right now, phrased for the person who has to fix it.
    /// `None` when it may run.
    pub fn off_reason(&self, enabled: &[String]) -> Option<String> {
        (!self.switched_on(enabled)).then(|| {
            format!(
                "not switched on — add '{}' to [agents] enabled to opt in",
                self.name
            )
        })
    }

    /// True when this agent should also fire once on tmux-server startup (`bn startup`).
    pub fn is_startup(&self) -> bool {
        self.triggers.contains(&Trigger::Startup)
    }

    /// True when this agent runs through the LLM staff pipeline.
    pub fn is_staff(&self) -> bool {
        self.kind == AgentKind::Staff
    }

    /// True when this agent is a repo-scoped review specialist — an agent with `repo:` set that
    /// compiles to native Copilot/Claude custom agents (and is skipped by every cron/staff path).
    pub fn is_specialist(&self) -> bool {
        self.kind == AgentKind::Specialist
    }

    /// Reject definitions that are catalog-only and cannot cross the execution boundary.
    pub fn ensure_runnable(&self) -> Result<(), AgentRunError> {
        if self.is_specialist() {
            Err(AgentRunError::Specialist {
                name: self.name.clone(),
            })
        } else {
            Ok(())
        }
    }

    /// Strictly render this agent's Markdown body with its declared named arguments.
    pub fn render(&self, args: Option<&Map<String, Value>>) -> Result<String, TemplateError> {
        template::render_checked(&self.body, &self.arguments, args)
    }

    /// Whether this agent is due now. A never-run agent catches up the latest missed occurrence; a
    /// successful run waits for a newer occurrence; a failed run retries only after `retry_after`.
    /// A startup-only agent (no `on_calendar`) is never due on the scheduler path.
    pub fn due(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        match self.on_calendar.as_deref() {
            Some(cal) => due(
                cal,
                last_run,
                now,
                self.retry_after_secs.unwrap_or(DEFAULT_RETRY_AFTER_SECS),
            ),
            None => false,
        }
    }

    /// Next permitted retry instant after a failed scheduled attempt.
    pub fn retry_at(&self, last_run: Option<&CronRunDetail>) -> Option<i64> {
        let run = last_run.filter(|run| run.exit_code != 0)?;
        self.on_calendar.as_ref()?;
        Some(
            run.ts
                .saturating_add(self.retry_after_secs.unwrap_or(DEFAULT_RETRY_AFTER_SECS)),
        )
    }

    /// Whether this agent may be launched *now* by the shared window/startup dispatch path. A
    /// **scheduled** agent (has `on_calendar`) is launchable only when its schedule is [`due`];
    /// an **unscheduled** startup-class agent
    /// (no `on_calendar`) is always launchable, so `bn startup` still fires it on every boot. This
    /// is the single gate that keeps a weekly agent from opening its window off-schedule
    /// regardless of which path (`agent-scheduler`, `bn startup`, …) reaches the dispatcher.
    pub fn launchable(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        self.on_calendar.is_none() || self.due(last_run, now)
    }
}

/// Last-attempt-aware due logic for the common scheduler.
pub fn due(
    on_calendar: &str,
    last_run: Option<&CronRunDetail>,
    now: i64,
    retry_after_secs: i64,
) -> bool {
    let Some(slot) = crate::staff_schedule::previous_occurrence(on_calendar, now) else {
        return false;
    };
    match last_run {
        None => return true,
        Some(run) if run.exit_code != 0 => {
            return now >= run.ts.saturating_add(retry_after_secs.max(1));
        }
        Some(_) => {}
    }
    slot > last_run.map(|run| run.ts).unwrap_or(0)
}

/// Parse one `agents/<name>.md`. `name` falls back to the file stem.
pub fn parse_agent(
    text: &str,
    stem: &str,
    scope: JobScope,
    path: PathBuf,
) -> Result<AgentDef, AgentCatalogError> {
    let (yaml, body) =
        crate::store::split_frontmatter(text).ok_or_else(|| AgentCatalogError::InvalidAgent {
            path: path.clone(),
            source: AgentParseError::MissingFrontmatter,
        })?;
    let front: AgentFrontmatter =
        serde_yaml::from_str(yaml).map_err(|error| AgentCatalogError::InvalidAgent {
            path: path.clone(),
            source: AgentParseError::InvalidFrontmatter {
                message: error.to_string(),
            },
        })?;
    let triggers = front
        .trigger
        .clone()
        .map(TriggerSpec::into_vec)
        .unwrap_or_default();
    let name = front.name.clone().unwrap_or_else(|| stem.to_string());
    let (kind, retry_after_secs) =
        validate_agent_frontmatter(&front, &name, body).map_err(|source| {
            AgentCatalogError::InvalidAgent {
                path: path.clone(),
                source,
            }
        })?;
    Ok(AgentDef {
        name,
        kind,
        on_calendar: front.on_calendar,
        catch_up: front.catch_up,
        retry_after_secs,
        run: front.run,
        network: front.network,
        queue: front.queue,
        triggers,
        description: front.description,
        gather: front.gather,
        sinks: front.sinks,
        replace: front.replace,
        opt_in: front.opt_in,
        doc_dir: front.doc_dir,
        max_severity: front.max_severity,
        skills: front.skills,
        mcps: front.mcps,
        link_domains: front.link_domains,
        harness: front.harness,
        requires_repo: front.requires_repo,
        host_repo: front.host_repo,
        host_window: front.host_window,
        repo: front.repo,
        references: front.references,
        source_repo: front.source_repo,
        worlds: front.worlds,
        tools: front.tools,
        model: front.model,
        arguments: front.arguments,
        body: body.trim_start().to_string(),
        scope,
        path,
    })
}

fn load_agent_files(dir: &Path, scope: JobScope) -> Result<Vec<AgentDef>, AgentCatalogError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AgentCatalogError::ReadDirectory {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| AgentCatalogError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|source| AgentCatalogError::ReadDirectory {
                path: dir.to_path_buf(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            paths.push(path);
        } else {
            return Err(AgentCatalogError::NonMarkdownAgent { path });
        }
    }
    paths.sort();

    let mut agents = Vec::with_capacity(paths.len());
    for path in paths {
        let text =
            std::fs::read_to_string(&path).map_err(|source| AgentCatalogError::ReadFile {
                path: path.clone(),
                source,
            })?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent")
            .to_string();
        agents.push(parse_agent(&text, &stem, scope, path)?);
    }
    Ok(agents)
}

/// Rewrite one agent's `on_calendar:` frontmatter line in its source `.md` (the tier it was
/// discovered from). `Some(expr)` sets or replaces the schedule after validating the systemd
/// `OnCalendar` spec parses; `None` removes it (the agent becomes trigger/manual-only). The
/// agent-scheduler reads the catalog every tick, so the change takes effect on the next pass —
/// no re-registration needed. Returns the file rewritten.
pub fn set_schedule(
    cfg: &Config,
    ctx: Option<&Resolved>,
    agent: &str,
    on_calendar: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(expr) = on_calendar {
        if crate::staff_schedule::Schedule::parse(expr).is_none() {
            return Err(format!(
                "'{expr}' is not a recognized OnCalendar spec (e.g. \"Mon..Fri *-*-* 14:50:00\")"
            ));
        }
    }
    let catalog = discover(cfg, ctx).map_err(|e| e.to_string())?;
    let def = catalog
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("no agent named '{agent}'"))?;
    if on_calendar.is_some() && def.requires_repo.is_none() {
        return Err(format!(
            "agent '{agent}' must declare requires_repo before it can be scheduled"
        ));
    }
    let text = std::fs::read_to_string(&def.path).map_err(|e| e.to_string())?;
    // The frontmatter is the leading `---` fence block; operate line-wise inside it only, so a
    // body that mentions `on_calendar:` is never touched.
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
    else {
        return Err(format!("{}: no frontmatter fence", def.path.display()));
    };
    let existing = lines[1..end]
        .iter()
        .position(|l| l.trim_start().starts_with("on_calendar:"))
        .map(|i| i + 1);
    match (existing, on_calendar) {
        (Some(i), Some(expr)) => lines[i] = format!("on_calendar: \"{expr}\""),
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, Some(expr)) => lines.insert(2.min(end), format!("on_calendar: \"{expr}\"")),
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    crate::store::write_atomic(&def.path, out.as_bytes()).map_err(|e| e.to_string())?;
    Ok(def.path.clone())
}

/// The validated, precedence-folded agent roster.
#[derive(Debug, Clone, Default)]
pub struct AgentCatalog {
    agents: Vec<AgentDef>,
}

impl AgentCatalog {
    pub fn iter(&self) -> impl Iterator<Item = &AgentDef> {
        self.agents.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    pub fn find(&self, name: &str) -> Option<&AgentDef> {
        self.agents.iter().find(|agent| agent.name == name)
    }

    pub fn require(&self, name: &str) -> Result<&AgentDef, AgentLookupError> {
        self.find(name).ok_or_else(|| AgentLookupError {
            name: name.to_string(),
        })
    }
}

impl IntoIterator for AgentCatalog {
    type Item = AgentDef;
    type IntoIter = std::vec::IntoIter<AgentDef>;

    fn into_iter(self) -> Self::IntoIter {
        self.agents.into_iter()
    }
}

impl<'a> IntoIterator for &'a AgentCatalog {
    type Item = &'a AgentDef;
    type IntoIter = std::slice::Iter<'a, AgentDef>;

    fn into_iter(self) -> Self::IntoIter {
        self.agents.iter()
    }
}

/// Every agent across all tiers for `ctx`, folded first-tier-wins by name (a branch `agents/`
/// shadows a repo or shipped default), then sorted by name for deterministic dispatch order.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Result<AgentCatalog, AgentCatalogError> {
    if let Some((path, value)) = resolver::quarter_review_cron_override() {
        return Err(AgentCatalogError::LegacyScheduleOverride { path, value });
    }
    discover_in(&resolver::agent_dirs(cfg, ctx))
}

fn discover_in(tiers: &[(JobScope, PathBuf)]) -> Result<AgentCatalog, AgentCatalogError> {
    let mut agents = Vec::new();
    for (scope, dir) in tiers {
        for definition in load_agent_files(dir, *scope)? {
            if agents
                .iter()
                .any(|agent: &AgentDef| agent.name == definition.name)
            {
                continue;
            }
            agents.push(definition);
        }
    }
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(AgentCatalog { agents })
}

#[cfg(test)]
#[path = "agents/tests.rs"]
mod tests;
