//! Answering "which worktree's editor are we talking about?" from whatever the caller knows.

use bn_core::resolver::{self, Config};
use std::path::{Path, PathBuf};

/// What the caller supplied. Checked in this order; the first that resolves wins.
#[derive(Debug, Clone)]
pub(crate) enum TargetSpec {
    /// A tmux pane id — what a keybinding passes as `#{pane_id}`.
    Pane(String),
    /// Any directory inside the worktree.
    Path(PathBuf),
    /// An explicit repo/branch pair, for scripts and the picker.
    RepoBranch(String, String),
    /// Work it out: the current pane's window stamp, then its cwd, then the process cwd.
    Infer,
}

/// A resolved editor target: the worktree to run in, and the note that describes it.
#[derive(Debug, Clone)]
pub(crate) struct EditTarget {
    pub(crate) repo: String,
    pub(crate) branch: String,
    pub(crate) worktree: PathBuf,
}

#[derive(Debug)]
pub(crate) enum TargetError {
    /// Nothing here identifies a worktree — the caller is outside any repo. A *normal* outcome
    /// for a keybinding pressed in a scratch pane, so it exits 2 rather than 1.
    NoWorktree(String),
    /// A named repo/branch that bn knows about but that has no checkout on this machine.
    NoCheckout(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::NoWorktree(m) | TargetError::NoCheckout(m) => write!(f, "{m}"),
        }
    }
}

/// Resolve a [`TargetSpec`] into the worktree and note it names.
pub(crate) fn resolve_target(cfg: &Config, spec: &TargetSpec) -> Result<EditTarget, TargetError> {
    match spec {
        TargetSpec::RepoBranch(repo, branch) => from_repo_branch(cfg, repo, branch),
        TargetSpec::Path(dir) => from_dir(cfg, dir),
        TargetSpec::Pane(pane) => from_pane(cfg, pane),
        TargetSpec::Infer => infer(cfg),
    }
}

/// Look the pair up through the note rollup rather than rebuilding
/// `<worktree_dir>/<repo>/<branch>` — that reconstruction is wrong for every pooled slot.
fn from_repo_branch(cfg: &Config, repo: &str, branch: &str) -> Result<EditTarget, TargetError> {
    let entry = bn_core::rollups::worktrees(cfg)
        .into_iter()
        .find(|w| w.repo == repo && w.branch == branch)
        .ok_or_else(|| TargetError::NoCheckout(format!("no note for {repo}/{branch}")))?;
    let worktree = entry.worktree.ok_or_else(|| {
        TargetError::NoCheckout(format!("{repo}/{branch} has no worktree on this machine"))
    })?;
    Ok(EditTarget {
        repo: repo.to_string(),
        branch: branch.to_string(),
        worktree: bn_core::editor::canonical(Path::new(&worktree)),
    })
}

fn from_dir(cfg: &Config, dir: &Path) -> Result<EditTarget, TargetError> {
    let ctx = resolver::resolve(cfg, dir)
        .map_err(|_| TargetError::NoWorktree(format!("{} is not in a git repo", dir.display())))?;
    let root = resolver::repo_root(dir).unwrap_or_else(|| dir.to_path_buf());
    Ok(EditTarget {
        repo: ctx.repo,
        branch: ctx.branch,
        worktree: bn_core::editor::canonical(&root),
    })
}

/// Resolve from a pane id. Prefers the window's `@bn_worktree` stamp — it is authoritative and
/// survives the pane `cd`-ing somewhere else — and falls back to the pane's cwd.
fn from_pane(cfg: &Config, pane: &str) -> Result<EditTarget, TargetError> {
    if let Some(wt) = pane_format(pane, "#{@bn_worktree}").filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(wt);
        if dir.is_dir() {
            return from_dir(cfg, &dir);
        }
    }
    let cwd = pane_format(pane, "#{pane_current_path}")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| TargetError::NoWorktree(format!("pane {pane} has no path")))?;
    from_dir(cfg, Path::new(&cwd))
}

fn infer(cfg: &Config) -> Result<EditTarget, TargetError> {
    if let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) {
        if let Ok(t) = from_pane(cfg, &pane) {
            return Ok(t);
        }
    }
    let cwd = std::env::current_dir()
        .map_err(|e| TargetError::NoWorktree(format!("no working directory: {e}")))?;
    from_dir(cfg, &cwd)
}

/// One `display-message -p` read of a format string against a pane.
fn pane_format(pane: &str, format: &str) -> Option<String> {
    crate::proc::tmux(&["display-message", "-p", "-t", pane, format]).map(|s| s.trim().to_string())
}
