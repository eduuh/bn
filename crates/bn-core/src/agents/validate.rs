//! Whether a manifest is a valid agent — the refusals, and the rules that produce them.
//!
//! Its own module because "what may an agent declare" is a contract read by people *writing*
//! manifests, not by the scheduler that runs them, and it gains a rule every time an agent gains a
//! capability. The read side next door stays about discovery and precedence.
//!
//! Every rule here refuses at *load* time, so a manifest that would misbehave never enters the
//! catalog and nothing downstream has to defend against one.
use super::*;

/// The closed sink set. Config (`sinks = [...]`) may only allowlist these; a card item routes to
/// exactly one of them via [`kind_sink`]. This is the privacy guard: no public sink exists, so
/// none can be configured.
pub const SINKS: &[&str] = &["attn", "daily", "note", "dojo", "research", "doc", "learn"];

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
    UnsafeDocDir {
        dir: String,
    },
    UnknownSeverity {
        severity: String,
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
                SINKS.join(" | ")
            ),
            Self::UnsafeSkill { skill } => write!(f, "unsafe skill path '{skill}'"),
            Self::UnsafeMcp { mcp } => {
                write!(f, "mcp name '{mcp}' must be alphanumeric/-/_")
            }
            Self::UnknownSeverity { severity } => write!(
                f,
                "max_severity '{severity}' is not a severity (want {})",
                crate::attention::SEVERITIES.join(" | ")
            ),
            Self::UnsafeDocDir { dir } => write!(
                f,
                "doc_dir '{dir}' must be a relative path of plain segments under the notes directory"
            ),
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
    NonMarkdownAgent {
        path: PathBuf,
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
            | Self::NonMarkdownAgent { path }
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
            Self::NonMarkdownAgent { path } => write!(
                f,
                "{} is in an agents/ directory but is not a Markdown .md manifest",
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
            Self::LegacyScheduleOverride { .. } | Self::NonMarkdownAgent { .. } => None,
            Self::ReadDirectory { source, .. } | Self::ReadFile { source, .. } => Some(source),
            Self::InvalidAgent { source, .. } => Some(source),
        }
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

/// A doc directory is one or more plain path segments under the machine's notes directory — never
/// absolute, never climbing out with `..`. It is the boundary an agent's file output cannot cross.
pub(crate) fn safe_doc_dir(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && s.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        })
}

/// A reusable skill path may contain `/`, but never an absolute path or `..`.
pub(crate) fn safe_skill(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && !s.starts_with('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
}

pub(super) fn validate_agent_frontmatter(
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
        if !crate::schedule::calendar_supported(schedule) {
            return Err(AgentParseError::InvalidSchedule {
                value: schedule.to_string(),
            });
        }
        if front
            .requires_repo
            .as_deref()
            .map(str::trim)
            .filter(|repo| !repo.is_empty())
            .is_none()
        {
            return Err(AgentParseError::RequiredField {
                kind,
                field: "requires_repo",
            });
        }
    }
    if front
        .requires_repo
        .as_deref()
        .is_some_and(|repo| repo.trim().is_empty())
    {
        return Err(AgentParseError::EmptyField {
            field: "requires_repo",
        });
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
    if let Some(max) = front.max_severity.as_deref().map(str::trim) {
        if !crate::attention::SEVERITIES.contains(&max) {
            return Err(AgentParseError::UnknownSeverity {
                severity: max.to_string(),
            });
        }
    }
    for s in &front.sinks {
        if !SINKS.contains(&s.as_str()) {
            return Err(AgentParseError::UnknownSink { sink: s.clone() });
        }
    }
    // The doc sink writes files, so its directory is not optional and is not a card's to choose.
    let writes_docs = front.sinks.iter().any(|s| s == "doc");
    match front.doc_dir.as_deref().map(str::trim) {
        Some(dir) if !writes_docs => {
            let _ = dir;
            return Err(AgentParseError::ForbiddenField {
                kind,
                field: "doc_dir",
            });
        }
        Some(dir) if !safe_doc_dir(dir) => {
            return Err(AgentParseError::UnsafeDocDir {
                dir: dir.to_string(),
            })
        }
        None if writes_docs => {
            return Err(AgentParseError::RequiredField {
                kind,
                field: "doc_dir",
            })
        }
        _ => {}
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
