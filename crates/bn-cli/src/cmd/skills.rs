//! `bn skills` command family — list/find/new/render/edit skills.

use crate::*;

/// List skills across all tiers (text or JSON envelope). Shadowed lower-tier definitions are
/// tagged so it's clear which file bn-mcp actually serves for each name.
pub(crate) fn emit_skills(cfg: &Config, base: &std::path::Path, json: bool) {
    let skills = bn_core::skills::discover(cfg, base)
        .unwrap_or_else(|error| fail(&format!("bn skills: {error}")));
    if json {
        println!(
            "{}",
            Envelope::success("skills", payload::skills_data(&skills)).to_json()
        );
        return;
    }
    if skills.is_empty() {
        println!("No skills found");
        return;
    }
    for s in &skills {
        let tag = if s.effective { "" } else { "  (shadowed)" };
        println!(
            "{:<22} {:<20} {}{tag}",
            s.name,
            s.scope.label(),
            s.description
        );
    }
}

/// Rank skills by relevance to `query` (text or JSON envelope) — the discovery view. Shows the
/// matching skill's trigger (`when`) and how to invoke it, so the next step is obvious.
pub(crate) fn emit_skills_find(cfg: &Config, base: &std::path::Path, query: &str, json: bool) {
    let skills = bn_core::skills::discover(cfg, base)
        .unwrap_or_else(|error| fail(&format!("bn skills find: {error}")));
    let ranked = bn_core::skills::rank(&skills, query);
    if json {
        println!(
            "{}",
            Envelope::success("skills", payload::skills_data(&ranked)).to_json()
        );
        return;
    }
    if ranked.is_empty() {
        println!("No skill matches \"{query}\" — try `bn skills` to see them all.");
        return;
    }
    for s in &ranked {
        println!("{:<22} {}", s.name, s.description);
        if let Some(when) = &s.when {
            println!("{:<22}   when: {when}", "");
        }
        println!("{:<22}   use:  /{} in Copilot CLI, /bn:{} in Claude Code", "", s.name, s.name);
    }
}

/// Scaffold a new skill file in the chosen tier (empty body — open it in $EDITOR to fill in).
/// Richer creation (body + arguments) is available through the bn_skill_new MCP tool.
pub(crate) fn emit_skill_new(
    cfg: &Config,
    base: &std::path::Path,
    name: &str,
    description: &str,
    scope: &str,
    overwrite: bool,
    json: bool,
) {
    let Some(scope) = bn_core::skills::SkillScope::parse(scope) else {
        fail(&format!(
            "bn skills new: unknown scope '{scope}' — use global-shared | global-personal | repo-shared | repo-personal-notes | repo-personal-local"
        ));
    };
    match bn_core::skills::create_skill(cfg, base, scope, name, description, "", &[], overwrite) {
        Ok(path) => {
            if json {
                println!(
                    "{}",
                    Envelope::success("skill_new", payload::skill_new_data(&path, scope, name))
                        .to_json()
                );
            } else {
                println!("Created skill: {}", path.display());
            }
        }
        Err(e) => fail(&format!("bn skills new: {e}")),
    }
}

/// Render the effective skill `name` to stdout, filling `{{key}}` placeholders from `--arg
/// key=value` pairs. The raw body is printed (no envelope) so a shell job can capture it
/// directly; an unknown skill or a malformed `--arg` exits non-zero.
pub(crate) fn emit_skill_render(cfg: &Config, base: &std::path::Path, name: &str, args: &[String]) {
    let mut map = serde_json::Map::new();
    for a in args {
        let Some((k, v)) = a.split_once('=') else {
            fail(&format!(
                "bn skills render: --arg must be key=value, got '{a}'"
            ));
        };
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }

    match bn_core::skills::load_skill(cfg, base, name) {
        Ok(Some(skill)) => match skill.render_checked(Some(&map)) {
            Ok(rendered) => print!("{rendered}"),
            Err(error) => fail(&format!("bn skills render: {error}")),
        },
        Ok(None) => fail(&format!("bn skills render: no skill named '{name}'")),
        Err(error) => fail(&format!("bn skills render: {error}")),
    }
}

