//! Prompt (and reference-skill) discovery + creation across the personal / global / per-repo
//! tiers — the same tiers `bn serve` serves session-safe prompts from over MCP (seeded from
//! [`global_skill_dirs`]). Invocable prompts live in `config/prompts/`. One engine, two faces:
//! `bn skills` (CLI) and the nvim picker call in here. Mirrors how cron jobs resolve through
//! [`crate::resolver::job_dirs`] / [`list_jobs`].

use crate::resolver::{self, Config};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MANAGED_MARKER_PREFIX: &str = "<!-- bn-managed-skill: ";

/// Native harness whose Agent Skills directory should receive materialized bn skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeHarness {
    Claude,
    Copilot,
}

impl NativeHarness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Copilot => "copilot",
        }
    }
}

/// Whether native skills are installed for the current repository or the current user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScope {
    Repo,
    Global,
}

/// Effect of an install on one managed native skill file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeInstallAction {
    Created,
    Updated,
    Unchanged,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeSkillChange {
    pub harness: NativeHarness,
    pub path: PathBuf,
    pub action: NativeInstallAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeSkillsReport {
    pub changes: Vec<NativeSkillChange>,
}

/// Where a skill file lives, lowest → highest precedence. When bn-mcp merges the tiers into
/// the served prompt set a higher tier overrides an earlier same-named skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// bn built-ins shipped with the server: `BN_MCP_SKILLS_DIR` (default `<cwd>/skills`).
    GlobalShared,
    /// Private, available in every repo: `BN_MCP_PERSONAL_SKILLS_DIR`
    /// (default `<config_dir>/skills`).
    GlobalPersonal,
    /// Committed with the project, shared with collaborators: `<repo>/.bn/skills`.
    RepoShared,
    /// Private to this repo, synced with your notes: `<notes>/<machine>/<repo>/skills`.
    RepoPersonalNotes,
    /// Private to this repo, machine-local and gitignored: `<repo>/.bn/skills.local`.
    RepoPersonalLocal,
}

impl SkillScope {
    /// Stable hyphenated label used in output, JSON, and `--scope` parsing.
    pub fn label(self) -> &'static str {
        match self {
            SkillScope::GlobalShared => "global-shared",
            SkillScope::GlobalPersonal => "global-personal",
            SkillScope::RepoShared => "repo-shared",
            SkillScope::RepoPersonalNotes => "repo-personal-notes",
            SkillScope::RepoPersonalLocal => "repo-personal-local",
        }
    }

    /// Precedence rank, low → high: a higher rank shadows a lower one with the same name.
    pub fn rank(self) -> u8 {
        match self {
            SkillScope::GlobalShared => 0,
            SkillScope::GlobalPersonal => 1,
            SkillScope::RepoShared => 2,
            SkillScope::RepoPersonalNotes => 3,
            SkillScope::RepoPersonalLocal => 4,
        }
    }

    /// True for the private tiers (never committed to the project's public history).
    pub fn is_personal(self) -> bool {
        matches!(
            self,
            SkillScope::GlobalPersonal
                | SkillScope::RepoPersonalNotes
                | SkillScope::RepoPersonalLocal
        )
    }

    /// Parse a `--scope` / tool-argument label back into a scope.
    pub fn parse(s: &str) -> Option<SkillScope> {
        match s {
            "global-shared" => Some(SkillScope::GlobalShared),
            "global-personal" => Some(SkillScope::GlobalPersonal),
            "repo-shared" => Some(SkillScope::RepoShared),
            "repo-personal-notes" => Some(SkillScope::RepoPersonalNotes),
            "repo-personal-local" => Some(SkillScope::RepoPersonalLocal),
            _ => None,
        }
    }
}

/// Serialize as the hyphenated [`SkillScope::label`] so the JSON wire form matches what
/// [`SkillScope::parse`] accepts (and stays stable across CLI and MCP surfaces).
impl Serialize for SkillScope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

/// One discovered skill file, tagged with its tier and whether it is the *effective*
/// (highest-precedence) definition for its name — i.e. the one bn-mcp actually serves.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// One-line trigger ("when to reach for this"), from the optional `when:` frontmatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Match terms from the optional `keywords:` frontmatter — the strongest ranking signal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    pub scope: SkillScope,
    pub path: String,
    pub effective: bool,
    /// Relevance score, present only in ranked results (`rank`); absent in a plain listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
}

