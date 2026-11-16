use super::CmdError;
use crate::resolver::{Config, Resolved};
use crate::todo::Task;

fn set_meta(cfg: &Config, ctx: &Resolved, key: &str, value: &str) -> Result<Task, CmdError> {
    let store = crate::task_store::task_store(cfg);
    let _lock = store.lock();
    let mut tasks = store.load().map_err(CmdError::from)?;
    let id = crate::pipeline::gate::governing_in(&tasks, &ctx.repo, &ctx.branch)
        .and_then(Task::id)
        .ok_or_else(|| CmdError::InvalidInput("capture or claim a task first".into()))?
        .to_string();
    let task = tasks
        .task_mut(&id)
        .ok_or_else(|| CmdError::InvalidInput("governing task disappeared".into()))?;
    task.set_meta(key, value);
    let task = task.clone();
    store.save(&tasks).map_err(CmdError::from)?;
    Ok(task)
}

#[derive(serde::Serialize)]
pub struct PrOutcome {
    pub pr: String,
}

pub fn set_pr(
    cfg: &Config,
    ctx: &Resolved,
    url: &str,
    _today: &str,
) -> Result<PrOutcome, CmdError> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(CmdError::InvalidInput("expected a pull request URL".into()));
    }
    set_meta(cfg, ctx, "pr", url)?;
    Ok(PrOutcome { pr: url.into() })
}

pub fn read_pr(cfg: &Config, ctx: &Resolved) -> Option<String> {
    crate::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch)
        .and_then(|task| task.meta("pr").map(str::to_string))
}

#[derive(serde::Serialize)]
pub struct WorkItemOutcome {
    pub work_item: String,
}

pub fn set_work_item(
    cfg: &Config,
    ctx: &Resolved,
    id: &str,
    _today: &str,
) -> Result<WorkItemOutcome, CmdError> {
    set_meta(cfg, ctx, "work_item", id)?;
    Ok(WorkItemOutcome {
        work_item: id.into(),
    })
}

pub fn read_work_item(cfg: &Config, ctx: &Resolved) -> Option<String> {
    crate::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch)
        .and_then(|task| task.meta("work_item").map(str::to_string))
}
