//! Pure pool policy (#457) — the decisions behind pool-backed dispatch, with no I/O.
//!
//! `bn wt pool` keeps a small set of *warm* worktree slots for a heavy monorepo so an agent task
//! skips the cold `npm install`/build every fresh checkout would pay (the CLI primitive lives in
//! `bn-cli`'s `wt_pool`). This module holds the two decisions that are pure enough to unit-test
//! away from git and the filesystem:
//!   - **which repos are pooled** — parsing the opt-in `~/.config/wt/pools` config
//!     ([`parse_pool_size`]); and
//!   - **slot selection** — picking the lowest idle slot ([`pick_slot`]); plus
//!   - the **dispatch mode** gate — pool vs cold — that `bn dispatch` routes on (pool by default,
//!     `--no-pool` to force cold) ([`dispatch_worktree_mode`]).
//!
//! Keeping these here mirrors [`crate::repo_resolve`]: the CLI does the I/O (read the config, lease
//! the slot, run git) while the policy — parse rules, clamping, precedence — is tested in one place.

use crate::resolver::Config;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The smallest usable pool: the reserved controller slot plus one worker. Slot
/// [`crate::agent::CONTROLLER_POOL_SLOT`] never serves workers, so a size of 1 would leave a pooled
/// repo with nowhere to dispatch — every configured size clamps up to this.
pub const MIN_POOL_SIZE: usize = 2;

/// Pool size for `repo` parsed from the pools config `content`, or `None` when the repo isn't
/// pooled. Lines are `<repo> <size>`; `#` starts a comment; blank lines are ignored.
///
/// The size counts **all** slots, of which slot 1 is the reserved controller slot — so `<repo> 4`
/// means one controller plus three workers. A missing or unparseable size, and any size below the
/// floor, clamps to [`MIN_POOL_SIZE`]: a pooled repo always has the controller slot plus at least
/// one worker.
///
/// A `default <size>` line — or its `*` alias — sets a fallback that makes *any* repo poolable at
/// that size, so a heavy checkout anywhere can lease a warm slot without being pre-registered. An
/// explicit `<repo> <size>` line always overrides the default; with no default line an unlisted repo
/// stays unpooled (`None`), exactly as before. `default` and `*` are reserved keys and can't name a
/// real repo. This is the sole definition of *which repos are pooled*.
pub fn parse_pool_size(content: &str, repo: &str) -> Option<usize> {
    let mut default_size: Option<usize> = None;
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let size = it
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(MIN_POOL_SIZE)
            .max(MIN_POOL_SIZE);
        if key == repo {
            // An explicit per-repo entry always wins over the default fallback.
            return Some(size);
        }
        if is_default_key(key) {
            default_size = Some(size);
        }
    }
    default_size
}

/// The reserved keys a pools-config line uses to declare the any-repo fallback size rather than a
/// concrete repo: `default` and its `*` alias.
fn is_default_key(key: &str) -> bool {
    key == "default" || key == "*"
}

/// Every `(repo, size)` pair in the pools config `content`, in file order — the multi-repo view of
/// [`parse_pool_size`] (same line grammar: `<repo> <size>`, `#` comments, blank lines skipped; a
/// missing/unparseable size clamps to [`MIN_POOL_SIZE`]). Used to enumerate every pooled
/// repo for a rollup (the dashboard's per-repo pool state) without probing each name individually.
/// The `default`/`*` fallback line is skipped — it isn't a concrete repo, only the any-repo size.
pub fn parse_pooled_repos(content: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(repo) = it.next() else { continue };
        if is_default_key(repo) {
            continue;
        }
        let size = it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
        out.push((repo.to_string(), size.max(MIN_POOL_SIZE)));
    }
    out
}

/// The largest pool a single repo may be given. Each slot is a full checkout plus its build cache,
/// so the ceiling is disk, not taste — and a typo in a size field shouldn't be able to ask for a
/// hundred of them.
pub const MAX_POOL_SIZE: usize = 16;

