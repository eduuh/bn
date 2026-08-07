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

/// A malformed agent source, independent of where it was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentParseError {
    MissingFrontmatter,
    InvalidFrontmatter {
        message: String,
    },
    MissingKind,
    InvalidName {
        name: String,
    },
    EmptyField {
        field: &'static str,
    },
    RequiredField {
        kind: AgentKind,
        field: &'static str,
    },
    ForbiddenField {
        kind: AgentKind,
        field: &'static str,
    },
    UnknownSink {
        sink: String,
    },
    UnsafeSkill {
        skill: String,
    },
    UnsafeMcp {
        mcp: String,
    },
    InvalidRetryAfter {
        value: String,
    },
    InvalidSchedule {
        value: String,
    },
    RecursiveRun {
        name: String,
    },
    InvalidTemplate {
        source: TemplateError,
    },
}

impl fmt::Display for AgentParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFrontmatter => write!(f, "missing YAML frontmatter"),
            Self::InvalidFrontmatter { message } => write!(f, "invalid frontmatter: {message}"),
            Self::MissingKind => write!(
                f,
                "missing required agent kind (command | staff | hosted | specialist)"
            ),
            Self::InvalidName { name } => {
                write!(f, "agent name '{name}' must be alphanumeric/-/_")
            }
            Self::EmptyField { field } => write!(f, "field '{field}' must not be empty"),
            Self::RequiredField { kind, field } => {
                write!(f, "{} agent requires non-empty '{field}'", kind.label())
            }
            Self::ForbiddenField { kind, field } => {
                write!(f, "{} agent forbids '{field}'", kind.label())
            }
            Self::UnknownSink { sink } => write!(
                f,
                "unknown sink '{sink}' (allowed: {})",
                crate::staff::SINKS.join(" | ")
            ),
            Self::UnsafeSkill { skill } => write!(f, "unsafe skill path '{skill}'"),
            Self::UnsafeMcp { mcp } => {
                write!(f, "mcp name '{mcp}' must be alphanumeric/-/_")
            }
            Self::InvalidRetryAfter { value } => {
                write!(
                    f,
                    "invalid retry_after '{value}'; use a positive duration such as 15m"
                )
            }
            Self::InvalidSchedule { value } => {
                write!(f, "unsupported on_calendar schedule '{value}'")
            }
            Self::RecursiveRun { name } => {
                write!(
                    f,
                    "agent '{name}' explicitly recurses through 'bn run {name}'"
                )
            }
            Self::InvalidTemplate { source } => write!(f, "invalid template: {source}"),
        }
    }
}

impl std::error::Error for AgentParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidTemplate { source } => Some(source),
            _ => None,
        }
    }
}

/// A path-qualified agent catalog failure.
#[derive(Debug)]
pub enum AgentCatalogError {
    LegacyScheduleOverride {
        path: PathBuf,
        value: String,
    },
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidAgent {
        path: PathBuf,
        source: AgentParseError,
    },
}

impl AgentCatalogError {
    pub fn path(&self) -> &Path {
        match self {
            Self::LegacyScheduleOverride { path, .. }
            | Self::ReadDirectory { path, .. }
            | Self::ReadFile { path, .. }
            | Self::InvalidAgent { path, .. } => path,
        }
    }
}

impl fmt::Display for AgentCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyScheduleOverride { path, value } => write!(
                f,
                "{} still sets the legacy quarter-review schedule '{value}'; remove it and override config/agents/quarter-review.md in a higher-precedence agents/ tier",
                path.display()
            ),
            Self::ReadDirectory { path, source } => {
                write!(f, "read agent directory {}: {source}", path.display())
            }
            Self::ReadFile { path, source } => {
                write!(f, "read agent {}: {source}", path.display())
            }
            Self::InvalidAgent { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for AgentCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LegacyScheduleOverride { .. } => None,
            Self::ReadDirectory { source, .. } | Self::ReadFile { source, .. } => Some(source),
            Self::InvalidAgent { source, .. } => Some(source),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLookupError {
    pub name: String,
}

impl fmt::Display for AgentLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no agent named '{}'", self.name)
    }
}

impl std::error::Error for AgentLookupError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunError {
    Specialist { name: String },
}

impl fmt::Display for AgentRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Specialist { name } => {
                write!(f, "specialist agent '{name}' is installable, not runnable")
            }
        }
    }
}

