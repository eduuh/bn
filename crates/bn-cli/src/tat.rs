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

mod codespaces;
mod commands;
pub(crate) mod discovery;
mod keys;
mod paths;
mod pick;
mod preview;
mod rows;
mod status;

use codespaces::{
    codespaces, cs_account, cs_account_cfg_path, cs_auth_cmd, cs_auth_needed, cs_auth_refresh,
    cs_fetch_blocking, cs_for_repo, cs_ref_branch, cs_ref_csname, cs_ref_repo, cs_window_name,
    is_cs_ref, open_codespace,
};
use commands::{command_from_pick, Step};
use discovery::{remote_prs, remote_prs_fetch_blocking, spawn_remote_warm};
use keys::{kill_from_line, release_pool, rm_worktree, warm_pool};
use paths::{bare_repo_main_path, is_bare_repo, project_path, resolve_project_path};
use pick::{sub as pick_sub, top_level as pick_top_level};
use preview::preview;
use rows::{build_list, build_sub_rows, sub_rows_feed, SubRow};

use crate::proc::{tmux_command, tmux_ok, tmux_raw, tmux_run_try, tmux_status, warn_unreachable};
use crate::wt;
use bincode::Options;
use bn_core::{resolver::Config, style};
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
    /// As [`TatStage::SubList`], but serving worktrees from the discovery cache rather than
    /// rescanning — the repaint feed, posted every 700ms while a command is running. `sublist`
    /// passes `refresh = true`, so using it here meant a `read_dir`, a rewrite of
    /// `tat-discovery.json` and a `git worktree list` eighty-six times a minute (63ms vs 47ms per
    /// paint on a 7-worktree repo). A repaint wants the running state, not a rediscovered disk.
    #[value(name = "subtick")]
    SubTick,
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
    /// Render the sub-picker's side pane for the focused row. `--pick-arg` carries `kind~payload`:
    /// unlike the project picker's rows these differ in kind, and each kind wants a different panel.
    #[value(name = "subpreview")]
    SubPreview,
    /// Run the focused command row's verb now, in a tmux window in the repo's session.
    /// `--pick-arg` carries `kind~action-id`; guarded — refuses a non-`cmd` row. A verb already
    /// running is left alone.
    #[value(name = "runcmd")]
    RunCmd,
    /// Internal: refresh the network-backed caches (codespaces, and `<name>`'s open PRs when given)
    /// in the background. Spawned detached by the picker so no paint ever waits on a forge.
    #[value(name = "warm-remote")]
    WarmRemote,
    /// Emit the one-line status header — attention and cron, the badges that used to
    /// sit in tmux's `status-format[0]` (#1349). Bound to `start` and `ctrl-r` on both pickers via
    /// `transform-header`, and deliberately NOT to the 700ms `subtick` repaint.
    #[value(name = "statusline")]
    StatusLine,
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

/// `$XDG_CACHE_HOME` or `~/.cache`.
fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
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
                    // …and skip the main worktree: picking "main" opens the main slot instead, so
                    // listing it here would offer a checkout nothing is supposed to work in.
                    let name = base.to_string_lossy().to_string();
                    if !crate::main_slot::is_main_selection(&name) {
                        res.push(name);
                    }
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
    bn_core::tmux::exact_target(&format!("{session}:{window}"))
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
    let target = exact_tmux_target(target);
    warn_unreachable("tat", tmux_run_try(&[verb, "-t", &target]));
}

