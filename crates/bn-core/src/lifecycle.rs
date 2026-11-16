//! Git-managed repository and worktree inventory.
use crate::resolver::Config;
use crate::runner::{CommandRunner, RealRunner};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Every repo bn manages on this machine — the worktree layout's own inventory, read from
/// `<worktree_dir>/../bare`. This is the only source that knows about a repo you have cloned but
/// not yet taken a note on, so cross-repo views seed their repo list from it rather than showing
/// only the repos that happen to have notes. Empty when the bare dir is unreadable.
pub fn cloned_repos(cfg: &Config) -> BTreeSet<String> {
    cloned_repo_dirs(cfg).into_keys().collect()
}

/// [`cloned_repos`] with each repo's git dir alongside its name — for callers that then have to
/// *read* the clone (its remote, its config). The key is lowercased to match the note-canonical repo
/// casing while the path keeps the directory's real case, so the lookup works on a case-sensitive
/// filesystem where rebuilding `<bare>/<repo>.git` from the lowercased name would miss `BigMono.git`.
pub fn cloned_repo_dirs(cfg: &Config) -> BTreeMap<String, PathBuf> {
    managed_repos(&cfg.bare_dir())
}

/// The worktree-managed repos: the lowercased stems of every `<name>.git` in `bare_dir`, mapped to
/// the dir itself. Lowercased so it matches the note's repo key even when the bare dir keeps its
/// original case (`BigMono.git`).
fn managed_repos(bare_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(bare_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(repo) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .filter(|s| !s.is_empty())
        {
            map.insert(repo.to_lowercase(), path.clone());
        }
    }
    map
}

/// Live worktree PATHS per repo, `repo_lc → { dashed-branch → worktree_path }`, discovered via git.
/// The pool-aware answer to "where is `<branch>` actually checked out": a branch dispatched into a
/// pool slot (`_pool/N`) lives there, not at the conventional `<worktree_dir>/<repo>/<branch>`, so
/// any consumer that needs the on-disk worktree (the session registry, `bn resume`) must consult
/// git rather than reconstruct the conventional path. Only repos whose bare git enumerated
/// successfully appear; a repo absent here (unreadable / not a real bare) leaves callers to their
/// own fallback. Keyed like note keys (`crate::resolver::normalize`): repo lowercased, branch
/// slashes→dashes.
pub fn live_worktree_paths(bare_dir: &Path) -> HashMap<String, HashMap<String, PathBuf>> {
    let mut map: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(bare_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(repo) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(porcelain) = git_worktree_list(&path) else {
            continue;
        };
        map.insert(
            repo.to_lowercase(),
            worktree_paths_from_porcelain(&porcelain),
        );
    }
    map
}

/// Where one branch is actually checked out, asked of git rather than reconstructed.
///
/// `<worktree_dir>/<repo>/<branch>` is only a convention: a dispatched branch lives in a pool slot
/// (`_pool/N`), and a worktree directory reused for later work keeps its original name. Any caller
/// that needs a real cwd — resuming a session, running a harness turn — must consult git, or it
/// ends up running in a directory that does not exist. One `git worktree list` for the named repo,
/// so it is cheap enough for a single lookup (the whole-machine sweep is [`live_worktree_paths`]).
pub fn worktree_path(cfg: &Config, repo: &str, branch: &str) -> Option<PathBuf> {
    let bare = cfg.bare_dir().join(format!("{}.git", repo.to_lowercase()));
    let porcelain = git_worktree_list(&bare)?;
    worktree_paths_from_porcelain(&porcelain)
        .remove(&branch.replace('/', "-"))
        .filter(|p| p.is_dir())
}

/// The `dashed-branch → worktree_path` pairs from `git worktree list --porcelain`: each `worktree
/// <path>` block is followed by a `branch refs/heads/<b>` line (the bare's own entry and any
/// detached-HEAD worktree carry no such line, so they are naturally skipped — the pending
/// `worktree` path is simply overwritten by the next block). Slashes in the branch collapse to
/// dashes to match the note/worktree-dir key shape.
fn worktree_paths_from_porcelain(porcelain: &str) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(path) = current.take() {
                map.insert(branch.replace('/', "-"), path);
            }
        }
    }
    map
}

/// `git --git-dir <bare> worktree list --porcelain`. `Some(stdout)` on success — an empty-worktree
/// bare still prints its own `bare` line, so success is non-empty and distinguishable from the
/// `None` a real failure (git missing, corrupt repo) returns.
fn git_worktree_list(bare: &Path) -> Option<String> {
    let bare_s = bare.to_string_lossy();
    let out = RealRunner.git(&[
        "--git-dir",
        bare_s.as_ref(),
        "worktree",
        "list",
        "--porcelain",
    ]);
    out.success.then_some(out.stdout)
}
