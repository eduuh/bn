//! Read-side rollups: `todo`, `list`, `summary`, `status`. These scan this machine's notes
//! under each storage root (personal then work) in the machine-subtree layout
//! `<root>/<machine>/<repo>/<branch>/note.md`, keep active+local notes, and produce the JSON
//! contract payloads. Per-item `line` is the item's line in the single-file rendering.

use crate::completed::{CompletedTodo, NoteLink, NoteLinker};
use crate::model::{Item, Note};
use crate::resolver::{Config, Resolved};
use crate::runner::CommandRunner;
use crate::store;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A discovered branch note with its derived repo/branch and absolute path.
pub struct BranchNote {
    pub repo: String,
    pub branch: String,
    pub path: PathBuf,
    pub note: Note,
}

impl BranchNote {
    fn open_todos(&self) -> impl Iterator<Item = &Item> {
        self.note.front.todos.iter().filter(|t| t.is_open_todo())
    }

    /// id → line in the single-file rendering, for the nvim cursor contract. Built on demand
    /// (render + reparse) — only `todos`/`summary` need it, so the cross-branch views that don't
    /// (list, worktrees, doctor, metrics, lifecycle) skip the work entirely.
    fn line_map(&self) -> HashMap<String, usize> {
        store::render(&self.note)
            .map(|t| store::item_line_map(&t))
            .unwrap_or_default()
    }
}

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
    pub work_item: Option<crate::model::Scalar>,
    pub pr: Option<crate::model::Scalar>,
}

/// All active, local branch notes, in oracle order (personal root then work root, each
/// sorted lexicographically by repo then branch).
pub fn all_notes(cfg: &Config) -> Vec<BranchNote> {
    machine_notes(cfg, true)
}

/// Every branch note under this machine's subtree, in oracle order (personal root then work
/// root, each sorted lexicographically by repo then branch). When `active_only` is true the
/// closed notes are dropped (the default cross-branch view); when false, closed notes are
/// included too — what the lifecycle commands (`prune`, `archive`) need. The walk is already
/// anchored to `<root>/<machine>/`, so every note returned is local by construction; other
/// machines' subtrees and the root-level `archive/` sit outside the anchor.
pub fn machine_notes(cfg: &Config, active_only: bool) -> Vec<BranchNote> {
    let mut out = Vec::new();
    for root in cfg.each_notes_root() {
        let host_root = root.join(cfg.machine());
        for repo_dir in sorted_subdirs(&host_root) {
            let repo = file_name(&repo_dir);
            // Defensive: skip an archive tier should one ever land under the machine dir.
            if repo == "archive" {
                continue;
            }
            // On a work machine, personal repos never surface in cross-repo views.
            if !cfg.is_visible_repo(&repo) {
                continue;
            }
            for branch_dir in sorted_subdirs(&repo_dir) {
                let note_md = branch_dir.join("note.md");
                if !note_md.is_file() {
                    continue;
                }
                let Ok(note) = store::read(&note_md) else {
                    continue;
                };
                if active_only && !is_active_local(cfg, &note) {
                    continue;
                }
                out.push(BranchNote {
                    repo: repo.clone(),
                    branch: file_name(&branch_dir),
                    path: note_md,
                    note,
                });
            }
        }
    }
    out
}

/// One logged achievement: a dated `- <date> — <text>` bullet from a note's `achievements.md`.
#[derive(Debug, Clone, Serialize)]
pub struct Achievement {
    pub repo: String,
    pub branch: String,
    pub date: String,
    pub text: String,
}

/// Every logged achievement across all local notes (active **and** closed — most achievements
/// land on a note as it ships, then the note closes), parsed from each note dir's
/// `achievements.md`. Lines that aren't a `- <YYYY-MM-DD> — <text>` bullet are skipped, so a
/// hand-edited file never poisons the read.
pub fn achievements(cfg: &Config) -> Vec<Achievement> {
    let mut out = Vec::new();
    for bn in machine_notes(cfg, false) {
        let Some(dir) = bn.path.parent() else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dir.join("achievements.md")) else {
            continue;
        };
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("- ") else {
                continue;
            };
            // The writer joins date and text with " — " (em dash); split on the first one.
            let Some((date, body)) = rest.split_once(" — ") else {
                continue;
            };
            let date = date.trim();
            if !crate::util::is_iso_date(date) {
                continue;
            }
            out.push(Achievement {
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                date: date.to_string(),
                text: body.trim().to_string(),
            });
        }
    }
    out
}

