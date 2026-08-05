//! `bn ref` command family (#623): list/get/edit/add reusable reference (standards) docs.
//! The sibling of `bn skills` for reusable *standards* rather than *workflows*. Reads/writes
//! through [`bn_core::references`]; the store is cross-machine (`<personal_root>/references`).

use crate::*;

/// Dispatch `bn ref` — bare list, `bn ref <name>` get, or an `edit`/`add` subcommand.
pub(crate) fn emit_ref(action: Option<RefCmd>, name: Option<String>, json: bool) {
    use bn_core::references;
    match action {
        None => match name {
            // `bn ref <name>` — print the matching reference's full body.
            Some(query) => match references::read(&query) {
                Some((r, body)) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::success(
                                "ref",
                                serde_json::json!({
                                    "name": r.name,
                                    "summary": r.summary,
                                    "path": r.path.display().to_string(),
                                    "body": body,
                                }),
                            )
                            .to_json()
                        );
                    } else {
                        print!("{body}");
                    }
                }
                None => fail_json(
                    json,
                    "ref",
                    "not_found",
                    &format!(
                        "no reference matches '{query}' — try `bn ref` to list them, or `bn ref add {query}` to create it"
                    ),
                ),
            },
            // Bare `bn ref` — list all references.
            None => {
                let refs = references::list();
                if json {
                    let data: Vec<_> = refs
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "name": r.name,
                                "summary": r.summary,
                                "path": r.path.display().to_string(),
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        Envelope::success("ref", serde_json::json!({ "references": data }))
                            .to_json()
                    );
                } else if refs.is_empty() {
                    println!(
                        "No references yet — create one with `bn ref add <name>` (e.g. `bn ref add javascript`)."
                    );
                } else {
                    for r in &refs {
                        println!("{:<20} {}", r.name, r.summary);
                    }
                }
            }
        },
        Some(RefCmd::Add { name }) => match references::ensure_file(&name) {
            Ok((path, existed)) => {
                if json {
                    println!(
                        "{}",
                        Envelope::success(
                            "ref_add",
                            serde_json::json!({
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
        },
        Some(RefCmd::Edit { name }) => {
            let (path, _existed) = match references::ensure_file(&name) {
                Ok(v) => v,
                Err(e) => fail(&format!("bn ref edit: {e}")),
            };
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "ref_edit",
                        serde_json::json!({ "path": path.display().to_string() }),
                    )
                    .to_json()
                );
                return;
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let status = std::process::Command::new(editor).arg(&path).status();
            std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
        }
    }
}