/// An argument to write into a new skill's YAML frontmatter.
#[derive(Debug, Clone)]
pub struct NewSkillArg {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

/// Skill source directories in precedence order (low → high), each tagged with its scope.
/// The single source of truth for the tier layout: bn-mcp loads MCP prompts from the paths,
/// while [`discover`] / [`create_skill`] also use the scopes. Per-repo tiers appear only when
/// `base_dir` is inside a git repo; otherwise just the two global tiers.
pub fn skill_dirs(cfg: &Config, base_dir: &Path) -> Vec<(SkillScope, PathBuf)> {
    let mut dirs = Vec::new();

    dirs.push((
        SkillScope::GlobalShared,
        std::env::var_os("BN_MCP_SKILLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("config").join("prompts")),
    ));
    dirs.push((
        SkillScope::GlobalPersonal,
        std::env::var_os("BN_MCP_PERSONAL_SKILLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| resolver::config_dir().join("skills")),
    ));

    if let Ok(ctx) = resolver::resolve(cfg, base_dir) {
        let root = resolver::repo_root(base_dir);
        if let Some(root) = &root {
            dirs.push((SkillScope::RepoShared, root.join(".bn").join("skills")));
        }
        dirs.push((
            SkillScope::RepoPersonalNotes,
            cfg.repo_skills_dir(&ctx.repo),
        ));
        if let Some(root) = &root {
            dirs.push((
                SkillScope::RepoPersonalLocal,
                root.join(".bn").join("skills.local"),
            ));
        }
    }

    dirs
}

/// The two **global** skill tiers (shared built-ins + personal), independent of any worktree.
/// The central HTTP MCP server has no launch directory, so it seeds its session-safe prompt set
/// from these alone: the shipped prompts (`BN_MCP_SKILLS_DIR` when exported, else the committed
/// `config/prompts/` reached through the stable `config_dir()/repo` install anchor) plus the
/// personal `BN_MCP_PERSONAL_SKILLS_DIR` / `config_dir()/skills`. Resolving the shipped tier via
/// the anchor means `bn serve` serves the session-safe prompts in any launch mode (systemd unit,
/// launchd, or a bare foreground run) with no per-process env or symlink to keep in sync. The
/// per-repo tiers in [`skill_dirs`] are deliberately omitted — they require a git resolve the
/// shared server can't do.
pub fn global_skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(shared) = std::env::var_os("BN_MCP_SKILLS_DIR") {
        dirs.push(PathBuf::from(shared));
    } else {
        let anchored = resolver::config_dir()
            .join("repo")
            .join("config")
            .join("prompts");
        if anchored.is_dir() {
            dirs.push(anchored);
        }
    }
    dirs.push(
        std::env::var_os("BN_MCP_PERSONAL_SKILLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| resolver::config_dir().join("skills")),
    );
    dirs
}

/// Every skill across all tiers for `base_dir`, sorted by name then ascending scope rank.
/// `effective` marks the highest-precedence definition per name (what bn-mcp serves); the
/// others are shadowed.
pub fn discover(cfg: &Config, base_dir: &Path) -> Vec<SkillInfo> {
    discover_in(&skill_dirs(cfg, base_dir))
}

/// Pure core of [`discover`] over an explicit tier list, so the precedence/effective rules
/// are unit-testable without touching env vars or a real repo.
fn discover_in(tiers: &[(SkillScope, PathBuf)]) -> Vec<SkillInfo> {
    let mut found: Vec<SkillInfo> = Vec::new();
    for (scope, dir) in tiers {
        for (skill, path) in load_skill_files(dir) {
            found.push(SkillInfo {
                name: skill.name,
                description: skill.description,
                when: skill.when,
                keywords: skill.keywords,
                scope: *scope,
                path,
                effective: false,
                score: None,
            });
        }
    }

    // Effective = the highest scope rank seen for each name.
    let mut best: BTreeMap<String, u8> = BTreeMap::new();
    for info in &found {
        let rank = info.scope.rank();
        best.entry(info.name.clone())
            .and_modify(|b| *b = (*b).max(rank))
            .or_insert(rank);
    }
    for info in &mut found {
        info.effective = best.get(&info.name) == Some(&info.scope.rank());
    }

    found.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.scope.rank().cmp(&b.scope.rank()))
    });
    found
}

/// Rank the *effective* skills in `infos` by lexical relevance to `query`, highest first
/// (ties broken by name). Only positive-scoring skills are returned, each carrying its `score`.
/// An empty/whitespace query returns nothing. Pure — no I/O — so the discovery tool, the CLI, and
/// tests all share one ranking. Matching is substring-based (so "wrap" hits "wrapping"), weighting
/// name and keywords above the `when` line and description.
pub fn rank(infos: &[SkillInfo], query: &str) -> Vec<SkillInfo> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<SkillInfo> = infos
        .iter()
        .filter(|s| s.effective)
        .filter_map(|s| {
            let score = score_skill(s, &terms);
            (score > 0).then(|| {
                let mut s = s.clone();
                s.score = Some(score);
                s
            })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    scored
}

/// Lowercased, de-noised query terms: alphanumeric runs of length ≥ 2 that aren't trivial
/// stopwords. Keeps short signal words like "pr" and "up".
fn tokenize(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "you", "your", "are", "was", "from", "into",
        "what", "when", "how", "can", "should", "want", "need", "get",
    ];
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_lowercase)
        .filter(|t| !STOP.contains(&t.as_str()))
        .collect()
}

/// Sum of per-term field hits: name (5) > keywords (4) > when (2) = description (2). A term that
/// appears in several fields scores in each, so a keyword+name match outranks a description-only one.
fn score_skill(s: &SkillInfo, terms: &[String]) -> u32 {
    let name = s.name.to_lowercase();
    let keywords = s.keywords.join(" ").to_lowercase();
    let when = s.when.as_deref().unwrap_or_default().to_lowercase();
    let description = s.description.to_lowercase();
    let mut score = 0;
    for t in terms {
        if name.contains(t) {
            score += 5;
        }
        if keywords.contains(t) {
            score += 4;
        }
        if when.contains(t) {
            score += 2;
        }
        if description.contains(t) {
            score += 2;
        }
    }
    score
}

// ---- Full skill load + render -------------------------------------------------------------
// The renderable engine shared by `bn skills render` (CLI) and the bn-mcp prompt server. Kept
// rmcp-free: bn-mcp maps [`SkillArgDef`] onto its own `PromptArgument` in a thin adapter.

/// A fully parsed skill: frontmatter metadata plus the renderable markdown body.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Optional one-line trigger ("when to use this"); appended to the served prompt description.
    pub when: Option<String>,
    /// Optional match terms for relevance ranking (`rank`).
    pub keywords: Vec<String>,
    pub arguments: Vec<SkillArgDef>,
    pub body: String,
}

