//! **Native agents** — Markdown agents that bn *compiles and installs* as native Copilot CLI /
//! Claude Code custom agents, so a running Copilot/Claude session discovers them (via `/agent`)
//! and can delegate to them.
//!
//! Review **specialists** are the original case and still the default one: a specialist is
//! authored as an ordinary `agents/<name>.md` (see [`crate::agents`]) with a `repo:` field, and
//! its `references:` docs are mined from real PR review comments and inlined into the compiled
//! agent so it is self-contained. Any *other* kind of agent publishes here too by declaring
//! `worlds:` — see [`crate::agents::AgentDef::publishes_natively`] for why that opt-in is
//! explicit rather than automatic.
//!
//! This is the agent analogue of [`crate::skills`]' native-skill machinery and deliberately
//! mirrors it: a managed-marker guard refuses to clobber user-authored agent files, stale
//! bn-managed files are pruned, and installs are idempotent and preflighted. It reuses skills'
//! [`NativeHarness`]/[`NativeScope`]/[`NativeInstallAction`] vocabulary rather than inventing a
//! second one.
//!
//! Native file layout (flat files, unlike skills' `<name>/SKILL.md` directories):
//! * Copilot — `~/.copilot/agents/<name>.agent.md` (global) or
//!   `<repo>/.github/agents/<name>.agent.md` (repo).
//! * Claude — `~/.claude/agents/<name>.md` (global) or `<repo>/.claude/agents/<name>.md` (repo).

use crate::agents::AgentDef;
use crate::resolver::{self, Config, Resolved};
use crate::skills::{NativeHarness, NativeInstallAction, NativeScope};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MANAGED_MARKER_PREFIX: &str = "<!-- bn-managed-agent: ";

/// Copilot caps an agent file body at ~30k chars; when inlining mined standards would exceed this
/// we fall back to pointing at `bn ref <name>` instead of embedding the full text.
const MAX_AGENT_CHARS: usize = 30_000;

/// A reference doc resolved to its full body, ready to inline into a compiled agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    pub name: String,
    pub body: String,
}

/// Effect of an install on one managed native agent file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeAgentChange {
    pub harness: NativeHarness,
    pub path: PathBuf,
    pub action: NativeInstallAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeAgentsReport {
    pub changes: Vec<NativeAgentChange>,
}

/// Parse a `worlds:` entry into a native harness. Unknown values are ignored by the caller.
fn parse_world(s: &str) -> Option<NativeHarness> {
    match s.trim().to_ascii_lowercase().as_str() {
        "copilot" | "github-copilot" => Some(NativeHarness::Copilot),
        "claude" | "claude-code" => Some(NativeHarness::Claude),
        _ => None,
    }
}

/// The native worlds a specialist compiles into: its declared `worlds:` filtered to known
/// harnesses, defaulting to **both** when unset/empty (so a bare specialist reaches everywhere).
pub fn specialist_worlds(spec: &AgentDef) -> Vec<NativeHarness> {
    let declared: Vec<NativeHarness> = spec.worlds.iter().filter_map(|w| parse_world(w)).collect();
    if declared.is_empty() {
        vec![NativeHarness::Copilot, NativeHarness::Claude]
    } else {
        // Dedup while preserving order.
        let mut seen = BTreeSet::new();
        declared
            .into_iter()
            .filter(|h| seen.insert(h.label()))
            .collect()
    }
}

/// A specialist name must be Agent Skills / custom-agent compatible: lowercase letters, digits,
/// and single hyphens, ≤64 chars (same rule the native skill installer enforces).
pub fn validate_specialist_name(name: &str) -> Result<(), String> {
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
            "specialist name '{name}' is not custom-agent compatible; use lowercase letters, \
             digits, and single hyphens"
        ))
    }
}

/// Where a compiled agent file lives for a `(harness, scope)`.
fn native_agent_path(
    harness: NativeHarness,
    scope: NativeScope,
    repo_root: &Path,
    home: &Path,
    name: &str,
) -> PathBuf {
    match (harness, scope) {
        (NativeHarness::Copilot, NativeScope::Global) => home
            .join(".copilot")
            .join("agents")
            .join(format!("{name}.agent.md")),
        (NativeHarness::Copilot, NativeScope::Repo) => repo_root
            .join(".github")
            .join("agents")
            .join(format!("{name}.agent.md")),
        (NativeHarness::Claude, NativeScope::Global) => home
            .join(".claude")
            .join("agents")
            .join(format!("{name}.md")),
        (NativeHarness::Claude, NativeScope::Repo) => repo_root
            .join(".claude")
            .join("agents")
            .join(format!("{name}.md")),
    }
}

