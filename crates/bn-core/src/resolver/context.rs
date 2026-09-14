//! Directory → (repo, branch) context resolution: the git-backed [`resolve`], the
//! subprocess-free [`resolve_hint`], explicit-target resolution, and the shared note-key
//! normalization.

use super::util::{git_common_dir, git_current_branch, git_toplevel};
use super::{Config, ContextError, Resolved};
use std::path::{Path, PathBuf};

/// Resolve repo + branch from a directory. Port of `resolve_note_context`.
///
/// Repo identity prefers **git** — but only when the worktree is backed by a *bare* repo
/// (`<repo>.git`) or an in-place `.bare`, the worktree-manager layout `wt`/this tool create.
/// For those, the bare repo's name is authoritative wherever the worktree directory physically
/// lives, so a worktree created outside `worktree_dir` (the native-Windows arena layout) still
/// resolves to its real repo instead of falling back to the branch name.
///
/// A *standalone clone* (its own plain `.git`, e.g. a sub-repo cloned inside another repo's
/// worktree dir like `worktree/BigMono/WebApp`) deliberately does **not** override the directory:
/// [`repo_from_git`] returns `None` for it, so [`repo_from_dir`] keeps the umbrella grouping
/// (`BigMono`) the directory layout expresses. That preserves the pre-fix behavior for nested
/// clones while still fixing bare-backed worktrees.
pub fn resolve(cfg: &Config, dir: &Path) -> Result<Resolved, ContextError> {
    let branch = git_current_branch(dir).unwrap_or_default();
    let repo = repo_from_git(dir).unwrap_or_else(|| repo_from_dir(cfg, dir));

    if repo.is_empty() || branch.is_empty() {
        return Err(ContextError::NotInGitRepo);
    }
    Ok(normalize(repo, branch))
}

/// Git-free context hint for capture paths (`bn q`): best-effort repo/branch derived by
/// reading `.git` metadata files directly — no `git` subprocess, so capture stays fast and
/// works even when git is absent. Mirrors [`resolve`]'s precedence (common dir for
/// bare-backed worktrees, directory layout otherwise) but is only ever a *hint*: `None`
/// outside a repo or on a detached HEAD, and callers must not treat it as a routing decision.
pub fn resolve_hint(cfg: &Config, dir: &Path) -> Option<Resolved> {
    // Walk up to the first `.git` entry (a dir for a plain clone, a file for a linked worktree).
    let mut cur = dir;
    let (toplevel, git_entry) = loop {
        let candidate = cur.join(".git");
        if candidate.exists() {
            break (cur, candidate);
        }
        cur = cur.parent()?;
    };

    // The actual git dir: `.git` itself, or the `gitdir:` target inside a worktree's `.git` file.
    let git_dir = if git_entry.is_dir() {
        git_entry
    } else {
        let text = std::fs::read_to_string(&git_entry).ok()?;
        let target = Path::new(text.strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target.to_path_buf()
        } else {
            toplevel.join(target)
        }
    };

    // Branch from HEAD (`ref: refs/heads/<branch>`); a detached HEAD yields no hint.
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?.to_string();

    // Repo: a linked worktree's git dir is `<common>/worktrees/<name>`, so the common dir
    // names the repo (as in [`repo_from_git`]); otherwise fall back to the directory-derived
    // name, reusing the already-found toplevel instead of shelling out for it.
    let repo = git_dir
        .parent()
        .filter(|p| p.file_name() == Some(std::ffi::OsStr::new("worktrees")))
        .and_then(Path::parent)
        .and_then(repo_from_common_dir)
        .unwrap_or_else(|| repo_from_dir_hint(cfg, dir, toplevel));

    if repo.is_empty() || branch.is_empty() {
        return None;
    }
    Some(normalize(repo, branch))
}

