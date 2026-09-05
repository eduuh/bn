//! Pure pool policy (#457) — the decisions behind pool-backed dispatch, with no I/O.
//!
//! `bn wt pool` keeps a small set of *warm* worktree slots for every repo so an agent task
//! skips the cold `npm install`/build every fresh checkout would pay (the CLI primitive lives in
//! `bn-cli`'s `wt_pool`). This module holds the decisions that are pure enough to unit-test
//! away from git and the filesystem:
//!   - **how big a pool is** — derived from the slots that exist rather than declared in a config
//!     ([`derived_size`]); and
//!   - **slot selection** — picking the lowest idle slot ([`pick_slot`]); plus
//!   - the **dispatch mode** gate — pool vs cold — that `bn dispatch` routes on (pool by default,
//!     `--no-pool` to force cold) ([`dispatch_worktree_mode`]).
//!
//! Keeping these here mirrors [`crate::repo_resolve`]: the CLI does the I/O (scan the slots, lease
//! one, run git) while the policy — the clamp, the pick, the mode — is tested in one place.

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
/// repo with nowhere to dispatch. Below [`DEFAULT_POOL_SIZE`], so it now only bounds the
/// slot-selection helpers; a derived size can never land here.
pub const MIN_POOL_SIZE: usize = 2;

/// The largest pool a single repo may grow to. Each slot is a full checkout plus its build cache,
/// so the ceiling is disk, not taste — and demand-driven growth needs a stop.
pub const MAX_POOL_SIZE: usize = 16;