/// The directory holding a harness's compiled agents for a scope (parent of every file above).
fn native_agent_dir(
    harness: NativeHarness,
    scope: NativeScope,
    repo_root: &Path,
    home: &Path,
) -> PathBuf {
    match (harness, scope) {
        (NativeHarness::Copilot, NativeScope::Global) => home.join(".copilot").join("agents"),
        (NativeHarness::Copilot, NativeScope::Repo) => repo_root.join(".github").join("agents"),
        (NativeHarness::Claude, NativeScope::Global) => home.join(".claude").join("agents"),
        (NativeHarness::Claude, NativeScope::Repo) => repo_root.join(".claude").join("agents"),
    }
}

/// Filename a compiled agent takes for a harness, so pruning matches the right extension.
fn native_agent_filename(harness: NativeHarness, name: &str) -> String {
    match harness {
        NativeHarness::Copilot => format!("{name}.agent.md"),
        NativeHarness::Claude => format!("{name}.md"),
    }
}

/// Minimal YAML scalar quoting for a one-line frontmatter value. Shared with
/// [`crate::agents::write`], which assembles the same frontmatter for non-specialist agents.
pub(crate) fn yaml_scalar(s: &str) -> String {
    let s = s.replace(['\n', '\r'], " ");
    let needs_quote = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with('-')
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

/// The one-line description written into the compiled agent's frontmatter — the agent's own
/// `description:`, or a synthesized fallback (Copilot requires the field to be present).
///
/// The fallback is kind-aware. A specialist's is the PR-review sentence naming its gated repo;
/// anything else gets a neutral one, because a non-specialist that publishes natively is not a
/// reviewer and telling a harness it reviews pull requests would be a lie the picker repeats.
fn effective_description(spec: &AgentDef) -> String {
    match spec.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ if spec.is_specialist() => {
            let repo = spec.repo.as_deref().unwrap_or("this repo");
            format!(
                "Reviews {repo} pull requests against standards mined from merged PRs. Use for \
                 {repo} PR review."
            )
        }
        _ => format!("bn {} agent: {}.", spec.kind.label(), spec.name),
    }
}

/// Assemble the shared markdown body (everything after the frontmatter fence). When `inline` is
/// false, mined standards are referenced by name instead of embedded — the oversize fallback.
fn render_body(spec: &AgentDef, refs: &[ResolvedRef], inline: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("{MANAGED_MARKER_PREFIX}{} -->\n", spec.name));

    if !spec.body.trim().is_empty() {
        out.push_str(spec.body.trim());
        out.push_str("\n\n");
    }

    if let Some(repo) = spec.repo.as_deref() {
        out.push_str("## Repository scope\n\n");
        out.push_str(&format!(
            "This specialist reviews pull requests for the `{repo}` repository only. If the \
             current repository is not `{repo}`, decline and hand control back to the main \
             agent.\n\n"
        ));
    }

    if !refs.is_empty() {
        out.push_str("## Review standards\n\n");
        out.push_str(
            "These standards were distilled from real reviewer comments on merged pull requests. \
             Apply them when reviewing, alongside anything the author added under \"My \
             additions\".\n\n",
        );
        for r in refs {
            out.push_str(&format!("### {}\n\n", r.name));
            if inline {
                out.push_str(r.body.trim());
                out.push_str("\n\n");
            } else {
                out.push_str(&format!(
                    "Run `bn ref {}` for the full standards (omitted here to stay within the \
                     agent-file size limit).\n\n",
                    r.name
                ));
            }
        }
    }

    if !spec.skills.is_empty() {
        out.push_str("## bn skills\n\n");
        out.push_str("Use these bn workflows while reviewing: ");
        let list = spec
            .skills
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&list);
        out.push_str(
            ". In Copilot CLI invoke a workflow as `/<name>`; in Claude Code as `/bn:<name>`.\n",
        );
    }

    out
}

