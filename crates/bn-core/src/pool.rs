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

/// What the pool's ledger must do about a slot a live agent is working in — the policy behind
/// "never lease a slot that is already in use".
///
/// `leases.json` is a *record* of what the pool handed out, not evidence of what is running, and
/// the two drift constantly: an agent launched in place takes an agent lease and no pool lease at
/// all, and a kill that resolves the wrong worktree hands back a slot somebody is still working in.
/// Once they drift, the pool reads its own ledger, calls the slot idle, and switches a new branch
/// into a checkout that is in use.
///
/// So the ledger defers to the live occupant: the agent-lease registry (and the tmux panes it
/// mirrors) is the authority on who is in a slot, and this decides how the record catches up.
/// Branch names on both sides must already be normalized ([`crate::agent::normalized_branch`]) —
/// the registry stores `feature-x` where the pool may have written `feature/X`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerRepair {
    /// Nothing to do: the slot has no live occupant, or the lease already names one of them.
    Agrees,
    /// The slot is occupied by an agent the lease doesn't name — adopt this tenant, so the slot
    /// reads as held (and by whom) rather than idle. Covers both an unleased occupied slot and a
    /// lease left behind by a tenant that is no longer the one in the checkout.
    Adopt(String),
}

/// How the pool ledger's entry for one slot should change given who is actually working in it.
/// `lease` is the branch the ledger records for the slot (`None` when unleased), `occupants` the
/// label of every live agent found in it, oldest first. See [`LedgerRepair`].
pub fn ledger_repair(lease: Option<&str>, occupants: &[String]) -> LedgerRepair {
    let Some(first) = occupants.first() else {
        // Nobody home. An unleased slot stays idle; a leased one keeps its lease, which is durable
        // until `return` precisely so a slot survives the gap between a dispatch and its agent
        // appearing in tmux.
        return LedgerRepair::Agrees;
    };
    match lease {
        Some(branch) if occupants.iter().any(|o| o == branch) => LedgerRepair::Agrees,
        _ => LedgerRepair::Adopt(first.clone()),
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
