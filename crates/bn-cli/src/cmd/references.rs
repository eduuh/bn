//! `bn ref` command family: list/get/edit/add/append reference docs in either scope — the
//! person-scoped standards store (#623) or one repo's reference notes. The sibling of `bn skills`
//! for reusable *standards* and durable *findings* rather than *workflows*. Reads/writes through
//! [`bn_core::references`].

use crate::*;
use bn_core::tokens;
use bn_core::references::{self, Scope};

/// Turn the `--repo [NAME]` switch into a store scope. Absent means the global standards store;
/// bare `--repo` (clap gives us an empty string) resolves the repo we're standing in; an explicit
/// name targets another repo from anywhere.
///
/// `Err` carries the message to print: only the bare form can fail, and only by being run outside
/// a git repo, where naming the repo explicitly is the fix.
fn scope_of(cfg: &Config, arg: &RefScopeArg) -> Result<Scope, String> {
    match arg.repo.as_deref() {
        None => Ok(Scope::Global),
        Some("") => {
            let cwd = std::env::current_dir().unwrap_or_default();
            resolver::resolve(cfg, &cwd)
                .map(|ctx| Scope::repo(&ctx.repo))
                .map_err(|_| {
                    "bn ref --repo: not inside a git repo — name one explicitly, e.g. \
                     `bn ref --repo bigmono`"
                        .to_string()
                })
        }
        Some(name) => Ok(Scope::repo(name)),
    }
}

/// How to talk about a scope in error messages and empty-state hints, so the suggestion a user
/// gets back is one they can paste.
fn repo_flag(scope: &Scope) -> String {
    match scope {
        Scope::Global => String::new(),
        Scope::Repo(repo) => format!(" --repo {repo}"),
    }
}

/// Dispatch `bn ref` — bare list, `bn ref <name>` get, or an `edit`/`add`/`append` subcommand.
pub(crate) fn emit_ref(
    cfg: &Config,
    action: Option<RefCmd>,
    name: Option<String>,
    scope: RefScopeArg,
    json: bool,
) {
    // Every form carries its own scope flag; the bare/get forms use the outer one.
    let outer = scope;
    match action {
        None => {
            let scope = match scope_of(cfg, &outer) {
                Ok(s) => s,
                Err(msg) => fail_json(json, "ref", "no_repo", &msg),
            };
            match name {
                Some(query) => emit_get(&scope, &query, json),
                None => emit_list(&scope, json),
            }
        }
        Some(RefCmd::Add { name, scope }) => match scope_of(cfg, &scope) {
            Ok(scope) => emit_add(&scope, &name, json),
            Err(msg) => fail_json(json, "ref_add", "no_repo", &msg),
        },
        Some(RefCmd::Edit { name, scope }) => match scope_of(cfg, &scope) {
            Ok(scope) => emit_edit(&scope, &name, json),
            Err(msg) => fail_json(json, "ref_edit", "no_repo", &msg),
        },
        Some(RefCmd::Append {
            name,
            text,
            section,
            scope,
        }) => match scope_of(cfg, &scope) {
            Ok(scope) => emit_append(&scope, &name, &text.join(" "), section.as_deref(), json),
            Err(msg) => fail_json(json, "ref_append", "no_repo", &msg),
        },
    }
}

/// `bn ref <name>` — print the matching reference's full body.
fn emit_get(scope: &Scope, query: &str, json: bool) {
    let (name, section) = references::split_ref(query);
    let Some((r, body)) = references::read_in(scope, name) else {
        let flag = repo_flag(scope);
        fail_json(
            json,
            "ref",
            "not_found",
            &format!(
                "no reference matches '{query}' — try `bn ref{flag}` to list them, or \
                 `bn ref add {query}{flag}` to create it"
            ),
        );
    };
    let body = match section {
        None => body,
        Some(sec) => references::section_of(&body, sec).unwrap_or_else(|| {
            let have: Vec<String> = references::sections(&body)
                .into_iter()
                .map(|(h, _)| h)
                .collect();
            fail_json(
                json,
                "ref",
                "no_section",
                &format!("{} has no section '{sec}' (has: {})", r.name, have.join(", ")),
            )
        }),
    };
    if json {
        let sections: Vec<_> = references::sections(&body)
            .into_iter()
            .map(|(h, t)| serde_json::json!({"heading": h, "tokens": tokens::estimate(&t)}))
            .collect();
        println!(
            "{}",
            Envelope::success(
                "ref",
                serde_json::json!({
                    "scope": scope.label(),
                    "name": r.name,
                    "section": section,
                    "summary": r.summary,
                    "path": r.path.display().to_string(),
                    "tokens": tokens::estimate(&body),
                    "sections": sections,
                    "body": body,
                }),
            )
            .to_json()
        );
    } else {
        print!("{body}");
    }
}