/// Compile a specialist to a native agent file for `harness`, inlining resolved `refs`. Copilot
/// gets `.agent.md` frontmatter (`description` required, plus `name`/`tools`/`model`); Claude gets
/// `.md` frontmatter (`name`/`description`) and inherits all tools. Guarded to `MAX_AGENT_CHARS`.
pub fn render_native_agent(
    spec: &AgentDef,
    harness: NativeHarness,
    refs: &[ResolvedRef],
) -> String {
    let description = effective_description(spec);

    let mut front = String::from("---\n");
    front.push_str(&format!("name: {}\n", spec.name));
    front.push_str(&format!("description: {}\n", yaml_scalar(&description)));
    if harness == NativeHarness::Copilot {
        if !spec.tools.is_empty() {
            let list = spec.tools.join(", ");
            front.push_str(&format!("tools: [{list}]\n"));
        }
        if let Some(model) = spec.model.as_deref().filter(|m| !m.trim().is_empty()) {
            front.push_str(&format!("model: {}\n", yaml_scalar(model)));
        }
    }
    front.push_str("---\n");

    // Inline the mined standards when they fit; otherwise point at `bn ref` to respect the limit.
    let inline_body = render_body(spec, refs, true);
    let body = if front.len() + inline_body.len() <= MAX_AGENT_CHARS {
        inline_body
    } else {
        render_body(spec, refs, false)
    };

    format!("{front}{body}")
}

/// Whether `content` is a bn-managed agent file (and, when `name` is given, for that name).
fn managed_agent_name(content: &str) -> Option<&str> {
    content.lines().find_map(|line| {
        line.strip_prefix(MANAGED_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(" -->"))
    })
}

fn is_managed_agent(content: &str, name: &str) -> bool {
    managed_agent_name(content) == Some(name)
}

fn is_any_managed_agent(content: &str) -> bool {
    managed_agent_name(content).is_some()
}

/// Resolve a specialist's `references:` to their full bodies from the bn reference store. Missing
/// references become a placeholder note so install stays robust before `bn agents bootstrap` runs.
pub fn resolve_refs(spec: &AgentDef) -> Vec<ResolvedRef> {
    spec.references
        .iter()
        .map(|name| match crate::references::read(name) {
            Some((_, body)) => ResolvedRef {
                name: name.clone(),
                body,
            },
            None => ResolvedRef {
                name: name.clone(),
                body: format!(
                    "_(reference `{name}` not found yet — run `bn agents bootstrap {}` to mine \
                     it.)_",
                    spec.name
                ),
            },
        })
        .collect()
}

/// The agents that should be materialized for `scope`: every discovered
/// [`AgentDef::publishes_natively`] agent for a global install; only those gated to the current
/// repo for a repo-scoped install.
///
/// The repo-scoped half is the specialist's `repo:` gate specifically. An agent that publishes
/// without one is global-only — it belongs to the person, not to a checkout — and the explicit
/// `is_some()` is what keeps it that way: without it a `None == None` comparison would match
/// every repo-less agent into a repo install whenever the context could not be resolved.
fn desired_native_agents(
    cfg: &Config,
    ctx: Option<&Resolved>,
    scope: NativeScope,
) -> Result<Vec<AgentDef>, String> {
    let repo = ctx.map(|c| c.repo.as_str());
    Ok(crate::agents::discover(cfg, ctx)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|a| a.publishes_natively())
        .filter(|a| in_scope(a, scope, repo))
        .collect())
}

/// Whether a publishing agent belongs in a `scope` install for the repo `repo`.
///
/// Split out from [`desired_native_agents`] because the repo half carries the one rule that is
/// easy to regress and impossible to see in an integration test: `repo.is_some()`. Without it a
/// repo-less agent (every non-specialist that publishes) compares `None == None` against an
/// unresolvable context and installs itself into whatever checkout happened to ask.
fn in_scope(agent: &AgentDef, scope: NativeScope, repo: Option<&str>) -> bool {
    match scope {
        NativeScope::Global => true,
        NativeScope::Repo => agent.repo.is_some() && agent.repo.as_deref() == repo,
    }
}