/// Create `ref`'s session (detached, a `main` window at `start`) if missing; record frecency.
fn ensure_session(r: &str, start: &Path) {
    if has_session(r) {
        return;
    }
    zoxide_add(start);
    let mut cmd = tmux_command(&[
        "new-session",
        "-d",
        "-s",
        &ref_session(r),
        "-n",
        "main",
        "-c",
    ]);
    warn_unreachable("tat", tmux_status(cmd.arg(start)));
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
        let target = exact_window_target(session, window);
        warn_unreachable("tat", tmux_run_try(&["select-window", "-t", &target]));
    } else {
        let mut cmd = tmux_command(&[
            "new-window",
            "-t",
            &session_window_target(session),
            "-n",
            window,
            "-c",
        ]);
        warn_unreachable("tat", tmux_status(cmd.arg(wt_path)));
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
    match name {
        Some(sel) if !sel.is_empty() => resolve_and_open(cfg, &roots, sel.to_string(), false),
        _ => {
            if let Some((sel, typed)) = pick_top_level(cfg, &roots, root_opt) {
                resolve_and_open(cfg, &roots, sel, typed);
            }
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
    // Neither a bare repo nor a flat clone: nothing to open. A direct `bn tat <typo>` used to fall
    // through to `open_project`, which created a tmux session named after the typo, rooted in a
    // directory that does not exist, and recorded that path in zoxide. Say so instead; `hold()` so
    // a typed query's message is read before the popup closes.
    if !is_bare_repo(roots, &sel) && !flat_git {
        eprintln!("No such project: {sel}");
        if typed {
            crate::picker::hold();
        }
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

    // "main" is a request for the main slot, never for the `worktree/<repo>/main` checkout — that
    // one exists to stay clean and current, and is not a place to work. Falls through to the
    // normal path only when the repo has no main slot (opted out of pooling, or no bare clone).
    if crate::main_slot::is_main_selection(branch) {
        let landing = bare_repo_main_path(roots, r);
        ensure_session(r, &landing);
        open_or_select_window(&sess, &sanitized, &landing);
        goto_session(&sess);
        return;
    }

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

/// Whether a new branch on `ref` should lease a warm pool slot instead of a cold worktree: any
/// untagged primary-root ref, since every repo pools now. Tagged / `--root win` refs keep the cold
/// path — pools are a primary-root, heavy-monorepo concept.
fn pool_eligible(roots: &Roots, r: &str) -> bool {
    ref_tag(r).is_empty() && !roots.win_scoped
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

/// Open a project. Bare repos and multi-window clones get a window/worktree sub-picker; a quiet flat
/// clone just switches to its session.
///
/// Running a command is not a request to leave the list, so a step that ran one reopens it. A loop
/// rather than recursion because there is no bound on how many verbs someone fires in a sitting.
fn open_project(cfg: &Config, roots: &Roots, name: &str) {
    while open_project_step(cfg, roots, name) == Step::Reopen {}
}

/// One paint of the sub-picker, from building the rows to acting on the selection.
fn open_project_step(cfg: &Config, roots: &Roots, name: &str) -> Step {
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
    // identical builder via `--pick-stage sublist` so the list refreshes in place. Identical means
    // `refresh = true` here too: the worktree discovery cache is for the 700ms repaint (`subtick`),
    // not for the open. Served from the cache, a worktree cut or removed in the last thirty seconds
    // — exactly the one the person just acted on — was missing from, or still on, the first paint.
    let (rows, win_count, states) = build_sub_rows(cfg, roots, name, true);
    // A quiet flat clone (≤ 1 window, no worktrees) just switches to its session.
    if !bare && win_count <= 1 {
        goto_session(&sess);
        return Step::Done;
    }
    let worktree_dir = roots.worktree_dir(name);
    let header = if bare {
        "Pick a branch, slot, window or command"
    } else {
        "Pick window/worktree"
    };
    let picked = pick_sub(
        cfg,
        roots,
        name,
        header,
        roots.win_scoped.then_some("win"),
        rows,
        &states,
    );

    if picked.row.is_none() && !picked.query.is_empty() && bare {
        open_worktree(cfg, roots, name, &picked.query);
        return Step::Done;
    }
    let Some(row) = picked.row else {
        goto_session(&sess);
        return Step::Done;
    };
    let payload = row.payload();

    // Acting on a row without leaving the list; what follows takes you somewhere instead.
    if let Some(step) = commands::act_on_row(cfg, roots, name, &picked.key, row.kind(), &payload) {
        return step;
    }
    match row {
        SubRow::Window(index) => goto_session(&format!("{sess}:{index}")),
        SubRow::RemotePr(number) => open_remote_pr(cfg, roots, name, &number),
        SubRow::Codespace {
            name: cn,
            branch: cb,
        } => {
            open_codespace(&format!("cs:{repo}/{cb}/{cn}"));
        }
        SubRow::PoolBranch(branch) => {
            // A leased pool slot — payload is the real leasing branch; open_worktree hands back its
            // warm slot (idempotent lease) and opens the window there.
            open_worktree(cfg, roots, name, &branch);
        }
        SubRow::PoolIdle(slot) => {
            // An idle warm slot → lease it to a branch. Typing a branch name filters this row out,
            // so the natural flow is still select-then-name when the query is empty.
            let branch = if picked.query.is_empty() {
                prompt_branch(&slot)
            } else {
                Some(picked.query.clone())
            };
            match branch {
                Some(b) if !b.trim().is_empty() => open_worktree(cfg, roots, name, b.trim()),
                _ => goto_session(&sess),
            }
        }
        SubRow::Worktree(branch) => {
            let wt_path = worktree_dir.join(repo).join(&branch);
            open_or_select_window(&sess, &branch, &wt_path);
            goto_session(&sess);
        }
        SubRow::CmdSep | SubRow::Cmd(_) => return Step::Reopen,
    }
    Step::Done
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
    // Unwrapped once: absent and empty mean the same thing to every handler below.
    let row = name.unwrap_or("");
    let arg = pick_arg.unwrap_or("");
    match stage {
        TatStage::List => build_list(&roots, false),
        TatStage::Refresh => {
            // ctrl-r is the one place a person asked for current data and can see it arrive, so it
            // is the one place the forge is worth waiting on.
            cs_fetch_blocking();
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                remote_prs_fetch_blocking(&roots, name);
            }
            build_list(&roots, true)
        }
        TatStage::Preview => preview(&roots, row),
        TatStage::Kill => kill_from_line(row),
        TatStage::SubList => {
            let (rows, _, _) = build_sub_rows(cfg, &roots, row, true);
            print!("{}", sub_rows_feed(&rows));
        }
        TatStage::SubTick => {
            let (rows, _, _) = build_sub_rows(cfg, &roots, row, false);
            print!("{}", sub_rows_feed(&rows));
        }
        TatStage::RmWorktree => rm_worktree(cfg, &roots, row, arg),
        TatStage::WarmPool => warm_pool(&roots, row),
        TatStage::SubPreview => preview::row(cfg, &roots, row, arg),
        TatStage::RunCmd => command_from_pick(cfg, &roots, row, arg),
        TatStage::ReleasePool => release_pool(cfg, &roots, row, arg),
        TatStage::WarmRemote => {
            cs_fetch_blocking();
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                remote_prs_fetch_blocking(&roots, name);
            }
        }
        TatStage::StatusLine => status::emit_status_line(cfg),
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
    let (tone, glyph) = match state {
        "working" => (style::Tone::Ok, '●'), // actively working
        "waiting" | "needs-decision" => (style::Tone::Warn, '●'), // waiting on you
        "blocked" | "failed" => (style::Tone::Danger, '●'), // stopped, not on its own account
        "limited" => (style::Tone::Meta, '●'), // paused on a usage cap
        "done" => (style::Tone::Accent, '●'), // finished
        _ => (style::Tone::Meta, '◌'),       // decorated but idle/unknown
    };
    format!("{} ", style::Ink::always().tone(tone, &glyph.to_string()))
}

#[cfg(test)]
#[path = "tat/discovery_cache_tests.rs"]
mod discovery_cache_tests;
