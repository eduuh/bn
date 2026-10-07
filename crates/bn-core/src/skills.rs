//! Prompt (and reference-skill) discovery + creation across the personal / global / per-repo
//! tiers — the same tiers `bn mcp serve` exposes through [`skill_dirs`].
//! Invocable prompts live in `config/prompts/`. One engine, two faces:
//! `bn skills` (CLI) and the nvim picker call in here. Mirrors how cron jobs resolve through
//! [`crate::resolver::job_dirs`] / [`list_jobs`].

use crate::resolver::{self, Config};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod catalog;
mod worktree;

pub use crate::template::ArgumentDef as SkillArgDef;
pub use catalog::{
    load_skill, load_skill_set, parse_skill, render_required, RuntimeSkillError, Skill,
    SkillCatalogError, SkillParseError,
};
pub use worktree::{seed_worktree_skills, SkipReason, WorktreeSkillsReport};

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
    /// Declared `{{name}}` arguments. Carried here so a caller can tell that a skill needs
    /// values *before* it renders one — [`crate::launchable`] shows it in the picker rather
    /// than letting a launch fail at the render step.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<crate::template::ArgumentDef>,
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

/// The shipped (global-shared) skill directory for `base_dir`.
///
/// `BN_MCP_SKILLS_DIR` wins when exported. Otherwise prefer `base_dir/config/prompts` — the
/// committed tier as seen from a bn checkout — but fall back to the shipped prompts
/// ([`resolver::shipped_root`]) when that isn't a directory. The fallback is what makes this
/// work from repositories other than bn's own checkout.
fn shared_skills_dir(base_dir: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("BN_MCP_SKILLS_DIR") {
        return PathBuf::from(dir);
    }
    let local = base_dir.join("config").join("prompts");
    if local.is_dir() {
        return local;
    }
    resolver::shipped_config("prompts")
        .filter(|dir| dir.is_dir())
        .unwrap_or(local)
}