/// Materialize the discovered specialists as native custom agents for the requested harnesses.
///
/// Mirrors [`crate::skills::install_native_skills`]: every target path is preflighted before any
/// write (a non-bn agent file of the same name fails the whole operation), stale bn-managed agent
/// files are pruned, and the result is idempotent. Default scope is [`NativeScope::Global`].
pub fn install_native_agents(
    cfg: &Config,
    ctx: Option<&Resolved>,
    base_dir: &Path,
    home: &Path,
    scope: NativeScope,
    harnesses: &[NativeHarness],
) -> Result<NativeAgentsReport, String> {
    let repo_root = match scope {
        NativeScope::Repo => resolver::repo_root(base_dir).ok_or_else(|| {
            "repo-scoped specialist installation must run inside a git repository".to_string()
        })?,
        NativeScope::Global => base_dir.to_path_buf(),
    };

    let specialists = desired_native_agents(cfg, ctx, scope)?;
    for spec in &specialists {
        validate_specialist_name(&spec.name)?;
    }

    let mut pending: Vec<(NativeHarness, PathBuf, Option<String>, NativeInstallAction)> =
        Vec::new();

    for &harness in harnesses {
        // Only specialists that opted into this world compile here.
        let for_harness: Vec<&AgentDef> = specialists
            .iter()
            .filter(|s| specialist_worlds(s).contains(&harness))
            .collect();
        let desired_files: BTreeSet<String> = for_harness
            .iter()
            .map(|s| native_agent_filename(harness, &s.name))
            .collect();

        for spec in &for_harness {
            let path = native_agent_path(harness, scope, &repo_root, home, &spec.name);
            let refs = resolve_refs(spec);
            let content = render_native_agent(spec, harness, &refs);
            if path.exists() {
                let existing = std::fs::read_to_string(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                if !is_managed_agent(&existing, &spec.name) {
                    return Err(format!(
                        "refusing to overwrite non-bn agent: {}",
                        path.display()
                    ));
                }
                let action = if existing == content {
                    NativeInstallAction::Unchanged
                } else {
                    NativeInstallAction::Updated
                };
                pending.push((harness, path, Some(content), action));
            } else {
                pending.push((harness, path, Some(content), NativeInstallAction::Created));
            }
        }

        // Prune bn-managed agent files in this dir that are no longer desired.
        let dir = native_agent_dir(harness, scope, &repo_root, home);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let path = entry.path();
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // Only consider files this harness owns (right extension) that we didn't just plan.
            let ext_ok = match harness {
                NativeHarness::Copilot => fname.ends_with(".agent.md"),
                NativeHarness::Claude => fname.ends_with(".md") && !fname.ends_with(".agent.md"),
            };
            if !ext_ok || desired_files.contains(fname) {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", path.display())),
            };
            if is_any_managed_agent(&content) {
                pending.push((harness, path, None, NativeInstallAction::Removed));
            }
        }
    }

    let mut changes = Vec::with_capacity(pending.len());
    for (harness, path, content, action) in pending {
        match action {
            NativeInstallAction::Created | NativeInstallAction::Updated => {
                let parent = path
                    .parent()
                    .ok_or_else(|| format!("invalid native agent path: {}", path.display()))?;
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
                let content = content
                    .ok_or_else(|| format!("missing native agent content: {}", path.display()))?;
                std::fs::write(&path, content)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
            NativeInstallAction::Removed => {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
            }
            NativeInstallAction::Unchanged => {}
        }
        changes.push(NativeAgentChange {
            harness,
            path,
            action,
        });
    }

    Ok(NativeAgentsReport { changes })
}

/// Remove only bn-managed native agent files for the requested harnesses. User-authored agent
/// files in the same directories are preserved (managed-marker guard).
pub fn uninstall_native_agents(
    base_dir: &Path,
    home: &Path,
    scope: NativeScope,
    harnesses: &[NativeHarness],
) -> Result<Vec<PathBuf>, String> {
    let repo_root = match scope {
        NativeScope::Repo => resolver::repo_root(base_dir).ok_or_else(|| {
            "repo-scoped specialist uninstall must run inside a git repository".to_string()
        })?,
        NativeScope::Global => base_dir.to_path_buf(),
    };
    let mut removed = Vec::new();

    for &harness in harnesses {
        let dir = native_agent_dir(harness, scope, &repo_root, home);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let path = entry.path();
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let ext_ok = match harness {
                NativeHarness::Copilot => fname.ends_with(".agent.md"),
                NativeHarness::Claude => fname.ends_with(".md") && !fname.ends_with(".agent.md"),
            };
            if !ext_ok {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", path.display())),
            };
            if is_any_managed_agent(&content) {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
                removed.push(path);
            }
        }
    }

    Ok(removed)
}

/// The default reference-doc name for a repo's mined review standards, e.g. `BigMono` →
/// `bigmono-review-standards`. Slugs the repo to lowercase alphanumerics + single hyphens.
pub fn standards_ref_name(repo: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in repo.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    format!("{slug}-review-standards")
}