/// Open a skill file (by name) or a whole tier's skills folder (no name) in $EDITOR — the nvim
/// entry point for browsing and editing skills. With `--json`, resolve and print the target path
/// instead of launching, so a headless caller can decide what to do with it.
pub(crate) fn emit_skill_edit(
    cfg: &Config,
    base: &std::path::Path,
    name: Option<&str>,
    scope: &str,
    json: bool,
) {
    let target: std::path::PathBuf = match name {
        Some(name) => {
            // Open the *effective* definition — the one bn-mcp actually serves for this name.
            let skills = bn_core::skills::discover(cfg, base)
                .unwrap_or_else(|error| fail(&format!("bn skills edit: {error}")));
            match skills.iter().find(|s| s.effective && s.name == name) {
                Some(s) => std::path::PathBuf::from(&s.path),
                None => fail(&format!(
                    "bn skills edit: no skill named '{name}' — try `bn skills` to see them all"
                )),
            }
        }
        None => {
            let Some(scope) = bn_core::skills::SkillScope::parse(scope) else {
                fail(&format!(
                    "bn skills edit: unknown scope '{scope}' — use global-shared | global-personal | repo-shared | repo-personal-notes | repo-personal-local"
                ));
            };
            match bn_core::skills::skill_dirs(cfg, base)
                .into_iter()
                .find(|(s, _)| *s == scope)
            {
                Some((_, dir)) => {
                    // The folder may not exist yet (a tier with no skills); create it so the
                    // editor opens on a real directory rather than erroring.
                    let _ = std::fs::create_dir_all(&dir);
                    dir
                }
                None => fail(&format!(
                    "bn skills edit: the '{}' tier isn't available here (per-repo tiers need a git repo)",
                    scope.label()
                )),
            }
        }
    };

    if json {
        println!(
            "{}",
            Envelope::success(
                "skill_edit",
                serde_json::json!({ "path": target.display().to_string() })
            )
            .to_json()
        );
        return;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
    let status = std::process::Command::new(editor).arg(&target).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => fail(&format!("bn skills edit: failed to launch editor: {e}")),
    }
}

/// (Re)generate the native Agent Skills under `~/.copilot/skills` **and** `~/.claude/skills` from
/// the two global bn skill tiers (the shipped `config/prompts` + the personal `config_dir()/skills`).
///
/// Copilot CLI surfaces workflows only as native `SKILL.md` skills, so it has always needed this.
/// Claude Code is covered twice over — natively from here and as `/bn:<name>` MCP prompts from
/// `bn serve` — which is deliberate: the dashboard's skill CRUD materializes into both roots on
/// every write, so an install that skipped Claude would leave the CLI and the web disagreeing about
/// what is on disk.
///
/// Run by `install.sh`; idempotent and safe — [`install_native_skills`] refuses to touch non-bn
/// files. Each harness installs independently, so a root that legitimately refuses (a name held by
/// a skill bn does not own) doesn't stop the other from being maintained; the command reports the
/// per-harness outcome and exits non-zero only if **every** root failed. The shipped tier resolves
/// through the stable `config_dir()/repo` install anchor, so this works from any directory.
pub(crate) fn emit_skill_install(cfg: &Config, json: bool) {
    use bn_core::skills::{NativeHarness, NativeInstallAction as A, NativeScope};

    let home = dirs::home_dir().unwrap_or_default();
    let anchor = bn_core::resolver::config_dir().join("repo");
    let base = if anchor.is_dir() {
        anchor
    } else {
        std::env::current_dir().unwrap_or_default()
    };

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for harness in bn_core::skills::NATIVE_HARNESSES {
        let root = match harness {
            NativeHarness::Claude => home.join(".claude").join("skills"),
            NativeHarness::Copilot => home.join(".copilot").join("skills"),
        };
        match bn_core::skills::install_native_skills(
            cfg,
            &base,
            &home,
            NativeScope::Global,
            &[harness],
        ) {
            Ok(report) => {
                let (mut created, mut updated, mut removed, mut unchanged) = (0, 0, 0, 0);
                for c in &report.changes {
                    match c.action {
                        A::Created => created += 1,
                        A::Updated => updated += 1,
                        A::Removed => removed += 1,
                        A::Unchanged => unchanged += 1,
                    }
                }
                rows.push(serde_json::json!({
                    "harness": harness.label(),
                    "root": root.display().to_string(),
                    "created": created,
                    "updated": updated,
                    "removed": removed,
                    "unchanged": unchanged,
                }));
                if !json {
                    println!(
                        "Regenerated {} skills in {} ({created} created, {updated} updated, {removed} removed, {unchanged} unchanged)",
                        harness.label(),
                        root.display()
                    );
                }
            }
            Err(e) => {
                errors.push(serde_json::json!({ "harness": harness.label(), "error": e }));
                if !json {
                    eprintln!("warning: {} skills not regenerated: {e}", harness.label());
                }
            }
        }
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "skill_install",
                serde_json::json!({ "harnesses": rows, "failures": errors }),
            )
            .to_json()
        );
    }
    // Only a total failure is an error: one harness refusing must not fail an install that did
    // refresh the other.
    if rows.is_empty() {
        fail("bn skills install: no harness could be regenerated");
    }
}
