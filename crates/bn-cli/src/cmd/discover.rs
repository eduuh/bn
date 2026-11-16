//! Compact read-only orientation and task-relevant workflow discovery.

use crate::*;
use serde_json::{json, Value};
use std::path::Path;

const LIMIT: usize = 3;
const TEXT_LIMIT: usize = 160;

mod hints;
mod native;

pub(crate) fn emit_discover(cfg: &Config, task: &[String], json: bool) {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|e| fail_json(json, "discover", "cwd", &e.to_string()));
    let data = discover(cfg, &cwd, &task.join(" "))
        .unwrap_or_else(|e| fail_json(json, "discover", "read", &e));
    if json {
        json_ok("discover", data);
    } else {
        print!("{}", render(&data));
    }
}

fn discover(cfg: &Config, cwd: &Path, task: &str) -> Result<Value, String> {
    let context = match resolver::resolve(cfg, cwd) {
        Ok(ctx) => {
            let tasks = bn_core::task_store::task_store(cfg).load()
                .map_err(|error| format!("cannot read configured task provider: {error}"))?;
            let task = bn_core::pipeline::gate::governing_in(&tasks, &ctx.repo, &ctx.branch);
            let doc = task.and_then(|task| bn_core::task_doc::read(cfg, task));
            json!({
                "repo": ctx.repo,
                "branch": ctx.branch,
                "task_id": task.and_then(|task| task.id()),
                "goal": task.map(|task| compact(&task.summary())),
                "doc": doc.map(|doc| doc.path),
                "stage": bn_core::pipeline::stage::current(&ctx.repo, &ctx.branch),
                "plan": bn_core::plan::status(cfg, &ctx).map_err(|error| error.to_string())?,
            })
        }
        Err(_) => Value::Null,
    };
    let task = task.trim();
    let ranked = if task.is_empty() {
        Vec::new()
    } else {
        let skills = native::catalog(cfg, cwd, &dirs::home_dir().unwrap_or_default())?;
        let infos = skills.values().map(|s| s.info.clone()).collect::<Vec<_>>();
        bn_core::skills::rank(&infos, task)
            .into_iter()
            .map(|info| skills[&info.name].with_score(info.score))
            .collect()
    };
    // Drop incidental lexical hits below half the strongest match, without changing the
    // shared ranker's ordering. Broad-query ties remain useful and stay together.
    let minimum = ranked
        .first()
        .and_then(|workflow| workflow.info.score)
        .unwrap_or_default()
        .div_ceil(2);
    let relevant = ranked
        .iter()
        .take_while(|workflow| workflow.info.score.unwrap_or_default() >= minimum)
        .collect::<Vec<_>>();
    let workflows: Vec<Value> = relevant
        .iter()
        .take(LIMIT)
        .map(|workflow| {
            let s = &workflow.info;
            Ok(json!({
                "name": s.name,
                "description": compact(&s.description),
                "when": s.when.as_deref().map(compact),
                "scope": s.scope,
                "score": s.score,
                "source": s.path,
                "invoke": {
                    "copilot": workflow.copilot,
                    "claude": workflow.claude,
                },
                "read_command": workflow.read_command(cfg, cwd)?,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut commands = Vec::new();
    if !context.is_null() {
        if let Some(id) = context["task_id"].as_str() {
            commands.push(hints::bn(&["task", "doc", id])?);
        } else {
            commands.push(hints::bn(&["task", "list"])?);
        }
        commands.push(hints::bn(&["plan", "status"])?);
        commands.push(hints::bn(&["pipeline", "status"])?);
    }
    if task.is_empty() {
        commands.push(hints::bn(&["discover", "<task>"])?);
    } else if workflows.is_empty() {
        commands.push(hints::bn(&["skills"])?);
    }
    Ok(json!({
        "task": task,
        "context": context,
        "workflows": workflows,
        "workflows_truncated": relevant.len() > LIMIT,
        "workflows_filtered": relevant.len() < ranked.len(),
        "commands": commands,
    }))
}

fn compact(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(TEXT_LIMIT).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn shell_word(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn render(data: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let ctx = &data["context"];
    if ctx.is_null() {
        out.push_str("No branch context (outside a git repo or detached HEAD); global workflows remain available.\n");
    } else {
        let _ = writeln!(
            out,
            "{}/{}: {}",
            ctx["repo"].as_str().unwrap_or_default(),
            ctx["branch"].as_str().unwrap_or_default(),
            ctx["goal"].as_str().unwrap_or("no governing task"),
        );
        if let Some(path) = ctx["doc"].as_str() {
            let _ = writeln!(out, "  task context: {path}");
        }
    }
    if data["task"].as_str().unwrap_or_default().is_empty() {
        out.push_str("Supply a task to select workflows.\n");
    } else if data["workflows"].as_array().unwrap().is_empty() {
        out.push_str("No matching workflow; try a more specific task or browse bn skills.\n");
    }
    for s in data["workflows"].as_array().unwrap() {
        let _ = writeln!(
            out,
            "{}: {}\n  use {} (Copilot) or {} (Claude); read: {}",
            s["name"].as_str().unwrap_or_default(),
            s["description"].as_str().unwrap_or_default(),
            s["invoke"]["copilot"].as_str().unwrap_or("not installed"),
            s["invoke"]["claude"].as_str().unwrap_or("not installed"),
            s["read_command"]
                .as_str()
                .unwrap_or_else(|| s["source"].as_str().unwrap_or_default()),
        );
    }
    if data["workflows_truncated"] == true {
        out.push_str("Showing top 3 matches; narrow the task for a different selection.\n");
    }
    for command in data["commands"].as_array().unwrap() {
        let _ = writeln!(out, "next: {}", command.as_str().unwrap_or_default());
    }
    out
}

#[cfg(test)]
mod tests;
