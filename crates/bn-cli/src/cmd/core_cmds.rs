//! Core note commands: status/why/add/done/goal/note/cat/calib/files + section append.
use crate::*;

/// `bn notes` / `bn tc` — append a bullet to a prose body section, or list it when no text is
/// given. JSON mode returns `{ "lines": [...] }` (list) or the append outcome.
pub(crate) fn emit_body_section(
    cfg: &Config,
    ctx: &Resolved,
    kw: &str,
    text: &[String],
    as_json: bool,
) {
    let label = if kw == "tc" { "Test Cases" } else { "Notes" };
    if text.is_empty() {
        match commands::read_body_section(cfg, ctx, kw) {
            Ok(lines) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success(kw, serde_json::json!({ "lines": lines })).to_json()
                    );
                } else if lines.is_empty() {
                    println!("No {label} for {}/{}", ctx.repo, ctx.branch);
                } else {
                    for l in lines {
                        println!("{l}");
                    }
                }
            }
            Err(e) => fail(&e.message()),
        }
        return;
    }
    let body = text.join(" ");
    match commands::append_body(cfg, ctx, kw, &body, &today()) {
        Ok(o) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(kw, serde_json::json!({ "added": body, "section": o.label }))
                        .to_json()
                );
            } else {
                println!("Added to {} ({}): {body}", o.label, ctx.branch);
            }
        }
        Err(e) => fail(&e.message()),
    }
}

pub(crate) fn emit_status(cfg: &Config, ctx: &Resolved, as_json: bool) {
    let status = rollups::status(cfg, ctx);
    // Reconcile + persist this branch's remote runs (#282): a run that finished while the machine
    // was off advances here, on the next status render. No-op (and no network) when the note has
    // no remote runs.
    let remote =
        crate::cmd::remote_track::track_current_note(cfg, ctx, &bn_core::runner::RealRunner);
    if as_json {
        let mut data = payload::status_data(&status, ctx);
        if let Some(obj) = data.as_object_mut() {
            obj.insert("remote_runs".into(), serde_json::json!(remote));
        }
        println!("{}", Envelope::success("status", data).to_json());
        return;
    }
    match status {
        Some(s) => println!(
            "{}/{}: {} open todos, {} blockers",
            s.repo, s.branch, s.open_todos, s.blockers
        ),
        None => println!("{}/{}: no note", ctx.repo, ctx.branch),
    }
    if !remote.is_empty() {
        println!("remote runs ({}):", remote.len());
        for v in &remote {
            println!("{}", crate::cmd::remote_track::render_line(v));
        }
    }
}

/// `bn why <file|commit>`: blame/inspect the target, resolve its `BN-Note:` trailers to note
/// context, and print grouped by branch — each group ends with its note path so `bn --cat` /
/// the editor is one step away. `-L` (or an existing path) forces file mode; otherwise the
/// target is treated as a commit-ish.
pub(crate) fn emit_why(
    cfg: &Config,
    ctx: &Resolved,
    target: &str,
    lines: Option<&str>,
    as_json: bool,
) {
    use bn_core::why;
    let runner = bn_core::runner::RealRunner;
    let treat_as_file = lines.is_some() || std::path::Path::new(target).exists();
    let res = if treat_as_file {
        why::why_file(cfg, ctx, &runner, target, lines)
    } else {
        why::why_commit(cfg, ctx, &runner, target)
    };
    let report = match res {
        Ok(r) => r,
        Err(e) => {
            if as_json {
                println!("{}", Envelope::failure("why", "not_found", &e).to_json());
                std::process::exit(1);
            }
            fail(&e);
        }
    };
    if as_json {
        let data = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
        println!("{}", Envelope::success("why", data).to_json());
        return;
    }
    if report.groups.is_empty() {
        println!("{target}: no commits found");
        return;
    }
    let total: usize = report.groups.iter().map(|g| g.commits.len()).sum();
    println!("why {} — {} commit(s)", report.target, total);
    for g in &report.groups {
        println!();
        match (&g.note_ref, &g.context) {
            (Some(r), Some(c)) => {
                let via = g
                    .fallback_pr
                    .map(|n| format!(" — via PR #{n}"))
                    .unwrap_or_default();
                println!("{r} [{}]{via}", c.status);
                if let Some(goal) = &c.goal {
                    println!("  goal: {goal}");
                }
                for cm in &g.commits {
                    println!("  {} {}", cm.sha, cm.subject);
                }
                if !c.decisions.is_empty() {
                    println!("  decisions:");
                    for d in &c.decisions {
                        println!("    - {d}");
                    }
                }
                if !c.done_todos.is_empty() {
                    println!("  done todos matching:");
                    for t in &c.done_todos {
                        println!("    - {t}");
                    }
                }
                println!("  note: {}", c.note_path);
            }
            (Some(r), None) => {
                println!("{r} [note not found locally]");
                for cm in &g.commits {
                    println!("  {} {}", cm.sha, cm.subject);
                }
            }
            (None, _) => {
                match g.fallback_pr {
                    Some(n) => println!(
                        "(no BN-Note trailer — squash subject points at PR #{n}, no matching note)"
                    ),
                    None => println!(
                        "(no provenance recorded — commits predate the hook; `bn hooks install` stamps future ones)"
                    ),
                }
                for cm in &g.commits {
                    println!("  {} {}", cm.sha, cm.subject);
                }
            }
        }
    }
}

