//! Read-side projections of the configured task provider and live worktrees.
use crate::completed::{CompletedTodo, ContextLink, ContextLinker};
use crate::resolver::{Config, Resolved};
use crate::runner::CommandRunner;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct TodoEntry {
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub id: String,
    pub text: String,
    pub line: usize,
    /// Partial progress (1–99) recorded via `bn done --pct`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
}

#[derive(Debug, Serialize)]
pub struct ListEntry {
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub status: String,
    pub open_todos: usize,
    pub blockers: usize,
}

#[derive(Debug, Serialize)]
pub struct ItemRef {
    pub id: String,
    pub text: String,
    pub line: usize,
    /// The item's creation date (`YYYY-MM-DD`) as recorded on the note — the recency signal the
    /// action center ranks todos by (a fresh todo outranks a months-old backlog item).
    pub created: String,
}

#[derive(Debug, Serialize)]
pub struct SummaryEntry {
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub status: String,
    pub todos: Vec<ItemRef>,
    pub blockers: Vec<ItemRef>,
}

#[derive(Debug, Serialize)]
pub struct StatusEntry {
    pub repo: String,
    pub branch: String,
    pub open_todos: usize,
    pub blockers: usize,
    pub work_item: Option<String>,
    pub pr: Option<String>,
}

/// One active branch note plus its live worktree dir, if the code is checked out on disk.
#[derive(Debug, Serialize)]
pub struct WorktreeEntry {
    pub repo: String,
    pub branch: String,
    /// `note.md` path — always present (the picker can always open the note).
    pub path: String,
    /// The checked-out worktree dir (`<worktree_dir>/<repo>/<branch>`), or null if the
    /// note has no live worktree. The plugin switches here; falls back to `path` if null.
    pub worktree: Option<String>,
    pub status: String,
    pub open_todos: usize,
    pub blockers: usize,
}

/// One logged achievement: a dated `- <date> — <text>` bullet from a note's `achievements.md`.
#[derive(Debug, Clone, Serialize)]
pub struct Achievement {
    pub repo: String,
    pub branch: String,
    pub date: String,
    pub text: String,
}

fn tasks(cfg: &Config) -> Vec<crate::todo::Task> {
    crate::task_store::task_store(cfg)
        .load()
        .map(|file| file.tasks().cloned().collect())
        .unwrap_or_default()
}

pub fn todos(cfg: &Config) -> Vec<TodoEntry> {
    let today = crate::util::today();
    tasks(cfg)
        .into_iter()
        .filter(|task| task.is_open(&today))
        .map(|task| TodoEntry {
            repo: task.meta("repo").unwrap_or_default().into(),
            branch: task.meta("branch").unwrap_or_default().into(),
            path: crate::task_doc::doc_path(cfg, &task)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            id: task.id().unwrap_or_default().into(),
            text: task.summary(),
            line: 1,
            pct: task.meta("pct").and_then(|p| p.parse().ok()),
        })
        .collect()
}

pub fn summary(cfg: &Config) -> Vec<SummaryEntry> {
    let mut groups: BTreeMap<(String, String), SummaryEntry> = BTreeMap::new();
    for task in todos(cfg) {
        let entry = groups
            .entry((task.repo.clone(), task.branch.clone()))
            .or_insert_with(|| SummaryEntry {
                repo: task.repo,
                branch: task.branch,
                path: task.path,
                status: "active".into(),
                todos: Vec::new(),
                blockers: Vec::new(),
            });
        entry.todos.push(ItemRef {
            id: task.id,
            text: task.text,
            line: task.line,
            created: String::new(),
        });
    }
    for item in crate::attention::open_items(&crate::util::today()) {
        let (Some(repo), Some(branch)) = (item.repo, item.branch) else {
            continue;
        };
        let entry = groups
            .entry((repo.clone(), branch.clone()))
            .or_insert_with(|| SummaryEntry {
                repo,
                branch,
                path: String::new(),
                status: "active".into(),
                todos: Vec::new(),
                blockers: Vec::new(),
            });
        entry.blockers.push(ItemRef {
            id: item.id,
            text: item.title,
            line: 0,
            created: String::new(),
        });
    }
    groups.into_values().collect()
}