/// Set (or with `size: None`, remove) `repo`'s entry in the pools config `content`, returning the
/// new file text. A rewrite rather than an append: an existing entry is edited in place, so setting
/// a size twice can't leave two lines whose winner depends on file order.
///
/// Comments, blank lines, ordering and the `default`/`*` fallback line are all preserved — this is
/// a file a human also edits, and a write from the dashboard has no business reformatting it. A new
/// entry appends. Size is clamped to `MIN_POOL_SIZE..=MAX_POOL_SIZE`, matching the parser's own
/// floor — a size of 1 is all controller slot and no worker, so the writer must not be able to
/// produce a line the reader would silently widen back to 2.
///
/// Pure so the whole edit is unit-tested; [`set_pool_size`] does the I/O.
pub fn edit_pool_size(content: &str, repo: &str, size: Option<usize>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in content.lines() {
        let key = line
            .split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next();
        if key == Some(repo) {
            // The repo's line: rewrite it, or drop it entirely when unpooling.
            if let Some(n) = size {
                if !replaced {
                    out.push(format!("{repo} {}", n.clamp(MIN_POOL_SIZE, MAX_POOL_SIZE)));
                }
            }
            replaced = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        if let Some(n) = size {
            out.push(format!("{repo} {}", n.clamp(MIN_POOL_SIZE, MAX_POOL_SIZE)));
        }
    }
    let mut text = out.join("\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Pool a repo at `size` slots, or unpool it with `size: None`, by rewriting the pools config at
/// [`pools_config_path`]. Creates the file (and its parent) on first use — a machine that has never
/// pooled anything has neither.
///
/// The write is atomic: a temp sibling renamed into place, so `bn wt pool get` reading the config
/// concurrently sees either the old file or the new one, never a half-written line that would parse
/// as a different pool size.
pub fn set_pool_size(repo: &str, size: Option<usize>) -> std::io::Result<()> {
    let path = pools_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let next = edit_pool_size(&current, repo, size);
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, next)?;
    std::fs::rename(&tmp, &path)
}

/// The lowest idle **worker** slot in `2..=size` that isn't in `leased`, or `None` when every one
/// is leased (the pool is full). Slots are 1-indexed to match their on-disk `_pool/<n>` dirs, and
/// slot [`CONTROLLER_POOL_SLOT`] is never returned here — it belongs to `bn control` alone (see
/// [`pick_controller_slot`]). Leases are durable (held until `return`), so callers try this first
/// and only reclaim genuinely abandoned slots as a fallback before deciding the pool is full.
///
/// [`CONTROLLER_POOL_SLOT`]: crate::agent::CONTROLLER_POOL_SLOT
pub fn pick_slot(size: usize, leased: &BTreeSet<u32>) -> Option<u32> {
    let first_worker = crate::agent::CONTROLLER_POOL_SLOT + 1;
    (first_worker..=size as u32).find(|n| !leased.contains(n))
}

/// The controller's reserved slot, or `None` when it is already leased. `bn control start` is the
/// only caller: the controller never rotates through the worker range, so this is a fixed-slot
/// availability check rather than a search.
pub fn pick_controller_slot(leased: &BTreeSet<u32>) -> Option<u32> {
    let n = crate::agent::CONTROLLER_POOL_SLOT;
    (!leased.contains(&n)).then_some(n)
}

/// Whether a pool lease's holder can still be alive — the decision behind recovering a pool after
/// the machine (or WSL) dies and comes back.
///
/// A lease records the *caller* pid (the long-lived agent/shell), so normally liveness is "is that
/// pid still in `/proc`?". But a pid check alone is unsound across a reboot: WSL shutdown / a host
/// restart kills every process, and the OS then hands the *same low pid numbers* back to brand-new,
/// unrelated processes. A stale lease's recorded pid can therefore read as **alive** after a
/// restart, pinning the slot leased forever ("Pool full") to a holder that no longer exists.
///
/// Tagging each lease with the current **boot id** (a value that changes on every boot) closes that
/// hole: a lease whose boot id differs from the current one was taken in a previous boot, so its
/// holder *cannot* still exist regardless of what the pid check says — it's [`StalePriorBoot`] and
/// safe to reclaim. Only when the boot ids match (or either is unknown — an off-Linux host, or a
/// lease written before boot-scoping) do we fall back to the pid check.
///
/// [`StalePriorBoot`]: LeaseLiveness::StalePriorBoot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseLiveness {
    /// The holder pid is alive in the *current* boot — a genuinely held lease.
    Live,
    /// Same boot, but the holder pid is gone: a normally-abandoned lease (holder crashed/exited).
    DeadThisBoot,
    /// The lease predates the current boot (its boot id differs), so the machine/WSL restarted and
    /// the holder is provably gone — reclaim it even though its recycled pid may read as alive.
    StalePriorBoot,
}

/// Classify a lease's holder liveness from its recorded boot id, the current boot id, and whether
/// its recorded pid is alive — see [`LeaseLiveness`]. A boot-id mismatch (both known) is decisive
/// (`StalePriorBoot`); otherwise the pid check decides. Pure so the recovery policy is unit-tested
/// away from `/proc` and git.
pub fn lease_liveness(
    lease_boot: Option<&str>,
    current_boot: Option<&str>,
    pid_alive: bool,
) -> LeaseLiveness {
    if let (Some(lease), Some(current)) = (lease_boot, current_boot) {
        if lease != current {
            return LeaseLiveness::StalePriorBoot;
        }
    }
    if pid_alive {
        LeaseLiveness::Live
    } else {
        LeaseLiveness::DeadThisBoot
    }
}

/// Whether a `bn dispatch` should lease a warm pool slot or cut a cold worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeMode {
    /// Lease a warm `bn wt pool` slot (deps/build cache intact) — skips the cold checkout+install.
    /// The default dispatch path.
    Pool,
    /// Create a fresh cold worktree, paying the post-create install hook. The `--no-pool` opt-out.
    Cold,
}

