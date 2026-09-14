//! Shared foundation for the `wt` submodules: project layout, bare-repo discovery,
//! git + fzf helpers, path utilities, and the ANSI palette. Moved verbatim from the
//! former wt.rs god file (issue #351).

use super::*;
use bn_core::style;

// The `wt` commands wrap a whole message in one colour run (`{RED}…{NC}`) rather than calling a
// helper per fragment, so they need the codes as `const` items. They are derived from the shared
// palette rather than respelled — these used to be the retired tmux-lib.sh's own `0;3xm` set, a
// second palette that drifted from `bn`'s.
//
// BLUE is gone: every one of its 27 sites marked a section or action header ("Merging PR for …",
// "pool metrics — N slots"), and headers are bold-and-uncoloured everywhere else in bn so they
// never compete with the rows that are actually red. It now aliases the head tone.
pub(crate) const RED: &str = style::Tone::Danger.sgr();
pub(crate) const GREEN: &str = style::Tone::Ok.sgr();
pub(crate) const YELLOW: &str = style::Tone::Warn.sgr();
pub(crate) const BLUE: &str = style::Tone::Head.sgr();
pub(crate) const NC: &str = style::RESET;
// Re-exported to the sibling `wt_pool` module so pool output matches the rest of `wt`.
pub(crate) const P_RED: &str = RED;
pub(crate) const P_GREEN: &str = GREEN;
pub(crate) const P_YELLOW: &str = YELLOW;
pub(crate) const P_BLUE: &str = BLUE;
pub(crate) const P_NC: &str = NC;

// --- layout ----------------------------------------------------------------

/// `(bare_dir, worktree_dir)` for the active project root. `$WT_ROOT` overrides bn's config so
/// the `wtw` wrapper can drive the same logic against a different filesystem root.
pub(crate) fn layout(cfg: &Config) -> (PathBuf, PathBuf) {
    match std::env::var("WT_ROOT") {
        Ok(root) if !root.is_empty() => {
            let root = PathBuf::from(root);
            (root.join("bare"), root.join("worktree"))
        }
        _ => (cfg.bare_dir(), cfg.worktree_dir.clone()),
    }
}

/// Bare repo names (`<bare_dir>/*.git` directories), without the `.git` suffix.
pub(crate) fn bare_repos(bare_dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(bare_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().map(|x| x == "git").unwrap_or(false))
        .collect();
    out.sort();
    out
}

