//! Pure pool policy (#457) — the decisions behind pool-backed dispatch, with no I/O.
//!
//! `bn wt pool` keeps a small set of *warm* worktree slots for every repo so an agent task
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

pub mod slot;
pub use slot::{
    pick_for, slot_state, BusyReason, Pick, SlotFacts, SlotState, RESERVATION_TTL_SECS,
};

use crate::resolver::Config;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The smallest usable pool: the reserved main slot plus one worker. Slot
/// [`crate::agent::MAIN_POOL_SLOT`] never serves workers, so a size of 1 would leave a pooled
/// repo with nowhere to dispatch — every configured size clamps up to this.
pub const MIN_POOL_SIZE: usize = 2;

/// Pool size for `repo` parsed from the pools config `content` — `None` only when the repo has
/// been explicitly opted *out*. Lines are `<repo> <size>`; `#` starts a comment; blank lines are
/// ignored.
///
/// The size counts **all** slots, of which slot 1 is the reserved main slot — so `<repo> 4` means
/// one main slot plus three workers. A missing or unparseable size, and any size below the floor,
/// clamps to [`MIN_POOL_SIZE`]: a pooled repo always has the main slot plus at least one worker.
///
/// **Every repo pools.** A repo the config never mentions gets [`DEFAULT_POOL_SIZE`], because
/// leasing a warm slot is the way work happens here — a checkout that needs a pool should not have
/// to be registered first, and the main slot has to exist for every repo whether or not anyone
/// wrote its name down. The file is therefore only ever an *override*: `<repo> <size>` pins a
/// different size, a `default <size>` line — or its `*` alias — moves the fallback for unlisted
/// repos, and `<repo> 0` (or `off`/`none`) is the one way to say "this repo does not pool at all".
/// `default` and `*` are reserved keys and can't name a real repo. This is the sole definition of
/// *which repos are pooled*.
pub fn parse_pool_size(content: &str, repo: &str) -> Option<usize> {
    let mut default_size: Option<usize> = Some(DEFAULT_POOL_SIZE);
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let size = parse_size(it.next());
        if key == repo {
            // An explicit per-repo entry always wins over the default fallback.
            return size;
        }
        if is_default_key(key) {
            default_size = size;
        }
    }
    default_size
}

/// One line's size field: `None` for the explicit opt-out (`0`, `off`, `none`, `-`), else the
/// number clamped up to [`MIN_POOL_SIZE`]. A missing or unparseable field is a typo, not an
/// opt-out — it clamps to the floor, so a malformed line can never quietly unpool a repo.
fn parse_size(field: Option<&str>) -> Option<usize> {
    let Some(field) = field else {
        return Some(MIN_POOL_SIZE);
    };
    if matches!(field, "0" | "off" | "none" | "-") {
        return None;
    }
    Some(
        field
            .parse::<usize>()
            .unwrap_or(MIN_POOL_SIZE)
            .max(MIN_POOL_SIZE),
    )
}

/// The reserved keys a pools-config line uses to declare the any-repo fallback size rather than a
/// concrete repo: `default` and its `*` alias.
fn is_default_key(key: &str) -> bool {
    key == "default" || key == "*"
}

/// Every `(repo, size)` pair *named* in the pools config `content`, in file order — the multi-repo
/// view of [`parse_pool_size`] (same line grammar: `<repo> <size>`, `#` comments, blank lines
/// skipped; a missing/unparseable size clamps to [`MIN_POOL_SIZE`]). Used to enumerate pools for a
/// rollup (the dashboard's per-repo pool state) and for the scheduled warm sweep, without probing
/// each name individually. Repos opted out with `<repo> 0` are dropped, and the `default`/`*`
/// fallback line is skipped — it isn't a concrete repo, only the any-repo size.
///
/// This lists the repos written down, **not** every repo that pools: since [`parse_pool_size`]
/// pools an unlisted repo at [`DEFAULT_POOL_SIZE`], the two answers differ on purpose. Leasing is
/// universal; the expensive scheduled *warm* (a checkout plus `build.sh` per slot) stays opt-in to
/// the repos whose names are in the file.
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
        let Some(size) = parse_size(it.next()) else {
            continue;
        };
        out.push((repo.to_string(), size));
    }
    out
}