/// A declared skill argument (fills a `{{name}}` placeholder).
#[derive(Debug, Clone)]
pub struct SkillArgDef {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

#[derive(Deserialize)]
struct SkillFront {
    name: Option<String>,
    description: String,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    arguments: Vec<SkillArgFront>,
}

#[derive(Deserialize)]
struct SkillArgFront {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
}

impl Skill {
    /// The body with `{{name}}` placeholders substituted from `args`: a provided arg is filled at
    /// every occurrence, an unfilled placeholder is left blank.
    pub fn render(&self, args: Option<&Map<String, Value>>) -> String {
        let mut out = self.body.clone();
        for arg in &self.arguments {
            let value = args
                .and_then(|m| m.get(&arg.name))
                .map(value_to_string)
                .unwrap_or_default();
            out = out.replace(&format!("{{{{{}}}}}", arg.name), &value);
        }
        out
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Split `---\n<yaml>\n---\n<body>` into a [`Skill`]; `None` when the frontmatter is absent or
/// invalid (e.g. missing the required `description`). `name` falls back to the file stem.
pub fn parse_skill(text: &str, stem: &str) -> Option<Skill> {
    let (yaml, body) = crate::store::split_frontmatter(text)?;
    let body = body.to_string();
    let front: SkillFront = serde_yaml::from_str(yaml).ok()?;
    let arguments = front
        .arguments
        .into_iter()
        .map(|a| SkillArgDef {
            name: a.name,
            description: a.description,
            required: a.required,
        })
        .collect();
    Some(Skill {
        name: front.name.unwrap_or_else(|| stem.to_string()),
        description: front.description,
        when: front.when,
        keywords: front.keywords,
        arguments,
        body,
    })
}

/// Parse every `*.md` skill in `dir`, each paired with its file path. Missing dir → empty; an
/// unparseable file is skipped.
fn load_skill_files(dir: &Path) -> Vec<(Skill, String)> {
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
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("skill");
        if let Some(skill) = parse_skill(&text, stem) {
            out.push((skill, path.display().to_string()));
        }
    }
    out
}

/// Merge skills from `dirs` low→high precedence (a later dir overrides an earlier same-named
/// skill), sorted by name — the effective set bn-mcp serves and `render` resolves against.
pub fn load_skill_set(dirs: &[PathBuf]) -> Vec<Skill> {
    merge_skills(dirs.iter().map(|d| {
        load_skill_files(d)
            .into_iter()
            .map(|(s, _)| s)
            .collect::<Vec<_>>()
    }))
}

/// Merge skill lists by `name`, later lists overriding earlier ones. Pure (no IO) so the
/// precedence rule is unit-testable.
fn merge_skills(sets: impl IntoIterator<Item = Vec<Skill>>) -> Vec<Skill> {
    let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
    for set in sets {
        for skill in set {
            by_name.insert(skill.name.clone(), skill);
        }
    }
    by_name.into_values().collect()
}

/// The effective (highest-precedence) skill named `name` for `base_dir`, fully parsed and ready
/// to [`Skill::render`]. `None` if no tier defines it.
pub fn load_skill(cfg: &Config, base_dir: &Path, name: &str) -> Option<Skill> {
    let dirs: Vec<PathBuf> = skill_dirs(cfg, base_dir)
        .into_iter()
        .map(|(_, d)| d)
        .collect();
    load_skill_set(&dirs).into_iter().find(|s| s.name == name)
}

trait HarnessTarget {
    fn harness(&self) -> NativeHarness;
    fn root(&self, scope: NativeScope, repo_root: &Path, home: &Path) -> PathBuf;
    fn argument_reference(&self, name: &str, index: usize) -> String;

    fn append_argument_frontmatter(&self, out: &mut String, skill: &Skill) {
        let _ = (out, skill);
    }

    fn argument_preamble(&self, skill: &Skill) -> String {
        let _ = skill;
        String::new()
    }
}

struct ClaudeTarget;

impl HarnessTarget for ClaudeTarget {
    fn harness(&self) -> NativeHarness {
        NativeHarness::Claude
    }

    fn root(&self, scope: NativeScope, repo_root: &Path, home: &Path) -> PathBuf {
        match scope {
            NativeScope::Repo => repo_root.join(".claude").join("skills"),
            NativeScope::Global => home.join(".claude").join("skills"),
        }
    }

    fn argument_reference(&self, _name: &str, index: usize) -> String {
        format!("$ARGUMENTS[{index}]")
    }

    fn append_argument_frontmatter(&self, out: &mut String, skill: &Skill) {
        if skill.arguments.is_empty() {
            return;
        }
        let hint = skill
            .arguments
            .iter()
            .map(|arg| format!("<{}>", arg.name))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("argument-hint: {}\n", yaml_scalar(&hint)));
        out.push_str("arguments:\n");
        for arg in &skill.arguments {
            out.push_str(&format!("  - {}\n", arg.name));
        }
    }
}

struct CopilotTarget;

impl HarnessTarget for CopilotTarget {
    fn harness(&self) -> NativeHarness {
        NativeHarness::Copilot
    }

    fn root(&self, scope: NativeScope, repo_root: &Path, home: &Path) -> PathBuf {
        match scope {
            NativeScope::Repo => repo_root.join(".github").join("skills"),
            NativeScope::Global => home.join(".copilot").join("skills"),
        }
    }

    fn argument_reference(&self, name: &str, index: usize) -> String {
        format!("the `{name}` invocation argument (position {})", index + 1)
    }

