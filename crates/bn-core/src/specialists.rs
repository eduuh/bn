//! Review **specialists** — repo-scoped PR-review experts that bn *compiles and installs* as
//! native Copilot CLI / Claude Code custom agents, so a running Copilot/Claude session discovers
//! them (via `/agent`) and can delegate to them. A specialist is authored as an ordinary
//! `agents/<name>.md` (see [`crate::agents`]) with a `repo:` field; its `references:` docs are
//! mined from real PR review comments and inlined into the compiled agent so it is self-contained.
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

/// Minimal YAML scalar quoting for a one-line frontmatter value.
fn yaml_scalar(s: &str) -> String {
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

/// The one-line description written into the compiled agent's frontmatter — the specialist's own
/// `description:`, or a synthesized fallback naming its gated repo (Copilot requires it).
fn effective_description(spec: &AgentDef) -> String {
    match spec.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => {
            let repo = spec.repo.as_deref().unwrap_or("this repo");
            format!(
                "Reviews {repo} pull requests against standards mined from merged PRs. Use for \
                 {repo} PR review."
            )
        }
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

/// The specialists that should be materialized for `scope`: every discovered `is_specialist()`
/// agent for a global install; only those gated to the current repo for a repo-scoped install.
fn desired_specialists(cfg: &Config, ctx: Option<&Resolved>, scope: NativeScope) -> Vec<AgentDef> {
    let repo = ctx.map(|c| c.repo.as_str());
    crate::agents::discover(cfg, ctx)
        .into_iter()
        .filter(|a| a.is_specialist())
        .filter(|a| match scope {
            NativeScope::Global => true,
            NativeScope::Repo => a.repo.as_deref() == repo,
        })
        .collect()
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

    let specialists = desired_specialists(cfg, ctx, scope);
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
    let body = format!(
        "You are a pull-request reviewer for the `{repo}` repository.\n\nReview the diff against \
         the standards below (mined from real reviewer comments on merged PRs) and flag concrete, \
         actionable issues — correctness, API misuse, missing tests, and the team's recurring \
         nits. Prefer the existing `auto-review-pr` workflow to fetch and post the review. Be \
         specific and cite the standard you are applying.\n"
    );
    std::fs::write(&path, format!("{front}{body}"))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}