/// The largest pool a single repo may be given. Each slot is a full checkout plus its build cache,
/// so the ceiling is disk, not taste — and a typo in a size field shouldn't be able to ask for a
/// hundred of them.
pub const MAX_POOL_SIZE: usize = 16;

/// Set (or with `size: None`, opt out of) pooling for `repo` in the pools config `content`,
/// returning the new file text. A rewrite rather than an append: an existing entry is edited in
/// place, so setting a size twice can't leave two lines whose winner depends on file order.
///
/// `size: None` writes `<repo> 0` — the explicit opt-out — rather than deleting the line. Deleting
/// it would no longer unpool anything now that [`parse_pool_size`] pools an unmentioned repo at
/// [`DEFAULT_POOL_SIZE`]; the line has to stay to say "not this one".
///
/// Comments, blank lines, ordering and the `default`/`*` fallback line are all preserved — this is
/// a file a human also edits, and a write from the dashboard has no business reformatting it. A new
/// entry appends. Size is clamped to `MIN_POOL_SIZE..=MAX_POOL_SIZE`, matching the parser's own
/// floor — a size of 1 is all main slot and no worker, so the writer must not be able to produce a
/// line the reader would silently widen back to 2.
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
            // The repo's line: rewrite it in place (a second one, if any, is dropped).
            if !replaced {
                out.push(pool_line(repo, size));
            }
            replaced = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        out.push(pool_line(repo, size));
    }
    let mut text = out.join("\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// One config line for `repo`: its clamped size, or the `0` opt-out when `size` is `None`.
fn pool_line(repo: &str, size: Option<usize>) -> String {
    match size {
        Some(n) => format!("{repo} {}", n.clamp(MIN_POOL_SIZE, MAX_POOL_SIZE)),
        None => format!("{repo} 0"),
    }
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
/// slot [`MAIN_POOL_SLOT`] is never returned here — it is the reserved main slot (see
/// [`pick_controller_slot`]). Leases are durable (held until `return`), so callers try this first
/// and only reclaim genuinely abandoned slots as a fallback before deciding the pool is full.
///
/// [`MAIN_POOL_SLOT`]: crate::agent::MAIN_POOL_SLOT
pub fn pick_slot(size: usize, leased: &BTreeSet<u32>) -> Option<u32> {
    let first_worker = crate::agent::MAIN_POOL_SLOT + 1;
    (first_worker..=size as u32).find(|n| !leased.contains(n))
}

/// The reserved main slot, or `None` when it is already leased. `bn control start` is the only
/// caller: the controller never rotates through the worker range, so this is a fixed-slot
/// availability check rather than a search.
pub fn pick_controller_slot(leased: &BTreeSet<u32>) -> Option<u32> {
    let n = crate::agent::MAIN_POOL_SLOT;
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

/// The facts gathered about one leased slot, for the "has this lease been abandoned?" decision.
/// The caller does the tmux/git I/O and hands the result to [`lease_abandoned`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseFacts {
    /// The recorded holder pid is still alive.
    pub holder_alive: bool,
    /// Seconds since the lease was taken.
    pub age_secs: i64,
    /// A live agent — or any live pane — is sitting in the checkout.
    pub occupied: bool,
    /// The checkout has uncommitted work.
    pub dirty: bool,
}

/// Whether a lease is *provably* abandoned, so its slot can go back to warm-idle.
///
/// A lease is durable until `return` on purpose, so this demands proof rather than a guess: nobody
/// is in the checkout, there is no uncommitted work to lose, and the lease is either held by a dead
/// process or old enough that an agent would long since have shown up.
///
/// That last clause is the load-bearing one. `holder_alive` is a much weaker signal than it looks:
/// the recorded holder is the *caller's* parent, which for a dispatch is the tmux server (a popup
/// command's parent) or the operator's shell — processes that outlive every agent they launch.
/// Gating reclamation on the pid alone therefore frees nothing in normal use: leases accumulate
/// until the pool reads full and every dispatch spills into a cold worktree instead of leasing a
/// warm slot, which is exactly [`LeaseLiveness`]'s blind spot.
///
/// `grace_secs` must comfortably cover the gap between a dispatch taking a slot and its agent
/// appearing in tmux — a lease younger than that is not yet evidence of anything.
pub fn lease_abandoned(facts: LeaseFacts, grace_secs: i64) -> bool {
    if facts.occupied || facts.dirty {
        return false;
    }
    !facts.holder_alive || facts.age_secs > grace_secs
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

/// Built-in pool size for a repo with no explicit `~/.config/wt/pools` entry — which is most of
/// them. Pooling is not opt-in: *every* repo pools at this size unless the config names its own
/// size (an explicit `<repo> <size>` line, or a `default`/`*` fallback) or opts out with
/// `<repo> 0`. Slots are created on demand and reused (reap-safe); a full pool spills to a cold
/// worktree. Kept small — a few concurrent lanes per repo before the cold spill. Counts the
/// reserved main slot, so this is three worker lanes plus `_pool/1`.
pub const DEFAULT_POOL_SIZE: usize = 4;

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

/// Pool size for `repo` — the file read behind [`parse_pool_size`], and the question every caller
/// asks before leasing. `None` means the repo was explicitly opted out (`<repo> 0`); every other
/// repo pools, at its configured size or [`DEFAULT_POOL_SIZE`].
///
/// A config file that is absent or unreadable is *not* an opt-out: a machine that has never
/// written one still pools every repo at the default, which is what makes `_pool/1` — the main
/// slot — something every repo has without anyone registering it.
pub fn pool_size(repo: &str) -> Option<usize> {
    let content = std::fs::read_to_string(pools_config_path()).unwrap_or_default();
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
    #[serde(default)]
    comparison_slot: Option<u32>,
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
        let owner = leases.get(&n).map(|lease| (n, lease)).or_else(|| {
            leases
                .iter()
                .find(|(_, lease)| lease.comparison_slot == Some(n))
                .map(|(owner, lease)| (*owner, lease))
        });
        match owner {
            Some((owner_slot, l)) => {
                leased += 1;
                slots.push(PoolSlot {
                    n,
                    leased: true,
                    warm: on_disk,
                    branch: Some(if owner_slot == n {
                        l.branch.clone()
                    } else {
                        format!("main (for {})", l.branch)
                    }),
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

    // --- parse_pool_size --------------------------------------------------

    #[test]
    fn explicit_repo_size_parses_and_wins_over_default() {
        let cfg = "default 3\nbn 5\n";
        assert_eq!(parse_pool_size(cfg, "bn"), Some(5));
        // A repo not listed falls back to the default.
        assert_eq!(parse_pool_size(cfg, "other"), Some(3));
    }

    #[test]
    fn unlisted_repo_pools_at_the_built_in_default() {
        // The point of the universal default: a repo nobody registered still pools, so it still
        // has a main slot and can still lease.
        assert_eq!(parse_pool_size("bn 4\n", "other"), Some(DEFAULT_POOL_SIZE));
        assert_eq!(parse_pool_size("", "anything"), Some(DEFAULT_POOL_SIZE));
    }

    #[test]
    fn zero_is_the_explicit_opt_out() {
        for field in ["0", "off", "none", "-"] {
            assert_eq!(parse_pool_size(&format!("bn {field}\n"), "bn"), None);
        }
        // Opting one repo out leaves every other repo pooled.
        assert_eq!(parse_pool_size("bn 0\n", "other"), Some(DEFAULT_POOL_SIZE));
    }

    #[test]
    fn a_default_line_of_zero_unpools_every_unlisted_repo() {
        let cfg = "default 0\nbn 4\n";
        assert_eq!(parse_pool_size(cfg, "other"), None);
        assert_eq!(parse_pool_size(cfg, "bn"), Some(4));
    }

    #[test]
    fn missing_or_bad_size_clamps_to_floor() {
        assert_eq!(parse_pool_size("bn\n", "bn"), Some(MIN_POOL_SIZE));
        assert_eq!(
            parse_pool_size("bn notanumber\n", "bn"),
            Some(MIN_POOL_SIZE)
        );
        assert_eq!(parse_pool_size("bn 1\n", "bn"), Some(MIN_POOL_SIZE));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let cfg = "\n  # a comment\nbn 4  # trailing\n\n";
        assert_eq!(parse_pool_size(cfg, "bn"), Some(4));
    }

    #[test]
    fn star_is_a_default_alias() {
        assert_eq!(parse_pool_size("* 6\n", "anything"), Some(6));
    }

    #[test]
    fn last_default_line_wins() {
        // default_size is overwritten as lines are scanned; the last one is the effective fallback.
        assert_eq!(parse_pool_size("default 3\ndefault 7\n", "x"), Some(7));
    }

    // --- parse_pooled_repos ----------------------------------------------

    #[test]
    fn pooled_repos_lists_concrete_repos_only() {
        let cfg = "default 3\nbn 5\n# c\napp 2\n";
        assert_eq!(
            parse_pooled_repos(cfg),
            vec![("bn".to_string(), 5), ("app".to_string(), 2)]
        );
    }

    #[test]
    fn pooled_repos_skips_star_fallback() {
        assert_eq!(
            parse_pooled_repos("* 4\nbn 5\n"),
            vec![("bn".to_string(), 5)]
        );
    }

    #[test]
    fn pooled_repos_clamps_floor() {
        assert_eq!(parse_pooled_repos("bn 1\n"), vec![("bn".to_string(), 2)]);
        assert_eq!(parse_pooled_repos("bn\n"), vec![("bn".to_string(), 2)]);
    }

    #[test]
    fn pooled_repos_skips_opted_out_repos() {
        assert_eq!(
            parse_pooled_repos("bn 5\napp 0\n"),
            vec![("bn".to_string(), 5)]
        );
    }

    // --- edit_pool_size ---------------------------------------------------

    #[test]
    fn edit_appends_new_entry() {
        assert_eq!(edit_pool_size("", "bn", Some(4)), "bn 4\n");
    }

    #[test]
    fn edit_rewrites_in_place_preserving_order_and_comments() {
        let cfg = "# header\napp 2\nbn 3\n";
        assert_eq!(
            edit_pool_size(cfg, "bn", Some(5)),
            "# header\napp 2\nbn 5\n"
        );
    }

    #[test]
    fn edit_writes_the_opt_out_marker_on_none() {
        // Deleting the line would no longer unpool anything — the repo would fall back to the
        // universal default — so unpooling has to leave a `0` behind.
        assert_eq!(edit_pool_size("app 2\nbn 3\n", "bn", None), "app 2\nbn 0\n");
        assert_eq!(parse_pool_size("app 2\nbn 0\n", "bn"), None);
    }

    #[test]
    fn edit_opts_out_a_repo_that_had_no_line() {
        assert_eq!(edit_pool_size("app 2\n", "bn", None), "app 2\nbn 0\n");
    }

    #[test]
    fn edit_clamps_size_bounds() {
        assert_eq!(edit_pool_size("", "bn", Some(1)), "bn 2\n");
        assert_eq!(
            edit_pool_size("", "bn", Some(999)),
            format!("bn {MAX_POOL_SIZE}\n")
        );
    }

    #[test]
    fn edit_does_not_duplicate_when_repo_already_present() {
        // Even a config with two lines for the repo collapses to one on rewrite.
        let out = edit_pool_size("bn 2\nbn 9\n", "bn", Some(4));
        assert_eq!(out.matches("bn ").count(), 1);
        assert_eq!(out, "bn 4\n");
    }

    #[test]
    fn edit_preserves_default_line() {
        assert_eq!(
            edit_pool_size("default 3\n", "bn", Some(4)),
            "default 3\nbn 4\n"
        );
    }

    // --- pick_slot / pick_controller_slot --------------------------------

    #[test]
    fn pick_slot_returns_lowest_free_worker() {
        let leased = BTreeSet::new();
        // Slot 1 is the controller's; the lowest worker is 2.
        assert_eq!(pick_slot(4, &leased), Some(2));
    }

    #[test]
    fn pick_slot_skips_leased_and_controller() {
        let leased: BTreeSet<u32> = [1, 2, 3].into_iter().collect();
        assert_eq!(pick_slot(4, &leased), Some(4));
    }

    #[test]
    fn pick_slot_none_when_full() {
        let leased: BTreeSet<u32> = [2, 3, 4].into_iter().collect();
        assert_eq!(pick_slot(4, &leased), None);
    }

    #[test]
    fn pick_slot_never_returns_controller_even_if_free() {
        // Only slot 1 (controller) is nominally in range for size 1 — never a worker.
        assert_eq!(pick_slot(1, &BTreeSet::new()), None);
    }

    #[test]
    fn controller_slot_availability() {
        assert_eq!(pick_controller_slot(&BTreeSet::new()), Some(1));
        let leased: BTreeSet<u32> = [1].into_iter().collect();
        assert_eq!(pick_controller_slot(&leased), None);
    }

    // --- lease_liveness ---------------------------------------------------

    #[test]
    fn prior_boot_is_stale_regardless_of_pid() {
        assert_eq!(
            lease_liveness(Some("boot-a"), Some("boot-b"), true),
            LeaseLiveness::StalePriorBoot
        );
    }

    #[test]
    fn same_boot_falls_back_to_pid() {
        assert_eq!(
            lease_liveness(Some("boot-a"), Some("boot-a"), true),
            LeaseLiveness::Live
        );
        assert_eq!(
            lease_liveness(Some("boot-a"), Some("boot-a"), false),
            LeaseLiveness::DeadThisBoot
        );
    }

    #[test]
    fn unknown_boot_falls_back_to_pid() {
        assert_eq!(lease_liveness(None, Some("b"), true), LeaseLiveness::Live);
        assert_eq!(
            lease_liveness(Some("a"), None, false),
            LeaseLiveness::DeadThisBoot
        );
    }

    // --- ledger_repair ----------------------------------------------------

    #[test]
    fn ledger_agrees_when_empty_or_matching() {
        assert_eq!(ledger_repair(None, &[]), LedgerRepair::Agrees);
        assert_eq!(ledger_repair(Some("feat"), &[]), LedgerRepair::Agrees);
        assert_eq!(
            ledger_repair(Some("feat"), &["feat".to_string()]),
            LedgerRepair::Agrees
        );
    }

    #[test]
    fn ledger_adopts_unleased_occupant() {
        assert_eq!(
            ledger_repair(None, &["feat".to_string()]),
            LedgerRepair::Adopt("feat".to_string())
        );
    }

    #[test]
    fn ledger_adopts_when_lease_names_wrong_tenant() {
        assert_eq!(
            ledger_repair(Some("stale"), &["feat".to_string(), "other".to_string()]),
            LedgerRepair::Adopt("feat".to_string())
        );
    }

    // --- lease_abandoned --------------------------------------------------

    const GRACE: i64 = 5 * 60;

    fn facts(holder_alive: bool, age_secs: i64) -> LeaseFacts {
        LeaseFacts {
            holder_alive,
            age_secs,
            occupied: false,
            dirty: false,
        }
    }

    #[test]
    fn dead_holder_is_abandoned_immediately() {
        assert!(lease_abandoned(facts(false, 0), GRACE));
    }

    #[test]
    fn immortal_holder_is_abandoned_once_past_grace() {
        // The regression this exists for: a tmux server / login shell holder never dies, so the pid
        // check alone would pin the slot forever and the pool would read full for good.
        assert!(!lease_abandoned(facts(true, GRACE), GRACE));
        assert!(lease_abandoned(facts(true, GRACE + 1), GRACE));
    }

    #[test]
    fn occupied_or_dirty_slot_is_never_abandoned() {
        let occupied = LeaseFacts {
            occupied: true,
            ..facts(false, 10 * GRACE)
        };
        assert!(!lease_abandoned(occupied, GRACE));
        let dirty = LeaseFacts {
            dirty: true,
            ..facts(false, 10 * GRACE)
        };
        assert!(!lease_abandoned(dirty, GRACE));
    }

    // --- dispatch_worktree_mode ------------------------------------------

    #[test]
    fn dispatch_mode_defaults_to_pool() {
        assert_eq!(dispatch_worktree_mode(false), WorktreeMode::Pool);
        assert_eq!(dispatch_worktree_mode(true), WorktreeMode::Cold);
    }

    // --- compute_status (on-disk pool read) ------------------------------

    #[test]
    fn compute_status_folds_leases_and_disk_slots() {
        let _g = crate::test_support::env_lock();
        // Neutralize $WT_ROOT so `worktree_root` uses cfg.worktree_dir deterministically.
        let _wt = crate::test_support::EnvGuard::remove("WT_ROOT");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cfg = crate::test_support::config_with_roots(root, root, root, "testhost");

        // Lay out `<worktree>/bn/_pool/{leases.json, 2, 3}` — slot 2 warm-on-disk, slot 3 absent.
        let pool_dir = root.join("bn").join("_pool");
        std::fs::create_dir_all(pool_dir.join("2")).unwrap();
        let now = crate::util::now_epoch_secs();
        let leases = format!(
            r#"{{"slots":{{"2":{{"branch":"feat","holder":"me","at":{}}}}}}}"#,
            now - 30
        );
        std::fs::write(pool_dir.join("leases.json"), leases).unwrap();

        let st = compute_status(&cfg, "bn", 4);
        assert_eq!(st.total, 4);
        assert_eq!(st.leased, 1);
        // Slot 2 is leased (not counted as free-warm); no other slot is on disk.
        assert_eq!(st.warm, 0);
        assert_eq!(st.idle, 3); // total - leased
        assert_eq!(st.slots.len(), 4);
        let s2 = &st.slots[1];
        assert!(s2.leased && s2.warm);
        assert_eq!(s2.branch.as_deref(), Some("feat"));
        assert!(s2.age_secs.unwrap() >= 30);
        // A slot with no dir and no lease is idle + cold.
        let s3 = &st.slots[2];
        assert!(!s3.leased && !s3.warm && s3.branch.is_none());
    }

    #[test]
    fn compute_status_counts_free_warm_slot() {
        let _g = crate::test_support::env_lock();
        let _wt = crate::test_support::EnvGuard::remove("WT_ROOT");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cfg = crate::test_support::config_with_roots(root, root, root, "testhost");
        // Slot 2 exists on disk but is unleased → a warm, idle slot ready to lease.
        std::fs::create_dir_all(root.join("bn").join("_pool").join("2")).unwrap();
        let st = compute_status(&cfg, "bn", 3);
        assert_eq!(st.leased, 0);
        assert_eq!(st.warm, 1);
        assert_eq!(st.idle, 3);
        assert!(st.last_warmed_ts.is_some());
    }
}