/// Built-in pool size for a dispatch to a repo with no explicit `~/.config/wt/pools` entry.
/// Leasing is the default dispatch path, so *every* repo pools at this size unless it names its own
/// size (an explicit `<repo> <size>` line — or a `default`/`*` fallback — overrides it). Slots are
/// created on demand and reused (reap-safe); a full pool spills to a cold worktree. Kept small — a
/// few concurrent dispatch lanes per repo before the cold spill. Counts the reserved controller
/// slot, so this is three worker lanes plus `bn control`.
pub const DEFAULT_DISPATCH_POOL_SIZE: usize = 4;

/// Route a dispatch to a warm pool slot or a cold worktree. Leasing is the **default**: every
/// dispatch pools (a reap-safe slot, created on demand, deps + build cache reused) unless
/// `--no-pool` forces the legacy cold checkout. The lease itself still degrades to cold at runtime
/// when the repo has no bare clone or its pool is full (see the dispatch fallback), so `Pool` is
/// always a safe choice — the caller need not pre-check whether the repo is pooled.
pub fn dispatch_worktree_mode(no_pool: bool) -> WorktreeMode {
    if no_pool {
        WorktreeMode::Cold
    } else {
        WorktreeMode::Pool
    }
}

// --- pools config + live status IO -----------------------------------------
//
// The `bn wt pool` primitive (deps/build cache, lease store) lives in `bn-cli`, but its *observable
// state* — which repos are pooled, and each slot's idle/leased shape — is a read the dashboard needs
// too. Keeping the read here (the IO counterpart of the pure policy above) lets `bn-web` surface a
// repo's warm-pool without depending on `bn-cli`, reading the very files the CLI writes:
//   - the opt-in config at [`pools_config_path`] (`<repo> <size>` lines), and
//   - the per-pool lease store `<worktree>/<repo>/_pool/leases.json` the CLI persists.

