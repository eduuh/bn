//! The `bn tat` discovery cache — what the picker knows without asking the world.
//!
//! Split out of `tat.rs` unchanged. Everything here answers a picker row: the frecency-ordered
//! project refs, a bare repo's worktree names, and the open PRs on its remote. The first two are
//! filesystem reads; the third is a `gh`/`az` network call, which is why it is cached at all.
//!
//! The rule for this module: **the interactive path never blocks on the network.** A stale answer
//! rendered now beats a fresh one rendered in two seconds, so a miss serves what it has and hands
//! the refetch to a detached `bn tat --pick-stage warm-remote` that nothing waits on.

use super::{
    cache_home, is_bare_repo, list_worktrees, ref_repo, sort_by_frecency, write_atomic, Roots,
};
use crate::wt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const DISCOVERY_CACHE_TTL_SECS: i64 = 30;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct DiscoveryCache {
    pub(super) roots_key: String,
    pub(super) refs_saved_at: i64,
    pub(super) refs: Vec<String>,
    pub(super) worktrees: HashMap<String, CachedWorktrees>,
    #[serde(default)]
    pub(super) remote_prs: HashMap<String, CachedRemotePrs>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct CachedWorktrees {
    pub(super) saved_at: i64,
    pub(super) names: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct CachedRemotePrs {
    pub(super) saved_at: i64,
    pub(super) prs: Vec<wt::RemotePr>,
}

pub(super) fn discovery_cache_path() -> PathBuf {
    cache_home().join("wt").join("tat-discovery.json")
}

fn discovery_ttl_secs() -> i64 {
    std::env::var("WF_TAT_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(DISCOVERY_CACHE_TTL_SECS)
}

pub(super) fn discovery_cache_fresh(saved_at: i64) -> bool {
    bn_core::util::now_epoch_secs().saturating_sub(saved_at) <= discovery_ttl_secs()
}

pub(super) fn read_discovery_cache() -> DiscoveryCache {
    std::fs::read_to_string(discovery_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(super) fn write_discovery_cache(cache: &DiscoveryCache) {
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
pub(super) fn ordered_refs(roots: &Roots, refresh: bool) -> Vec<String> {
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

/// Fast worktree discovery for a bare repo: the branch-dir names under `worktree_dir/<repo>/`, read
/// straight from the filesystem (skipping the `_pool` warm-slot dir and dotfiles) so the sub-picker
/// paints without a `git worktree list` subprocess. Falls back to the git listing when that dir
/// can't be read yet (e.g. before the first worktree is cut). Sorted for a stable row order.
pub(super) fn scan_worktrees(roots: &Roots, name: &str, refresh: bool) -> Vec<String> {
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
pub(super) fn remote_prs(roots: &Roots, name: &str) -> Vec<wt::RemotePr> {
    if !is_bare_repo(roots, name) {
        return Vec::new();
    }
    let key = remote_pr_cache_key(roots, name);
    let hit = read_discovery_cache().remote_prs.remove(&key);
    if !hit
        .as_ref()
        .is_some_and(|h| discovery_cache_fresh(h.saved_at))
    {
        spawn_remote_warm(Some(name), roots.win_scoped);
    }
    hit.map(|h| h.prs).unwrap_or_default()
}

/// The discovery-cache key for a repo's open PRs: its bare path, so two roots holding the same repo
/// name keep separate entries.
fn remote_pr_cache_key(roots: &Roots, name: &str) -> String {
    roots
        .bare_dir(name)
        .join(format!("{}.git", ref_repo(name)))
        .to_string_lossy()
        .to_string()
}

/// The blocking half of [`remote_prs`]: ask the forge for my open PRs and write them to the
/// discovery cache. Background-only, like [`cs_fetch_blocking`] — `gh pr list` and the Azure
/// DevOps equivalent are seconds of network apiece.
pub(super) fn remote_prs_fetch_blocking(roots: &Roots, name: &str) {
    if !is_bare_repo(roots, name) {
        return;
    }
    let bare = roots.bare_dir(name).join(format!("{}.git", ref_repo(name)));
    let remote = wt::remote_for(&bare);
    let prs = if remote == wt::Remote::Other {
        Vec::new()
    } else {
        wt::my_open_prs(&remote)
    };
    let mut cache = read_discovery_cache();
    cache.remote_prs.insert(
        remote_pr_cache_key(roots, name),
        CachedRemotePrs {
            saved_at: bn_core::util::now_epoch_secs(),
            prs,
        },
    );
    write_discovery_cache(&cache);
}

/// How long a spawned warm run suppresses another for the same target. A picker open triggers a
/// warm; the sub-picker opened a second later must not trigger a duplicate for the same repo, and
/// an fzf `reload()` must not start one per keystroke.
const WARM_DEBOUNCE_SECS: u64 = 15;

fn warm_stamp_path(target: &str) -> PathBuf {
    let safe: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    cache_home().join("wt").join(format!("tat-warm-{safe}"))
}

/// Refresh the network-backed caches in a **detached** `bn tat --pick-stage warm-remote` and return
/// at once. Nothing waits on it: stdio is closed, so an fzf `reload()` that spawns it does not
/// block on the child either, and the run outlives the popup that started it.
///
/// Debounced through a stamp file so repeated opens (or a reload bind) cannot fan out into a pile
/// of concurrent `gh` processes. Best effort throughout — a machine that cannot spawn simply keeps
/// serving the cache it has.
pub(super) fn spawn_remote_warm(name: Option<&str>, win_scoped: bool) {
    // The valve, in the shape the rest of bn uses (`BN_AGENT_GUARD=0`): a machine that wants the
    // picker to touch nothing but its caches sets `BN_TAT_WARM=0`. Tests set it so a spawn can
    // never re-enter the test binary with picker arguments.
    if matches!(
        std::env::var("BN_TAT_WARM").as_deref().map(str::trim),
        Ok("0")
    ) {
        return;
    }
    let stamp = warm_stamp_path(name.unwrap_or("all"));
    let recent = std::fs::metadata(&stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() < WARM_DEBOUNCE_SECS);
    if recent {
        return;
    }
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Stamped before the spawn, so a failed spawn still backs off rather than retrying on every
    // paint.
    let _ = std::fs::write(&stamp, "");
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("tat");
    if let Some(name) = name {
        cmd.arg(name);
    }
    if win_scoped {
        cmd.args(["--root", "win"]);
    }
    let _ = cmd
        .args(["--pick-stage", "warm-remote"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