pub(crate) fn repo_name(bare: &Path) -> String {
    bare.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Resolve a repo *name* to its bare-repo dir under the configured bare root, for from-anywhere
/// targeting (`bn dispatch --repo <name>`): the caller need not be inside the repo, or inside any
/// repo at all. The name may be the **local bare stem** (`tracker`), a **GitHub/ADO repo name**
/// (`wira360`), or a full **`owner/repo` slug** — the #452 [`bn_core::repo_resolve`] policy maps
/// any of them onto the local bare (so `--repo wira360` finds `tracker.git`). Matching is
/// case-insensitive and `.git`-tolerant. On success returns the resolved name (canonical casing) +
/// bare path; on failure, an `Err` message listing the known repos so a typo is self-correcting.
/// `None`/empty name → `Ok(None)` so callers can fall back to cwd context.
pub(crate) fn resolve_bare_by_name(
    cfg: &Config,
    name: Option<&str>,
) -> Result<Option<(String, PathBuf)>, String> {
    let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let (bare_dir, _) = layout(cfg);
    let bares = bare_repos(&bare_dir);
    if bares.is_empty() {
        return Err(format!(
            "no bare repos under {} — clone one first (bn wt clone <url>)",
            bare_dir.display()
        ));
    }
    // Build each clone's identity (local stem + its origin slug) so the pure #452 resolver can map
    // a GitHub name onto the local bare, not just the local stem. Reading the remote is a cheap
    // one-off next to the worktree/agent work a dispatch then does.
    let identities: Vec<bn_core::repo_resolve::RepoIdentity> = bares
        .iter()
        .map(|b| {
            let local = repo_name(b);
            match remote_for(b) {
                Remote::Other => bn_core::repo_resolve::RepoIdentity::local_only(local),
                remote => bn_core::repo_resolve::RepoIdentity::new(local, remote.label()),
            }
        })
        .collect();
    match bn_core::repo_resolve::resolve_repo_name(name, &identities) {
        Ok(local) => {
            let bare = bare_dir.join(format!("{local}.git"));
            Ok(Some((local, bare)))
        }
        Err(e) => Err(e.message()),
    }
}

pub(crate) fn sanitize_branch(branch: &str) -> String {
    branch.replace('/', "-")
}

/// Repos cloned as a regular (non-bare) checkout rather than bare+worktree. Empty by default; set
/// `BN_REGULAR_CLONE_REPOS` to a comma/space/newline-separated list of repo names. The per-user
/// override (the shared `setup/regular-repos.zsh`) lives outside this public repo.
pub(crate) fn is_regular_clone(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    std::env::var("BN_REGULAR_CLONE_REPOS")
        .map(|list| {
            list.split([',', ' ', '\n', '\t'])
                .any(|entry| entry.trim() == name)
        })
        .unwrap_or(false)
}

// --- git helpers -----------------------------------------------------------

/// Run git with args, returning trimmed stdout on success. Thin delegate to the runner-backed
/// [`crate::git::git_capture`] (trim=true preserves this wrapper's long-standing trimming).
pub(crate) fn git_out(args: &[&str]) -> Option<String> {
    crate::git::git_capture(args, None, true)
}

/// Find the bare repo backing a directory, by reading the worktree's `.git` pointer file and
/// walking up to `<repo>.git`. Mirrors the oracle's `find_bare_repo`.
pub(crate) fn find_bare_repo(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        let gitfile = dir.join(".git");
        if gitfile.is_file() {
            if let Ok(content) = std::fs::read_to_string(&gitfile) {
                let raw = content
                    .trim()
                    .strip_prefix("gitdir:")
                    .unwrap_or(content.trim())
                    .trim();
                let gitdir = if Path::new(raw).is_absolute() {
                    PathBuf::from(raw)
                } else {
                    dir.join(raw)
                };
                // gitdir = <bare>/worktrees/<name>; the bare repo is two levels up.
                if let Some(bare) = gitdir.parent().and_then(|p| p.parent()) {
                    if let Ok(canon) = bare.canonicalize() {
                        if canon.join("HEAD").is_file() {
                            return Some(canon);
                        }
                    }
                }
            }
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => return None,
        }
    }
}

/// Non-empty trimmed lines of `stdout` — one commit SHA per line. Split out so the parse is
/// testable without shelling out to `gh`/`az`.
pub(crate) fn oid_lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// --- fzf -------------------------------------------------------------------

/// Present `items` in fzf and return the chosen line, or None if cancelled / fzf missing.
pub(crate) fn fzf_select(items: &[String], header: &str) -> Option<String> {
    let mut child = Command::new("fzf")
        .args(["--reverse", "--header", header])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let input = items.join("\n");
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sel.is_empty() {
        None
    } else {
        Some(sel)
    }
}