/// Path of the pools config — `$WT_POOLS` if set, else `<XDG>/wt/pools` (sibling of the clone
/// manifest `wt/repos`). The single source of truth both `bn-cli`'s `wt_pool` and this read use.
pub fn pools_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("WT_POOLS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let cfgdir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_str()));
    PathBuf::from(format!("{cfgdir}/wt/pools"))
}

fn home_str() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// Pool size for `repo` from the config, or `None` when the repo has no explicit/`default` entry —
/// the file read behind [`parse_pool_size`]. `bn wt pool` and `bn orchestrate` ask this directly;
/// a plain `bn dispatch` instead pools *every* repo, falling back to [`DEFAULT_DISPATCH_POOL_SIZE`]
/// when this is `None`.
pub fn pool_size(repo: &str) -> Option<usize> {
    let content = std::fs::read_to_string(pools_config_path()).ok()?;
    parse_pool_size(&content, repo)
}

/// Every pooled `(repo, size)` from the config, or empty when it's absent/unset — the file read
/// behind [`parse_pooled_repos`], for enumerating pools without knowing the repo names up front.
pub fn pooled_repos() -> Vec<(String, usize)> {
    std::fs::read_to_string(pools_config_path())
        .map(|c| parse_pooled_repos(&c))
        .unwrap_or_default()
}

/// The worktree root the pool slots live under, honoring `$WT_ROOT` (the `wtw` wrapper's override)
/// exactly as `bn-cli`'s `wt::layout` does, else the resolved `cfg.worktree_dir`.
fn worktree_root(cfg: &Config) -> PathBuf {
    match std::env::var("WT_ROOT") {
        Ok(root) if !root.is_empty() => PathBuf::from(root).join("worktree"),
        _ => cfg.worktree_dir.clone(),
    }
}

/// A pool's on-disk lease store shape (a read-only mirror of what `bn-cli`'s `wt_pool` writes): the
/// fields this status view reads. Extra fields the CLI writes (e.g. `pid`) are ignored.
#[derive(Deserialize, Default)]
struct LeaseFile {
    #[serde(default)]
    slots: BTreeMap<u32, LeaseEntry>,
}

#[derive(Deserialize)]
struct LeaseEntry {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    holder: String,
    /// Epoch seconds the lease was taken.
    #[serde(default)]
    at: i64,
}

/// One pool slot's live state, 1-indexed to match its on-disk `_pool/<n>` dir.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PoolSlot {
    pub n: u32,
    /// Held by a branch (present in `leases.json`).
    pub leased: bool,
    /// The checkout exists on disk (warm) — ready to lease without a cold create.
    pub warm: bool,
    /// The leased branch, when leased.
    pub branch: Option<String>,
    /// The lease holder (its recorded `USER`), when leased.
    pub holder: Option<String>,
    /// Lease age in seconds, when leased.
    pub age_secs: Option<i64>,
    /// This slot dir's mtime (epoch secs) — a best-effort "last warmed/refreshed" proxy, or `None`
    /// when the slot isn't on disk yet.
    pub warmed_ts: Option<i64>,
}

/// A repo's warm-pool state — configured size plus each slot's idle/leased shape. Only produced for
/// a pooled repo (so `enabled` is always `true`); a non-pooled repo yields `None`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PoolStatus {
    pub repo: String,
    /// Always `true` for a returned status — the repo is in the pools config. Present so the
    /// serialized shape carries an explicit enabled flag alongside a not-pooled repo's absence.
    pub enabled: bool,
    /// Configured slot count (the pool size).
    pub total: usize,
    /// Slots currently leased to a branch.
    pub leased: usize,
    /// Idle slots that are warm on disk (a live checkout ready to lease).
    pub warm: usize,
    /// Idle slots (`total - leased`) — warm plus not-yet-created.
    pub idle: usize,
    /// Best-effort "last warmed": the newest slot-dir mtime (epoch secs), or `None` when no slot
    /// exists on disk yet. A proxy — the pool has no explicit warm timestamp.
    pub last_warmed_ts: Option<i64>,
    pub slots: Vec<PoolSlot>,
}

