//! `bn agents` command family — list / show / new / install / uninstall / bootstrap review
//! specialists (repo-scoped PR reviewers compiled to native Copilot/Claude custom agents).
//! Mirrors [`crate::cmd::skills`]; the compiler + guarded installer live in
//! [`bn_core::specialists`].

use crate::*;
use bn_core::skills::{NativeHarness, NativeScope};

/// Parse a `--harness` filter into the native worlds to act on (default: both).
fn parse_harnesses(harness: Option<&str>) -> Result<Vec<NativeHarness>, String> {
    match harness.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(vec![NativeHarness::Copilot, NativeHarness::Claude]),
        Some(h) => match h.to_ascii_lowercase().as_str() {
            "copilot" => Ok(vec![NativeHarness::Copilot]),
            "claude" => Ok(vec![NativeHarness::Claude]),
            other => Err(format!("unknown harness '{other}' — use copilot | claude")),
        },
    }
}

/// The discovered review specialists visible for `base` (repo-gated tiers folded in).
fn discover_specialists(cfg: &Config, base: &std::path::Path) -> Vec<bn_core::agents::AgentDef> {
    let ctx = bn_core::resolver::resolve(cfg, base).ok();
    bn_core::agents::discover(cfg, ctx.as_ref())
        .into_iter()
        .filter(|a| a.is_specialist())
        .collect()
}

/// List review specialists across every tier visible here (text or JSON envelope).
pub(crate) fn emit_agents(cfg: &Config, base: &std::path::Path, json: bool) {
    let specialists = discover_specialists(cfg, base);
    if json {
        let data: Vec<_> = specialists
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "repo": a.repo,
                    "description": a.description,
                    "source_repo": a.source_repo,
                    "references": a.references,
                    "scope": format!("{:?}", a.scope),
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success("agents", serde_json::json!({ "specialists": data })).to_json()
        );
        return;
    }
    if specialists.is_empty() {
        println!(
            "No review specialists found. Create one with `bn agents new <name> --repo <repo>`."
        );
        return;
    }
    for a in &specialists {
        let repo = a.repo.as_deref().unwrap_or("-");
        let desc = a.description.as_deref().unwrap_or("");
        println!("{:<32} {:<12} {desc}", a.name, repo);
    }
}

/// Preview the compiled native agent for `name` (Copilot by default, `--claude` for Claude Code).
pub(crate) fn emit_agent_show(
    cfg: &Config,
    base: &std::path::Path,
    name: &str,
    claude: bool,
    json: bool,
) {
    let specialists = discover_specialists(cfg, base);
    let Some(spec) = specialists.iter().find(|a| a.name == name) else {
        fail(&format!(
            "bn agents show: no specialist named '{name}' — try `bn agents` to list them"
        ));
    };
    let harness = if claude {
        NativeHarness::Claude
    } else {
        NativeHarness::Copilot
    };
    let refs = bn_core::specialists::resolve_refs(spec);
    let rendered = bn_core::specialists::render_native_agent(spec, harness, &refs);
    if json {
        println!(
            "{}",
            Envelope::success(
                "agent_show",
                serde_json::json!({
                    "name": spec.name,
                    "harness": harness.label(),
                    "content": rendered,
                }),
            )
            .to_json()
        );
        return;
    }
    print!("{rendered}");
}

