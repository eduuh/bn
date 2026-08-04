//! Related-branch recall: for a given branch, the *past* branches that touched the same
//! files — zero-effort discovery of prior art, the recall axis PKM tools nail and a per-branch
//! note silo misses. Knowledge written on `bn/wsl-paths` is invisible from `bigmono/x` until you
//! remember to search; this surfaces it automatically.
//!
//! Wholly offline, derived from data bn already has: git tells us which files each branch
//! touched (`git diff --name-only <base>...<branch>`), so overlap is free. Ranking is by
//! shared-file count, with a shared work-item area as a tiebreak. All git reads go through the
//! [`CommandRunner`] seam so it's testable with [`FakeRunner`] — no network, no new tracking.

use crate::model::Scalar;
use crate::resolver::Config;
use crate::runner::CommandRunner;
use crate::store;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One past branch that overlaps the target, with the evidence and the note metadata that
/// makes it worth jumping to (decisions recorded there, whether it's still live).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RelatedBranch {
    pub repo: String,
    pub branch: String,
    /// The owning note's status (`active`/`closed`), or empty when no note exists for the branch.
    pub status: String,
    /// Files touched by both this branch and the target, sorted.
    pub shared_files: Vec<String>,
    /// `shared_files.len()`, hoisted so JSON consumers can rank without recounting.
    pub shared_count: usize,
    /// True when this branch's note shares the target's work-item id (same work area).
    pub work_item_match: bool,
    /// Number of decisions recorded on this branch's note — the prior art most worth recalling.
    pub decisions: usize,
    /// Absolute path to the branch note, when one exists (for one-keystroke open).
    pub path: Option<String>,
}

/// The note metadata we attach to a related branch, keyed by branch name within a repo.
struct NoteMeta {
    repo: String,
    status: String,
    decisions: usize,
    work_item: Option<String>,
    path: PathBuf,
}

/// Past branches that touched the same files as `target` (default: the current branch), ranked
/// by shared-file overlap then by shared work-item. `limit` caps the result (0 = uncapped).
///
/// Offline: file-touch sets come from `git diff --name-only <base>...<branch>` via `runner`,
/// note metadata from the on-disk notes. Branches with zero overlap are dropped. The target
/// itself is never returned.
pub fn related_branches(
    cfg: &Config,
    runner: &dyn CommandRunner,
    target: Option<&str>,
    limit: usize,
) -> Vec<RelatedBranch> {
    let target = match target {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => match current_branch(runner) {
            Some(b) => b,
            None => return Vec::new(),
        },
    };

    let Some(base) = base_ref(runner) else {
        eprintln!(
            "bn: related-branch recall skipped — no base branch to diff against (origin/HEAD \
             unset, no branch upstream, no local main/master). Run `git remote set-head origin \
             -a` to record the remote's default branch."
        );
        return Vec::new();
    };
    let target_files = touched_files(runner, &base, &target);
    if target_files.is_empty() {
        return Vec::new();
    }

    let notes = index_notes(cfg);
    let target_work_item = notes.get(&target).and_then(|m| m.work_item.clone());

    let mut out = Vec::new();
    for branch in local_branches(runner) {
        if branch == target {
            continue;
        }
        let files = touched_files(runner, &base, &branch);
        let mut shared: Vec<String> = files
            .iter()
            .filter(|f| target_files.contains(*f))
            .cloned()
            .collect();
        if shared.is_empty() {
            continue;
        }
        shared.sort();
        let meta = notes.get(&branch);
        let work_item_match = match (&target_work_item, meta.and_then(|m| m.work_item.as_ref())) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        out.push(RelatedBranch {
            repo: meta.map(|m| m.repo.clone()).unwrap_or_default(),
            branch,
            status: meta.map(|m| m.status.clone()).unwrap_or_default(),
            shared_count: shared.len(),
            shared_files: shared,
            work_item_match,
            decisions: meta.map(|m| m.decisions).unwrap_or(0),
            path: meta.map(|m| m.path.display().to_string()),
        });
    }

    // Rank: most shared files first, then shared work-item, then more decisions, then name
    // (stable, deterministic — important for the FakeRunner tests).
    out.sort_by(|a, b| {
        b.shared_count
            .cmp(&a.shared_count)
            .then(b.work_item_match.cmp(&a.work_item_match))
            .then(b.decisions.cmp(&a.decisions))
            .then(a.branch.cmp(&b.branch))
    });
    if limit > 0 && out.len() > limit {
        out.truncate(limit);
    }
    out
}

