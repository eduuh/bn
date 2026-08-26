//! Native `bn tat` — the project + worktree picker (prefix `C-o`). Phase 3 of the picker migration:
//! ports `workflow/bin/tat` (and its `worktree.sh` primitives) into the binary, dispatching into
//! the native worktree engine (`wt.rs`) that already owns clone / worktree-add / session-per-repo.
//!
//! Like the `doctor`/`jump` pickers it is self-referencing — fzf's `start` reload, `--preview`, and
//! `ctrl-x` re-invoke `bn tat --pick-stage <list|preview|kill>`. The list rows are
//! `display \t ref`: fzf shows `display` (`--with-nth=1`) and the hidden `ref` rides along as the
//! last field (`{-1}`) for preview/kill/selection.
//!
//! Scope: the project + worktree picker across every root. Phase 3a landed the single-root core;
//! 3b (this slice) adds the cross-OS roots — WSL `C:\`→`/mnt/c` path normalization, the auto-loaded
//! Windows extra root (so one picker spans `~/projects` and `/mnt/c/repos`), and `--root win` (the
//! `tatw` replacement, scoping to the Windows root alone). Codespaces (3c) and plain `tmux` only —
//! `psmux` stays shell-side.

use crate::proc::{tmux, tmux_command, tmux_ok, tmux_raw, tmux_run};
use crate::wt;
use bincode::Options;
use bn_core::resolver::Config;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// The hidden callback stages a `tat` fzf session re-invokes (`bn tat --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum TatStage {
    /// Emit the decorated, frecency-sorted project list (the `start`/`ctrl-x` reload feed).
    List,
    /// Force a filesystem/frecency rescan, rewrite the discovery cache, then emit the list.
    Refresh,
    /// Render the preview pane for the focused row's hidden `ref`.
    Preview,
    /// Kill the session backing the focused row's hidden `ref` (the `ctrl-x` binding).
    Kill,
    /// Emit the per-project window/worktree/pool rows for `<name>` — the sub-picker's reload feed
    /// (mirrors the initial in-process build so a post-action `reload()` refreshes in place).
    #[value(name = "sublist")]
    SubList,
    /// Delete the focused worktree row (the `ctrl-d` binding). `--pick-arg` carries `kind~payload`;
    /// guarded — refuses a non-worktree row, a dirty tree, or unpushed commits.
    #[value(name = "rmworktree")]
    RmWorktree,
    /// Warm the focused repo's pool up to its configured size (the `ctrl-w` binding) — fills cold
    /// slots via `bn wt pool warm`. Guarded — refuses a repo that isn't pooled.
    #[value(name = "warmpool")]
    WarmPool,
    /// Release the focused *leased* pool slot back to the warm pool (the `ctrl-r` binding) —
    /// `bn wt pool return` drops the lease and resets the slot (kept warm). `--pick-arg` carries
    /// `kind~branch`; guarded — only a `poolbranch` row, and `return` itself refuses a dirty slot.
    #[value(name = "releasepool")]
    ReleasePool,
}

// --- ref helpers -----------------------------------------------------------
// A project is identified by a *ref*: "<repo>" (primary root) or "<repo>@<tag>" (an extra root).
// In phase 3a there are no tags, but the helpers stay total so 3b can light up `--root win`.

/// The bare repo name in a ref (everything before `@`).
fn ref_repo(r: &str) -> &str {
    r.split_once('@').map(|(repo, _)| repo).unwrap_or(r)
}

/// The extra-root tag in a ref (after `@`), or empty for a primary-root ref.
fn ref_tag(r: &str) -> &str {
    r.split_once('@').map(|(_, tag)| tag).unwrap_or("")
}

/// The tmux session name for a ref: the ref with `@` → `-` (so `arena@win` → `arena-win`).
fn ref_session(r: &str) -> String {
    r.replace('@', "-")
}

// --- roots (primary + tagged extra roots) ----------------------------------
// `tat` spans more than one project root: the primary (`~/projects`) plus tagged "extra roots"
// (e.g. `win` = the Windows filesystem `/mnt/c/repos` on WSL). A ref carries its root in its tag —
// untagged refs live in the primary, `repo@win` lives in the `win` root — so discovery/preview/
// dispatch stay stateless across fzf's self-reinvocations. `--root win` (the `tatw` replacement)
// instead scopes the whole picker to the Windows root as a lone primary.

/// The primary root plus any tagged extra roots, resolved once per invocation.
struct Roots {
    /// The primary project root (parent of `bare/`).
    primary: PathBuf,
    /// The primary root's bare dir (honours `$WT_ROOT` / a custom config layout).
    primary_bare: PathBuf,
    /// The primary root's worktree dir.
    primary_worktree: PathBuf,
    /// Tagged extra roots: `(tag, root_dir)`, each `<root_dir>/{bare,worktree}` by convention.
    extras: Vec<(String, PathBuf)>,
    /// `--root win` is active: the primary is the Windows root, so a clone must be steered there
    /// (via `WT_ROOT`) rather than the configured default.
    win_scoped: bool,
}

impl Roots {
    /// Resolve the root context for this invocation. `--root win` scopes to the Windows root as the
    /// sole primary (the `tatw` behaviour); otherwise the configured root is primary and the `win`
    /// extra root is auto-loaded so one picker spans both `~/projects` and `/mnt/c/repos`.
    fn resolve(cfg: &Config, root_opt: Option<&str>) -> Roots {
        let (cfg_bare, cfg_worktree) = wt::layout(cfg);
        let cfg_primary = cfg_bare
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cfg_bare.clone());
        if root_opt == Some("win") {
            let win = win_root().unwrap_or(cfg_primary);
            return Roots {
                primary: win.clone(),
                primary_bare: win.join("bare"),
                primary_worktree: win.join("worktree"),
                extras: Vec::new(),
                win_scoped: true,
            };
        }
        let extras = load_extra_roots(&cfg_primary);
        Roots {
            primary: cfg_primary,
            primary_bare: cfg_bare,
            primary_worktree: cfg_worktree,
            extras,
            win_scoped: false,
        }
    }

    /// The root dir a ref points at (its tag's extra root, or the primary when untagged/unknown).
    fn ref_root(&self, r: &str) -> &Path {
        let tag = ref_tag(r);
        if tag.is_empty() {
            return &self.primary;
        }
        self.extras
            .iter()
            .find(|(t, _)| t == tag)
            .map(|(_, p)| p.as_path())
            .unwrap_or(&self.primary)
    }

    /// A ref's bare dir: the primary's configured `bare/` for untagged refs, else `<root>/bare`.
    fn bare_dir(&self, r: &str) -> PathBuf {
        if ref_tag(r).is_empty() {
            self.primary_bare.clone()
        } else {
            self.ref_root(r).join("bare")
        }
    }

    /// A ref's worktree dir: the primary's configured `worktree/` for untagged refs, else
    /// `<root>/worktree`.
    fn worktree_dir(&self, r: &str) -> PathBuf {
        if ref_tag(r).is_empty() {
            self.primary_worktree.clone()
        } else {
            self.ref_root(r).join("worktree")
        }
    }

    /// Project refs across every root: bare/flat names for the primary, `<repo>@<tag>` for each
    /// extra root. A name present in two roots yields two refs (e.g. `arena` AND `arena@win`).
    fn all_refs(&self) -> Vec<String> {
        let mut out = list_root_projects(&self.primary);
        for (tag, path) in &self.extras {
            for repo in list_root_projects(path) {
                out.push(format!("{repo}@{tag}"));
            }
        }
        out
    }
}

/// Is `ref` a bare repo (i.e. `<ref-root>/bare/<repo>.git` exists)?
fn is_bare_repo(roots: &Roots, r: &str) -> bool {
    roots
        .bare_dir(r)
        .join(format!("{}.git", ref_repo(r)))
        .is_dir()
}

/// Where the "main" checkout for a bare repo lives (worktree if present, else the bare dir).
fn bare_repo_main_path(roots: &Roots, r: &str) -> PathBuf {
    let repo = ref_repo(r);
    let m = roots.worktree_dir(r).join(repo).join("main");
    if m.is_dir() {
        m
    } else {
        roots.bare_dir(r).join(format!("{repo}.git"))
    }
}

/// `ref` → path. `ref/branch` → a worktree path; a bare `ref` → the project root checkout.
fn resolve_project_path(roots: &Roots, r: &str) -> PathBuf {
    let refpart = r.split_once('/').map(|(a, _)| a).unwrap_or(r);
    match r.split_once('/') {
        Some((_, branch)) => roots
            .worktree_dir(refpart)
            .join(ref_repo(refpart))
            .join(branch),
        None => roots.ref_root(r).join(ref_repo(r)),
    }
}

/// The checkout path for a ref, used for frecency scoring (bare → main path, else project path).
fn project_path(roots: &Roots, r: &str) -> PathBuf {
    if is_bare_repo(roots, r) {
        bare_repo_main_path(roots, r)
    } else {
        resolve_project_path(roots, r)
    }
}

// A leading `$HOME` is *not* collapsed to `~`: the oracle's `${path/#$HOME/~}` is a silent no-op
// (bash tilde-expands the `~` replacement back to `$HOME`), so paths render in full, verbatim.

// --- cross-OS roots (WSL path normalization + the Windows extra root) -------

/// True on WSL — the kernel release carries a `microsoft`/`WSL` marker. Mirrors `adapters.sh`'s
/// `WF_WSL` detection.
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