/// Scaffold a new specialist source file in the chosen tier.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_agent_new(
    cfg: &Config,
    name: &str,
    repo: &str,
    description: Option<&str>,
    source: Option<&str>,
    scope: &str,
    overwrite: bool,
    json: bool,
) {
    match bn_core::specialists::scaffold(cfg, scope, name, repo, description, source, overwrite) {
        Ok(path) => {
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "agent_new",
                        serde_json::json!({
                            "path": path.display().to_string(),
                            "name": name,
                            "repo": repo,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!("Created specialist: {}", path.display());
                println!(
                    "Next: `bn agents bootstrap {name}` to mine its review standards, then \
                     `bn agents install`."
                );
            }
        }
        Err(e) => fail(&format!("bn agents new: {e}")),
    }
}

/// Resolve the install `base` and `NativeScope`: repo-scoped uses the cwd (needs a git repo);
/// global uses the stable `config_dir()/repo` install anchor (so `install.sh` is cwd-independent),
/// falling back to the cwd.
fn install_base(repo_scope: bool) -> (std::path::PathBuf, NativeScope) {
    if repo_scope {
        (std::env::current_dir().unwrap_or_default(), NativeScope::Repo)
    } else {
        let anchor = bn_core::resolver::config_dir().join("repo");
        let base = if anchor.is_dir() {
            anchor
        } else {
            std::env::current_dir().unwrap_or_default()
        };
        (base, NativeScope::Global)
    }
}

/// (Re)compile the discovered specialists into native Copilot/Claude agent files.
pub(crate) fn emit_agents_install(
    cfg: &Config,
    repo_scope: bool,
    harness: Option<&str>,
    json: bool,
) {
    let harnesses = match parse_harnesses(harness) {
        Ok(h) => h,
        Err(e) => fail(&format!("bn agents install: {e}")),
    };
    let home = dirs::home_dir().unwrap_or_default();
    let (base, scope) = install_base(repo_scope);
    let ctx = bn_core::resolver::resolve(cfg, &base).ok();

    match bn_core::specialists::install_native_agents(
        cfg,
        ctx.as_ref(),
        &base,
        &home,
        scope,
        &harnesses,
    ) {
        Ok(report) => {
            use bn_core::skills::NativeInstallAction as A;
            let (mut created, mut updated, mut removed, mut unchanged) = (0, 0, 0, 0);
            for c in &report.changes {
                match c.action {
                    A::Created => created += 1,
                    A::Updated => updated += 1,
                    A::Removed => removed += 1,
                    A::Unchanged => unchanged += 1,
                }
            }
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "agent_install",
                        serde_json::json!({
                            "scope": if repo_scope { "repo" } else { "global" },
                            "created": created,
                            "updated": updated,
                            "removed": removed,
                            "unchanged": unchanged,
                            "changes": report.changes,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "Installed review specialists ({created} created, {updated} updated, {removed} removed, {unchanged} unchanged)"
                );
            }
        }
        Err(e) => fail(&format!("bn agents install: {e}")),
    }
}

/// Remove only bn-managed native agent files (user-authored agents are preserved).
pub(crate) fn emit_agents_uninstall(
    _cfg: &Config,
    repo_scope: bool,
    harness: Option<&str>,
    json: bool,
) {
    let harnesses = match parse_harnesses(harness) {
        Ok(h) => h,
        Err(e) => fail(&format!("bn agents uninstall: {e}")),
    };
    let home = dirs::home_dir().unwrap_or_default();
    let (base, scope) = install_base(repo_scope);

    match bn_core::specialists::uninstall_native_agents(&base, &home, scope, &harnesses) {
        Ok(removed) => {
            if json {
                let paths: Vec<_> = removed.iter().map(|p| p.display().to_string()).collect();
                println!(
                    "{}",
                    Envelope::success(
                        "agent_uninstall",
                        serde_json::json!({ "removed": paths }),
                    )
                    .to_json()
                );
            } else {
                println!("Removed {} bn-managed agent file(s)", removed.len());
            }
        }
        Err(e) => fail(&format!("bn agents uninstall: {e}")),
    }
}

/// Print the mining plan for a specialist: which ADO repo to harvest, which reference doc to
/// (re)write, and the exact skill to dispatch. The actual mining runs as a dispatched agent via
/// the `mine-review-standards` skill.
pub(crate) fn emit_agent_bootstrap(cfg: &Config, base: &std::path::Path, name: &str, json: bool) {
    let specialists = discover_specialists(cfg, base);
    let Some(spec) = specialists.iter().find(|a| a.name == name) else {
        fail(&format!(
            "bn agents bootstrap: no specialist named '{name}' — try `bn agents` to list them"
        ));
    };
    let Some(source_repo) = spec.source_repo.as_deref().filter(|s| !s.trim().is_empty()) else {
        fail(&format!(
            "bn agents bootstrap: specialist '{name}' has no `source_repo:` to mine — add one to \
             its source file"
        ));
    };
    let repo = spec.repo.as_deref().unwrap_or(name);
    let ref_name = bn_core::specialists::standards_ref_name(repo);
    let goal = format!(
        "Mine {source_repo} PR review comments into the '{ref_name}' reference for {repo} \
         (run the mine-review-standards skill)"
    );
    let dispatch = format!(
        "bn dispatch \"{goal}\" --name mine-{ref_name} --skill mine-review-standards --scout"
    );

    if json {
        println!(
            "{}",
            Envelope::success(
                "agent_bootstrap",
                serde_json::json!({
                    "name": spec.name,
                    "repo": repo,
                    "source_repo": source_repo,
                    "reference": ref_name,
                    "skill": "mine-review-standards",
                    "dispatch": dispatch,
                }),
            )
            .to_json()
        );
        return;
    }

    println!("Bootstrap plan for specialist '{name}':");
    println!("  source repo (ADO):  {source_repo}");
    println!("  writes reference:   {ref_name}  (bn ref {ref_name})");
    println!("  mining skill:       mine-review-standards");
    println!();
    println!("Deploy the mining agent (a separate dispatched agent), then reinstall:");
    println!("  {dispatch}");
    println!("  bn agents install");
}