pub fn list(cfg: &Config) -> Vec<ListEntry> {
    summary(cfg)
        .into_iter()
        .map(|e| ListEntry {
            repo: e.repo,
            branch: e.branch,
            path: e.path,
            status: e.status,
            open_todos: e.todos.len(),
            blockers: e.blockers.len(),
        })
        .collect()
}

pub fn worktrees(cfg: &Config) -> Vec<WorktreeEntry> {
    list(cfg)
        .into_iter()
        .map(|e| {
            let root = cfg.worktree_dir.join(&e.repo).join(&e.branch);
            WorktreeEntry {
                repo: e.repo,
                branch: e.branch,
                path: e.path,
                status: e.status,
                worktree: root.is_dir().then(|| root.display().to_string()),
                open_todos: e.open_todos,
                blockers: e.blockers,
            }
        })
        .collect()
}

pub fn status(cfg: &Config, ctx: &Resolved) -> Option<StatusEntry> {
    let task = crate::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch);
    let entry = summary(cfg)
        .into_iter()
        .find(|e| e.repo == ctx.repo && e.branch == ctx.branch)?;
    Some(StatusEntry {
        repo: entry.repo,
        branch: entry.branch,
        open_todos: entry.todos.len(),
        blockers: entry.blockers.len(),
        work_item: task
            .as_ref()
            .and_then(|t| t.meta("src"))
            .map(str::to_string),
        pr: task.as_ref().and_then(|t| t.meta("pr")).map(str::to_string),
    })
}

pub fn completed_todos(cfg: &Config) -> Vec<CompletedTodo> {
    tasks(cfg)
        .into_iter()
        .filter(|task| task.done)
        .map(|task| CompletedTodo {
            repo: task.meta("repo").unwrap_or_default().into(),
            branch: task.meta("branch").unwrap_or_default().into(),
            id: task.id().unwrap_or_default().into(),
            text: task.summary(),
            done_at: task.completion.clone().unwrap_or_default(),
        })
        .collect()
}

pub fn achievements(cfg: &Config) -> Vec<Achievement> {
    completed_todos(cfg)
        .into_iter()
        .map(|task| Achievement {
            repo: task.repo,
            branch: task.branch,
            date: task.done_at,
            text: task.text,
        })
        .collect()
}

pub struct FsContextLinker<'a> {
    cfg: &'a Config,
}

impl<'a> FsContextLinker<'a> {
    pub fn new(cfg: &'a Config, _runner: &'a dyn CommandRunner) -> Self {
        Self { cfg }
    }
}

impl ContextLinker for FsContextLinker<'_> {
    fn link(&self, repo: &str, branch: &str) -> ContextLink {
        let task = crate::pipeline::gate::governing_task(self.cfg, repo, branch);
        let path = task
            .as_ref()
            .and_then(|task| crate::task_doc::doc_path(self.cfg, task));
        ContextLink {
            local_path: path
                .filter(|path| path.is_file())
                .map(|p| p.display().to_string()),
            remote_url: None,
        }
    }
}

/// Normalize a git remote URL to its https web base, stripping the `.git` suffix and any auth /
/// scheme quirk. Handles the three shapes git remotes come in — scp-like (`git@host:owner/repo`),
/// `ssh://git@host/owner/repo`, and `https://host/owner/repo` — returning `https://host/owner/repo`.
/// `None` for anything it can't confidently map (so the remote jump is simply omitted). Pure.
pub fn remote_web_base(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    // scp-like: git@github.com:owner/repo.git
    let normalized = if let Some(rest) = remote.strip_prefix("git@") {
        // rest = host:owner/repo(.git)
        let (host, path) = rest.split_once(':')?;
        format!("https://{host}/{path}")
    } else if let Some(rest) = remote.strip_prefix("ssh://") {
        // rest = [git@]host/owner/repo(.git)
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        format!("https://{rest}")
    } else if remote.starts_with("http://") || remote.starts_with("https://") {
        remote.to_string()
    } else {
        return None;
    };
    let trimmed = normalized.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    Some(trimmed.trim_end_matches('/').to_string())
}