/// Open todos across all branches.
pub fn todos(cfg: &Config) -> Vec<TodoEntry> {
    let mut out = Vec::new();
    for bn in all_notes(cfg) {
        let path = bn.path.display().to_string();
        let lines = bn.line_map();
        for t in bn.open_todos() {
            out.push(TodoEntry {
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                path: path.clone(),
                id: t.id.clone(),
                text: t.text.clone(),
                line: lines.get(&t.id).copied().unwrap_or(0),
                pct: t.pct,
            });
        }
    }
    out
}

/// Every **completed** branch-note todo across this machine's notes — the closed-todo half of the
/// Completed fold (#561). Walks active **and** closed notes (a todo often lands done on a note that
/// then closes), yielding each `done == Some(true)` todo with its `done_at` stamp for the recency
/// sort. Only todos carry `done`/`done_at`, so non-todo note items are naturally excluded.
pub fn completed_todos(cfg: &Config) -> Vec<CompletedTodo> {
    let mut out = Vec::new();
    for bn in machine_notes(cfg, false) {
        for t in &bn.note.front.todos {
            if t.done != Some(true) {
                continue;
            }
            out.push(CompletedTodo {
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                id: t.id.clone(),
                text: t.text.clone(),
                done_at: t.done_at.clone().unwrap_or_default(),
            });
        }
    }
    out
}