/// The `<repo>` segment of `dir` relative to the configured worktree dir, or `None` when `dir`
/// isn't under it (the caller then falls back to the git toplevel's basename). `Some("")` when
/// `dir` *is* the worktree dir, preserving the "no segment → empty repo" answer callers already
/// treat as unresolved.
///
/// The literal prefix test comes first, then a symlink-resolved retry: a process cwd comes back
/// from the kernel fully resolved, so on macOS standing in `<worktree_dir>/acme/main` under a
/// `/var/folders/...` root reports as `/private/var/folders/...` and never matches the configured
/// `/var/...` root. Same shape on Linux whenever `worktree_dir` is reached through a symlink (a
/// symlinked `$HOME`, `/home` → `/mnt/home`). Missing the match isn't inert — it silently derives
/// the repo from the toplevel basename, so `bn link` in `<wt>/acme/main` filed its note under
/// repo `main`. Resolution is best-effort: unresolvable paths (a config pointing at a directory
/// that doesn't exist yet) keep the literal answer.
fn repo_segment(cfg: &Config, dir: &Path) -> Option<String> {
    let first = |rel: &Path| {
        rel.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if let Ok(rel) = dir.strip_prefix(&cfg.worktree_dir) {
        return Some(first(rel));
    }
    let root = std::fs::canonicalize(&cfg.worktree_dir).ok()?;
    let here = std::fs::canonicalize(dir).ok()?;
    here.strip_prefix(&root).ok().map(first)
}

/// [`repo_from_dir`] minus the `git rev-parse --show-toplevel` subprocess: the caller already
/// walked to the toplevel, so only the pure path logic remains (worktree-dir segment first,
/// then toplevel basename with the in-place `.bare`-sibling override).
fn repo_from_dir_hint(cfg: &Config, dir: &Path, toplevel: &Path) -> String {
    if let Some(repo) = repo_segment(cfg, dir) {
        return repo;
    }
    let mut repo = toplevel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(parent) = toplevel.parent() {
        if parent.join(".bare").is_dir() {
            if let Some(name) = parent.file_name() {
                repo = name.to_string_lossy().into_owned();
            }
        }
    }
    repo
}

/// Repo name from git's common dir — authoritative for *bare-backed* worktrees only. The
/// common dir is the shared bare repo (`<repo>.git`) or an in-place `<repo>/.bare`. `None`
/// when `dir` isn't a git work tree, **or** when the common dir is a standalone clone's plain
/// `.git` (the caller then defers to [`repo_from_dir`] — see [`repo_from_common_dir`]).
fn repo_from_git(dir: &Path) -> Option<String> {
    let common = git_common_dir(dir)?;
    let common = Path::new(&common);
    // git may print a relative common dir (".git") for a normal clone's main worktree;
    // resolve it against `dir` so the parent/basename logic sees a real path.
    let abs = if common.is_absolute() {
        common.to_path_buf()
    } else {
        dir.join(common)
    };
    repo_from_common_dir(&abs)
}

/// Map a git common-dir path to a repo name, for *bare-backed* worktrees only. Pure (no IO) so
/// it stays unit-testable:
///   - `<repo>.git`  (a bare repo)        → `<repo>`
///   - `<repo>/.bare` (in-place bare)     → `<repo>` (the enclosing dir)
///   - `<repo>/.git`  (a standalone clone) → `None` — defer to the directory, so a clone nested
///     inside another repo's worktree keeps that umbrella grouping instead of splitting off.
pub(super) fn repo_from_common_dir(common: &Path) -> Option<String> {
    let name = common.file_name()?.to_string_lossy().into_owned();
    if name == ".bare" {
        return common
            .parent()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
    }
    // A real bare repo dir ends in `.git` with a non-empty stem (`arena.git` → `arena`).
    // A plain `.git` (stem empty) is a standalone clone → `None`, deferring to the directory.
    match name.strip_suffix(".git") {
        Some(stem) if !stem.is_empty() => Some(stem.to_string()),
        _ => None,
    }
}

/// Directory-derived repo name — the fallback when git can't name the common dir. Port of
/// the original two-case logic: under `worktree_dir` the repo is the first path segment;
/// otherwise it's the git toplevel's basename, with the in-place `.bare`-sibling override.
fn repo_from_dir(cfg: &Config, dir: &Path) -> String {
    // Case A: under the worktree dir → repo is the directory segment.
    if let Some(repo) = repo_segment(cfg, dir) {
        return repo;
    }
    // Case B: repo = basename(git toplevel), with .bare-sibling override.
    let toplevel = git_toplevel(dir);
    let mut repo = toplevel
        .as_deref()
        .and_then(|t| Path::new(t).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Case C: in-place `.bare` layout — toplevel basename is the branch dir, so use
    // the parent dir name as the repo instead.
    if let Some(t) = toplevel.as_deref() {
        let top = Path::new(t);
        if let Some(parent) = top.parent() {
            if parent.join(".bare").is_dir() {
                if let Some(name) = parent.file_name() {
                    repo = name.to_string_lossy().into_owned();
                }
            }
        }
    }
    repo
}

/// Apply the canonical note-key normalization: repo lowercased, branch slashes → dashes.
/// The single source of truth for how a `(repo, branch)` pair maps onto a note directory,
/// shared by directory-derived [`resolve`] and the explicit-target [`resolve_target`].
fn normalize(repo: String, branch: String) -> Resolved {
    Resolved {
        repo: repo.to_lowercase(),
        branch: branch.replace('/', "-"),
    }
}

/// Resolve a target note: an explicit `repo`+`branch` pair (normalized via [`normalize`]), a
/// `repo` alone — which means that repo's `main` — or, when neither is supplied, the context
/// derived from `dir`. A branch with no repo is rejected here so every caller gets the same
/// answer instead of one falling back to cwd.
///
/// A repo alone used to be a partial target too, and that refusal failed real work: a cron job
/// bound to the repo it serves with no branch runs as `bn run <job> --repo R`, so every such job
/// (`watch` sixty times a day, `pr-observe`, `agent-observe`) exited 1 with "--repo and --branch
/// must be given together" for work that only ever meant the repo. `main` is where the job tiers
/// resolve and where the repo's main slot sits, so it is the branch a repo-only target means.
pub fn resolve_target(
    cfg: &Config,
    dir: &Path,
    repo: Option<String>,
    branch: Option<String>,
) -> Result<Resolved, ContextError> {
    match (repo, branch) {
        (Some(r), Some(b)) => Ok(normalize(r, b)),
        (Some(r), None) => Ok(normalize(r, "main".to_string())),
        (None, None) => resolve(cfg, dir),
        (None, Some(_)) => Err(ContextError::PartialTarget),
    }
}

/// The git working-tree root for `dir` (`git rev-parse --show-toplevel`), or `None` when
/// `dir` isn't inside a git work tree. Lets callers locate repo-local config such as
/// `<root>/.bn/skills` without shelling out to git themselves.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git_toplevel(dir).map(PathBuf::from)
}

#[cfg(test)]
mod resolve_target_tests {
    use super::*;

    #[test]
    fn a_repo_alone_means_its_main() {
        let cfg = Config::default();
        let got = resolve_target(&cfg, Path::new("/nowhere"), Some("BigMono".to_string()), None)
            .expect("a repo alone is a whole target");
        assert_eq!(got.repo, "bigmono", "normalized like any explicit pair");
        assert_eq!(got.branch, "main");
    }

    #[test]
    fn a_branch_without_a_repo_is_still_partial() {
        let cfg = Config::default();
        assert!(matches!(
            resolve_target(&cfg, Path::new("/nowhere"), None, Some("feat".to_string())),
            Err(ContextError::PartialTarget)
        ));
    }
}