/// The size a pool derives to, from its high-water mark — the highest slot that already exists on
/// disk or holds a lease.
///
/// **There is no pool config.** There used to be one (`~/.config/wt/pools`, `<repo> <size>` lines
/// plus a `<repo> 0` opt-out), and it was a standing tax: a repo nobody had written down still got
/// slots but never got warmed, the size drifted from the slots actually on disk, and every new
/// clone was one more line somebody had to remember. So size is *observed* rather than declared:
///
///   * never below [`DEFAULT_POOL_SIZE`], so every repo — a clone made a minute ago included —
///     has the main slot plus three worker lanes without anyone registering it;
///   * never below what is already there, so a pool cannot shrink under its own live leases (the
///     failure mode of a flat constant: this machine had nine of ten `bn` slots leased); and
///   * never above [`MAX_POOL_SIZE`], because each slot is a full checkout plus its build cache.
///
/// Growth is the other half, and it lives at the lease: a dispatch that finds the pool full raises
/// the mark by one (see `wt_pool::ensure_spare_slot`), so a busy repo climbs to fit its work and an
/// idle one stays at four. Nothing is stored — the slots and the lease store *are* the record.
pub fn derived_size(high_water: usize) -> usize {
    high_water.clamp(DEFAULT_POOL_SIZE, MAX_POOL_SIZE)
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

/// The floor every pool starts at, and the size every repo has until its own work pushes it
/// higher. Pooling is unconditional — there is nothing to opt into and nothing to opt out of, so
/// this is what a brand-new clone is worth on its first dispatch. Slots are created on demand and
/// reused (reap-safe); a full pool grows by one, up to [`MAX_POOL_SIZE`], before spilling cold.
/// Counts the reserved main slot, so this is three worker lanes plus `_pool/1`.
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

// --- live pool IO ----------------------------------------------------------
//
// The `bn wt pool` primitive (deps/build cache, lease store) lives in `bn-cli`, but its *observable
// state* — how big each pool is, and each slot's idle/leased shape — is a read the dashboard needs
// too. Keeping the read here (the IO counterpart of the pure policy above) lets `bn-web` surface a
// repo's warm-pool without depending on `bn-cli`, reading the very files the CLI writes: the
// `_pool/<n>` slot dirs and the lease store `<worktree>/<repo>/_pool/leases.json`. There is no
// config to read — see [`derived_size`].

/// The directory holding `repo`'s pool slots: `<worktree_root>/<repo>/_pool`.
pub fn pool_dir(cfg: &Config, repo: &str) -> PathBuf {
    worktree_root(cfg).join(repo).join("_pool")
}

/// A pool's high-water mark: the highest slot number that exists on disk or holds a lease.
///
/// Both halves matter and neither is sufficient. The slot dirs alone miss a lease whose checkout
/// was removed out of band, and the lease store alone misses every warm idle slot — which is most
/// of a healthy pool. Taking the max of the two is what lets [`derived_size`] promise a pool never
/// shrinks under its own live leases.
///
/// Zero for a repo with no pool yet, which [`derived_size`] then floors to [`DEFAULT_POOL_SIZE`].
pub fn pool_high_water(pool_dir: &Path) -> usize {
    let on_disk = std::fs::read_dir(pool_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let leased = read_lease_slots(pool_dir)
        .keys()
        .copied()
        .max()
        .unwrap_or(0);
    on_disk.max(leased) as usize
}

/// Pool size for `repo` — the question every caller asks before leasing, and now an observation
/// rather than a lookup. Always a number: every repo pools (see [`derived_size`]).
pub fn pool_size(cfg: &Config, repo: &str) -> usize {
    derived_size(pool_high_water(&pool_dir(cfg, repo)))
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
/// every repo, so `enabled` is always `true`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PoolStatus {
    pub repo: String,
    /// Always `true`: pooling is unconditional now. Kept in the serialized shape so the dashboard
    /// and its generated TypeScript types don't churn over a field that only ever had one value.
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

/// Live warm-pool status for `repo`. Reads the lease store and scans the `_pool/<n>` dirs — the
/// read counterpart of the CLI's `pool status`, exposed for the dashboard. No longer optional:
/// every repo pools, so "is this one pooled?" has stopped being a question anyone can ask.
pub fn pool_status(cfg: &Config, repo: &str) -> PoolStatus {
    compute_status(cfg, repo, pool_size(cfg, repo))
}

/// Warm-pool status for every repo with a bn-managed bare clone, for a multi-repo rollup.
pub fn all_pool_status(cfg: &Config) -> Vec<PoolStatus> {
    pooled_repos(cfg)
        .into_iter()
        .map(|repo| compute_status(cfg, &repo, pool_size(cfg, &repo)))
        .collect()
}

/// Every repo that pools: each bn-managed bare clone (`<bare_dir>/<repo>.git`), in name order.
/// Membership is the clone — there is no register to be on.
pub fn pooled_repos(cfg: &Config) -> Vec<String> {
    let mut repos: Vec<String> = std::fs::read_dir(bare_root(cfg))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|x| x == "git"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    repos.sort();
    repos
}

/// The bare-clone root, honoring `$WT_ROOT` exactly as [`worktree_root`] does.
fn bare_root(cfg: &Config) -> PathBuf {
    match std::env::var("WT_ROOT") {
        Ok(root) if !root.is_empty() => PathBuf::from(root).join("bare"),
        _ => cfg.bare_dir(),
    }
}

/// Fold a pool's lease store + on-disk slot dirs into its [`PoolStatus`]. The view is the
/// `1..=size` window, matching the CLI's `pool status` — and since `size` is now derived from those
/// very slots, nothing can fall outside it.
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

    // --- derived_size -----------------------------------------------------

    #[test]
    fn a_repo_with_no_pool_yet_starts_at_the_default() {
        assert_eq!(derived_size(0), DEFAULT_POOL_SIZE);
    }

    #[test]
    fn a_small_pool_is_floored_not_honoured() {
        // `life` had a single slot on disk. Deriving 1 would leave it no worker lane at all.
        for high_water in 1..=DEFAULT_POOL_SIZE {
            assert_eq!(derived_size(high_water), DEFAULT_POOL_SIZE);
        }
    }

    #[test]
    fn a_pool_never_shrinks_under_the_slots_it_already_has() {
        // The regression a flat constant would have caused: this machine had 9 of `bn`'s 10 slots
        // leased, and clamping to 4 would have stranded six of them.
        assert_eq!(derived_size(10), 10);
        assert_eq!(derived_size(6), 6);
    }

    #[test]
    fn growth_stops_at_the_ceiling() {
        assert_eq!(derived_size(MAX_POOL_SIZE), MAX_POOL_SIZE);
        assert_eq!(derived_size(MAX_POOL_SIZE + 50), MAX_POOL_SIZE);
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
