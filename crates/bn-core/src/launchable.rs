//! The merged "what can I launch" catalog — Markdown agents ([`crate::agents`]) and workflow
//! skills ([`crate::skills`]) folded into one addressable roster.
//!
//! bn grew two catalogs for the same question. An agent manifest (`config/agents/<name>.md`)
//! declares an execution contract and is reached by cron; a skill (`config/prompts/<name>.md`)
//! declares a process and is reached by `bn dispatch --skill`. Both are a Markdown body that
//! seeds a harness, and from the launcher's side the distinction is bookkeeping — so
//! `bn agent run` addresses them through one list rather than making a person remember which
//! catalog a name lives in.
//!
//! Each source keeps its own discovery and its own precedence rules (both are already
//! first-tier-wins by name); this only merges the results and tags where each row came from.
//! A name held by *both* resolves to the agent, and the skill it shadows is marked rather than
//! dropped — [`find`] picks the winner, and a caller that wants to say so can see the loser.

use crate::agents::{self, AgentDef, AgentKind};
use crate::resolver::{Config, Resolved};
use crate::skills;
use crate::template::ArgumentDef;
use std::path::{Path, PathBuf};

/// Which catalog a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Agent,
    Skill,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Skill => "skill",
        }
    }

    /// Parse a `--pick-source` label back into a source, so the CLI can round-trip the filter
    /// through an fzf callback without bn-core taking a clap dependency.
    pub fn parse(s: &str) -> Option<Source> {
        match s.trim().to_ascii_lowercase().as_str() {
            "agent" | "agents" => Some(Self::Agent),
            "skill" | "skills" => Some(Self::Skill),
            _ => None,
        }
    }

    /// Plural noun for a "no matches" line or a picker header.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Agent => "agents",
            Self::Skill => "skills",
        }
    }
}

/// One launchable entry, flattened across both catalogs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Launchable {
    pub name: String,
    pub source: Source,
    /// The agent's declared execution contract; `None` for a skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<AgentKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tier label — `JobScope`'s for an agent, `SkillScope`'s for a skill.
    pub scope: String,
    /// `on_calendar` for a scheduled agent; `None` means on demand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Every declared `{{name}}` argument, required or not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<ArgumentDef>,
    /// Skills this entry declares it works through (agents only — `skills:` frontmatter).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Harness the manifest asks for, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Repository this entry requires before it can run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_repo: Option<String>,
    /// True for a skill whose name is also an agent's: [`find`] resolves the name to the agent.
    pub shadowed: bool,
    pub path: PathBuf,
}

impl Launchable {
    /// The arguments that must be supplied before this entry can render.
    pub fn required_args(&self) -> Vec<&str> {
        self.arguments
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name.as_str())
            .collect()
    }

    /// Whether a launch needs `--arg` values before it can go anywhere.
    pub fn needs_args(&self) -> bool {
        self.arguments.iter().any(|a| a.required)
    }

    /// What to show in a schedule column: the calendar, or why there isn't one.
    pub fn schedule_label(&self) -> &str {
        self.schedule.as_deref().unwrap_or("on demand")
    }
}

/// Every launchable entry for `ctx`/`base_dir`, agents first then skills, each half sorted by
/// name.
///
/// Discovery failures **degrade to an empty half rather than failing the call**: a broken skill
/// file should not make every agent unlaunchable, and vice versa. `bn agents validate` and
/// `bn skills list` are where a malformed catalog is diagnosed; this is a launcher, and its job
/// is to still offer what does load. Callers that need the error use the underlying discovery.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>, base_dir: &Path) -> Vec<Launchable> {
    let agents: Vec<Launchable> = agents::discover(cfg, ctx)
        .map(|catalog| catalog.iter().map(from_agent).collect())
        .unwrap_or_default();

    let taken: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    let mut skills: Vec<Launchable> = skills::discover(cfg, base_dir)
        .map(|infos| {
            infos
                .iter()
                .filter(|info| info.effective)
                .map(|info| from_skill(info, taken.contains(&info.name.as_str())))
                .collect()
        })
        .unwrap_or_default();
    skills.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = agents;
    out.extend(skills);
    out
}

fn from_agent(def: &AgentDef) -> Launchable {
    Launchable {
        name: def.name.clone(),
        source: Source::Agent,
        kind: Some(def.kind),
        description: def.description.clone(),
        scope: def.scope.label().to_string(),
        schedule: def.on_calendar.clone(),
        arguments: def.arguments.clone(),
        skills: def.skills.clone(),
        harness: def.harness.clone(),
        requires_repo: def.requires_repo.clone(),
        shadowed: false,
        path: def.path.clone(),
    }
}

fn from_skill(info: &skills::SkillInfo, shadowed: bool) -> Launchable {
    Launchable {
        name: info.name.clone(),
        source: Source::Skill,
        kind: None,
        // A skill's `when:` line is the better one-liner when it has one: it says when to reach
        // for the skill, which is what a picker row is for.
        description: Some(
            info.when
                .clone()
                .unwrap_or_else(|| info.description.clone()),
        ),
        scope: info.scope.label().to_string(),
        schedule: None,
        arguments: info.arguments.clone(),
        skills: Vec::new(),
        harness: None,
        requires_repo: None,
        shadowed,
        path: PathBuf::from(&info.path),
    }
}