/// Resolve git context or exit 1 with the oracle's "Not in a git repo" message.
pub(crate) fn resolve_or_exit(cfg: &Config) -> Resolved {
    let cwd = std::env::current_dir().unwrap_or_default();
    match resolver::resolve(cfg, &cwd) {
        Ok(ctx) => ctx,
        Err(_) => fail("Not in a git repo"),
    }
}

pub(crate) fn emit_add(res: Result<AddOutcome, CmdError>, as_json: bool) {
    match res {
        Ok(a) => {
            if as_json {
                let env = Envelope::success("add", payload::add_data(&a));
                println!("{}", env.to_json());
            } else if a.skipped {
                println!(
                    "Already in {} [{}] — skipped: {}",
                    a.section.label(),
                    a.id,
                    a.text
                );
            } else {
                println!("Added to {} [{}]: {}", a.section.label(), a.id, a.text);
            }
        }
        Err(e) => emit_err("add", &e, as_json),
    }
}

pub(crate) fn emit_done(res: Result<DoneOutcome, CmdError>, as_json: bool) {
    match res {
        Ok(d) => {
            if as_json {
                let env = Envelope::success("done", payload::done_data(&d));
                println!("{}", env.to_json());
            } else if let Some(p) = d.pct {
                println!("Progress [{}] {p}%: {}", d.id, d.text);
            } else {
                let cal = d
                    .actual
                    .as_deref()
                    .map(|a| format!("  (actual {a})"))
                    .unwrap_or_default();
                println!("Done [{}]: {}{cal}", d.id, d.text);
            }
        }
        Err(e) => emit_err("done", &e, as_json),
    }
}

/// Validate a `--est` duration against the `bn focus start` grammar, returning the trimmed
/// string to persist. Exits with a usage error on a malformed/empty value so a bad estimate
/// never lands on an item. `None` passes through untouched (no estimate given).
pub(crate) fn parse_est_or_exit(cmd: &str, est: Option<&str>) -> Option<String> {
    let raw = est?.trim();
    if bn_core::focus::parse_duration(raw).is_none() {
        fail(&format!(
            "{cmd}: invalid --est '{raw}' (expected a duration like 90m, 2h, 1h30m)"
        ));
    }
    Some(raw.to_string())
}

pub(crate) fn emit_goal(res: Result<bn_core::commands::GoalOutcome, CmdError>, as_json: bool) {
    match res {
        Ok(g) => {
            if as_json {
                let env = Envelope::success("goal", payload::goal_data(&g));
                println!("{}", env.to_json());
            } else {
                println!("Goal: {}", g.goal);
            }
        }
        Err(e) => emit_err("goal", &e, as_json),
    }
}

pub(crate) fn emit_note(res: Result<std::path::PathBuf, CmdError>, as_json: bool) {
    match res {
        Ok(path) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("note", payload::note_data(&path)).to_json()
                );
            } else {
                println!("{}", path.display());
            }
        }
        Err(e) => emit_err("note", &e, as_json),
    }
}