/// Skill source directories in precedence order (low → high), each tagged with its scope.
/// The single source of truth for the tier layout: bn-mcp loads MCP prompts from the paths,
/// while [`discover`] / [`create_skill`] also use the scopes. Per-repo tiers appear only when
/// `base_dir` is inside a git repo; otherwise just the two global tiers.
pub fn skill_dirs(cfg: &Config, base_dir: &Path) -> Vec<(SkillScope, PathBuf)> {
    let mut dirs = Vec::new();

    dirs.push((SkillScope::GlobalShared, shared_skills_dir(base_dir)));
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
/// Used by cross-worktree workflows such as PR review. Per-repo tiers belong to [`skill_dirs`].
pub fn global_skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(shared) = std::env::var_os("BN_MCP_SKILLS_DIR") {
        dirs.push(PathBuf::from(shared));
    } else if let Some(shipped) = resolver::shipped_config("prompts").filter(|dir| dir.is_dir()) {
        dirs.push(shipped);
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
pub fn discover(cfg: &Config, base_dir: &Path) -> Result<Vec<SkillInfo>, SkillCatalogError> {
    discover_in(&skill_dirs(cfg, base_dir))
}

/// Pure core of [`discover`] over an explicit tier list, so the precedence/effective rules
/// are unit-testable without touching env vars or a real repo.
fn discover_in(tiers: &[(SkillScope, PathBuf)]) -> Result<Vec<SkillInfo>, SkillCatalogError> {
    let mut found: Vec<SkillInfo> = Vec::new();
    for (scope, dir) in tiers {
        for (skill, path) in catalog::load_skill_files(dir)? {
            if skill.internal {
                continue;
            }
            found.push(SkillInfo {
                name: skill.name,
                description: skill.description,
                when: skill.when,
                keywords: skill.keywords,
                arguments: skill.arguments,
                scope: *scope,
                path: path.display().to_string(),
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
    Ok(found)
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
    let skills: Vec<Skill> = load_skill_set(&tiers)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|skill| !skill.internal)
        .collect();
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

/// Every native harness bn can materialize skills into — the set a source mutation refreshes.
pub const NATIVE_HARNESSES: [NativeHarness; 2] = [NativeHarness::Claude, NativeHarness::Copilot];

/// One harness root a sync could not refresh, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeSyncFailure {
    pub harness: NativeHarness,
    /// `"global"` or `"repo"` — which root of that harness failed.
    pub scope: &'static str,
    pub error: String,
}

/// The outcome of [`sync_native_skills`]: what changed, plus the roots that could not be refreshed.
/// Partial by design — see the function docs for why a failure is per-root, not fatal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct NativeSyncReport {
    pub changes: Vec<NativeSkillChange>,
    pub failures: Vec<NativeSyncFailure>,
}

impl NativeSyncReport {
    /// Whether every root bn tried to refresh actually got refreshed.
    pub fn complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Re-materialize the native Agent Skills after a bn skill source file was created, edited, or
/// deleted, so Claude and Copilot never serve a stale copy of what `bn skills` shows.
///
/// The **global** roots (`~/.claude/skills`, `~/.copilot/skills`) are always regenerated: those are
/// the harnesses' user-level skill sets and a bn source mutation is exactly the moment they should
/// follow. The **repo** roots (`.claude/skills`, `.github/skills`) are refreshed only when they
/// already hold a bn-managed skill ([`has_native_install`]) — a dashboard edit must not start
/// committing generated files into a repo that never opted in.
///
/// [`install_native_skills`] does the work for one root, so each root keeps that function's
/// guarantees: idempotent, every path preflighted before any write, and it refuses to overwrite a
/// same-named skill bn does not own. Deletion needs no separate pass — a managed native skill whose
/// source is gone is swept by the same call.
///
/// **Each root is installed independently**, and a failing one is collected rather than returned.
/// The preflight is deliberately all-or-nothing *within* a root, but that must not reach across
/// roots: `install.sh` drops an upstream `lavish` skill into `~/.claude/skills`, and bn ships a
/// skill of the same name, so on those machines the Claude root legitimately refuses. Batching the
/// harnesses into one call made that refusal abort the preflight for **both**, leaving Copilot
/// silently unsynced too — a name clash in one harness is not a reason to stop maintaining another.
pub fn sync_native_skills(cfg: &Config, base_dir: &Path, home: &Path) -> NativeSyncReport {
    let mut report = NativeSyncReport::default();
    for (scope, label) in [(NativeScope::Global, "global"), (NativeScope::Repo, "repo")] {
        for harness in NATIVE_HARNESSES {
            if scope == NativeScope::Repo && !has_native_install(base_dir, home, scope, harness) {
                continue;
            }
            match install_native_skills(cfg, base_dir, home, scope, &[harness]) {
                Ok(ok) => report.changes.extend(ok.changes),
                Err(error) => report.failures.push(NativeSyncFailure {
                    harness,
                    scope: label,
                    error,
                }),
            }
        }
    }
    report
}

/// Delete the skill source file at `path` — the create/read/update/**delete** counterpart of
/// [`save_skill`]. Like `save_skill`, `path` is expected to come from [`effective_info`] (never raw
/// caller input), so the file is known to be a real skill in a known tier. Deleting the effective
/// definition unshadows any lower-tier skill of the same name, which then becomes what bn-mcp
/// serves — deliberate: a delete removes one definition, not the name.
pub fn delete_skill(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
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
pub fn effective_info(
    cfg: &Config,
    base_dir: &Path,
    name: &str,
) -> Result<Option<SkillInfo>, SkillCatalogError> {
    Ok(discover(cfg, base_dir)?
        .into_iter()
        .find(|skill| skill.effective && skill.name == name))
}

/// Overwrite the skill file at `path` with `content`, but only after `content` parses as a valid
/// skill (frontmatter with a `description`) — so a dashboard edit can never corrupt a skill into
/// one bn-mcp silently drops. `Err(reason)` on unparseable content or an I/O failure; the write is
/// never attempted when validation fails. `path` is expected to be a real skill file located via
/// [`effective_info`] (never raw caller input), so this validates the *body*, not the location.
pub fn save_skill(path: &Path, content: &str) -> Result<(), String> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("skill");
    parse_skill(content, stem).map_err(|error| format!("content is not a valid skill: {error}"))?;
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
    let template_args: Vec<SkillArgDef> = args
        .iter()
        .map(|argument| SkillArgDef {
            name: argument.name.clone(),
            description: argument.description.clone(),
            required: argument.required,
        })
        .collect();
    crate::template::validate(body, &template_args)
        .map_err(|error| io_err(&format!("invalid skill template: {error}")))?;

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