impl std::error::Error for AgentRunError {}

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
    body: &str,
) -> Result<(AgentKind, Option<i64>), AgentParseError> {
    let kind = front.kind.ok_or(AgentParseError::MissingKind)?;
    if !safe_name(name) {
        return Err(AgentParseError::InvalidName {
            name: name.to_string(),
        });
    }
    if front
        .on_calendar
        .as_deref()
        .is_some_and(|schedule| schedule.trim().is_empty())
    {
        return Err(AgentParseError::EmptyField {
            field: "on_calendar",
        });
    }
    if let Some(schedule) = front.on_calendar.as_deref() {
        if !crate::staff_schedule::calendar_supported(schedule) {
            return Err(AgentParseError::InvalidSchedule {
                value: schedule.to_string(),
            });
        }
    }
    if front
        .run
        .as_deref()
        .is_some_and(|run| run.trim().is_empty())
    {
        return Err(AgentParseError::EmptyField { field: "run" });
    }
    if front.gather.as_deref().is_some_and(|g| g.trim().is_empty()) {
        return Err(AgentParseError::EmptyField { field: "gather" });
    }
    for s in &front.sinks {
        if !crate::staff::SINKS.contains(&s.as_str()) {
            return Err(AgentParseError::UnknownSink { sink: s.clone() });
        }
    }
    for sk in &front.skills {
        if !safe_skill(sk) {
            return Err(AgentParseError::UnsafeSkill { skill: sk.clone() });
        }
    }
    for m in &front.mcps {
        if !safe_name(m) {
            return Err(AgentParseError::UnsafeMcp { mcp: m.clone() });
        }
    }
    template::validate(body, &front.arguments)
        .map_err(|source| AgentParseError::InvalidTemplate { source })?;

    let require = |field| AgentParseError::RequiredField { kind, field };
    let forbid = |field| AgentParseError::ForbiddenField { kind, field };
    match kind {
        AgentKind::Command => {
            if front.gather.is_some() {
                return Err(forbid("gather"));
            }
            if front.repo.is_some() {
                return Err(forbid("repo"));
            }
        }
        AgentKind::Staff => {
            if body.trim().is_empty() {
                return Err(require("body"));
            }
            if front.gather.is_none() {
                return Err(require("gather"));
            }
            if front.sinks.is_empty() {
                return Err(require("sinks"));
            }
            if front.repo.is_some() {
                return Err(forbid("repo"));
            }
        }
        AgentKind::Hosted => {
            if body.trim().is_empty() {
                return Err(require("body"));
            }
            if front.gather.is_some() {
                return Err(forbid("gather"));
            }
            if front.repo.is_some() {
                return Err(forbid("repo"));
            }
        }
        AgentKind::Specialist => {
            if body.trim().is_empty() {
                return Err(require("body"));
            }
            if front
                .repo
                .as_deref()
                .map(str::trim)
                .filter(|repo| !repo.is_empty())
                .is_none()
            {
                return Err(require("repo"));
            }
            if front.on_calendar.is_some() {
                return Err(forbid("on_calendar"));
            }
            if front.trigger.is_some() {
                return Err(forbid("trigger"));
            }
            if front.gather.is_some() {
                return Err(forbid("gather"));
            }
            if front.run.is_some() {
                return Err(forbid("run"));
            }
        }
    }

    if front
        .run
        .as_deref()
        .is_some_and(|run| explicit_recursive_run(run, name))
    {
        return Err(AgentParseError::RecursiveRun {
            name: name.to_string(),
        });
    }

    let retry_after_secs = match front.retry_after.as_deref() {
        Some(value) => Some(
            crate::focus::parse_duration(value)
                .filter(|seconds| *seconds > 0)
                .ok_or_else(|| AgentParseError::InvalidRetryAfter {
                    value: value.to_string(),
                })?,
        ),
        None if front.on_calendar.is_some() => Some(DEFAULT_RETRY_AFTER_SECS),
        None => None,
    };
    Ok((kind, retry_after_secs))
}

fn explicit_recursive_run(run: &str, name: &str) -> bool {
    let mut parts = run.split_whitespace();
    parts.next() == Some("bn")
        && parts.next() == Some("run")
        && parts.next() == Some(name)
        && parts.next().is_none()
}

/// Raw frontmatter as declared in `agents/<name>.md`, before normalization into [`AgentDef`].
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    #[serde(default)]
    kind: Option<AgentKind>,
    #[serde(default)]
    on_calendar: Option<String>,
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
        retry_after_secs,
        run: front.run,
        network: front.network,
        queue: front.queue,
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
        if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            paths.push(path);
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