/// Read the pool's lease store (`<pool_dir>/leases.json`), treating a missing/corrupt file as an
/// empty pool — mirroring the CLI's defensive read.
fn read_lease_slots(pool_dir: &Path) -> BTreeMap<u32, LeaseEntry> {
    std::fs::read_to_string(pool_dir.join("leases.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<LeaseFile>(&s).ok())
        .unwrap_or_default()
        .slots
}

/// A slot dir's mtime as epoch-seconds, or `None` when unreadable.
fn dir_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Live warm-pool status for `repo`, or `None` when it isn't pooled. Reads the lease store and scans
/// the `_pool/<n>` dirs — the read counterpart of the CLI's `pool status`, exposed for the dashboard.
pub fn pool_status(cfg: &Config, repo: &str) -> Option<PoolStatus> {
    let size = pool_size(repo)?;
    Some(compute_status(cfg, repo, size))
}

/// Warm-pool status for every pooled repo (from the config), for a multi-repo rollup.
pub fn all_pool_status(cfg: &Config) -> Vec<PoolStatus> {
    pooled_repos()
        .into_iter()
        .map(|(repo, size)| compute_status(cfg, &repo, size))
        .collect()
}

/// Fold a pool's lease store + on-disk slot dirs into its [`PoolStatus`]. Over-size lease entries
/// (a slot beyond the current config, e.g. after shrinking) are ignored — the view is the configured
/// `1..=size` window, matching the CLI's `pool status`.
fn compute_status(cfg: &Config, repo: &str, size: usize) -> PoolStatus {
    let pool_dir = worktree_root(cfg).join(repo).join("_pool");
    let leases = read_lease_slots(&pool_dir);
    let now = crate::util::now_epoch_secs();

    let mut slots = Vec::with_capacity(size);
    let (mut leased, mut warm) = (0usize, 0usize);
    let mut last_warmed_ts: Option<i64> = None;

    for n in 1..=size as u32 {
        let slot_dir = pool_dir.join(n.to_string());
        let on_disk = slot_dir.is_dir();
        let warmed_ts = if on_disk { dir_mtime(&slot_dir) } else { None };
        if let Some(ts) = warmed_ts {
            last_warmed_ts = Some(last_warmed_ts.map_or(ts, |m| m.max(ts)));
        }
        match leases.get(&n) {
            Some(l) => {
                leased += 1;
                slots.push(PoolSlot {
                    n,
                    leased: true,
                    warm: on_disk,
                    branch: Some(l.branch.clone()),
                    holder: Some(l.holder.clone()),
                    age_secs: Some((now - l.at).max(0)),
                    warmed_ts,
                });
            }
            None => {
                if on_disk {
                    warm += 1;
                }
                slots.push(PoolSlot {
                    n,
                    leased: false,
                    warm: on_disk,
                    branch: None,
                    holder: None,
                    age_secs: None,
                    warmed_ts,
                });
            }
        }
    }

    PoolStatus {
        repo: repo.to_string(),
        enabled: true,
        total: size,
        leased,
        warm,
        idle: size.saturating_sub(leased),
        last_warmed_ts,
        slots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pool_size_reads_and_clamps() {
        let cfg = "# comment\narena 4\nbigmono   3  # trailing\nzero 0\n\n";
        assert_eq!(parse_pool_size(cfg, "arena"), Some(4));
        assert_eq!(parse_pool_size(cfg, "bigmono"), Some(3));
        // A size below the floor clamps up — a pooled repo always has the reserved controller
        // slot plus at least one worker.
        assert_eq!(parse_pool_size(cfg, "zero"), Some(MIN_POOL_SIZE));
        // Not listed → not pooled.
        assert_eq!(parse_pool_size(cfg, "missing"), None);
    }

    #[test]
    fn parse_pool_size_defaults_absent_size_to_the_floor() {
        assert_eq!(parse_pool_size("arena\n", "arena"), Some(MIN_POOL_SIZE));
    }

    #[test]
    fn default_line_makes_any_repo_poolable() {
        let cfg = "# any repo can have a pool\ndefault 2\narena 4\n";
        // An explicit entry still wins over the default.
        assert_eq!(parse_pool_size(cfg, "arena"), Some(4));
        // Any unlisted repo now resolves to the default size.
        assert_eq!(parse_pool_size(cfg, "some-other-repo"), Some(2));
        assert_eq!(parse_pool_size(cfg, "bigmono"), Some(2));
    }

    #[test]
    fn star_is_an_alias_for_default_and_clamps() {
        // `*` behaves exactly like `default`; a 0 size clamps up to the floor.
        assert_eq!(parse_pool_size("* 0\n", "whatever"), Some(MIN_POOL_SIZE));
        assert_eq!(parse_pool_size("*  3\n", "whatever"), Some(3));
    }

    #[test]
    fn without_a_default_line_unlisted_repos_stay_unpooled() {
        // Backward compatible: no default/* line → an unlisted repo is not pooled.
        assert_eq!(parse_pool_size("arena 4\n", "unlisted"), None);
    }

    #[test]
    fn parse_pooled_repos_skips_the_default_fallback() {
        // The default/* fallback isn't a concrete repo, so enumeration omits it.
        let pools = parse_pooled_repos("default 2\narena 4\n* 3\nforge 1\n");
        assert_eq!(
            pools,
            vec![
                ("arena".to_string(), 4),
                ("forge".to_string(), MIN_POOL_SIZE)
            ]
        );
    }

    #[test]
    fn pick_slot_returns_lowest_idle_worker_never_the_controller() {
        let mut leased = BTreeSet::new();
        // Slot 1 belongs to `bn control`, so an empty pool hands a worker slot 2.
        assert_eq!(pick_slot(4, &leased), Some(2));
        leased.insert(2);
        assert_eq!(pick_slot(4, &leased), Some(3));
        // A hole (3 free while 2 and 4 are taken) is filled lowest-first.
        leased.insert(4);
        assert_eq!(pick_slot(4, &leased), Some(3));
        // Even with slot 1 idle, a worker never gets it.
        let only_workers_leased: BTreeSet<u32> = [2, 3].into_iter().collect();
        assert_eq!(pick_slot(3, &only_workers_leased), None);
    }

    #[test]
    fn pick_slot_none_when_full() {
        let leased: BTreeSet<u32> = [1, 2].into_iter().collect();
        assert_eq!(pick_slot(2, &leased), None);
        // A pool with no worker range at all.
        assert_eq!(pick_slot(1, &BTreeSet::new()), None);
        assert_eq!(pick_slot(0, &BTreeSet::new()), None);
    }

    #[test]
    fn controller_slot_is_handed_out_only_while_idle() {
        assert_eq!(pick_controller_slot(&BTreeSet::new()), Some(1));
        // Workers holding 2..n never block the controller.
        let workers: BTreeSet<u32> = [2, 3].into_iter().collect();
        assert_eq!(pick_controller_slot(&workers), Some(1));
        let held: BTreeSet<u32> = [1].into_iter().collect();
        assert_eq!(pick_controller_slot(&held), None);
    }

    #[test]
    fn lease_liveness_boot_mismatch_is_stale_even_if_pid_reads_alive() {
        // The WSL/reboot case: the recorded pid was recycled to a live process, but the lease was
        // taken in a *different* boot — so its real holder is gone and the slot must be reclaimed.
        assert_eq!(
            lease_liveness(Some("boot-A"), Some("boot-B"), true),
            LeaseLiveness::StalePriorBoot
        );
        assert_eq!(
            lease_liveness(Some("boot-A"), Some("boot-B"), false),
            LeaseLiveness::StalePriorBoot
        );
    }

    #[test]
    fn lease_liveness_same_boot_falls_back_to_pid() {
        // Same boot → the pid check is authoritative: alive holder stays, dead holder is abandoned.
        assert_eq!(
            lease_liveness(Some("boot-A"), Some("boot-A"), true),
            LeaseLiveness::Live
        );
        assert_eq!(
            lease_liveness(Some("boot-A"), Some("boot-A"), false),
            LeaseLiveness::DeadThisBoot
        );
    }

    #[test]
    fn lease_liveness_unknown_boot_ids_use_pid_only() {
        // A pre-boot-scoping lease (no id) or an off-Linux host (no current id) can't be compared,
        // so we never wrongly declare it prior-boot — the pid check alone decides.
        assert_eq!(
            lease_liveness(None, Some("boot-A"), true),
            LeaseLiveness::Live
        );
        assert_eq!(
            lease_liveness(Some("boot-A"), None, false),
            LeaseLiveness::DeadThisBoot
        );
        assert_eq!(lease_liveness(None, None, true), LeaseLiveness::Live);
    }

    #[test]
    fn dispatch_pools_by_default_unless_no_pool() {
        // Leasing is the default dispatch path: a plain dispatch pools.
        assert_eq!(
            dispatch_worktree_mode(false),
            WorktreeMode::Pool,
            "a plain dispatch leases a warm slot by default"
        );
        // `--no-pool` forces the legacy cold worktree.
        assert_eq!(
            dispatch_worktree_mode(true),
            WorktreeMode::Cold,
            "--no-pool opts out to a cold worktree"
        );
    }

    #[test]
    fn parse_pooled_repos_enumerates_every_pool() {
        let cfg = "# heavy monorepos\narena 4\nbigmono   3  # trailing\nzero 0\nbare\n\n";
        let pools = parse_pooled_repos(cfg);
        assert_eq!(
            pools,
            vec![
                ("arena".to_string(), 4),
                ("bigmono".to_string(), 3),
                // Size 0 clamps up to the floor; a bare line defaults to it.
                ("zero".to_string(), MIN_POOL_SIZE),
                ("bare".to_string(), MIN_POOL_SIZE),
            ]
        );
        assert!(parse_pooled_repos("# only comments\n\n").is_empty());
    }

    #[test]
    fn compute_status_folds_leases_and_warm_slots() {
        use crate::test_support::{env_lock, EnvGuard};
        use std::fs;

        let _g = env_lock();
        // Pin the clock so the lease age is deterministic, and neutralize $WT_ROOT so the pool dir
        // resolves under the config's worktree_dir.
        let _now = EnvGuard::set("BN_NOW", "1000");
        let _root = EnvGuard::remove("WT_ROOT");

        let tmp = tempfile::tempdir().unwrap();
        let cfg =
            crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");
        let pool_dir = tmp.path().join("arena").join("_pool");
        fs::create_dir_all(&pool_dir).unwrap();
        // Slot 1 warm-idle (checkout dir on disk, unleased); slot 2 leased; slot 3 cold (no dir).
        fs::create_dir_all(pool_dir.join("1")).unwrap();
        fs::write(
            pool_dir.join("leases.json"),
            r#"{"slots":{"2":{"branch":"dispatch/x","holder":"edd","pid":42,"at":700}}}"#,
        )
        .unwrap();

        let st = compute_status(&cfg, "arena", 3);
        assert_eq!(st.total, 3);
        assert_eq!(st.leased, 1);
        assert_eq!(st.warm, 1);
        assert_eq!(st.idle, 2);
        assert!(st.enabled);
        assert_eq!(st.slots.len(), 3);
        // Slot 1: warm, idle.
        assert!(st.slots[0].warm && !st.slots[0].leased);
        // Slot 2: leased with its branch/holder/age (1000 - 700).
        assert!(st.slots[1].leased);
        assert_eq!(st.slots[1].branch.as_deref(), Some("dispatch/x"));
        assert_eq!(st.slots[1].holder.as_deref(), Some("edd"));
        assert_eq!(st.slots[1].age_secs, Some(300));
        // Slot 3: neither warm nor leased.
        assert!(!st.slots[2].warm && !st.slots[2].leased);
    }

    #[test]
    fn pool_status_reads_the_config_and_all_pool_status_enumerates() {
        use crate::test_support::{env_lock, EnvGuard};
        use std::fs;

        let _g = env_lock();
        let _root = EnvGuard::remove("WT_ROOT");

        let tmp = tempfile::tempdir().unwrap();
        let pools = tmp.path().join("pools");
        fs::write(&pools, "arena 2\nforge 1\n").unwrap();
        let _cfg_env = EnvGuard::set("WT_POOLS", &pools);

        let cfg =
            crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");

        // A pooled repo resolves to a status; a non-pooled one is None.
        let arena = pool_status(&cfg, "arena").expect("arena is pooled");
        assert_eq!(arena.total, 2);
        assert!(pool_status(&cfg, "unlisted").is_none());

        // The rollup enumerates every configured pool.
        let all = all_pool_status(&cfg);
        let names: Vec<&str> = all.iter().map(|s| s.repo.as_str()).collect();
        assert_eq!(names, vec!["arena", "forge"]);
    }

    #[test]
    fn edit_pool_size_preserves_the_file_a_human_also_edits() {
        let before = "# my pools\n\ndefault 2\narena 4  # the monorepo\nforge 1\n";
        // An existing entry is edited in place — comments, blanks, order and the default line stay.
        let after = edit_pool_size(before, "arena", Some(6));
        assert_eq!(after, "# my pools\n\ndefault 2\narena 6\nforge 1\n");
        // Unpooling drops only that line.
        assert_eq!(
            edit_pool_size(before, "arena", None),
            "# my pools\n\ndefault 2\nforge 1\n"
        );
        // A repo not yet listed appends.
        assert_eq!(
            edit_pool_size(before, "bn", Some(3)),
            "# my pools\n\ndefault 2\narena 4  # the monorepo\nforge 1\nbn 3\n"
        );
    }

    #[test]
    fn edit_pool_size_clamps_and_never_leaves_a_duplicate() {
        // The parser floors at MIN_POOL_SIZE (slot 1 is the controller's) and a slot is a whole
        // checkout, so both ends are bounded.
        assert_eq!(
            edit_pool_size("", "bn", Some(0)),
            format!("bn {MIN_POOL_SIZE}\n")
        );
        assert_eq!(
            edit_pool_size("", "bn", Some(1)),
            format!("bn {MIN_POOL_SIZE}\n")
        );
        assert_eq!(
            edit_pool_size("", "bn", Some(9_999)),
            format!("bn {MAX_POOL_SIZE}\n")
        );
        // A file that already has two entries for one repo collapses to one, so the result can't
        // depend on which line the parser happens to reach first.
        let dupes = "bn 2\nbn 5\nforge 1\n";
        assert_eq!(edit_pool_size(dupes, "bn", Some(3)), "bn 3\nforge 1\n");
        assert_eq!(edit_pool_size(dupes, "bn", None), "forge 1\n");
        // Unpooling the only entry leaves an empty file, not a stray newline.
        assert_eq!(edit_pool_size("bn 2\n", "bn", None), "");
    }

    #[test]
    fn edit_then_parse_round_trips_through_the_reader() {
        // The write has to be legible to the parser that gates every pooled dispatch.
        let text = edit_pool_size("", "arena", Some(4));
        assert_eq!(parse_pool_size(&text, "arena"), Some(4));
        assert_eq!(parse_pool_size(&text, "other"), None);
        let off = edit_pool_size(&text, "arena", None);
        assert_eq!(parse_pool_size(&off, "arena"), None);
        assert!(parse_pooled_repos(&off).is_empty());
    }
}