/// Bare `bn ref` — list every reference in the scope, name + one-line summary. The cheap read: an
/// agent learns what exists here without paying for any body.
fn emit_list(scope: &Scope, json: bool) {
    let refs = references::list_in(scope);
    if json {
        let data: Vec<_> = refs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "summary": r.summary,
                    "path": r.path.display().to_string(),
                    "tokens": ref_tokens(r),
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success(
                "ref",
                serde_json::json!({
                    "scope": scope.label(),
                    "count": data.len(),
                    "references": data,
                }),
            )
            .to_json()
        );
        return;
    }
    if refs.is_empty() {
        let flag = repo_flag(scope);
        match scope {
            Scope::Global => println!(
                "No references yet — create one with `bn ref add <name>` (e.g. `bn ref add javascript`)."
            ),
            Scope::Repo(repo) => println!(
                "No reference notes for {repo} yet — create one with `bn ref add <name>{flag}` \
                 (e.g. `bn ref add build-and-run{flag}`)."
            ),
        }
        return;
    }
    let width = refs.iter().map(|r| r.name.len()).max().unwrap_or(20).max(20);
    for r in &refs {
        println!(
            "{:<width$} {:>6}  {}",
            r.name,
            format!("~{}", ref_tokens(r)),
            r.summary,
            width = width
        );
    }
}

/// Estimated tokens in a reference's body — what reading it costs.
fn ref_tokens(r: &references::Reference) -> usize {
    std::fs::read_to_string(&r.path)
        .map(|b| tokens::estimate(&b))
        .unwrap_or(0)
}

/// `bn ref add <name>` — scaffold from the scope's template, never overwriting.
fn emit_add(scope: &Scope, name: &str, json: bool) {
    match references::ensure_file_in(scope, name) {
        Ok((path, existed)) => {
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "ref_add",
                        serde_json::json!({
                            "scope": scope.label(),
                            "name": name,
                            "path": path.display().to_string(),
                            "existed": existed,
                        }),
                    )
                    .to_json()
                );
            } else if existed {
                println!("Reference already exists: {}", path.display());
            } else {
                println!("Created reference: {}", path.display());
            }
        }
        Err(e) => fail_json(json, "ref_add", "io_error", &format!("bn ref add: {e}")),
    }
}

/// `bn ref edit <name>` — open in $EDITOR, creating from the template first if missing.
fn emit_edit(scope: &Scope, name: &str, json: bool) {
    let (path, _existed) = match references::ensure_file_in(scope, name) {
        Ok(v) => v,
        Err(e) => fail(&format!("bn ref edit: {e}")),
    };
    if json {
        println!(
            "{}",
            Envelope::success(
                "ref_edit",
                serde_json::json!({
                    "scope": scope.label(),
                    "path": path.display().to_string(),
                }),
            )
            .to_json()
        );
        return;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
    let status = std::process::Command::new(editor).arg(&path).status();
    std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
}

/// `bn ref append <name> <text>` — record one fact, deduped. The mid-session write path: a
/// finding lands in the note without the agent reading, rewriting, and re-uploading the document.
fn emit_append(scope: &Scope, name: &str, text: &str, section: Option<&str>, json: bool) {
    if text.trim().is_empty() {
        fail_json(json, "ref_append", "empty", "bn ref append: nothing to record");
    }
    match references::append(scope, name, section, text) {
        Ok(out) => {
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "ref_append",
                        serde_json::json!({
                            "scope": scope.label(),
                            "name": name,
                            "heading": out.heading,
                            "path": out.path.display().to_string(),
                            "appended": out.appended,
                        }),
                    )
                    .to_json()
                );
            } else if out.appended {
                println!("Recorded under {} in {}", out.heading, out.path.display());
            } else {
                println!("Already recorded in {} — nothing written", out.path.display());
            }
        }
        Err(e) => fail_json(
            json,
            "ref_append",
            "io_error",
            &format!("bn ref append: {e}"),
        ),
    }
}