/// Per-branch rollup with open-todo and blocker counts.
pub fn list(cfg: &Config) -> Vec<ListEntry> {
    all_notes(cfg)
        .into_iter()
        .map(|bn| ListEntry {
            repo: bn.repo.clone(),
            branch: bn.branch.clone(),
            path: bn.path.display().to_string(),
            status: bn.note.front.status.clone(),
            open_todos: bn.open_todos().count(),
            blockers: bn.note.front.blockers.len(),
        })
        .collect()
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

/// Like `list`, but each entry also carries its on-disk worktree dir (for "switch to
/// worktree"). Note-only branches (no checkout) get `worktree: null`.
pub fn worktrees(cfg: &Config) -> Vec<WorktreeEntry> {
    all_notes(cfg)
        .into_iter()
        .map(|bn| {
            let wt = cfg.worktree_dir.join(&bn.repo).join(&bn.branch);
            WorktreeEntry {
                worktree: wt.is_dir().then(|| wt.display().to_string()),
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                path: bn.path.display().to_string(),
                status: bn.note.front.status.clone(),
                open_todos: bn.open_todos().count(),
                blockers: bn.note.front.blockers.len(),
            }
        })
        .collect()
}

/// Per-branch rollup with the open todos and blockers themselves.
pub fn summary(cfg: &Config) -> Vec<SummaryEntry> {
    all_notes(cfg)
        .into_iter()
        .map(|bn| {
            let lines = bn.line_map();
            let todos = bn
                .open_todos()
                .map(|t| ItemRef {
                    id: t.id.clone(),
                    text: t.text.clone(),
                    line: lines.get(&t.id).copied().unwrap_or(0),
                    created: t.created.clone(),
                })
                .collect();
            let blockers = bn
                .note
                .front
                .blockers
                .iter()
                .map(|b| ItemRef {
                    id: b.id.clone(),
                    text: b.text.clone(),
                    line: lines.get(&b.id).copied().unwrap_or(0),
                    created: b.created.clone(),
                })
                .collect();
            SummaryEntry {
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                path: bn.path.display().to_string(),
                status: bn.note.front.status.clone(),
                todos,
                blockers,
            }
        })
        .collect()
}

/// Status for the current branch only. Unlike the other rollups, this does NOT apply the
/// active/local filter (matches the oracle): it reports the note if it exists.
pub fn status(cfg: &Config, ctx: &Resolved) -> Option<StatusEntry> {
    let note_md = cfg.note_file(ctx);
    let note = store::read(&note_md).ok()?;
    Some(StatusEntry {
        repo: ctx.repo.clone(),
        branch: ctx.branch.clone(),
        open_todos: note.front.todos.iter().filter(|t| t.is_open_todo()).count(),
        blockers: note.front.blockers.len(),
        work_item: note.front.work_item.clone(),
        pr: note.front.pr.clone(),
    })
}

/// A note is shown in cross-branch views when it is active (status != "closed") and
/// local (machine empty or == this host). Ports `_is_active_local` (bn:1062).
///
/// The host match is **case-insensitive**: hostnames are case-insensitive by convention, and the
/// same machine genuinely reports both casings depending on who asked — macOS answers `Mac.lan`
/// from the Bonjour/SystemConfiguration name and `mac.lan` from `hostname`. A case-sensitive `==`
/// silently dropped every note stamped with the other casing, so whole repos vanished from
/// `bn status` and the dashboard's repo list while their notes sat right there on disk.
fn is_active_local(cfg: &Config, note: &Note) -> bool {
    let active = note.front.status != "closed";
    let machine = &note.front.machine;
    let local = machine.is_empty() || machine.eq_ignore_ascii_case(&cfg.hostname);
    active && local
}

fn sorted_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Note-link resolution (#561) — the local-path + remote-URL jump for a branch note.
// ---------------------------------------------------------------------------

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

/// Build a GitHub-style blob URL to a file at `rel_path` on `branch` of the repo at `web_base`.
/// Pure; `rel_path` is expected already forward-slashed and repo-relative.
pub fn note_blob_url(web_base: &str, branch: &str, rel_path: &str) -> String {
    format!(
        "{}/blob/{}/{}",
        web_base.trim_end_matches('/'),
        branch,
        rel_path.trim_start_matches('/')
    )
}

/// The resolved remote coordinates of one notes-storage root: its web base, the checked-out
/// branch, and the git top-level (so a note path can be made repo-relative). `None` for a root
/// that isn't a git repo or has no resolvable `origin`.
#[derive(Debug, Clone)]
struct RemoteInfo {
    web_base: String,
    branch: String,
    toplevel: PathBuf,
}

/// The production [`NoteLinker`]: resolves a `(repo, branch)` to its branch note's local path (the
/// machine-subtree `note.md`, when present on this host) and a remote blob URL (from the notes
/// repo's git `origin`). Shells git via the injected [`CommandRunner`] and memoizes the per-root
/// remote so a list of many completed items resolves with at most one git probe per storage root.
pub struct FsNoteLinker<'a> {
    cfg: &'a Config,
    runner: &'a dyn CommandRunner,
    /// notes-root → resolved remote (or `None` when the root has no usable git remote), memoized.
    cache: RefCell<HashMap<PathBuf, Option<RemoteInfo>>>,
}

impl<'a> FsNoteLinker<'a> {
    /// Build a linker over `cfg`, shelling git through `runner`.
    pub fn new(cfg: &'a Config, runner: &'a dyn CommandRunner) -> Self {
        Self {
            cfg,
            runner,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// The absolute `note.md` path for `repo`/`branch` under this machine's subtree.
    fn note_path(&self, repo: &str, branch: &str) -> PathBuf {
        self.cfg
            .notes_dir_for(repo)
            .join(self.cfg.machine())
            .join(repo)
            .join(branch)
            .join("note.md")
    }

    /// Resolve (and memoize) the remote coordinates of the storage root backing `repo`.
    fn remote_for(&self, repo: &str) -> Option<RemoteInfo> {
        let root = self.cfg.notes_dir_for(repo).to_path_buf();
        if let Some(hit) = self.cache.borrow().get(&root) {
            return hit.clone();
        }
        let root_str = root.to_string_lossy().to_string();
        let toplevel = self
            .runner
            .git(&["-C", &root_str, "rev-parse", "--show-toplevel"])
            .ok_stdout()
            .map(PathBuf::from);
        let origin = self
            .runner
            .git(&["-C", &root_str, "remote", "get-url", "origin"])
            .ok_stdout();
        let info = match (toplevel, origin.as_deref().and_then(remote_web_base)) {
            (Some(toplevel), Some(web_base)) => {
                let branch = self
                    .runner
                    .git(&["-C", &root_str, "rev-parse", "--abbrev-ref", "HEAD"])
                    .ok_stdout()
                    .filter(|b| b != "HEAD")
                    .unwrap_or_else(|| "main".to_string());
                Some(RemoteInfo {
                    web_base,
                    branch,
                    toplevel,
                })
            }
            _ => None,
        };
        self.cache.borrow_mut().insert(root, info.clone());
        info
    }
}

impl NoteLinker for FsNoteLinker<'_> {
    fn link(&self, repo: &str, branch: &str) -> NoteLink {
        let path = self.note_path(repo, branch);
        let local_path = path.is_file().then(|| path.display().to_string());
        let remote_url = self.remote_for(repo).and_then(|info| {
            let rel = path.strip_prefix(&info.toplevel).ok()?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            Some(note_blob_url(&info.web_base, &info.branch, &rel))
        });
        NoteLink {
            local_path,
            remote_url,
        }
    }
}