fn cat_json_data(note: Option<bn_core::model::Note>, rendered: String) -> serde_json::Value {
    match note {
        Some(note) => serde_json::json!({
            "rendered": rendered,
            "front": note.front,
            "body": note.body,
        }),
        None => serde_json::json!({
            "rendered": rendered,
            "front": serde_json::Value::Null,
            "body": "",
        }),
    }
}

/// `bn cat` / `bn --cat`: print the current branch note's action view (the same
/// `store::read` + `render::cat` the MCP `bn_cat` tool uses), then this repo's saved links and a
/// related-branches footer.
pub(crate) fn emit_cat(cfg: &Config, ctx: &Resolved, as_json: bool) {
    let note_md = cfg.note_file(ctx);
    // Before the first `bn add`/`bn goal` there is no note yet: say so, like `bn status` (#1528).
    if !note_md.exists() {
        let rendered = format!("{}/{}: no note", ctx.repo, ctx.branch);
        if as_json {
            println!(
                "{}",
                Envelope::success("cat", cat_json_data(None, rendered)).to_json()
            );
        } else {
            println!("{rendered}");
        }
        return;
    }
    match bn_core::store::read(&note_md) {
        Ok(note) => {
            let links = bn_core::links::for_note(
                cfg,
                &ctx.repo,
                &ctx.branch,
                &today(),
                bn_core::links::NOTE_LINKS,
            );
            let rendered = format!(
                "{}{}",
                bn_core::render::cat(&note),
                bn_core::render::links_footer(&links)
            );
            if as_json {
                println!(
                    "{}",
                    Envelope::success("cat", cat_json_data(Some(note), rendered)).to_json()
                );
            } else {
                print!("{rendered}");
                // Zero-effort recall: surface past branches that touched the same files.
                let related = bn_core::related::related_branches(
                    cfg,
                    &bn_core::runner::RealRunner,
                    Some(&ctx.branch),
                    5,
                );
                print!("{}", bn_core::render::related_footer(&related));
            }
        }
        Err(e) => {
            let message = format!("bn: {e}");
            if as_json {
                println!("{}", Envelope::failure("cat", "store_read_failed", message).to_json());
                std::process::exit(1);
            }
            fail(&message);
        }
    }
}

pub(crate) fn emit_files(
    (dir, files): (std::path::PathBuf, Vec<bn_core::commands::FileEntry>),
    as_json: bool,
) {
    if as_json {
        println!(
            "{}",
            Envelope::success("files", payload::files_data(&dir, &files)).to_json()
        );
    } else if files.is_empty() {
        println!("(no investigation files in {})", dir.display());
    } else {
        for f in &files {
            println!("{}", f.name);
        }
    }
}

pub(crate) fn emit_err(command: &str, e: &CmdError, as_json: bool) {
    if as_json {
        let env = Envelope::failure(command, e.code(), e.message());
        println!("{}", env.to_json());
    } else {
        eprintln!("{}", e.message());
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::cat_json_data;
    use bn_core::model::Note;
    use bn_core::resolver::RepoBranch;

    #[test]
    fn cat_json_data_for_missing_note_is_read_only_shape() {
        let data = cat_json_data(None, "repo/topic: no note".to_string());
        assert_eq!(data["rendered"], serde_json::json!("repo/topic: no note"));
        assert_eq!(data["front"], serde_json::Value::Null);
        assert_eq!(data["body"], serde_json::json!(""));
    }

    #[test]
    fn cat_json_data_preserves_note_frontmatter_and_body() {
        let note = Note::new(RepoBranch::new("repo", "topic"), "2026-10-12", "testhost");
        let data = cat_json_data(Some(note.clone()), "rendered".to_string());
        assert_eq!(data["rendered"], serde_json::json!("rendered"));
        assert_eq!(data["front"]["repo"], serde_json::json!("repo"));
        assert_eq!(data["front"]["branch"], serde_json::json!("topic"));
        assert_eq!(data["body"], serde_json::json!(note.body));
    }
}