    fn argument_preamble(&self, skill: &Skill) -> String {
        if skill.arguments.is_empty() {
            return String::new();
        }
        let mut out = format!(
            "## Invocation arguments\n\nMap values supplied after `/{}` positionally:\n",
            skill.name
        );
        for (index, arg) in skill.arguments.iter().enumerate() {
            out.push_str(&format!("{}. `{}`\n", index + 1, arg.name));
        }
        out.push_str(
            "\nReferences to those invocation arguments below mean the corresponding value from \
             the user's slash-command prompt; treat an omitted value as empty.\n\n",
        );
        out
    }
}

fn harness_target(harness: NativeHarness) -> &'static dyn HarnessTarget {
    match harness {
        NativeHarness::Claude => &ClaudeTarget,
        NativeHarness::Copilot => &CopilotTarget,
    }
}

/// Materialize the effective bn skills as native Agent Skills for the requested harnesses.
///
/// Repo installs merge every available tier, preserving the normal highest-precedence winner.
/// Global installs merge only the two global tiers. All target paths are preflighted before any
/// write, so a non-bn skill with the same name fails the operation without partial updates.
pub fn install_native_skills(
    cfg: &Config,
    base_dir: &Path,
    home: &Path,
    scope: NativeScope,
    harnesses: &[NativeHarness],
) -> Result<NativeSkillsReport, String> {
    let repo_root = match scope {
        NativeScope::Repo => resolver::repo_root(base_dir).ok_or_else(|| {
            "repo-scoped native skill installation must run inside a git repository".to_string()
        })?,
        NativeScope::Global => base_dir.to_path_buf(),
    };
    let tiers: Vec<PathBuf> = skill_dirs(cfg, base_dir)
        .into_iter()
        .filter(|(skill_scope, _)| {
            scope == NativeScope::Repo
                || matches!(
                    skill_scope,
                    SkillScope::GlobalShared | SkillScope::GlobalPersonal
                )
        })
        .map(|(_, path)| path)
        .collect();
    let skills = load_skill_set(&tiers);
    for skill in &skills {
        validate_native_skill_name(&skill.name)?;
    }
    let desired_names: BTreeSet<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    let mut pending = Vec::new();

    for harness in harnesses {
        let target = harness_target(*harness);
        let root = target.root(scope, &repo_root, home);
        for skill in &skills {
            let path = root.join(&skill.name).join("SKILL.md");
            let content = render_native_skill(skill, target);
            if path.exists() {
                let existing = std::fs::read_to_string(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                if !is_managed_skill(&existing, &skill.name) {
                    return Err(format!(
                        "refusing to overwrite non-bn skill: {}",
                        path.display()
                    ));
                }
                let action = if existing == content {
                    NativeInstallAction::Unchanged
                } else {
                    NativeInstallAction::Updated
                };
                pending.push((target.harness(), path, Some(content), action));
            } else {
                pending.push((
                    target.harness(),
                    path,
                    Some(content),
                    NativeInstallAction::Created,
                ));
            }
        }

        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read {}: {e}", root.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", root.display()))?;
            let path = entry.path().join("SKILL.md");
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", path.display())),
            };
            let Some(name) = managed_skill_name(&content) else {
                continue;
            };
            let expected = root.join(name).join("SKILL.md");
            if desired_names.contains(name) && path == expected {
                continue;
            }
            pending.push((target.harness(), path, None, NativeInstallAction::Removed));
        }
    }

    let mut changes = Vec::with_capacity(pending.len());
    for (harness, path, content, action) in pending {
        match action {
            NativeInstallAction::Created | NativeInstallAction::Updated => {
                let parent = path
                    .parent()
                    .ok_or_else(|| format!("invalid native skill path: {}", path.display()))?;
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
                let content = content
                    .ok_or_else(|| format!("missing native skill content: {}", path.display()))?;
                std::fs::write(&path, content)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
            NativeInstallAction::Removed => remove_native_skill_file(&path)?,
            NativeInstallAction::Unchanged => {}
        }
        changes.push(NativeSkillChange {
            harness,
            path,
            action,
        });
    }

    Ok(NativeSkillsReport { changes })
}

/// Remove only bn-managed `SKILL.md` files for the requested native harnesses. Any neighboring
/// user files are preserved; the skill directory is removed only when it becomes empty.
pub fn uninstall_native_skills(
    base_dir: &Path,
    home: &Path,
    scope: NativeScope,
    harnesses: &[NativeHarness],
) -> Result<Vec<PathBuf>, String> {
    let repo_root = match scope {
        NativeScope::Repo => resolver::repo_root(base_dir).ok_or_else(|| {
            "repo-scoped native skill uninstall must run inside a git repository".to_string()
        })?,
        NativeScope::Global => base_dir.to_path_buf(),
    };
    let mut removed = Vec::new();

    for harness in harnesses {
        let root = harness_target(*harness).root(scope, &repo_root, home);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read {}: {e}", root.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", root.display()))?;
            let skill_path = entry.path().join("SKILL.md");
            let content = match std::fs::read_to_string(&skill_path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", skill_path.display())),
            };
            if !is_any_managed_skill(&content) {
                continue;
            }
            remove_native_skill_file(&skill_path)?;
            removed.push(skill_path);
        }
    }

    Ok(removed)
}

/// Whether a target root currently contains at least one bn-managed native skill. Source-skill
/// mutations use this to refresh only destinations the user previously opted into installing.
pub fn has_native_install(
    base_dir: &Path,
    home: &Path,
    scope: NativeScope,
    harness: NativeHarness,
) -> bool {
    let repo_root = match scope {
        NativeScope::Repo => {
            let Some(root) = resolver::repo_root(base_dir) else {
                return false;
            };
            root
        }
        NativeScope::Global => base_dir.to_path_buf(),
    };
    let root = harness_target(harness).root(scope, &repo_root, home);
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read_to_string(entry.path().join("SKILL.md"))
            .is_ok_and(|content| is_any_managed_skill(&content))
    })
}