/// Resolve a scaffold scope keyword to the `agents/` dir it writes into. `global-personal`
/// prefers the personal overlay (`~/projects/personal-notes/bn/agents`) when configured, so
/// authored specialists land in the migrated overlay; it falls back to `personal_root/agents`.
fn scaffold_dir(cfg: &Config, scope: &str) -> Result<PathBuf, String> {
    match scope {
        "global-personal" => Ok(cfg
            .overlay_root
            .as_ref()
            .map(|r| r.join("agents"))
            .unwrap_or_else(|| cfg.personal_root.join("agents"))),
        "global-shared" => Ok(resolver::agents_dir()),
        other => Err(format!(
            "unknown scope '{other}' — use global-personal | global-shared"
        )),
    }
}

/// Scaffold a specialist source file (`agents/<name>.md`) in the chosen tier, pre-wired with a
/// `repo:` gate, a `references:` standards doc name, the `auto-review-pr` skill, and a reviewer
/// persona. Returns the written path. Errors if the file exists and `overwrite` is false.
pub fn scaffold(
    cfg: &Config,
    scope: &str,
    name: &str,
    repo: &str,
    description: Option<&str>,
    source_repo: Option<&str>,
    overwrite: bool,
) -> Result<PathBuf, String> {
    validate_specialist_name(name)?;
    let dir = scaffold_dir(cfg, scope)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.md"));
    if path.exists() && !overwrite {
        return Err(format!(
            "{} already exists (use --overwrite)",
            path.display()
        ));
    }
    let ref_name = standards_ref_name(repo);
    let desc = description.map(str::to_string).unwrap_or_else(|| {
        format!(
            "Reviews {repo} pull requests against standards mined from merged PRs. Use for {repo} \
             PR review."
        )
    });

    let mut front = String::from("---\n");
    front.push_str(&format!("name: {name}\n"));
    front.push_str("kind: specialist\n");
    front.push_str(&format!("repo: {}\n", yaml_scalar(repo)));
    front.push_str(&format!("description: {}\n", yaml_scalar(&desc)));
    if let Some(src) = source_repo.filter(|s| !s.trim().is_empty()) {
        front.push_str(&format!("source_repo: {}\n", yaml_scalar(src)));
    }
    front.push_str(&format!("references: [{ref_name}]\n"));
    front.push_str("skills: [auto-review-pr]\n");
    front.push_str("worlds: [copilot, claude]\n");
    front.push_str("tools: [read, search, execute]\n");
    front.push_str("---\n");
    let mut args = serde_json::Map::new();
    args.insert("repo".into(), serde_json::Value::String(repo.to_string()));
    let base = std::env::current_dir().unwrap_or_default();
    let body = crate::skills::render_required(cfg, &base, "specialist-scaffold", Some(&args))
        .map_err(|error| error.to_string())?;
    std::fs::write(&path, format!("{front}{body}"))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::parse_agent;
    use crate::resolver::JobScope;

    fn agent(front: &str) -> AgentDef {
        parse_agent(
            &format!("---\n{front}\n---\nBody.\n"),
            "demo",
            JobScope::Global,
            PathBuf::from("config/agents/demo.md"),
        )
        .expect("agent parses")
    }

    #[test]
    fn a_repo_gated_specialist_installs_only_into_its_own_repo() {
        let spec = agent("name: demo\nkind: specialist\nrepo: bn");
        assert!(in_scope(&spec, NativeScope::Repo, Some("bn")));
        assert!(!in_scope(&spec, NativeScope::Repo, Some("wira360")));
    }

    #[test]
    fn a_repo_less_agent_is_global_only() {
        // The regression the `repo.is_some()` guard exists for: without it this agent's
        // `None == None` would match a repo install whose context failed to resolve.
        let published = agent("name: demo\nkind: command\nrequires_repo: bn\nworlds: [claude]");
        assert!(in_scope(&published, NativeScope::Global, Some("bn")));
        assert!(!in_scope(&published, NativeScope::Repo, None));
        assert!(!in_scope(&published, NativeScope::Repo, Some("bn")));
    }

    #[test]
    fn every_publishing_agent_reaches_a_global_install() {
        let spec = agent("name: demo\nkind: specialist\nrepo: bn");
        assert!(in_scope(&spec, NativeScope::Global, None));
    }
}