/// The current branch name via `git rev-parse --abbrev-ref HEAD`, or `None` (detached / not a
/// repo / git absent).
fn current_branch(runner: &dyn CommandRunner) -> Option<String> {
    let name = runner
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .ok_stdout()?;
    (name != "HEAD").then_some(name)
}

/// The base ref to diff branches against, scoping each branch's touched-file set to *its own*
/// work rather than the whole history. Resolution, all offline (never `git remote show origin`,
/// which hits the network):
///   1. `origin/HEAD`'s short name — the remote's default branch, when recorded locally.
///   2. the current branch's upstream (e.g. `origin/develop`) — the real default on a repo whose
///      default branch is custom *and* whose `origin/HEAD` was never set (`git remote set-head`).
///   3. local `main`, then local `master`.
///
/// Returns `None` when none resolve, so the caller degrades with a diagnostic instead of diffing
/// against a literal `main` that doesn't exist — which silently yields an empty result.
fn base_ref(runner: &dyn CommandRunner) -> Option<String> {
    if let Some(sym) = runner
        .git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok_stdout()
    {
        if !sym.is_empty() {
            return Some(sym);
        }
    }
    if let Some(upstream) = runner
        .git(&["rev-parse", "--abbrev-ref", "@{upstream}"])
        .ok_stdout()
    {
        if verify_ref(runner, &upstream) {
            return Some(upstream);
        }
    }
    for candidate in ["main", "master"] {
        if verify_ref(runner, candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

/// True when `git rev-parse --verify --quiet <r>` resolves `r` to an object in this repo.
fn verify_ref(runner: &dyn CommandRunner, r: &str) -> bool {
    runner.git(&["rev-parse", "--verify", "--quiet", r]).success
}

/// Local branch short-names via `git for-each-ref --format='%(refname:short)' refs/heads`.
fn local_branches(runner: &dyn CommandRunner) -> Vec<String> {
    let out = runner.git(&["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
    match out.ok_stdout() {
        Some(s) => s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// Files `branch` touched relative to `base`, via `git diff --name-only <base>...<branch>` (the
/// three-dot form: changes on `branch` since it diverged, ignoring later `base` commits). Empty
/// on any failure (branch is the base, base missing, git absent) — best-effort, never an error.
fn touched_files(runner: &dyn CommandRunner, base: &str, branch: &str) -> Vec<String> {
    let range = format!("{base}...{branch}");
    let out = runner.git(&["diff", "--name-only", &range]);
    match out.ok_stdout() {
        Some(s) => s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// Index every branch note (active + archived, all roots) by branch name, keeping the most
/// recently-created note when two branches share a name. Mirrors `search::find_notes` so the
/// recall corpus is the same.
fn index_notes(cfg: &Config) -> BTreeMap<String, NoteMeta> {
    let mut by_branch: BTreeMap<String, (String, NoteMeta)> = BTreeMap::new();
    for root in cfg.each_notes_root() {
        let mut files = Vec::new();
        find_notes(root, &mut files);
        for note_md in files {
            let Ok(note) = store::read(&note_md) else {
                continue;
            };
            let f = &note.front;
            let created = f.created.clone();
            let meta = NoteMeta {
                repo: f.repo.clone(),
                status: f.status.clone(),
                decisions: f.decisions.len(),
                work_item: f.work_item.as_ref().map(Scalar::display),
                path: note_md.clone(),
            };
            match by_branch.get(&f.branch) {
                // Keep the most recently-created note when a branch name recurs.
                Some((prev_created, _)) if *prev_created >= created => {}
                _ => {
                    by_branch.insert(f.branch.clone(), (created, meta));
                }
            }
        }
    }
    // Drop the created-at tiebreak key, keeping just the chosen meta per branch.
    by_branch.into_iter().map(|(k, (_, m))| (k, m)).collect()
}

/// Recursively collect every `note.md` under `dir`, skipping VCS/dependency dirs. (Same walk
/// as `search::find_notes`; kept local so the two recall paths don't couple.)
fn find_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, ".git" | "node_modules") {
                continue;
            }
            find_notes(&path, out);
        } else if path.file_name().and_then(|s| s.to_str()) == Some("note.md") {
            out.push(path);
        }
    }
}