fn render_native_skill(skill: &Skill, target: &dyn HarnessTarget) -> String {
    let mut body = skill.body.clone();
    for (index, arg) in skill.arguments.iter().enumerate() {
        body = body.replace(
            &format!("{{{{{}}}}}", arg.name),
            &target.argument_reference(&arg.name, index),
        );
    }

    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", skill.name));
    out.push_str(&format!(
        "description: {}\n",
        yaml_scalar(&skill.description)
    ));
    target.append_argument_frontmatter(&mut out, skill);
    out.push_str("---\n");
    out.push_str(&format!("{MANAGED_MARKER_PREFIX}{} -->\n", skill.name));
    out.push_str(&target.argument_preamble(skill));
    out.push_str(&body);
    out
}

fn is_managed_skill(content: &str, name: &str) -> bool {
    managed_skill_name(content) == Some(name)
}

fn is_any_managed_skill(content: &str) -> bool {
    managed_skill_name(content).is_some()
}

fn managed_skill_name(content: &str) -> Option<&str> {
    content.lines().find_map(|line| {
        line.strip_prefix(MANAGED_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(" -->"))
    })
}

fn validate_native_skill_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "skill name '{name}' is not Agent Skills-compatible; use lowercase letters, digits, and single hyphens"
        ))
    }
}

fn remove_native_skill_file(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid native skill path: {}", path.display()))?;
    if std::fs::read_dir(parent)
        .map_err(|e| format!("read {}: {e}", parent.display()))?
        .next()
        .is_none()
    {
        std::fs::remove_dir(parent).map_err(|e| format!("remove {}: {e}", parent.display()))?;
    }
    Ok(())
}

/// The effective [`SkillInfo`] named `name` for `base_dir` — the metadata row (tier, path,
/// `effective`) bn-mcp actually serves, so a caller can locate the on-disk file to view or edit it.
/// `None` if no tier defines `name`.
pub fn effective_info(cfg: &Config, base_dir: &Path, name: &str) -> Option<SkillInfo> {
    discover(cfg, base_dir)
        .into_iter()
        .find(|s| s.effective && s.name == name)
}

/// Overwrite the skill file at `path` with `content`, but only after `content` parses as a valid
/// skill (frontmatter with a `description`) — so a dashboard edit can never corrupt a skill into
/// one bn-mcp silently drops. `Err(reason)` on unparseable content or an I/O failure; the write is
/// never attempted when validation fails. `path` is expected to be a real skill file located via
/// [`effective_info`] (never raw caller input), so this validates the *body*, not the location.
pub fn save_skill(path: &Path, content: &str) -> Result<(), String> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("skill");
    if parse_skill(content, stem).is_none() {
        return Err(
            "content is not a valid skill: needs `---` YAML frontmatter with a `description`"
                .to_string(),
        );
    }
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

/// Create `<scope dir>/<name>.md` with YAML frontmatter and a markdown body, returning the
/// written path. Refuses to overwrite an existing file unless `overwrite`. `base_dir`
/// resolves the per-repo tiers exactly as [`skill_dirs`] does, so a repo-scoped create lands
/// beside what bn-mcp serves. A repo scope outside a git repo is an error.
#[allow(clippy::too_many_arguments)]
pub fn create_skill(
    cfg: &Config,
    base_dir: &Path,
    scope: SkillScope,
    name: &str,
    description: &str,
    body: &str,
    args: &[NewSkillArg],
    overwrite: bool,
) -> std::io::Result<PathBuf> {
    let dir = scope_dir(cfg, base_dir, scope).ok_or_else(|| {
        io_err("that scope needs a git repo — run inside the repo, or pick a global scope")
    })?;
    write_skill_file(&dir, name, description, body, args, overwrite)
}

/// The directory for one scope (where [`create_skill`] writes), reusing [`skill_dirs`] so the
/// create target always matches discovery. `None` for a repo scope when `base_dir` isn't in a
/// git repo.
fn scope_dir(cfg: &Config, base_dir: &Path, scope: SkillScope) -> Option<PathBuf> {
    skill_dirs(cfg, base_dir)
        .into_iter()
        .find(|(s, _)| *s == scope)
        .map(|(_, d)| d)
}

/// Validate inputs, render the skill file, and write it into `dir` (created if missing).
/// Split out from [`create_skill`] so the validation + on-disk format are testable without
/// resolving scopes or env.
fn write_skill_file(
    dir: &Path,
    name: &str,
    description: &str,
    body: &str,
    args: &[NewSkillArg],
    overwrite: bool,
) -> std::io::Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return Err(io_err("skill name must not be empty"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(io_err(
            "skill name may only contain letters, digits, '-' and '_'",
        ));
    }
    let description = description.trim();
    if description.is_empty() {
        return Err(io_err("skill description must not be empty"));
    }

    let path = dir.join(format!("{name}.md"));
    if path.exists() && !overwrite {
        return Err(io_err(&format!(
            "skill already exists: {} (pass overwrite to replace)",
            path.display()
        )));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, render_skill_file(name, description, body, args))?;
    Ok(path)
}

/// Render a skill file: `---` YAML frontmatter (name, description, optional arguments) `---`
/// then the markdown body. Hand-written (not serde) so the output is stable and human-friendly,
/// matching the existing committed skills.
fn render_skill_file(name: &str, description: &str, body: &str, args: &[NewSkillArg]) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {}\n", yaml_scalar(description)));
    if !args.is_empty() {
        out.push_str("arguments:\n");
        for a in args {
            out.push_str(&format!("  - name: {}\n", a.name));
            if let Some(d) = &a.description {
                out.push_str(&format!("    description: {}\n", yaml_scalar(d)));
            }
            if a.required {
                out.push_str("    required: true\n");
            }
        }
    }
    out.push_str("---\n");
    let body = body.trim_end();
    if !body.is_empty() {
        out.push_str(body);
        out.push('\n');
    }
    out
}