/// Resolve `name` to the entry that wins it: the agent when both catalogs hold the name.
///
/// Relies on [`discover`] ordering agents ahead of skills, which is also what makes the
/// `shadowed` flag consistent with what this returns.
pub fn find<'a>(rows: &'a [Launchable], name: &str) -> Option<&'a Launchable> {
    rows.iter().find(|row| row.name == name)
}

/// The skill `name` shadows, when an agent took the name from one.
pub fn shadowed_skill<'a>(rows: &'a [Launchable], name: &str) -> Option<&'a Launchable> {
    rows.iter()
        .find(|row| row.name == name && row.source == Source::Skill && row.shadowed)
}

/// How many leading characters two names must share before one is worth suggesting for the other.
const SHARED_PREFIX: usize = 3;

/// Length of the common leading run of `a` and `b`.
fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Names close enough to `query` to be worth suggesting after a miss, best first.
///
/// Deliberately cheap — prefix, substring and a shared leading run — rather than an edit
/// distance. A person who typed a name that does not exist has usually typed *part* of a real
/// one, and "did you mean" only earns its place when it is obviously right.
///
/// The shared-run rule is what catches the common typo shapes a pure substring test misses:
/// `plann` is neither a prefix nor a substring of `plan`, but they agree for four characters.
/// Returns at most `limit`.
pub fn near_misses<'a>(rows: &'a [Launchable], query: &str, limit: usize) -> Vec<&'a Launchable> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u8, usize, &Launchable)> = rows
        .iter()
        .filter_map(|row| {
            let name = row.name.to_ascii_lowercase();
            let shared = shared_prefix_len(&name, &q);
            // Rank: a prefix match beats a substring match beats a shared leading run beats a
            // shared word. Longer agreement wins inside a rank.
            let rank = if name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else if shared >= SHARED_PREFIX {
                2
            } else if q
                .split(['-', '_'])
                .any(|part| part.len() >= SHARED_PREFIX && name.contains(part))
            {
                3
            } else {
                return None;
            };
            Some((rank, shared, row))
        })
        .collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.cmp(&a.1))
            .then(a.2.name.cmp(&b.2.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, row)| row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, source: Source, shadowed: bool) -> Launchable {
        Launchable {
            name: name.to_string(),
            source,
            kind: None,
            description: None,
            scope: "global-shared".to_string(),
            schedule: None,
            arguments: Vec::new(),
            skills: Vec::new(),
            harness: None,
            requires_repo: None,
            shadowed,
            path: PathBuf::new(),
        }
    }

    #[test]
    fn an_agent_wins_a_name_a_skill_also_holds() {
        let rows = vec![
            row("scout", Source::Agent, false),
            row("scout", Source::Skill, true),
        ];
        assert_eq!(find(&rows, "scout").map(|r| r.source), Some(Source::Agent));
        assert_eq!(
            shadowed_skill(&rows, "scout").map(|r| r.source),
            Some(Source::Skill)
        );
    }

    #[test]
    fn an_unshadowed_skill_is_not_reported_as_shadowed() {
        let rows = vec![row("debug", Source::Skill, false)];
        assert_eq!(find(&rows, "debug").map(|r| r.source), Some(Source::Skill));
        assert!(shadowed_skill(&rows, "debug").is_none());
    }

    #[test]
    fn near_misses_prefer_a_prefix_then_a_substring() {
        let rows = vec![
            row("plan-work", Source::Skill, false),
            row("lavish-plan", Source::Skill, false),
            row("plan", Source::Skill, false),
        ];
        let hits: Vec<&str> = near_misses(&rows, "plan", 5)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(hits, vec!["plan", "plan-work", "lavish-plan"]);
    }

    #[test]
    fn a_typo_with_an_extra_character_still_suggests_the_real_name() {
        // The shape a pure substring test misses: neither is a substring of the other.
        let rows = vec![
            row("plan", Source::Skill, false),
            row("stow", Source::Skill, false),
        ];
        let hits: Vec<&str> = near_misses(&rows, "plann", 5)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(hits, vec!["plan"]);
    }

    #[test]
    fn an_unrelated_name_is_not_suggested() {
        let rows = vec![row("stow", Source::Skill, false)];
        assert!(near_misses(&rows, "plann", 5).is_empty());
    }

    #[test]
    fn an_empty_query_suggests_nothing() {
        let rows = vec![row("plan", Source::Skill, false)];
        assert!(near_misses(&rows, "   ", 5).is_empty());
    }

    #[test]
    fn required_arguments_are_what_gates_a_launch() {
        let mut r = row("today-agent", Source::Agent, false);
        r.arguments = vec![
            ArgumentDef {
                name: "date".into(),
                description: None,
                required: true,
            },
            ArgumentDef {
                name: "packet".into(),
                description: None,
                required: false,
            },
        ];
        assert!(r.needs_args());
        assert_eq!(r.required_args(), vec!["date"]);
    }
}