/// A Windows path (`C:\dir` / `C:/dir`) → its WSL path (`/mnt/c/dir`) via `wslpath`; identity off
/// WSL or for an already-unix path. Mirrors `adapters.sh:wf_unixpath`.
fn wf_unixpath(p: &str) -> String {
    let b = p.as_bytes();
    let drive_prefixed = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'/' || b[2] == b'\\');
    if is_wsl() && drive_prefixed {
        if let Ok(out) = Command::new("wslpath").args(["-u", p]).output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    p.to_string()
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

/// The Windows-filesystem repos root (the `--root win` / `tatw` scope): machine-local
/// `~/.config/wt/win-root`, else `<%USERPROFILE%>/code` resolved via `wslpath`. WSL-normalized and
/// de-slashed; `None` when neither resolves.
fn win_root() -> Option<PathBuf> {
    let cfg_path = config_dir().join("wt").join("win-root");
    let raw = match std::fs::read_to_string(&cfg_path) {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ if is_wsl() => {
            let home = Command::new("cmd.exe")
                .args(["/c", "echo %USERPROFILE%"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().replace('\r', ""))
                .filter(|s| !s.is_empty())?;
            let unix = Command::new("wslpath")
                .arg(&home)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
            format!("{unix}/code")
        }
        _ => return None,
    };
    let norm = wf_unixpath(raw.trim_end_matches('/'));
    let norm = norm.trim_end_matches('/');
    (!norm.is_empty()).then(|| PathBuf::from(norm))
}

/// Extra project roots beyond the primary: `WF_EXTRA_ROOTS` (`tag=path:tag2=path2`) when set, else
/// the auto-loaded `win=<win-root config>`. Delegates parsing to [`parse_extra_roots`].
fn load_extra_roots(primary: &Path) -> Vec<(String, PathBuf)> {
    let raw = match std::env::var("WF_EXTRA_ROOTS") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            let cfg_path = config_dir().join("wt").join("win-root");
            match std::fs::read_to_string(&cfg_path) {
                Ok(s) if !s.trim().is_empty() => format!("win={}", s.trim()),
                _ => return Vec::new(),
            }
        }
    };
    parse_extra_roots(&raw, primary)
}

/// Parse a `WF_EXTRA_ROOTS` spec (`tag=path:tag2=path2`) into `(tag, root)` pairs. Each path is
/// WSL-normalized + de-slashed, and any entry equal to `primary` is dropped (so a root pointed at
/// itself collapses to single-root, like `tatw`). Paths are expected unix/MSYS — the `:` separator
/// forbids a literal `C:\` — matching bash `worktree.sh:wf_extra_roots`.
fn parse_extra_roots(raw: &str, primary: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for entry in raw.split(':') {
        if entry.is_empty() {
            continue;
        }
        let (tag, path) = match entry.split_once('=') {
            Some((t, p)) => (t.to_string(), p.to_string()),
            None => ("ext".to_string(), entry.to_string()),
        };
        let norm = wf_unixpath(&path);
        let norm = norm.trim_end_matches('/');
        if norm.is_empty() || Path::new(norm) == primary {
            continue;
        }
        out.push((tag, PathBuf::from(norm)));
    }
    out
}

// --- discovery -------------------------------------------------------------

/// Repo names in a single root: bare repos first, then flat working clones. Skips the
/// `bare`/`worktree` dirs, dotfiles, and stray non-git dirs. Both groups sorted (bash glob order).
fn list_root_projects(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bare_dir = root.join("bare");
    if bare_dir.is_dir() {
        for p in wt::bare_repos(&bare_dir) {
            out.push(wt::repo_name(&p));
        }
    }
    let mut flats: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = match p.file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => continue,
            };
            if name == "bare" || name == "worktree" || name.starts_with('.') {
                continue;
            }
            if bare_dir.join(format!("{name}.git")).is_dir() {
                continue;
            }
            if !p.join(".git").exists() {
                continue;
            }
            flats.push(name);
        }
    }
    flats.sort();
    out.extend(flats);
    out
}

/// List worktree branch-dir names for a bare repo, excluding the bare dir itself (porcelain `bare`
/// marker), in git's registration order. A faithful port of `worktree.sh:list_worktrees`.
fn list_worktrees(roots: &Roots, r: &str) -> Vec<String> {
    let bare = roots.bare_dir(r).join(format!("{}.git", ref_repo(r)));
    if !bare.is_dir() {
        return Vec::new();
    }
    let out = match wt::git_out(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "list",
        "--porcelain",
    ]) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut res = Vec::new();
    let mut wt_path: Option<String> = None;
    let mut is_bare = false;
    let flush = |wt_path: &mut Option<String>, is_bare: &mut bool, res: &mut Vec<String>| {
        if let Some(p) = wt_path.take() {
            // Skip the pool slots (`<repo>/_pool/<n>`): they're leased/warmed via `bn wt pool` and
            // surfaced as dedicated pool rows in the sub-picker, never as bare numbered worktrees.
            let pooled = Path::new(&p).components().any(|c| c.as_os_str() == "_pool");
            if !*is_bare && !pooled {
                if let Some(base) = Path::new(&p).file_name() {
                    res.push(base.to_string_lossy().to_string());
                }
            }
        }
        *is_bare = false;
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut wt_path, &mut is_bare, &mut res);
            wt_path = Some(p.to_string());
            is_bare = false;
        } else if line == "bare" {
            is_bare = true;
        } else if line.is_empty() {
            flush(&mut wt_path, &mut is_bare, &mut res);
        }
    }
    flush(&mut wt_path, &mut is_bare, &mut res);
    res
}

// --- frecency --------------------------------------------------------------

/// Reorder refs by zoxide frecency (descending); identity if zoxide is absent.
fn sort_by_frecency(roots: &Roots, refs: Vec<String>) -> Vec<String> {
    let Some(scores) = zoxide_scores() else {
        return refs;
    };
    let scored: Vec<(f64, String)> = refs
        .into_iter()
        .map(|r| {
            let path = project_path(roots, &r).to_string_lossy().to_string();
            let score = scores.get(&path).copied().unwrap_or(0.0);
            (score, r)
        })
        .collect();
    frecency_order(scored)
}

/// The `sort -rn` ordering over `(score, ref)`: by score descending, ties broken by ref descending
/// (GNU `sort -rn` reverses its whole-line tiebreak). Split out so it is unit-testable.
fn frecency_order(mut scored: Vec<(f64, String)>) -> Vec<String> {
    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.reverse();
    scored.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Deserialize, Serialize)]
struct ZoxideDir {
    path: String,
    rank: f64,
    last_accessed: u64,
}

/// Read zoxide's bincode `db.zo` directly instead of shelling out to `zoxide query`.
fn zoxide_scores() -> Option<HashMap<String, f64>> {
    let bytes = std::fs::read(zoxide_db_path()?).ok()?;
    let dirs = parse_zoxide_db(&bytes)?;
    let now = now_epoch_secs();
    let mut scores = HashMap::new();
    for dir in dirs {
        if zoxide_path_exists(&dir.path) {
            scores.insert(dir.path.clone(), zoxide_score(&dir, now));
        }
    }
    Some(scores)
}

fn zoxide_db_path() -> Option<PathBuf> {
    match std::env::var_os("_ZO_DATA_DIR") {
        Some(path) => {
            let path = PathBuf::from(path);
            path.is_absolute().then(|| path.join("db.zo"))
        }
        None => dirs::data_local_dir().map(|dir| dir.join("zoxide").join("db.zo")),
    }
}

fn parse_zoxide_db(bytes: &[u8]) -> Option<Vec<ZoxideDir>> {
    const VERSION: u32 = 3;

    let version_size = zoxide_bincode_options().serialized_size(&VERSION).ok()? as usize;
    if bytes.len() < version_size {
        return None;
    }
    let (version_bytes, dirs_bytes) = bytes.split_at(version_size);
    let version: u32 = zoxide_bincode_options().deserialize(version_bytes).ok()?;
    if version != VERSION {
        return None;
    }
    zoxide_bincode_options().deserialize(dirs_bytes).ok()
}

fn serialize_zoxide_db(dirs: &[ZoxideDir]) -> Option<Vec<u8>> {
    const VERSION: u32 = 3;

    let size = zoxide_bincode_options().serialized_size(&VERSION).ok()?
        + zoxide_bincode_options().serialized_size(dirs).ok()?;
    let mut bytes = Vec::with_capacity(size as usize);
    zoxide_bincode_options()
        .serialize_into(&mut bytes, &VERSION)
        .ok()?;
    zoxide_bincode_options()
        .serialize_into(&mut bytes, dirs)
        .ok()?;
    Some(bytes)
}

fn zoxide_bincode_options() -> impl bincode::Options {
    const MAX_SIZE: u64 = 32 << 20;
    bincode::options()
        .with_fixint_encoding()
        .with_limit(MAX_SIZE)
}

fn zoxide_score(dir: &ZoxideDir, now: u64) -> f64 {
    const HOUR: u64 = 60 * 60;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;

    let age = now.saturating_sub(dir.last_accessed);
    let score = if age < HOUR {
        dir.rank * 4.0
    } else if age < DAY {
        dir.rank * 2.0
    } else if age < WEEK {
        dir.rank * 0.5
    } else {
        dir.rank * 0.25
    };
    score.clamp(0.0, 9999.0)
}

fn now_epoch_secs() -> u64 {
    // Delegate to the canonical clock so `$BN_NOW` (the deterministic-test override) is honored
    // here too — a raw SystemTime read silently ignored it. Epoch is non-negative, cast is safe.
    bn_core::util::now_epoch_secs().max(0) as u64
}

fn zoxide_path_exists(path: &str) -> bool {
    let meta = if std::env::var_os("_ZO_RESOLVE_SYMLINKS").is_some_and(|var| var == "1") {
        std::fs::symlink_metadata(path)
    } else {
        std::fs::metadata(path)
    };
    meta.map(|m| m.is_dir()).unwrap_or(false)
}

fn zoxide_add(path: &Path) {
    let Some(db_path) = zoxide_db_path() else {
        return;
    };
    let Some(path) = zoxide_add_path(path) else {
        return;
    };
    let now = now_epoch_secs();
    let max_age = zoxide_max_age();
    let _ = zoxide_add_to_db(&db_path, path, now, max_age);
}