/// Quote a single-line YAML scalar when it could be misparsed (special leading char,
/// edge whitespace, or YAML indicator chars); otherwise emit it plain. Newlines are folded to
/// spaces to keep the scalar on one line.
fn yaml_scalar(s: &str) -> String {
    let s = s.replace(['\n', '\r'], " ");
    let needs_quote = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with('-')
        // Leading `?` (mapping-key indicator) and `~` (null) parse back as non-strings.
        || s.starts_with('?')
        || s.starts_with('~')
        || s.contains(|c: char| {
            matches!(
                c,
                ':' | '#'
                    | '"'
                    | '\''
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | ','
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '%'
                    | '@'
                    | '`'
            )
        });
    if needs_quote {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, file: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), body).unwrap();
    }

    fn git_repo() -> tempfile::TempDir {
        use crate::runner::CommandRunner;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_string_lossy();
        let output = crate::runner::RealRunner.git(&["-C", &path, "init", "--quiet"]);
        assert!(output.success, "git init failed: {}", output.stderr);
        tmp
    }

    #[test]
    fn yaml_scalar_quotes_leading_indicators_so_they_round_trip() {
        for raw in ["? what to do", "~ approximate"] {
            let line = format!("description: {}\n", yaml_scalar(raw));
            let v: serde_yaml::Value = serde_yaml::from_str(&line).unwrap();
            assert_eq!(v["description"].as_str(), Some(raw), "from {line:?}");
        }
    }

    #[test]
    fn parse_skill_requires_description() {
        assert!(parse_skill("---\nname: x\n---\nbody\n", "x").is_none());
        assert!(parse_skill("no frontmatter\n", "x").is_none());
        let s = parse_skill("---\ndescription: hi\n---\nb\n", "stem").unwrap();
        assert_eq!(s.name, "stem"); // name falls back to the file stem
        assert_eq!(s.description, "hi");
    }

    #[test]
    fn parse_skill_reads_args_and_body() {
        let text = "---\nname: wrap\ndescription: Wrap it\narguments:\n  - name: branch\n    description: Target\n    required: true\n  - name: note\n---\nBody for {{branch}}.\n";
        let s = parse_skill(text, "wrap-session").unwrap();
        assert_eq!(s.name, "wrap");
        assert_eq!(s.arguments.len(), 2);
        assert_eq!(s.arguments[0].name, "branch");
        assert!(s.arguments[0].required);
        assert!(!s.arguments[1].required); // omitted → false
        assert_eq!(s.body, "Body for {{branch}}.\n");
    }

    #[test]
    fn parse_skill_reads_when_and_keywords() {
        let text = "---\ndescription: Wrap it\nwhen: at the end of a session\nkeywords: [wrap, finish, log off]\n---\nbody\n";
        let s = parse_skill(text, "wrap-session").unwrap();
        assert_eq!(s.when.as_deref(), Some("at the end of a session"));
        assert_eq!(s.keywords, vec!["wrap", "finish", "log off"]);
        // Both fields are optional — a skill without them still parses.
        let bare = parse_skill("---\ndescription: d\n---\nbody\n", "x").unwrap();
        assert!(bare.when.is_none());
        assert!(bare.keywords.is_empty());
    }

    #[test]
    fn shipped_stow_skill_is_discoverable_and_routes_to_existing_surfaces() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/prompts");
        let skills = load_skill_set(&[dir]);
        let stow = skills
            .iter()
            .find(|skill| skill.name == "stow")
            .expect("the shipped stow skill should be discoverable");
        let rendered = stow.render(None);

        for surface in [
            "bn agent digest --json",
            "bn learned",
            "bn reflect",
            "bn_add decision",
            "bn_add research",
            "bn_add ask",
            "bn_add todo",
            "bn_handoff_write",
            "bn_handoff_read",
        ] {
            assert!(
                rendered.contains(surface),
                "stow should route through {surface}: {rendered}"
            );
        }
        assert!(rendered.contains("Do not create or edit another memory file"));
        assert!(rendered.contains("whether the session is safe to reset"));
    }

    #[test]
    fn rank_orders_on_topic_skill_first_and_ignores_gibberish() {
        let mk = |name: &str, desc: &str, when: &str, kw: &[&str]| SkillInfo {
            name: name.into(),
            description: desc.into(),
            when: Some(when.into()),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            scope: SkillScope::GlobalShared,
            path: format!("/s/{name}.md"),
            effective: true,
            score: None,
        };
        let infos = vec![
            mk(
                "wrap-session",
                "Wrap up the session",
                "at session end",
                &["wrap", "finish", "log off"],
            ),
            mk(
                "pull-ado",
                "Pull ADO work items",
                "starting work from a ticket",
                &["ado", "tickets"],
            ),
        ];
        let ranked = rank(&infos, "wrap up my work and log off");
        assert_eq!(
            ranked.first().map(|s| s.name.as_str()),
            Some("wrap-session")
        );
        assert!(ranked[0].score.unwrap() > 0);
        // An unrelated query and an empty query both return nothing.
        assert!(rank(&infos, "xyzzy quux").is_empty());
        assert!(rank(&infos, "   ").is_empty());
    }

    #[test]
    fn rank_excludes_shadowed_definitions() {
        let mut shadowed = SkillInfo {
            name: "wrap".into(),
            description: "wrap".into(),
            when: None,
            keywords: vec!["wrap".into()],
            scope: SkillScope::GlobalShared,
            path: "/a.md".into(),
            effective: false,
            score: None,
        };
        shadowed.effective = false;
        // Only the effective definition is rankable.
        assert!(rank(&[shadowed], "wrap").is_empty());
    }

    #[test]
    fn render_substitutes_fills_and_blanks() {
        let s = parse_skill(
            "---\ndescription: d\narguments:\n  - name: branch\n  - name: repo\n---\nrepo={{repo}} branch={{branch}} again={{branch}}\n",
            "x",
        )
        .unwrap();
        let args = serde_json::json!({ "branch": "feat" });
        assert_eq!(s.render(args.as_object()), "repo= branch=feat again=feat\n");
        assert_eq!(s.render(None), "repo= branch= again=\n");
    }

    #[test]
    fn render_stringifies_non_string_values() {
        let s = parse_skill(
            "---\ndescription: d\narguments:\n  - name: count\n---\nn={{count}}\n",
            "x",
        )
        .unwrap();
        assert_eq!(
            s.render(serde_json::json!({ "count": 3 }).as_object()),
            "n=3\n"
        );
    }

    #[test]
    fn merge_later_set_overrides_same_name() {
        fn sk(name: &str, body: &str) -> Skill {
            Skill {
                name: name.into(),
                description: "d".into(),
                when: None,
                keywords: vec![],
                arguments: vec![],
                body: body.into(),
            }
        }
        let merged = merge_skills([
            vec![sk("start", "global"), sk("wrap", "w")],
            vec![sk("start", "personal")],
        ]);
        let names: Vec<_> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["start", "wrap"]);
        assert_eq!(
            merged.iter().find(|s| s.name == "start").unwrap().body,
            "personal"
        );
    }

    #[test]
    fn discover_marks_highest_tier_effective() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        let personal = tmp.path().join("personal");
        // `wrap` is global-shared only; `start` is defined in both tiers.
        write(&shared, "wrap.md", "---\ndescription: w\n---\nbody\n");
        write(
            &shared,
            "start.md",
            "---\ndescription: shared-start\n---\nbody\n",
        );
        write(
            &personal,
            "start.md",
            "---\ndescription: personal-start\n---\nbody\n",
        );

        let tiers = vec![
            (SkillScope::GlobalShared, shared),
            (SkillScope::GlobalPersonal, personal),
        ];
        let found = discover_in(&tiers);

        // Sorted by name: start (shared, shadowed), start (personal, effective), wrap.
        assert_eq!(found.len(), 3);
        let starts: Vec<_> = found.iter().filter(|s| s.name == "start").collect();
        assert_eq!(starts.len(), 2);
        let effective = starts.iter().find(|s| s.effective).unwrap();
        assert_eq!(effective.scope, SkillScope::GlobalPersonal);
        assert_eq!(effective.description, "personal-start");
        assert!(starts
            .iter()
            .any(|s| s.scope == SkillScope::GlobalShared && !s.effective));
        // A skill defined in a single tier is effective.
        assert!(found.iter().find(|s| s.name == "wrap").unwrap().effective);
    }

    #[test]
    fn write_skill_file_writes_frontmatter_and_refuses_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        let args = vec![NewSkillArg {
            name: "branch".to_string(),
            description: Some("Target branch".to_string()),
            required: true,
        }];
        let path = write_skill_file(
            &dir,
            "ship",
            "Ship it: now",
            "Body for {{branch}}",
            &args,
            false,
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\nname: ship\n"));
        // Description has a `:` so it must be quoted.
        assert!(text.contains("description: \"Ship it: now\"\n"));
        assert!(text.contains("arguments:\n  - name: branch\n"));
        assert!(text.contains("    required: true\n"));
        assert!(text.trim_end().ends_with("Body for {{branch}}"));

        // The written file round-trips through the parser.
        let parsed = parse_skill(&text, "ship").unwrap();
        assert_eq!(parsed.name, "ship");
        assert_eq!(parsed.description, "Ship it: now");

        // Second write without overwrite is refused; with overwrite it succeeds.
        assert!(write_skill_file(&dir, "ship", "x", "", &[], false).is_err());
        assert!(write_skill_file(&dir, "ship", "x", "", &[], true).is_ok());
    }

    #[test]
    fn write_skill_file_rejects_bad_name_and_empty_description() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        assert!(write_skill_file(&dir, "bad name", "d", "", &[], false).is_err());
        assert!(write_skill_file(&dir, "ok", "   ", "", &[], false).is_err());
    }

    #[test]
    fn save_skill_writes_valid_content_and_refuses_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("wrap.md");
        std::fs::write(&path, "---\ndescription: old\n---\nold body\n").unwrap();

        // Invalid content (no frontmatter) is rejected and the file is left untouched.
        assert!(save_skill(&path, "just prose, no frontmatter\n").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "---\ndescription: old\n---\nold body\n"
        );

        // Valid content overwrites.
        let next = "---\ndescription: new\n---\nnew body\n";
        assert!(save_skill(&path, next).is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), next);
    }

    #[test]
    fn effective_info_returns_highest_tier_path() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        let personal = tmp.path().join("personal");
        write(&shared, "s.md", "---\ndescription: shared\n---\nb\n");
        write(&personal, "s.md", "---\ndescription: personal\n---\nb\n");
        // personal tier outranks shared → the effective info points at the personal file.
        let infos = discover_in(&[
            (SkillScope::GlobalShared, shared),
            (SkillScope::GlobalPersonal, personal.clone()),
        ]);
        let eff = infos
            .iter()
            .find(|i| i.effective && i.name == "s")
            .expect("effective s");
        assert_eq!(eff.scope, SkillScope::GlobalPersonal);
        assert_eq!(eff.path, personal.join("s.md").display().to_string());
    }

    #[test]
    fn native_install_renders_paths_arguments_and_idempotent_updates() {
        let _env = crate::test_support::env_lock();
        let repo = git_repo();
        let home = tempfile::tempdir().unwrap();
        let personal = tempfile::tempdir().unwrap();
        let _personal =
            crate::test_support::EnvGuard::set("BN_MCP_PERSONAL_SKILLS_DIR", personal.path());
        let cfg = crate::test_support::config(repo.path());
        write(
            &repo.path().join("config").join("prompts"),
            "ship.md",
            "---\nname: ship\ndescription: Ship a branch\narguments:\n  - name: repo\n  - name: branch\n---\nShip {{branch}} in {{repo}}.\n",
        );

        let first = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Claude, NativeHarness::Copilot],
        )
        .unwrap();
        assert_eq!(first.changes.len(), 2);
        assert!(first
            .changes
            .iter()
            .all(|change| change.action == NativeInstallAction::Created));

        let claude = repo.path().join(".claude/skills/ship").join("SKILL.md");
        let copilot = repo.path().join(".github/skills/ship").join("SKILL.md");
        let claude_content = std::fs::read_to_string(&claude).unwrap();
        assert!(claude_content.contains("argument-hint: \"<repo> <branch>\""));
        assert!(claude_content.contains("arguments:\n  - repo\n  - branch\n"));
        assert!(claude_content.contains("Ship $ARGUMENTS[1] in $ARGUMENTS[0]."));
        assert!(claude_content.contains("<!-- bn-managed-skill: ship -->"));

        let copilot_content = std::fs::read_to_string(&copilot).unwrap();
        assert!(!copilot_content.contains("$ARGUMENTS"));
        assert!(!copilot_content.contains("arguments:\n"));
        assert!(copilot_content.contains("## Invocation arguments"));
        assert!(copilot_content.contains("1. `repo`\n2. `branch`\n"));
        assert!(copilot_content.contains(
            "Ship the `branch` invocation argument (position 2) in the `repo` invocation argument (position 1)."
        ));
        assert!(copilot_content.contains("<!-- bn-managed-skill: ship -->"));
        assert!(has_native_install(
            repo.path(),
            home.path(),
            NativeScope::Repo,
            NativeHarness::Claude
        ));

        let second = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Claude, NativeHarness::Copilot],
        )
        .unwrap();
        assert!(second
            .changes
            .iter()
            .all(|change| change.action == NativeInstallAction::Unchanged));

        let global = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Global,
            &[NativeHarness::Copilot],
        )
        .unwrap();
        assert_eq!(
            global.changes[0].path,
            home.path().join(".copilot/skills/ship/SKILL.md")
        );

        std::fs::write(claude.parent().unwrap().join("notes.md"), "keep").unwrap();
        std::fs::remove_file(repo.path().join("config/prompts/ship.md")).unwrap();
        let stale = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Claude, NativeHarness::Copilot],
        )
        .unwrap();
        assert_eq!(
            stale
                .changes
                .iter()
                .filter(|change| change.action == NativeInstallAction::Removed)
                .count(),
            2
        );
        assert!(!claude.exists());
        assert!(!copilot.exists());
        assert!(claude.parent().unwrap().join("notes.md").exists());
    }

    #[test]
    fn native_install_preflights_collisions_and_uninstall_preserves_user_files() {
        let _env = crate::test_support::env_lock();
        let repo = git_repo();
        let home = tempfile::tempdir().unwrap();
        let personal = tempfile::tempdir().unwrap();
        let _personal =
            crate::test_support::EnvGuard::set("BN_MCP_PERSONAL_SKILLS_DIR", personal.path());
        let cfg = crate::test_support::config(repo.path());
        let source = repo.path().join("config").join("prompts");
        write(&source, "ship.md", "---\ndescription: Ship\n---\nfirst\n");
        install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Copilot],
        )
        .unwrap();
        let ship = repo.path().join(".github/skills/ship/SKILL.md");
        let original = std::fs::read_to_string(&ship).unwrap();

        let invalid_source = source.join("invalid.md");
        std::fs::write(
            &invalid_source,
            "---\nname: ../../escape\ndescription: Invalid\n---\nbody\n",
        )
        .unwrap();
        let error = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Copilot],
        )
        .unwrap_err();
        assert!(error.contains("not Agent Skills-compatible"));
        std::fs::remove_file(invalid_source).unwrap();

        write(&source, "ship.md", "---\ndescription: Ship\n---\nupdated\n");
        write(
            &source,
            "owned.md",
            "---\ndescription: Owned\n---\nbn source\n",
        );
        let owned_dir = repo.path().join(".github/skills/owned");
        write(
            &owned_dir,
            "SKILL.md",
            "---\nname: owned\ndescription: User owned\n---\nkeep\n",
        );
        let error = install_native_skills(
            &cfg,
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Copilot],
        )
        .unwrap_err();
        assert!(error.contains("refusing to overwrite non-bn skill"));
        assert_eq!(std::fs::read_to_string(&ship).unwrap(), original);

        std::fs::write(ship.parent().unwrap().join("notes.md"), "keep").unwrap();
        // Uninstall derives its paths from `git rev-parse --show-toplevel`, which reports the
        // repo root symlink-resolved — on macOS this tempdir is `/var/folders/...` to us and
        // `/private/var/folders/...` to git. Capture the resolved path while the file still
        // exists and compare against that.
        let expected = std::fs::canonicalize(&ship).unwrap();
        let removed = uninstall_native_skills(
            repo.path(),
            home.path(),
            NativeScope::Repo,
            &[NativeHarness::Copilot],
        )
        .unwrap();
        assert_eq!(removed, vec![expected]);
        assert!(!ship.exists());
        assert!(ship.parent().unwrap().join("notes.md").exists());
        assert!(owned_dir.join("SKILL.md").exists());
    }
}
