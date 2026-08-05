//! `bn task` — repo task list backed by the note task section.
use crate::*;

/// A mutation on a single task, dispatched by [`emit_task_mutate`].
pub(crate) enum TaskOp<'a> {
    Do,
    Wip,
    Snooze(&'a str),
    Cancel(Option<&'a str>),
}

/// Ensure `<repo>/.gitattributes` carries `TODO.txt merge=union`, so two branches that each
/// appended tasks concatenate on merge instead of conflicting (the duplicate ids that produces
/// are healed on the next read — see `bn_core::todo::heal_ids`). Idempotent and best-effort: a
/// write failure is not fatal to the add.
pub(crate) fn ensure_union_merge(repo_root: &std::path::Path) {
    const LINE: &str = "TODO.txt merge=union";
    let ga = repo_root.join(".gitattributes");
    let existing = std::fs::read_to_string(&ga).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == LINE) {
        return;
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(LINE);
    content.push('\n');
    let _ = std::fs::write(&ga, content);
}

/// Resolve the repo's `TODO.txt` path and the current branch, or exit if not in a git repo.
pub(crate) fn task_ctx(cfg: &Config) -> (std::path::PathBuf, Resolved) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root =
        bn_core::resolver::repo_root(&cwd).unwrap_or_else(|| fail("bn task: not in a git repo"));
    let ctx = target_ctx(cfg, None, None).unwrap_or_else(|| fail("bn task: not in a git repo"));
    (root.join("TODO.txt"), ctx)
}

pub(crate) fn emit_task_list(cfg: &Config, all: bool, as_json: bool) {
    let (path, ctx) = task_ctx(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();
    let branch = (!all).then_some(ctx.branch.as_str());
    let items = file.open_sorted(&today, branch);
    if as_json
        && json_ok(
            "task",
            serde_json::json!({
                "branch": ctx.branch,
                "all": all,
                "tasks": items.iter().map(|t| t.to_json(&today)).collect::<Vec<_>>(),
            }),
        )
    {
        return;
    }
    if items.is_empty() {
        println!("No open tasks{}", if all { "" } else { " on this branch" });
        return;
    }
    for t in &items {
        let pri = t.priority.map(|c| format!("({c}) ")).unwrap_or_default();
        let due = t
            .meta("due")
            .map(|d| format!("  due:{d}"))
            .unwrap_or_default();
        let br = if all {
            format!("  [{}]", t.meta("branch").unwrap_or("?"))
        } else {
            String::new()
        };
        println!(
            "{}  {}{}{}{}",
            t.id().unwrap_or("?"),
            pri,
            t.summary(),
            due,
            br
        );
    }
}

pub(crate) fn emit_task_add(
    cfg: &Config,
    text: &str,
    priority: Option<char>,
    due: Option<&str>,
    src: &str,
    as_json: bool,
) {
    if text.trim().is_empty() {
        fail("bn task add: text must not be empty");
    }
    let priority = priority.map(|c| c.to_ascii_uppercase());
    if let Some(p) = priority {
        if !('A'..='C').contains(&p) {
            fail("bn task add: -p/--priority must be A, B, or C");
        }
    }
    if let Some(d) = due {
        if bn_core::util::epoch_day(d).is_none() {
            fail("bn task add: --due must be a date (YYYY-MM-DD)");
        }
    }
    let (path, ctx) = task_ctx(cfg);
    // Serialize this read-modify-write against a concurrent mutate/ingest rewrite (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    // Append-only: read just to mint the next id, then append one line — a concurrent add
    // concatenates rather than clobbering, and existing lines are never reflowed.
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();
    let id = file.next_id();
    let mut task = bn_core::todo::Task::new_todo(&id, text, priority, &ctx.branch, src, &today);
    if let Some(d) = due {
        task.set_meta("due", d);
    }
    let is_new = !path.exists();
    if raw == file.render() {
        // File is already normalized (unique ids, canonical form) — keep the concurrency-safe
        // append: a parallel add concatenates instead of clobbering, and no line is reflowed.
        bn_core::todo::append_task(&path, &task).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    } else {
        // `load` had to heal the file (duplicate/missing ids, e.g. after a union merge). Persist
        // the healed numbering now, with the new task, so the ids the user just saw in `list` stay
        // stable instead of being re-derived — and shifted — on the next read.
        file.push_task(task);
        bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    }
    if is_new {
        // Provision a union merge driver so branches' appends concatenate instead of conflicting.
        if let Some(root) = path.parent() {
            ensure_union_merge(root);
        }
    }
    if as_json {
        let data = serde_json::json!({ "id": id, "branch": ctx.branch });
        println!("{}", Envelope::success("task_add", data).to_json());
    } else {
        println!("Added {id}");
    }
}

pub(crate) fn emit_task_mutate(cfg: &Config, op: TaskOp, id: &str, as_json: bool) {
    if let TaskOp::Snooze(until) = op {
        if bn_core::util::epoch_day(until).is_none() {
            fail("bn task snooze: date must be YYYY-MM-DD");
        }
    }
    let (path, _) = task_ctx(cfg);
    // Serialize the load→mutate→save against a concurrent append add (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();
    let (ok, verb) = match op {
        TaskOp::Do => (file.mark_done(id, &today), "Done"),
        TaskOp::Wip => (file.mark_wip(id), "In progress:"),
        TaskOp::Snooze(until) => (file.snooze(id, until), "Snoozed"),
        TaskOp::Cancel(reason) => (file.cancel(id, &today, reason), "Cancelled"),
    };
    if !ok {
        fail(&format!("bn task: no task {id}"));
    }
    bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    if as_json {
        let data = serde_json::json!({ "id": id, "ok": true });
        println!("{}", Envelope::success("task", data).to_json());
    } else {
        println!("{verb} {id}");
    }
}

pub(crate) fn emit_task_ingest(
    cfg: &Config,
    source: &str,
    branch: Option<&str>,
    replace: bool,
    as_json: bool,
) {
    use std::io::Read;
    let (path, ctx) = task_ctx(cfg);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("bn task ingest: failed to read stdin");
    }
    let items = bn_core::todo::parse_ingest_todos(&input);
    let default_branch = branch.unwrap_or(&ctx.branch);
    // Serialize the load→ingest→save against a concurrent append add (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    let is_new = !path.exists();
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let out = file
        .ingest(&items, source, default_branch, replace, &today())
        .unwrap_or_else(|e| fail(&format!("bn task ingest: {e}")));
    bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    if is_new {
        if let Some(root) = path.parent() {
            ensure_union_merge(root);
        }
    }
    if as_json {
        let data = serde_json::json!({
            "added": out.added, "skipped": out.skipped, "resolved": out.resolved, "ids": out.ids,
        });
        println!("{}", Envelope::success("task_ingest", data).to_json());
    } else {
        println!(
            "todo ingest '{}': +{} added, {} skipped, {} resolved",
            source, out.added, out.skipped, out.resolved
        );
    }
}