/// Multi-select variant of [`fzf_select`] (`fzf -m`): returns every chosen line (Tab to mark),
/// or an empty Vec if cancelled / nothing picked / fzf missing.
pub(crate) fn fzf_select_multi(items: &[String], header: &str) -> Vec<String> {
    let Ok(mut child) = Command::new("fzf")
        .args(["--reverse", "--multi", "--header", header])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    else {
        return Vec::new();
    };
    let input = items.join("\n");
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let Ok(out) = child.wait_with_output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The bare repo dir for `<repo>.git` under the active layout, or `None` if it doesn't exist.
///
/// Matched case-insensitively on a miss: a `Resolved.repo` is lowercased by the note resolver
/// (`bigmono`) while the bare dir keeps the clone's case (`BigMono.git`), so a caller holding a resolved
/// context would otherwise miss the repo on every case-sensitive filesystem.
pub(crate) fn bare_for(cfg: &Config, repo: &str) -> Option<PathBuf> {
    let (bare_dir, _) = layout(cfg);
    let want = format!("{repo}.git");
    let want_lower = want.to_lowercase();

    // Always resolve through the directory listing, never by probing `bare_dir/<repo>.git`
    // directly. On a case-insensitive filesystem (macOS APFS is one) that probe SUCCEEDS for
    // `bigmono.git` when only `BigMono.git` exists, and hands back the path as spelled rather than the
    // one on disk — so the same call returned a different string on macOS than on Linux. Git
    // does not care on macOS, but anything comparing or logging the path does, and it made this
    // function's result depend on the filesystem underneath it.
    //
    // An exact match always wins; a case-insensitive one is the fallback, which is the point of
    // the function: a job's repo name is lowercased by the target resolver while the
    // clone keeps its case, and both have to find the same bare repo or a pool warm for `BigMono`
    // reports "nothing to warm" and exits 0.
    let mut insensitive = None;
    for entry in std::fs::read_dir(&bare_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == want {
            return Some(path);
        }
        if insensitive.is_none() && name.to_lowercase() == want_lower {
            insensitive = Some(path);
        }
    }
    insensitive
}

// --- small helpers ---------------------------------------------------------

pub(crate) fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

pub(crate) fn home() -> PathBuf {
    PathBuf::from(home_str())
}

pub(crate) fn home_str() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// Resolve a worktree arg to an absolute path: absolute stays, otherwise it's a slug under the
/// worktree root.
pub(crate) fn abs_worktree(arg: &str, worktree_dir: &Path) -> PathBuf {
    if Path::new(arg).is_absolute() {
        PathBuf::from(arg)
    } else {
        worktree_dir.join(arg)
    }
}

/// The current bare repo, or an fzf pick across all bare repos. None means cancelled / none found.
pub(crate) fn resolve_bare_or_pick(bare_dir: &Path) -> Option<PathBuf> {
    if let Some(b) = find_bare_repo(&cwd()) {
        return Some(b);
    }
    let repos = bare_repos(bare_dir);
    if repos.is_empty() {
        ceprintln!(
            "{RED}Error: No bare repos found in {}{NC}",
            bare_dir.display()
        );
        return None;
    }
    let items: Vec<String> = repos
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    fzf_select(&items, "Select bare repo").map(PathBuf::from)
}

/// Remove the now-empty `<worktree_dir>/<repo>` parent of a removed worktree (best effort).
pub(crate) fn rmdir_if_empty_repo(wt_path: &Path, worktree_dir: &Path) {
    if let Some(repo_dir) = wt_path.parent() {
        if repo_dir.starts_with(worktree_dir) {
            let _ = std::fs::remove_dir(repo_dir); // only succeeds if empty
        }
    }
}

/// Run git inheriting stdio; returns whether it succeeded. Thin delegate to the shared
/// [`crate::git::git_run`] (byte-identical body): its callers (clone/fetch/push/`worktree add`)
/// rely on git's live progress reaching the terminal, which the capture-only runner would swallow.
pub(crate) fn run_git(args: &[&str]) -> bool {
    crate::git::git_run(args)
}

/// Minimal single-quote shell escaping for embedding a path in a tmux command string.
pub(crate) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod bare_for_tests {
    use super::*;

    fn cfg_at(root: &Path) -> Config {
        Config {
            worktree_dir: root.join("worktree"),
            ..Config::default()
        }
    }

    /// The name a job carries is lowercased by the target resolver; the clone keeps its
    /// case. Both have to find the same bare repo, or a pool warm for `BigMono` reports "nothing to
    /// warm" and exits 0.
    #[test]
    fn a_lowercased_repo_still_finds_its_bare_clone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("bare/BigMono.git")).unwrap();
        let cfg = cfg_at(tmp.path());
        let want = tmp.path().join("bare/BigMono.git");
        assert_eq!(bare_for(&cfg, "BigMono").as_deref(), Some(want.as_path()));
        assert_eq!(bare_for(&cfg, "bigmono").as_deref(), Some(want.as_path()));
        assert_eq!(bare_for(&cfg, "nope"), None);
    }
}