fn zoxide_add_path(path: &Path) -> Option<String> {
    let path = if std::env::var_os("_ZO_RESOLVE_SYMLINKS").is_some_and(|var| var == "1") {
        std::fs::canonicalize(path).ok()?
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let path = path.to_string_lossy().to_string();
    (!path.contains('\n') && !path.contains('\r') && Path::new(&path).is_dir()).then_some(path)
}

fn zoxide_max_age() -> f64 {
    std::env::var("_ZO_MAXAGE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|v| v as f64)
        .unwrap_or(10_000.0)
}

fn zoxide_add_to_db(db_path: &Path, path: String, now: u64, max_age: f64) -> std::io::Result<()> {
    let mut dirs = match std::fs::read(db_path) {
        Ok(bytes) => match parse_zoxide_db(&bytes) {
            Some(dirs) => dirs,
            None => return Ok(()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };

    match dirs.iter_mut().find(|dir| dir.path == path) {
        Some(dir) => {
            dir.rank = (dir.rank + 1.0).max(0.0);
            dir.last_accessed = now;
        }
        None => dirs.push(ZoxideDir {
            path,
            rank: 1.0,
            last_accessed: now,
        }),
    }
    zoxide_age(&mut dirs, max_age);
    let Some(bytes) = serialize_zoxide_db(&dirs) else {
        return Ok(());
    };
    write_atomic(db_path, &bytes)
}

fn zoxide_age(dirs: &mut Vec<ZoxideDir>, max_age: f64) {
    let total_age = dirs.iter().map(|dir| dir.rank).sum::<f64>();
    if total_age <= max_age {
        return;
    }
    let factor = 0.9 * max_age / total_age;
    for idx in (0..dirs.len()).rev() {
        dirs[idx].rank *= factor;
        if dirs[idx].rank < 1.0 {
            dirs.swap_remove(idx);
        }
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("db.zo"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// --- session helpers -------------------------------------------------------

#[derive(Default)]
struct TmuxSessions {
    counts: HashMap<String, usize>,
    order: Vec<String>,
}

impl TmuxSessions {
    fn count(&self, session: &str) -> Option<usize> {
        self.counts.get(session).copied()
    }

    fn orphans(&self, known: &HashSet<String>) -> Vec<String> {
        self.order
            .iter()
            .filter(|s| !known.contains(*s))
            .cloned()
            .collect()
    }
}

fn tmux_sessions() -> TmuxSessions {
    tmux_raw(&["list-windows", "-a", "-F", "#{session_name}"])
        .map(|raw| parse_tmux_sessions(&raw))
        .unwrap_or_default()
}

fn parse_tmux_sessions(raw: &str) -> TmuxSessions {
    let mut sessions = TmuxSessions::default();
    for line in raw.lines().filter(|l| !l.is_empty()) {
        let session = line.to_string();
        let count = sessions.counts.entry(session.clone()).or_insert_with(|| {
            sessions.order.push(session);
            0
        });
        *count += 1;
    }
    sessions
}

fn has_session(r: &str) -> bool {
    tmux_ok(&["has-session", "-t", &exact_session_target(&ref_session(r))])
}

/// True when running inside a tmux session (pane or popup).
fn in_session() -> bool {
    std::env::var_os("TMUX").is_some()
        || std::env::var_os("PSMUX_SESSION").is_some()
        || std::env::var_os("TMUX_PANE").is_some()
}

// tmux's `=` makes session lookup exact; `new-window -t` needs the trailing `:` session target.
fn exact_session_target(session: &str) -> String {
    format!("={session}")
}

fn session_window_target(session: &str) -> String {
    format!("={session}:")
}

fn exact_window_target(session: &str, window: &str) -> String {
    format!("={session}:{window}")
}

fn exact_tmux_target(target: &str) -> String {
    if target.starts_with('=') {
        target.to_string()
    } else if let Some((session, window)) = target.split_once(':') {
        exact_window_target(session, window)
    } else {
        exact_session_target(target)
    }
}

/// Switch (in-session) or attach (outside) the client to `target` (`sess` or `sess:window`).
fn goto_session(target: &str) {
    let verb = if in_session() {
        "switch-client"
    } else {
        "attach-session"
    };
    let _ = tmux_run(&[verb, "-t", &exact_tmux_target(target)]);
}

/// Create `ref`'s session (detached, a `main` window at `start`) if missing; record frecency.
fn ensure_session(r: &str, start: &Path) {
    if has_session(r) {
        return;
    }
    zoxide_add(start);
    let _ = tmux_command(&[
        "new-session",
        "-d",
        "-s",
        &ref_session(r),
        "-n",
        "main",
        "-c",
    ])
    .arg(start)
    .status();
}

/// True when `session` has a window named exactly `window`.
fn window_named_exists(session: &str, window: &str) -> bool {
    tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(session),
        "-F",
        "#{window_name}",
    ])
    .map(|s| s.lines().any(|w| w == window))
    .unwrap_or(false)
}

/// Select `session:window` if it exists, else open it as a new window rooted at `wt_path`. The
/// session must already exist (the caller `ensure_session`s first). Generalizes
/// `wt::open_worktree_window` to any session name — needed for tagged `repo@tag` refs whose session
/// is `repo-tag`. Mirrors `worktree.sh:open_worktree_window` + `tat:open_worktree`'s select branch.
fn open_or_select_window(session: &str, window: &str, wt_path: &Path) {
    if window_named_exists(session, window) {
        let _ = tmux_run(&["select-window", "-t", &exact_window_target(session, window)]);
    } else {
        let _ = tmux_command(&[
            "new-window",
            "-t",
            &session_window_target(session),
            "-n",
            window,
            "-c",
        ])
        .arg(wt_path)
        .status();
    }
}

// --- list (the --pick-stage list feed) -------------------------------------

const DISCOVERY_CACHE_TTL_SECS: i64 = 30;

#[derive(Default, Deserialize, Serialize)]
struct DiscoveryCache {
    roots_key: String,
    refs_saved_at: i64,
    refs: Vec<String>,
    worktrees: HashMap<String, CachedWorktrees>,
    #[serde(default)]
    remote_prs: HashMap<String, CachedRemotePrs>,
}

#[derive(Deserialize, Serialize)]
struct CachedWorktrees {
    saved_at: i64,
    names: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct CachedRemotePrs {
    saved_at: i64,
    prs: Vec<wt::RemotePr>,
}

fn discovery_cache_path() -> PathBuf {
    cache_home().join("wt").join("tat-discovery.json")
}

fn discovery_ttl_secs() -> i64 {
    std::env::var("WF_TAT_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(DISCOVERY_CACHE_TTL_SECS)
}

fn discovery_cache_fresh(saved_at: i64) -> bool {
    bn_core::util::now_epoch_secs().saturating_sub(saved_at) <= discovery_ttl_secs()
}

fn read_discovery_cache() -> DiscoveryCache {
    std::fs::read_to_string(discovery_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_discovery_cache(cache: &DiscoveryCache) {
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    let _ = write_atomic(&discovery_cache_path(), &bytes);
}

fn roots_cache_key(roots: &Roots) -> String {
    let mut parts = vec![
        roots.primary.to_string_lossy().to_string(),
        roots.primary_bare.to_string_lossy().to_string(),
        roots.primary_worktree.to_string_lossy().to_string(),
        roots.win_scoped.to_string(),
    ];
    parts.extend(
        roots
            .extras
            .iter()
            .map(|(tag, path)| format!("{tag}={}", path.display())),
    );
    parts.join("\n")
}

/// Repository discovery and zoxide ordering are stable across the short-lived processes fzf
/// starts for callbacks. Snapshot those filesystem-derived values; tmux and pool state stay live.
fn ordered_refs(roots: &Roots, refresh: bool) -> Vec<String> {
    let key = roots_cache_key(roots);
    let mut cache = read_discovery_cache();
    if !refresh
        && cache.roots_key == key
        && discovery_cache_fresh(cache.refs_saved_at)
        && !cache.refs.is_empty()
    {
        return cache.refs;
    }

    let mut refs = roots.all_refs();
    refs.sort();
    refs.dedup();
    let refs = sort_by_frecency(roots, refs);
    cache.roots_key = key;
    cache.refs_saved_at = bn_core::util::now_epoch_secs();
    cache.refs.clone_from(&refs);
    write_discovery_cache(&cache);
    refs
}

/// Decorated list rows: `<display>\t<ref>`. fzf shows only `<display>` (`--with-nth=1`); the
/// `<ref>` rides along as the hidden last field (`{-1}`) for preview/kill/selection.
fn build_list(roots: &Roots, refresh: bool) {
    let refs = ordered_refs(roots, refresh);
    let known_sessions: HashSet<String> = refs.iter().map(|r| ref_session(r)).collect();
    let known_repos: HashSet<String> = refs.iter().map(|r| ref_repo(r).to_string()).collect();
    let tmux = tmux_sessions();
    for r in refs {
        let sess = ref_session(&r);
        let marker = if let Some(count) = tmux.count(&sess) {
            format!("[*{count}w] ")
        } else {
            "      ".to_string()
        };
        let tag = ref_tag(&r);
        let suffix = if tag.is_empty() {
            String::new()
        } else {
            format!(" ({tag})")
        };
        println!("{marker}{}{suffix}\t{r}", ref_repo(&r));
    }
    for sess in tmux.orphans(&known_sessions) {
        let count = tmux.count(&sess).unwrap_or(0);
        println!("[*{count}w] {sess} (session)\t{sess}");
    }
    // Codespaces whose repo is NOT cloned locally — top-level rows, like orphans (a cloned repo's
    // codespaces surface inside its worktree sub-picker instead). The hidden field is a `cs:` ref,
    // routed by `is_cs_ref` before any `ref_*` call.
    for cs in codespaces() {
        if known_repos.contains(&cs.repo) {
            continue;
        }
        println!(
            "      {}-{} (codespace)\tcs:{}/{}/{}",
            cs.repo, cs.branch, cs.repo, cs.branch, cs.name
        );
    }
    // Codespace auth lapsed (scope expired / logged out): a single actionable row, Enter runs the
    // gh remedy. Shown only when the cached list failed for auth.
    if cs_auth_needed() {
        println!("   ⚠  codespaces: gh auth expired — Enter to fix\tcs-auth");
    }
}

// --- preview (the fzf side pane) -------------------------------------------

fn preview(roots: &Roots, project: &str) {
    if project.is_empty() {
        return;
    }
    // The codespace auth-fix row.
    if project == "cs-auth" {
        let acct = cs_account();
        println!("━━━ codespace auth ━━━\n");
        println!("`gh cs list` failed — no codespace access on the active gh account.\n");
        if !acct.is_empty() {
            println!("Enter: switch to your codespace account ({acct}); if that still");
            println!("       lacks the scope, refresh it (browser/device login).");
        } else {
            println!("Fix:  {}", cs_auth_cmd());
            println!("Enter runs it (browser/device login). Tip: if your codespaces live");
            println!(
                "under another gh account, put its username in:\n  {}",
                cs_account_cfg_path().display()
            );
        }
        return;
    }
    // Codespace rows have no local session/dir — render state from the cached gh list, before any
    // `ref_*`/`project_path` call mis-parses the `cs:` ref.
    if is_cs_ref(project) {
        let r = cs_ref_repo(project);
        let b = cs_ref_branch(project);
        let n = cs_ref_csname(project);
        let st = codespaces()
            .into_iter()
            .find(|c| c.name == n)
            .map(|c| c.state)
            .unwrap_or_default();
        println!("━━━ {r} / {b} (codespace) ━━━\n");
        println!("Repo:   {r}");
        println!("Branch: {b}");
        println!("State:  {}", if st.is_empty() { "unknown" } else { &st });
        println!("Name:   {n}\n");
        println!(
            "Enter: open \"{}\" window in session {r} (gh cs ssh + remote tmux)",
            cs_window_name(b)
        );
        return;
    }
    let tag = ref_tag(project);
    let sess = ref_session(project);
    let path = project_path(roots, project);
    let path_is_dir = path.is_dir();
    let window_rows = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(&sess),
        "-F",
        "#{window_index}|#{window_name}|#{pane_current_command}|#{pane_current_path}|#{window_active}",
    ]);
    let session_up = window_rows.is_some();

    if !path_is_dir && !session_up {
        println!("Directory not found: {}", path.display());
        return;
    }

    if !tag.is_empty() {
        println!("━━━ {} ({tag}) ━━━\n", ref_repo(project));
    } else if !path_is_dir {
        println!("━━━ {project} (session) ━━━\n");
    } else {
        println!("━━━ {project} ━━━\n");
    }

    if let Some(raw) = window_rows {
        println!("Windows:");
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let f = split5(line);
            let marker = if f[4] == "1" { "*" } else { "" };
            // The oracle's `${wpath/#$HOME/~}` is a silent no-op: bash tilde-expands the `~`
            // replacement back to $HOME, so it replaces $HOME with $HOME. The path stays full.
            println!("  {}{marker}: {:<14} {:<8} {}", f[0], f[1], f[2], f[3]);
        }
        println!();
    }

    let path_s = path.to_string_lossy().to_string();
    if path.join(".git").exists() {
        let branch = wt::git_out(&["-C", &path_s, "branch", "--show-current"])
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "detached".to_string());
        println!("Branch: {branch}");
        let staged = numstat_count(&["-C", &path_s, "diff", "--cached", "--numstat"]);
        let unstaged = numstat_count(&["-C", &path_s, "diff", "--numstat"]);
        let untracked =
            numstat_count(&["-C", &path_s, "ls-files", "--others", "--exclude-standard"]);
        if staged > 0 {
            println!("Staged: {staged}");
        }
        if unstaged > 0 {
            println!("Modified: {unstaged}");
        }
        if untracked > 0 {
            println!("Untracked: {untracked}");
        }
        if staged == 0 && unstaged == 0 && untracked == 0 {
            println!("Clean");
        }
        println!();
        println!("Recent commits:");
        if let Some(log) = wt::git_out(&["-C", &path_s, "log", "--oneline", "-5"]) {
            if !log.is_empty() {
                println!("{log}");
            }
        }
        println!();
    }

    if is_bare_repo(roots, project) {
        let wts = list_worktrees(roots, project);
        if !wts.is_empty() {
            println!("Worktrees: ({})", wts.len());
            for w in &wts {
                println!("  {w}");
            }
            println!();
        }
    }

    if path_is_dir {
        let t = if path.join("Cargo.toml").is_file() {
            "Rust"
        } else if path.join("package.json").is_file() {
            "Node.js"
        } else if path.join("go.mod").is_file() {
            "Go"
        } else if path.join("pyproject.toml").is_file() || path.join("requirements.txt").is_file() {
            "Python"
        } else {
            "Unknown"
        };
        println!("Type: {t}");
    }
}

/// Number of output lines for a git command (the `… | wc -l` counters).
fn numstat_count(args: &[&str]) -> usize {
    wt::git_out(args)
        .map(|s| if s.is_empty() { 0 } else { s.lines().count() })
        .unwrap_or(0)
}

// --- kill ------------------------------------------------------------------

/// Kill `ref`'s session (passed via `{-1}`), unless it's the current one.
fn kill_from_line(r: &str) {
    if r.is_empty() {
        return;
    }
    let sess = ref_session(r);
    let current = tmux(&["display-message", "-p", "#S"]).unwrap_or_default();
    if sess == current {
        return;
    }
    if has_session(r) {
        let _ = tmux_run(&["kill-session", "-t", &exact_session_target(&sess)]);
    }
}

// --- codespaces (3c) -------------------------------------------------------
// GitHub Codespaces as remote worktrees: a codespace is a (repo, branch) pair, so it slots into
// tat's model — session = repo, window = "<branch> ☁", remote tmux session = branch. This is the
// ONLY place tat shells out to `gh`. Everything degrades silently: no gh / not-logged-in / a missing
// `codespace` scope all yield an empty list (with an actionable auth row), never an error.

/// A normalized codespace row: `repo` is the LOCAL clone name (resolved through the repo map).
struct Codespace {
    repo: String,
    branch: String,
    state: String,
    name: String,
}

/// `$XDG_CACHE_HOME` or `~/.cache`.
fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
}

/// `$HOME` (the codespace session/window root), or `.` as a last resort.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The cached `gh cs list` JSON path (`$WF_CS_CACHE`, else `<cache>/wt/codespaces.json`).
fn cs_cache_path() -> PathBuf {
    std::env::var_os("WF_CS_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache_home().join("wt").join("codespaces.json"))
}

/// The auth-marker path (`$WF_CS_AUTH_MARKER`, else `<cache>.auth`): non-empty when `gh cs list`
/// last failed for an auth reason, holding the remedy command.
fn cs_auth_marker_path() -> PathBuf {
    if let Some(p) = std::env::var_os("WF_CS_AUTH_MARKER") {
        return PathBuf::from(p);
    }
    let mut p = cs_cache_path().into_os_string();
    p.push(".auth");
    PathBuf::from(p)
}

/// The codespace gh-account config (`$WF_CS_ACCOUNT_CFG`, else `~/.config/wt/cs-account`).
fn cs_account_cfg_path() -> PathBuf {
    std::env::var_os("WF_CS_ACCOUNT_CFG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("wt").join("cs-account"))
}

/// The codespace-repo→local-repo map (`$WF_CS_REPO_MAP`, else `~/.config/wt/cs-repo-map`).
fn cs_repo_map_path() -> PathBuf {
    std::env::var_os("WF_CS_REPO_MAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("wt").join("cs-repo-map"))
}

/// Resolve a codespace repo basename to its local repo name via the repo map; identity when
/// unmapped or no config. Mirrors `codespaces.sh:wf_cs_repo_alias`.
fn cs_repo_alias(cs: &str) -> String {
    let Ok(content) = std::fs::read_to_string(cs_repo_map_path()) else {
        return cs.to_string();
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k: String = k.chars().filter(|c| !c.is_whitespace()).collect();
        let v: String = v.chars().filter(|c| !c.is_whitespace()).collect();
        if k == cs {
            return if v.is_empty() { cs.to_string() } else { v };
        }
    }
    cs.to_string()
}

/// Is `cmd` runnable (a cheap `--version` probe; the silent-degrade gate for `gh`)?
fn has_cmd(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Raw `gh cs list` JSON, cached to [`cs_cache_path`] with a TTL (`$WF_CS_TTL`, default 60s). Always
/// returns a JSON array string, never errors. An auth failure is distinguished from "no codespaces":
/// the remedy is stashed in the auth marker and `[]` negative-cached. Mirrors `codespaces.sh`.
fn cs_raw() -> String {
    let ttl: u64 = std::env::var("WF_CS_TTL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let cache = cs_cache_path();
    if let Ok(meta) = std::fs::metadata(&cache) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|age| age.as_secs() < ttl)
            .unwrap_or(false);
        if fresh {
            if let Ok(s) = std::fs::read_to_string(&cache) {
                return s;
            }
        }
    }
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let limit = std::env::var("WF_CS_LIMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "100".to_string());
    let out = crate::gh::gh_command(&[
        "cs",
        "list",
        "--limit",
        &limit,
        "--json",
        "name,repository,gitStatus,state,lastUsedAt",
    ])
    .output();
    let marker = cs_auth_marker_path();
    match out {
        Ok(o)
            if o.status.success() && {
                let s = String::from_utf8_lossy(&o.stdout);
                s.trim_start().starts_with('[')
            } =>
        {
            let json = String::from_utf8_lossy(&o.stdout).into_owned();
            let _ = std::fs::write(&cache, &json);
            let _ = std::fs::remove_file(&marker); // auth healthy again
            json
        }
        out => {
            // Failure: classify gh's stderr so the picker can offer the exact fix; negative-cache
            // `[]` so we don't re-shell every open until the TTL.
            let stderr = out
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stderr).to_lowercase())
                .unwrap_or_default();
            let scope_issue = stderr.contains("required scopes")
                || stderr.contains("auth refresh")
                || (stderr.contains("missing") && stderr.contains("scope"));
            let login_issue = stderr.contains("not logged in")
                || stderr.contains("gh auth login")
                || stderr.contains("authentication");
            if scope_issue {
                let _ = std::fs::write(&marker, "gh auth refresh -h github.com -s codespace\n");
            } else if login_issue {
                let _ = std::fs::write(&marker, "gh auth login\n");
            }
            let _ = std::fs::write(&cache, "[]");
            "[]".to_string()
        }
    }
}

/// Normalized codespaces (repo resolved to the local clone name; rows with no repo/branch dropped).
/// Parses `gh cs list --json` natively — no `jq` dependency (the bash oracle required it).
fn codespaces() -> Vec<Codespace> {
    parse_codespaces(&cs_raw())
}

/// Parse `gh cs list --json name,repository,gitStatus,state,…` into [`Codespace`] rows. Split out
/// (pure) so it is unit-testable. Mirrors the oracle's `jq … | @tsv | awk '$1!="" && $2!=""'`.
fn parse_codespaces(json: &str) -> Vec<Codespace> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cs in arr {
        let repo_full = cs.get("repository").and_then(|x| x.as_str()).unwrap_or("");
        let repo_base = repo_full.rsplit('/').next().unwrap_or(repo_full);
        let branch = cs
            .get("gitStatus")
            .and_then(|g| g.get("ref"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let state = cs.get("state").and_then(|x| x.as_str()).unwrap_or("");
        let name = cs.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if repo_base.is_empty() || branch.is_empty() {
            continue;
        }
        out.push(Codespace {
            repo: cs_repo_alias(repo_base),
            branch: branch.to_string(),
            state: state.to_string(),
            name: name.to_string(),
        });
    }
    out
}

/// Codespace rows for a single local repo (by name).
fn cs_for_repo(repo: &str) -> Vec<Codespace> {
    codespaces()
        .into_iter()
        .filter(|c| c.repo == repo)
        .collect()
}

/// The tmux window name for a codespace branch: sanitized branch + the ☁ marker (the single source
/// of truth shared by the row display and the open/dedup paths).
fn cs_window_name(branch: &str) -> String {
    format!("{} ☁", wt::sanitize_branch(branch))
}

/// The shell command a "<branch> ☁" window runs: ssh into the codespace and attach-or-create its
/// per-branch tmux session (installing tmux first if the codespace lacks it). The branch is quoted
/// for BOTH the remote shell (inner) and the local shell (outer) so a crafted ref can't inject.
fn cs_attach_cmd(name: &str, branch: &str) -> String {
    let b = wt::sanitize_branch(branch);
    let payload = format!(
        "command -v tmux >/dev/null 2>&1 || {{ sudo apt-get update -qq && sudo apt-get install -y -qq tmux; }}; tmux new-session -A -s {}",
        crate::shq(&b)
    );
    format!(
        "gh cs ssh -c {} -- -t {}",
        crate::shq(name),
        crate::shq(&payload)
    )
}

/// Does the cached `gh cs list` indicate an auth problem we can prompt to fix?
fn cs_auth_needed() -> bool {
    std::fs::metadata(cs_auth_marker_path())
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// The remedy command gh suggested (e.g. `gh auth refresh -h github.com -s codespace`).
fn cs_auth_cmd() -> String {
    std::fs::read_to_string(cs_auth_marker_path())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The configured codespace gh account (machine-local), or empty if unset.
fn cs_account() -> String {
    std::fs::read_to_string(cs_account_cfg_path())
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect())
        .unwrap_or_default()
}

/// Codespace ref helpers. A codespace ref is `cs:<repo>/<branch>/<csname>` — a hidden picker field,
/// NOT a project ref, so it must be intercepted before any `ref_*`/path call. `csname` has no `/`
/// (so it right-splits cleanly) and a branch may itself contain `/`.
fn is_cs_ref(s: &str) -> bool {
    s.starts_with("cs:")
}
fn cs_ref_csname(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}
fn cs_ref_repo(s: &str) -> &str {
    let r = s.strip_prefix("cs:").unwrap_or(s);
    r.split_once('/').map(|(a, _)| a).unwrap_or(r)
}
fn cs_ref_branch(s: &str) -> &str {
    let r = s.strip_prefix("cs:").unwrap_or(s);
    let r = r.rsplit_once('/').map(|(a, _)| a).unwrap_or(r); // drop csname
    r.split_once('/').map(|(_, b)| b).unwrap_or("")
}

/// Focus the window named `name` in `session` by its index. The codespace window name carries a
/// space (the ☁ marker), so `select-window -t "sess:name"` would mis-parse — look the index up.
fn cs_focus_window(session: &str, name: &str) {
    let Some(raw) = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(session),
        "-F",
        "#{window_index}\t#{window_name}",
    ]) else {
        return;
    };
    for line in raw.lines() {
        if let Some((idx, wname)) = line.split_once('\t') {
            if wname == name {
                let _ = tmux_run(&["select-window", "-t", &exact_window_target(session, idx)]);
                return;
            }
        }
    }
}

/// Open a codespace (ref `cs:<repo>/<branch>/<csname>`): ensure the local session `<repo>`, then
/// focus-or-open its "<branch> ☁" window (a `gh cs ssh` attach), and switch to it.
fn open_codespace(r: &str) {
    let repo = cs_ref_repo(r);
    let branch = cs_ref_branch(r);
    let csname = cs_ref_csname(r);
    let win = cs_window_name(branch);
    let home = home_dir();
    if !has_session(repo) {
        let _ = tmux_command(&["new-session", "-d", "-s", repo, "-n", "main", "-c"])
            .arg(&home)
            .status();
    }
    if window_named_exists(repo, &win) {
        cs_focus_window(repo, &win);
    } else {
        let _ = tmux_command(&[
            "new-window",
            "-t",
            &session_window_target(repo),
            "-n",
            &win,
            "-c",
        ])
        .arg(&home)
        .arg(cs_attach_cmd(csname, branch))
        .status();
    }
    goto_session(repo);
}

/// Fix codespace auth: first try switching to the configured account (the common cause); only if
/// that doesn't restore `gh cs list` fall back to the interactive scope refresh (browser/device).
fn cs_auth_refresh() {
    if !has_cmd("gh") {
        println!("gh not installed");
        return;
    }
    let acct = cs_account();
    if !acct.is_empty() {
        println!("Switching to codespace account: {acct}");
        let _ = crate::gh::gh_ok(&["auth", "switch", "-u", &acct]);
        let _ = std::fs::remove_file(cs_cache_path());
        let _ = std::fs::remove_file(cs_auth_marker_path());
        let healthy = crate::gh::gh_ok(&["cs", "list"]);
        if healthy {
            println!("Done — {acct} has codespace access.");
            return;
        }
        println!("{acct} still lacks the codespace scope — refreshing it.\n");
    }
    let cmd = {
        let c = cs_auth_cmd();
        if c.is_empty() {
            "gh auth refresh -h github.com -s codespace".to_string()
        } else {
            c
        }
    };
    println!("→ {cmd}");
    println!("  Copy the one-time code shown, then approve in the browser that opens");
    println!("  (else open https://github.com/login/device manually).\n");
    let ok = Command::new("sh")
        .args(["-c", &cmd])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let _ = std::fs::remove_file(cs_auth_marker_path());
        let _ = std::fs::remove_file(cs_cache_path());
        println!("\nDone — codespaces will reappear on the next picker open.");
    } else {
        println!("\nAuth command failed.");
    }
}

// --- dispatch --------------------------------------------------------------

/// Does `<s>` look like a git remote URL we can clone?
fn is_git_url(s: &str) -> bool {
    s.ends_with(".git")
        || s.starts_with("ssh://")
        || s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("git://")
        || (s
            .strip_prefix("git@")
            .map(|r| r.contains(':'))
            .unwrap_or(false))
}

/// The bare repo name a clone URL resolves to (`basename`, sans a `.git` suffix).
fn clone_name(url: &str) -> String {
    let base = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    base.strip_suffix(".git").unwrap_or(base).to_string()
}

/// `bn tat` (interactive) or `bn tat <name>` (direct). `root_opt` is the `--root` scope (`Some("win")`
/// = the Windows root, the `tatw` replacement). Mirrors bash `resolve_and_open`.
pub fn run(cfg: &Config, name: Option<&str>, root_opt: Option<&str>) {
    let roots = Roots::resolve(cfg, root_opt);
    // Carry the scope into every fzf self-reinvocation so the callbacks stay rooted the same way.
    let root_flag = if root_opt == Some("win") {
        " --root win"
    } else {
        ""
    };
    match name {
        Some(sel) if !sel.is_empty() => resolve_and_open(cfg, &roots, sel.to_string(), false),
        _ => {
            // Interactive: stream the list into fzf, capture the typed query + selection.
            let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
            let bn = crate::shq(&exe.display().to_string());
            let child = Command::new("fzf")
                .arg("--reverse")
                .arg("--ansi")
                .arg("--print-query")
                .arg("--delimiter=\t")
                .arg("--with-nth=1")
                .arg("--header=Enter: open | ctrl-r: refresh | ctrl-x: kill | type repo/branch or paste a git URL")
                .arg(format!(
                    "--preview={bn} tat{root_flag} --pick-stage preview {{-1}}"
                ))
                .arg("--preview-window=right:50%:wrap")
                .arg(format!(
                    "--bind=start:reload({bn} tat{root_flag} --pick-stage list)"
                ))
                .arg(format!(
                    "--bind=ctrl-x:execute-silent({bn} tat{root_flag} --pick-stage kill {{-1}})+reload({bn} tat{root_flag} --pick-stage list)"
                ))
                .arg(format!(
                    "--bind=ctrl-r:reload({bn} tat{root_flag} --pick-stage refresh)"
                ))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn();
            let child = match child {
                Ok(c) => c,
                Err(_) => {
                    // No fzf on PATH — degrade to a plain list, then hold.
                    build_list(&roots, false);
                    crate::picker::hold();
                    return;
                }
            };
            let Ok(out) = child.wait_with_output() else {
                return;
            };
            if out.status.code() == Some(130) {
                return; // Esc / ctrl-c
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let query = lines.next().unwrap_or("");
            let selection = lines.next().unwrap_or("");
            let (sel, typed) = if !selection.is_empty() {
                (
                    selection
                        .rsplit('\t')
                        .next()
                        .unwrap_or(selection)
                        .to_string(),
                    false,
                )
            } else {
                (query.to_string(), true)
            };
            if sel.is_empty() {
                return;
            }
            resolve_and_open(cfg, &roots, sel, typed);
        }
    }
}

/// Route a resolved selection: git URL → clone; `repo/branch` → worktree; a live orphan session →
/// switch; an unknown typed string → nothing; otherwise open the project.
fn resolve_and_open(cfg: &Config, roots: &Roots, sel: String, typed: bool) {
    if is_git_url(&sel) {
        clone_and_open(cfg, roots, &sel);
        return;
    }
    // The codespace auth-fix row: run the gh remedy, then pause so output is read.
    if sel == "cs-auth" {
        cs_auth_refresh();
        println!();
        println!("press any key…");
        crate::picker::hold();
        return;
    }
    // A codespace ref — must precede the `repo/branch` case, which would hijack its slashes.
    if is_cs_ref(&sel) {
        open_codespace(&sel);
        return;
    }
    if let Some((repo, branch)) = sel.split_once('/') {
        open_worktree(cfg, roots, repo, branch);
        return;
    }
    let flat_git = resolve_project_path(roots, &sel).join(".git").exists();
    if has_session(&sel) && !is_bare_repo(roots, &sel) && !flat_git {
        goto_session(&ref_session(&sel));
        return;
    }
    if typed && !is_bare_repo(roots, &sel) && !flat_git {
        return;
    }
    open_project(cfg, roots, &sel);
}

/// Open/create a worktree as a window in `ref`'s session, then focus it. An existing worktree is
/// just re-opened; a new branch on a **pooled** repo leases a warm pool slot (#457 — "create a lease
/// branch on top of a pool worktree") instead of paying a cold checkout, falling back to a fresh
/// worktree when the repo isn't pooled or the pool is full. Root-aware, so a tagged `repo@win` lands
/// in `<win-root>/worktree` under session `repo-win`. Mirrors bash `tat:open_worktree` + `goto_session`.
fn open_worktree(cfg: &Config, roots: &Roots, r: &str, branch: &str) {
    let repo = ref_repo(r);
    let sanitized = wt::sanitize_branch(branch);
    let worktree_dir = roots.worktree_dir(r);
    let wt_path = worktree_dir.join(repo).join(&sanitized);
    let sess = ref_session(r);

    // Already a normal worktree on disk → just open/select its window.
    if wt_path.is_dir() {
        ensure_session(r, &bare_repo_main_path(roots, r));
        open_or_select_window(&sess, &sanitized, &wt_path);
        goto_session(&sess);
        return;
    }

    // Pooled repo: lease a warm slot for this branch (idempotent — an already-leased branch hands
    // back its slot), and open the window at the slot path rather than a cold `worktree/<repo>/…`.
    // A configured pool with no capacity is a direct-open failure, not permission to grow an
    // unbounded parallel set of cold worktrees.
    if pool_eligible(roots, r) {
        match crate::wt_pool::lease_for_direct_open(cfg, repo, branch) {
            crate::wt_pool::DirectLease::Leased(slot) => {
                ensure_session(r, &bare_repo_main_path(roots, r));
                open_or_select_window(&sess, &sanitized, &slot);
                goto_session(&sess);
                return;
            }
            crate::wt_pool::DirectLease::Unavailable => {
                crate::picker::hold();
                return;
            }
            crate::wt_pool::DirectLease::MissingRepo => {
                eprintln!("Not a bare repo: {r}");
                crate::picker::hold();
                return;
            }
            crate::wt_pool::DirectLease::Unpooled => {}
        }
    }

    let bare = roots.bare_dir(r).join(format!("{repo}.git"));
    if !bare.is_dir() {
        eprintln!("Not a bare repo: {r}");
        crate::picker::hold();
        return;
    }
    let _ = std::fs::create_dir_all(worktree_dir.join(repo));
    if !wt::create_worktree(
        &bare.to_string_lossy(),
        branch,
        &wt_path.to_string_lossy(),
        true,
    ) {
        eprintln!("press any key…");
        crate::picker::hold();
        return;
    }
    ensure_session(r, &bare_repo_main_path(roots, r));
    open_or_select_window(&sess, &sanitized, &wt_path);
    goto_session(&sess);
}

/// Whether a new branch on `ref` should lease a warm pool slot instead of a cold worktree: an
/// untagged primary-root ref whose repo is in the pools config (`~/.config/wt/pools`). Tagged /
/// `--root win` refs keep the cold path — pools are a primary-root, heavy-monorepo concept.
fn pool_eligible(roots: &Roots, r: &str) -> bool {
    ref_tag(r).is_empty() && !roots.win_scoped && bn_core::pool::pool_size(ref_repo(r)).is_some()
}

/// Clone `<url>` into the bare+worktree layout via the native `bn wt clone`, then open it. Under
/// `--root win` the clone is steered to the Windows root (`WT_ROOT`); otherwise `bn wt clone` uses
/// its configured default, preserving the single-root behaviour exactly.
fn clone_and_open(cfg: &Config, roots: &Roots, url: &str) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let mut cmd = Command::new(&exe);
    if roots.win_scoped {
        cmd.env("WT_ROOT", &roots.primary);
    }
    let ok = cmd
        .args(["wt", "clone", url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("press any key…");
        crate::picker::hold();
        return;
    }
    open_project(cfg, roots, &clone_name(url));
}

/// Fast worktree discovery for a bare repo: the branch-dir names under `worktree_dir/<repo>/`, read
/// straight from the filesystem (skipping the `_pool` warm-slot dir and dotfiles) so the sub-picker
/// paints without a `git worktree list` subprocess. Falls back to the git listing when that dir
/// can't be read yet (e.g. before the first worktree is cut). Sorted for a stable row order.
fn scan_worktrees(roots: &Roots, name: &str, refresh: bool) -> Vec<String> {
    let dir = roots.worktree_dir(name).join(ref_repo(name));
    let key = dir.to_string_lossy().to_string();
    let mut cache = read_discovery_cache();
    if !refresh {
        if let Some(hit) = cache
            .worktrees
            .get(&key)
            .filter(|hit| discovery_cache_fresh(hit.saved_at))
        {
            return hit.names.clone();
        }
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return list_worktrees(roots, name);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "_pool" && !n.starts_with('.'))
        .collect();
    names.sort();
    cache.worktrees.insert(
        key,
        CachedWorktrees {
            saved_at: bn_core::util::now_epoch_secs(),
            names: names.clone(),
        },
    );
    write_discovery_cache(&cache);
    names
}

/// The authenticated user's open PRs for one bare repository, cached alongside TAT's filesystem
/// discovery so opening the picker repeatedly does not repeatedly query GitHub or Azure DevOps.
fn remote_prs(roots: &Roots, name: &str) -> Vec<wt::RemotePr> {
    if !is_bare_repo(roots, name) {
        return Vec::new();
    }
    let bare = roots.bare_dir(name).join(format!("{}.git", ref_repo(name)));
    let key = bare.to_string_lossy().to_string();
    let mut cache = read_discovery_cache();
    if let Some(hit) = cache
        .remote_prs
        .get(&key)
        .filter(|hit| discovery_cache_fresh(hit.saved_at))
    {
        return hit.prs.clone();
    }

    let remote = wt::remote_for(&bare);
    let prs = if remote == wt::Remote::Other {
        Vec::new()
    } else {
        wt::my_open_prs(&remote)
    };
    cache.remote_prs.insert(
        key,
        CachedRemotePrs {
            saved_at: bn_core::util::now_epoch_secs(),
            prs: prs.clone(),
        },
    );
    write_discovery_cache(&cache);
    prs
}

/// Render open PRs whose branch has no local representation. The payload is only the numeric PR id;
/// branch names and titles are display data and may contain fzf's `|` delimiter.
fn remote_pr_rows(prs: &[wt::RemotePr], represented: &HashSet<String>) -> String {
    let mut rows = String::new();
    for pr in prs {
        let branch = wt::sanitize_branch(&pr.branch);
        if represented.contains(&branch) {
            continue;
        }
        let branch_display = branch.replace('|', " ");
        let title = pr.title.replace(['\r', '\n', '|'], " ");
        let display = format!(
            " PR: {:<14} {:<8} {}",
            branch_display,
            format!("(#{} remote)", pr.number),
            title
        );
        rows.push_str(&format!("remotepr|{}|{display}\n", pr.number));
    }
    rows
}

/// Build the sub-picker's `kind|payload|display` rows for one project: live windows (each with an
/// agent-status dot), idle worktrees (fast dir-scan), remote PRs, codespace branches, and warm-pool
/// slots.
/// Returns the rows plus the live-window count (the caller's quiet-clone shortcut). Shared by the
/// initial in-process paint and the `--pick-stage sublist` reload feed so the two stay identical.
fn build_sub_rows(
    cfg: &Config,
    roots: &Roots,
    name: &str,
    refresh_worktrees: bool,
) -> (String, usize) {
    let sess = ref_session(name);
    let repo = ref_repo(name);
    let bare = is_bare_repo(roots, name);

    let window_rows = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(&sess),
        "-F",
        "#{window_index}|#{window_name}|#{pane_current_command}|#{window_active}|#{@bn_agent}|#{@bn_agent_since}|#{pane_current_path}",
    ]);
    let mut open_windows = Vec::new();
    let mut rows = String::new();
    if let Some(raw) = &window_rows {
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let f = split7(line);
            open_windows.push(f[1].to_string());
            let marker = if f[3] == "1" { "*" } else { "" };
            // A live-agent status dot in front of the window row (#457): green ● working, yellow ●
            // waiting-on-you, dim ◌ idle, blank when no agent runs there.
            let dot = agent_dot(f[4], f[5]);
            let display = format!("{dot}{}{marker}: {:<14} {:<8} {}", f[0], f[1], f[2], f[6]);
            rows.push_str(&format!("window|{}|{display}\n", f[0]));
        }
    }

    let worktree_dir = roots.worktree_dir(name);
    let worktrees = if bare {
        scan_worktrees(roots, name, refresh_worktrees)
    } else {
        Vec::new()
    };
    if bare {
        for w in &worktrees {
            if open_windows.iter().any(|ow| ow == w) {
                continue; // already a window
            }
            // Same `${short/#$HOME/~}` no-op as the preview pane — the path stays full.
            let short = worktree_dir
                .join(repo)
                .join(w)
                .to_string_lossy()
                .to_string();
            let display = format!(" +: {:<14} {:<8} {}", w, "(worktree)", short);
            rows.push_str(&format!("worktree|{w}|{display}\n"));
        }
    }

    let pool_status = if bare && pool_eligible(roots, name) {
        bn_core::pool::pool_status(cfg, repo)
    } else {
        None
    };
    let mut represented: HashSet<String> = open_windows.iter().cloned().collect();
    represented.extend(worktrees.iter().cloned());
    if bare {
        let bare_s = roots
            .bare_dir(name)
            .join(format!("{repo}.git"))
            .to_string_lossy()
            .to_string();
        represented.extend(
            wt::checked_out_branches(&bare_s)
                .into_keys()
                .map(|branch| wt::sanitize_branch(&branch)),
        );
    }
    if let Some(status) = &pool_status {
        represented.extend(
            status
                .slots
                .iter()
                .filter_map(|slot| slot.branch.as_deref())
                .map(wt::sanitize_branch),
        );
    }
    rows.push_str(&remote_pr_rows(&remote_prs(roots, name), &represented));

    // Codespace branches for this repo — "<branch> ☁" rows beside the worktrees, skipping any
    // already open as a window. The payload packs `csname\tbranch` (neither contains `|`).
    for cs in cs_for_repo(repo) {
        let cs_win = cs_window_name(&cs.branch);
        if open_windows.iter().any(|ow| ow == &cs_win) {
            continue;
        }
        let display = format!(
            " ☁: {:<14} {:<8} {}",
            cs.branch,
            format!("({})", cs.state),
            cs.name
        );
        rows.push_str(&format!("codespace|{}\t{}|{display}\n", cs.name, cs.branch));
    }

    // Pool slots (#457): a leased warm slot is shown by its *leasing branch* (selectable → hands
    // back that slot's window); an idle warm slot advertises a "type a branch to lease" row. Only
    // for a pooled primary-root repo, and a leased branch already open as a window is skipped so it
    // isn't listed twice.
    if let Some(status) = &pool_status {
        let now = bn_core::util::now_epoch_secs();
        for slot in &status.slots {
            // A compact "warmed <ago>" tag from the slot dir's mtime, blank when never warmed.
            let warmed = slot
                .warmed_ts
                .map(|ts| format!(" · warmed {}", crate::cmd::cron::ago((now - ts).max(0))))
                .unwrap_or_default();
            match slot.branch.as_deref() {
                Some(branch) if !branch.is_empty() => {
                    let win = wt::sanitize_branch(branch);
                    if open_windows.iter().any(|ow| ow == &win) {
                        continue; // already a live window
                    }
                    let who = slot
                        .holder
                        .as_deref()
                        .filter(|h| !h.is_empty())
                        .map(|h| format!(" · {h}"))
                        .unwrap_or_default();
                    let display = format!(
                        "  ◍: {:<14} {:<8} pool#{}{warmed}{who}",
                        branch, "(leased)", slot.n
                    );
                    rows.push_str(&format!("poolbranch|{branch}|{display}\n"));
                }
                _ if slot.warm => {
                    let display = format!(
                        "  ○: {:<14} {:<8} pool#{}{warmed} · type a branch to lease",
                        "(idle)", "(pool)", slot.n
                    );
                    rows.push_str(&format!("poolidle|{}|{display}\n", slot.n));
                }
                _ => {} // cold slot (not on disk yet) — nothing to show.
            }
        }
    }

    (rows, open_windows.len())
}

/// Open a project. Bare repos and multi-window clones get a window/worktree sub-picker; a quiet flat
/// clone just switches to its session.
fn open_project(cfg: &Config, roots: &Roots, name: &str) {
    let sess = ref_session(name);
    let repo = ref_repo(name);
    let bare = is_bare_repo(roots, name);
    let start = if bare {
        bare_repo_main_path(roots, name)
    } else {
        resolve_project_path(roots, name)
    };
    ensure_session(name, &start);

    // Build the rows once, in-process, for the initial paint; a post-action `reload()` re-runs the
    // identical builder via `--pick-stage sublist` so the list refreshes in place.
    let (rows, win_count) = build_sub_rows(cfg, roots, name, false);
    // A quiet flat clone (≤ 1 window, no worktrees) just switches to its session.
    if !bare && win_count <= 1 {
        goto_session(&sess);
        return;
    }
    let worktree_dir = roots.worktree_dir(name);

    let header = if bare {
        "Pick · PR remote branch · type a branch (pooled → warm slot) · ● live agent · ctrl-d del · ctrl-w warm · ctrl-r release"
    } else {
        "Pick window/worktree (Esc: current)"
    };

    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let root_flag = if roots.win_scoped { " --root win" } else { "" };
    let name_q = crate::shq(name);
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--delimiter=|")
        .arg("--with-nth=3")
        .arg("--print-query")
        .arg(format!("--header={header}"))
        // ctrl-d: delete the focused *idle worktree* row (guarded: refuses a dirty tree, unpushed
        // commits, or a non-worktree row), then reload in place so the removed row disappears.
        .arg(format!(
            "--bind=ctrl-d:execute({bn} tat {name_q}{root_flag} --pick-stage rmworktree --pick-arg {{1}}~{{2}})+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        // ctrl-w: warm this repo's pool to its configured size (spawns a `pool-warm` window to
        // build in and returns), guarded to a pooled repo; reload picks up any freshly-cut slots.
        .arg(format!(
            "--bind=ctrl-w:execute({bn} tat {name_q}{root_flag} --pick-stage warmpool)+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        // ctrl-r: release the focused *leased* (◍) slot back to the warm pool (`wt pool return`),
        // guarded — refuses a dirty slot; reload flips it back to an idle (○) row.
        .arg(format!(
            "--bind=ctrl-r:execute({bn} tat {name_q}{root_flag} --pick-stage releasepool --pick-arg {{1}}~{{2}})+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            goto_session(&sess);
            return;
        }
    };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(rows.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        goto_session(&sess);
        return;
    };
    if out.status.code() == Some(130) {
        goto_session(&sess);
        return;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let query = lines.next().unwrap_or("");
    let selection = lines.next().unwrap_or("");

    if selection.is_empty() && !query.is_empty() && bare {
        open_worktree(cfg, roots, name, query);
        return;
    }
    if selection.is_empty() {
        goto_session(&sess);
        return;
    }

    let mut parts = selection.splitn(3, '|');
    let kind = parts.next().unwrap_or("");
    let payload = parts.next().unwrap_or("");
    if kind == "window" {
        goto_session(&format!("{sess}:{payload}"));
    } else if kind == "remotepr" {
        open_remote_pr(cfg, roots, name, payload);
    } else if kind == "codespace" {
        // payload packs `csname\tbranch`.
        let (cn, cb) = payload.split_once('\t').unwrap_or((payload, ""));
        open_codespace(&format!("cs:{repo}/{cb}/{cn}"));
    } else if kind == "poolbranch" {
        // A leased pool slot — payload is the real leasing branch; open_worktree hands back its
        // warm slot (idempotent lease) and opens the window there.
        open_worktree(cfg, roots, name, payload);
    } else if kind == "poolidle" {
        // An idle warm slot → lease it to a branch. Typing a branch name in fzf filters this row
        // out (the query won't fuzzy-match the "(idle)" display), so the natural flow is
        // select-then-name: use the typed query when it survived, else prompt for the branch here.
        let branch = if query.is_empty() {
            prompt_branch(payload)
        } else {
            Some(query.to_string())
        };
        match branch {
            Some(b) if !b.trim().is_empty() => open_worktree(cfg, roots, name, b.trim()),
            _ => goto_session(&sess),
        }
    } else {
        let wt_path = worktree_dir.join(repo).join(payload);
        open_or_select_window(&sess, payload, &wt_path);
        goto_session(&sess);
    }
}

/// Fetch a remote-only PR's exact source branch, then hand it to the normal TAT opener. Keeping the
/// checkout decision in [`open_worktree`] preserves warm-pool leases and root-aware cold worktrees.
fn open_remote_pr(cfg: &Config, roots: &Roots, name: &str, number: &str) {
    let Ok(number) = number.parse::<i64>() else {
        eprintln!("Invalid PR number: {number}");
        crate::picker::hold();
        return;
    };
    let Some(pr) = remote_prs(roots, name)
        .into_iter()
        .find(|pr| pr.number == number)
    else {
        eprintln!("PR #{number} is no longer open or is not yours");
        crate::picker::hold();
        return;
    };
    let repo = ref_repo(name);
    let bare = roots.bare_dir(name).join(format!("{repo}.git"));
    let remote = wt::remote_for(&bare);
    if remote == wt::Remote::Other
        || !wt::prepare_remote_pr_branch(&remote, &bare.to_string_lossy(), &pr)
    {
        crate::picker::hold();
        return;
    }
    open_worktree(cfg, roots, name, &pr.branch);
}

/// Prompt on the controlling TTY for a branch name to lease onto an idle warm pool slot. Used when
/// the sub-picker's `poolidle` row is chosen with an empty query (typing a branch filters the row
/// out, so select-then-name is the fallback path). Returns `None` on EOF or an empty line.
fn prompt_branch(slot: &str) -> Option<String> {
    print!("Lease pool slot {slot} — branch to create: ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return None;
    }
    let t = buf.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// The guard for `ctrl-d` delete: an idle worktree row is only removable when it still exists, is
/// clean, and has no unpushed commits. Returns `Err(reason)` (shown to the user) otherwise. Pure so
/// it can be unit-tested without touching git.
fn delete_block(kind: &str, exists: bool, dirty: usize, unpushed: u32) -> Result<(), String> {
    if kind != "worktree" {
        return Err(format!(
            "ctrl-d deletes an idle worktree — a {} row can't be removed here",
            if kind.is_empty() { "that" } else { kind }
        ));
    }
    if !exists {
        return Err("that worktree is already gone".into());
    }
    if dirty > 0 {
        return Err(format!(
            "{dirty} uncommitted change{} — commit or discard first",
            if dirty == 1 { "" } else { "s" }
        ));
    }
    if unpushed > 0 {
        return Err(format!(
            "{unpushed} unpushed commit{} — push or merge first",
            if unpushed == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// Count commits on the worktree's `HEAD` not yet on its remote, so `ctrl-d` won't silently drop
/// unpushed work. Tries the configured upstream, then a same-named `origin/<branch>`, then the
/// remote's default branch; returns 0 when no remote base resolves (a fresh local-only branch — the
/// dirty guard still protects uncommitted work there).
fn unpushed_commits(wt: &Path) -> u32 {
    let count = |range: &str| {
        crate::git::git_capture(&["rev-list", "--count", range], Some(wt), true)
            .and_then(|s| s.trim().parse::<u32>().ok())
    };
    if let Some(n) = count("@{u}..HEAD") {
        return n;
    }
    let branch = crate::git::git_capture(&["rev-parse", "--abbrev-ref", "HEAD"], Some(wt), true)
        .unwrap_or_default();
    if !branch.is_empty() && branch != "HEAD" {
        let ob = format!("origin/{branch}");
        if crate::git::git_capture(&["rev-parse", "--verify", "--quiet", &ob], Some(wt), true)
            .is_some()
        {
            if let Some(n) = count(&format!("{ob}..HEAD")) {
                return n;
            }
        }
    }
    if let Some(base) = crate::git::git_capture(
        &["rev-parse", "--abbrev-ref", "origin/HEAD"],
        Some(wt),
        true,
    ) {
        if !base.is_empty() {
            if let Some(n) = count(&format!("{base}..HEAD")) {
                return n;
            }
        }
    }
    0
}

/// `ctrl-d` handler: remove the focused idle worktree after the [`delete_block`] guard clears. `arg`
/// is fzf's `{1}~{2}` (`kind~payload`); only a `worktree` kind is removable. Refusals print a `✗`
/// reason and `hold()` so the message survives the fzf `reload`.
fn rm_worktree(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, payload) = arg.split_once('~').unwrap_or(("", arg));
    let repo = ref_repo(project);
    let wt_path = roots.worktree_dir(project).join(repo).join(payload);
    let exists = wt_path.is_dir();
    let wt_s = wt_path.to_string_lossy().to_string();
    let dirty = if exists {
        crate::wt::dirty_count(&wt_s)
    } else {
        0
    };
    let unpushed = if exists && dirty == 0 {
        unpushed_commits(&wt_path)
    } else {
        0
    };
    if let Err(reason) = delete_block(kind, exists, dirty, unpushed) {
        eprintln!("✗ {reason}");
        crate::picker::hold();
        return;
    }
    let Some(bare) = crate::wt::find_bare_repo(&wt_path) else {
        eprintln!("✗ couldn't locate the bare repo for {repo}/{payload}");
        crate::picker::hold();
        return;
    };
    let ok = crate::wt::run_git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "remove",
        &wt_s,
    ]);
    if ok {
        crate::wt::rmdir_if_empty_repo(&wt_path, &roots.worktree_dir(project));
        let _ = bn_core::lifecycle::prune(cfg);
        println!("✓ removed {repo}/{payload}");
    } else {
        eprintln!("✗ git refused to remove {repo}/{payload} (locked or raced dirty)");
        crate::picker::hold();
    }
}

/// `ctrl-w` handler: warm the focused repo's pool up to its configured size via `bn wt pool warm`
/// (which, inside tmux, spawns a `pool-warm` window to build in — so this returns promptly and the
/// picker can reload). Refuses a repo that isn't pooled, pointing at the opt-in config.
fn warm_pool(roots: &Roots, project: &str) {
    let repo = ref_repo(project);
    if !pool_eligible(roots, project) {
        eprintln!("✗ {repo} isn't pooled — add it to ~/.config/wt/pools first");
        crate::picker::hold();
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let ok = Command::new(&exe)
        .args(["wt", "pool", "warm", "--repo", repo])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("✗ pool warm failed for {repo}");
        crate::picker::hold();
    }
}

/// `ctrl-r` handler: release the focused *leased* slot back to the warm pool. `arg` is fzf's
/// `{1}~{2}` (`kind~branch`); only a `poolbranch` row is releasable. Resolves the branch to its
/// slot via [`bn_core::pool::pool_status`], then runs `bn wt pool return <slot-path>` *without*
/// `--force`, so an uncommitted slot is refused (the message + `hold()` survive the reload).
fn release_pool(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, branch) = arg.split_once('~').unwrap_or(("", arg));
    if kind != "poolbranch" {
        eprintln!("✗ ctrl-r releases a leased pool slot — focus a leased (◍) row");
        crate::picker::hold();
        return;
    }
    let repo = ref_repo(project);
    let Some(status) = bn_core::pool::pool_status(cfg, repo) else {
        eprintln!("✗ {repo} isn't pooled");
        crate::picker::hold();
        return;
    };
    let Some(slot) = status
        .slots
        .iter()
        .find(|s| s.branch.as_deref() == Some(branch))
    else {
        eprintln!("✗ no leased slot found for {branch}");
        crate::picker::hold();
        return;
    };
    let slot_path = roots
        .worktree_dir(project)
        .join(repo)
        .join("_pool")
        .join(slot.n.to_string());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let ok = Command::new(&exe)
        .args(["wt", "pool", "return", &slot_path.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!(
            "✗ release refused for {branch} — commit/stash, or `bn wt pool return {} --force`",
            slot.n
        );
        crate::picker::hold();
    }
}

/// Dispatch a hidden `--pick-stage` callback. `name` is the focused row's hidden `ref` (`{-1}`);
/// `root_opt` re-applies the `--root` scope so the callback's discovery matches the live picker.
pub fn stage(
    cfg: &Config,
    stage: TatStage,
    name: Option<&str>,
    root_opt: Option<&str>,
    pick_arg: Option<&str>,
) {
    let roots = Roots::resolve(cfg, root_opt);
    match stage {
        TatStage::List => build_list(&roots, false),
        TatStage::Refresh => build_list(&roots, true),
        TatStage::Preview => preview(&roots, name.unwrap_or("")),
        TatStage::Kill => kill_from_line(name.unwrap_or("")),
        TatStage::SubList => {
            let (rows, _) = build_sub_rows(cfg, &roots, name.unwrap_or(""), true);
            print!("{rows}");
        }
        TatStage::RmWorktree => {
            rm_worktree(cfg, &roots, name.unwrap_or(""), pick_arg.unwrap_or(""))
        }
        TatStage::WarmPool => warm_pool(&roots, name.unwrap_or("")),
        TatStage::ReleasePool => {
            release_pool(cfg, &roots, name.unwrap_or(""), pick_arg.unwrap_or(""))
        }
    }
}

/// Split a `|`-delimited window row into exactly 5 fields (the last absorbs any extra `|`, matching
/// bash `read -r a b c d e`).
fn split5(line: &str) -> [&str; 5] {
    let mut it = line.splitn(5, '|');
    [
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
    ]
}

/// Split a decorated window row into 7 `|`-delimited fields
/// (`index|name|cmd|active|@bn_agent|@bn_agent_since|path`); the trailing `path` absorbs any extra
/// `|` it may contain, so the agent fields stay aligned.
fn split7(line: &str) -> [&str; 7] {
    let mut it = line.splitn(7, '|');
    [
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
        it.next().unwrap_or(""),
    ]
}

/// A single status dot for a window's live agent, from its `@bn_agent` state (`working` | `waiting`
/// | `done`) and `@bn_agent_since` decoration: a colored ● (working) / ● (waiting on you) / ● (done)
/// / ◌ (decorated but idle), or a two-space blank when no agent runs in the window. ANSI-colored —
/// `open_project` runs fzf with `--ansi`. Mirrors the fleet board's circular vocabulary so both read
/// the same.
fn agent_dot(state: &str, since: &str) -> String {
    let decorated = since.trim().parse::<i64>().map(|s| s > 0).unwrap_or(false);
    let state = state.trim();
    if !decorated && state.is_empty() {
        return "  ".to_string();
    }
    let (color, glyph) = match state {
        "working" => ("32", '●'), // green — actively working
        "waiting" => ("33", '●'), // yellow — waiting on you
        "done" => ("36", '●'),    // cyan — finished
        _ => ("2", '◌'),          // dim — decorated but idle/unknown
    };
    format!("\x1b[{color}m{glyph}\x1b[0m ")
}

#[cfg(test)]
mod discovery_cache_tests {
    use super::*;

    fn roots(root: &Path) -> Roots {
        Roots {
            primary: root.to_path_buf(),
            primary_bare: root.join("bare"),
            primary_worktree: root.join("worktree"),
            extras: Vec::new(),
            win_scoped: false,
        }
    }

    fn add_bare(root: &Path, repo: &str) {
        std::fs::create_dir_all(root.join("bare").join(format!("{repo}.git"))).unwrap();
    }

    #[test]
    fn ordered_refs_cache_hits_until_refresh_and_isolates_roots() {
        let _g = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
        let _ttl = bn_core::test_support::EnvGuard::set("WF_TAT_TTL", "30");
        let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "100");
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        add_bare(&first, "alpha");
        add_bare(&second, "other");

        assert!(discovery_cache_fresh(70));
        assert!(!discovery_cache_fresh(69));
        assert_eq!(ordered_refs(&roots(&first), true), vec!["alpha"]);
        add_bare(&first, "beta");
        assert_eq!(
            ordered_refs(&roots(&first), false),
            vec!["alpha"],
            "fresh cache should avoid another filesystem walk"
        );
        assert_eq!(
            ordered_refs(&roots(&first), true),
            vec!["beta", "alpha"],
            "forced refresh should expose new repositories"
        );
        assert_eq!(
            ordered_refs(&roots(&second), false),
            vec!["other"],
            "a different root must not reuse the first root's snapshot"
        );
    }

    #[test]
    fn worktree_cache_refreshes_and_corrupt_cache_is_a_miss() {
        let _g = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
        let root = tmp.path().join("root");
        let repo_dir = root.join("worktree").join("alpha");
        std::fs::create_dir_all(repo_dir.join("one")).unwrap();
        let roots = roots(&root);

        assert_eq!(scan_worktrees(&roots, "alpha", true), vec!["one"]);
        std::fs::create_dir_all(repo_dir.join("two")).unwrap();
        assert_eq!(scan_worktrees(&roots, "alpha", false), vec!["one"]);
        assert_eq!(scan_worktrees(&roots, "alpha", true), vec!["one", "two"]);

        std::fs::write(
            discovery_cache_path(),
            r#"{"roots_key":"legacy","refs_saved_at":1,"refs":["alpha"],"worktrees":{}}"#,
        )
        .unwrap();
        let legacy = read_discovery_cache();
        assert_eq!(legacy.refs, vec!["alpha"]);
        assert!(legacy.remote_prs.is_empty());

        std::fs::write(discovery_cache_path(), "not json").unwrap();
        assert!(read_discovery_cache().refs.is_empty());
        assert!(read_discovery_cache().worktrees.is_empty());
    }

    #[test]
    fn remote_pr_rows_hide_locally_represented_branches_and_escape_display_text() {
        let prs = vec![
            wt::RemotePr {
                number: 41,
                title: "Already local".into(),
                branch: "feat/local".into(),
                url: "https://example.test/41".into(),
            },
            wt::RemotePr {
                number: 42,
                title: "Remote | work\nfrom laptop".into(),
                branch: "feat/remote".into(),
                url: "https://example.test/42".into(),
            },
        ];
        let represented = HashSet::from([wt::sanitize_branch("feat/local")]);

        let rows = remote_pr_rows(&prs, &represented);

        assert!(!rows.contains("Already local"));
        assert!(rows.contains("remotepr|42|"));
        assert!(rows.contains("feat-remote"));
        assert!(rows.contains("Remote   work from laptop"));
        assert_eq!(rows.lines().count(), 1);
    }

    #[test]
    fn remote_pr_discovery_reuses_a_fresh_per_repo_cache_entry() {
        let _g = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
        let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "100");
        let root = tmp.path().join("root");
        add_bare(&root, "alpha");
        let roots = roots(&root);
        let bare = root.join("bare/alpha.git").to_string_lossy().to_string();
        let expected = wt::RemotePr {
            number: 42,
            title: "Cross-laptop work".into(),
            branch: "feat/remote".into(),
            url: "https://example.test/42".into(),
        };
        let mut discovery = DiscoveryCache::default();
        discovery.remote_prs.insert(
            bare,
            CachedRemotePrs {
                saved_at: 100,
                prs: vec![expected.clone()],
            },
        );
        write_discovery_cache(&discovery);

        assert_eq!(remote_prs(&roots, "alpha"), vec![expected]);
    }
}
