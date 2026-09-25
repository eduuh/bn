//! `bn task add --body/--link/--attach` and `bn task attach` — the CLI half of rich capture
//! (#1006, EPIC #993).
//!
//! Split from `cmd/task.rs`, which is at the file-size ratchet's door.
//!
//! # Why the CLI captures at all
//!
//! The dashboard is the pretty way in, but it is not the way most context arrives. Context arrives
//! as a failing command's output in a terminal that is already open, or a screenshot sitting in
//! `~/Desktop`. `bn task add "fix the flaky reconnect" --body - --attach ~/Desktop/shot.png`, with
//! the failure piped into stdin, is the shortest path from "this is broken" to a durable task an
//! agent can act on — and no shorter path means no context lost to the friction of switching.
//!
//! # `bn task attach` is the agent's accessor
//!
//! The doc's `## Attachments` manifest deliberately lists names, not paths (see
//! [`bn_core::task_attach`]). This command is the other half of that decision: it resolves those
//! names against wherever the doc lives *on this machine, right now*. An agent handed a task runs
//! it and gets real paths to read.

use crate::*;
use bn_core::resolver::Config;
use bn_core::todo::Task;
use std::io::Read;
use std::path::{Path, PathBuf};

/// What `bn task add` captured alongside the task line.
#[derive(Debug, Default)]
pub(crate) struct Capture {
    /// Markdown for `## Context`. `-` means stdin.
    pub body: Option<String>,
    pub links: Vec<String>,
    pub attach: Vec<PathBuf>,
}

impl Capture {
    /// Whether there is anything to record beyond the task line itself.
    pub fn is_empty(&self) -> bool {
        self.body.is_none() && self.links.is_empty() && self.attach.is_empty()
    }
}

/// Read `--body`, resolving `-` to stdin so a pipeline can feed it.
fn resolve_body(body: Option<&str>) -> Option<String> {
    match body {
        Some("-") => {
            let mut buf = String::new();
            if std::io::stdin().read_to_string(&mut buf).is_err() {
                fail("bn task add: --body - could not read stdin");
            }
            Some(buf)
        }
        other => other.map(str::to_string),
    }
}

/// The same http(s)-only rule the web endpoint enforces. A task link is rendered as markdown and
/// followed by a person or an agent, so `javascript:`/`file:` have no business in it.
fn checked_link(url: &str) -> bn_core::task_doc::Link {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        fail(&format!("bn task add: --link must be an http(s) URL, got {url}"));
    }
    bn_core::task_doc::Link {
        url: url.to_string(),
        title: url.to_string(),
    }
}

/// Copy one file into the task's attachment directory, reporting what it was stored as.
fn attach_one(cfg: &Config, task: &Task, path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment");
    bn_core::task_attach::save(cfg, task, name, &bytes)
        .map(|saved| saved.name)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Record everything captured with a freshly added task.
///
/// Runs **after** the task line is durable, and never fails the command: a task that exists with
/// no screenshot is recoverable, and one that was refused because its screenshot was too big is
/// just lost work. Problems are returned to be printed, not raised.
pub(crate) fn record(cfg: &Config, task: &Task, capture: &Capture) -> (Vec<String>, Vec<String>) {
    let mut stored = Vec::new();
    let mut problems = Vec::new();

    if let Err(e) = bn_core::task_doc::ensure(cfg, task) {
        problems.push(format!("could not create the context doc: {e}"));
        return (stored, problems);
    }

    let body = resolve_body(capture.body.as_deref());
    let links: Vec<_> = capture.links.iter().map(|u| checked_link(u)).collect();
    if body.is_some() || !links.is_empty() {
        if let Err(e) = bn_core::task_doc::capture(cfg, task, body.as_deref(), &links) {
            problems.push(format!("could not record the captured context: {e}"));
        }
    }

    for path in &capture.attach {
        match attach_one(cfg, task, path) {
            Ok(name) => stored.push(name),
            Err(e) => problems.push(format!("could not attach {e}")),
        }
    }
    if !stored.is_empty() {
        if let Err(e) = bn_core::task_doc::sync_attachments(cfg, task) {
            problems.push(format!("could not list the attachments in the doc: {e}"));
        }
    }
    (stored, problems)
}

/// Print what a capture recorded, in the human form. JSON callers get it in the envelope instead.
pub(crate) fn report(stored: &[String], problems: &[String]) {
    for name in stored {
        println!("  attached {name}");
    }
    for problem in problems {
        eprintln!("  warning: {problem}");
    }
}

/// Find one task by id, or exit.
fn task_by_id(cfg: &Config, id: &str) -> Task {
    let store = crate::cmd::task::task_store(cfg);
    let _lock = store.lock();
    store
        .load()
        .unwrap_or_else(|e| fail(&format!("bn task attach: {e}")))
        .tasks()
        .find(|t| t.id() == Some(id))
        .cloned()
        .unwrap_or_else(|| fail(&format!("bn task attach: no task {id}")))
}

/// `bn task attach <id> [--add <path>…] [--rm <name>…]` — list, capture, or drop a task's files.
///
/// The listing prints **absolute paths**, which is the whole point: this is what an agent runs to
/// turn the doc's manifest of names into files it can open.
pub(crate) fn emit_task_attach(
    cfg: &Config,
    id: &str,
    add: &[PathBuf],
    rm: &[String],
    as_json: bool,
) {
    let task = task_by_id(cfg, id);
    let mut stored = Vec::new();
    let mut problems = Vec::new();
    let mut removed = Vec::new();

    if !add.is_empty() {
        if let Err(e) = bn_core::task_doc::ensure(cfg, &task) {
            fail(&format!("bn task attach: could not create the context doc: {e}"));
        }
        for path in add {
            match attach_one(cfg, &task, path) {
                Ok(name) => stored.push(name),
                Err(e) => problems.push(format!("could not attach {e}")),
            }
        }
    }
    for name in rm {
        match bn_core::task_attach::remove(cfg, &task, name) {
            Ok(true) => removed.push(name.clone()),
            Ok(false) => problems.push(format!("no attachment named {name}")),
            Err(e) => problems.push(format!("could not remove {name}: {e}")),
        }
    }
    if !stored.is_empty() || !removed.is_empty() {
        if let Err(e) = bn_core::task_doc::sync_attachments(cfg, &task) {
            problems.push(format!("could not update the doc's manifest: {e}"));
        }
    }

    let files = bn_core::task_attach::list(cfg, &task);
    if as_json {
        let data = serde_json::json!({
            "id": id,
            "added": stored,
            "removed": removed,
            "warnings": problems,
            "attachments": files.iter().map(|f| serde_json::json!({
                "name": f.name,
                "path": f.path,
                "size": f.size,
                "mime": f.mime,
            })).collect::<Vec<_>>(),
        });
        println!("{}", Envelope::success("task_attach", data).to_json());
        return;
    }

    if files.is_empty() {
        println!("No files captured with {id}.");
    } else {
        println!("{} file(s) captured with {id}:", files.len());
        for file in &files {
            println!(
                "  {}  {}  {}",
                file.path.display(),
                file.mime,
                file.human_size()
            );
        }
    }
    for name in &removed {
        println!("  removed {name}");
    }
    for problem in &problems {
        eprintln!("  warning: {problem}");
    }
}
