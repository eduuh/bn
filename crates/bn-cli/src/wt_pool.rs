//! `bn wt pool` — a treehouse-style pool of warm, reusable worktrees for monorepos.
//!
//! The problem this solves: spinning up a fresh worktree for every agent task pays a cold
//! `build.sh` / `npm install` each time. A *pool* keeps a small set of checkouts around with their
//! deps and build cache intact; `pool get` leases an idle one and switches your branch into it,
//! `pool return` resets it (keeping ignored deps, minus Rust incremental dirs) so `get` is instant.
//!
//! Model (decided): **named-branch reuse**, not treehouse's anonymous detached pool — slots still
//! hold a real branch, so `bn`'s branch-keyed notes/metrics keep working untouched. Every repo with
//! a bn-managed bare clone pools, with no config and no registration: the size is derived from the
//! slots that exist ([`bn_core::pool::derived_size`]) and grows by one whenever a lease finds the
//! pool full, up to [`bn_core::pool::MAX_POOL_SIZE`].
//!
//! Layout: slots live at `<root>/worktree/<repo>/_pool/<n>` beside the normal branch worktrees,
//! and lease state in `<root>/worktree/<repo>/_pool/leases.json`. The `_pool` dir is excluded from
//! every broad `wt` sweep (cleanup/prune/status/go) so the pool is never torn down by housekeeping;
//! a direct `wt cleanup` from inside a slot is treated as `wt pool return`.

use crate::wt;
use bn_core::resolver::Config;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use wt::{P_BLUE as BLUE, P_GREEN as GREEN, P_NC as NC, P_RED as RED, P_YELLOW as YELLOW};

/// Dispatch from `wt::run`. Returns whether the command succeeded (CLI exit semantics).
pub fn run(cfg: &Config, action: wt::PoolCmd) -> bool {
    match action {
        wt::PoolCmd::Get {
            branch,
            repo,
            holder,
            path,
            no_tmux,
        } => pool_get(cfg, branch, repo, holder, path, no_tmux),
        wt::PoolCmd::Return { target, force } => pool_return(cfg, target, force),
        wt::PoolCmd::Status { repo } => pool_status(cfg, repo),
        wt::PoolCmd::Warm {
            repo,
            here,
            new,
            slot,
        } => pool_warm(cfg, repo, here, new, slot),
        wt::PoolCmd::Metrics { path, repo } => pool_metrics(cfg, path, repo),
        wt::PoolCmd::Prune {
            repo,
            yes,
            all,
            verbose,
        } => pool_prune(cfg, repo, yes, all, verbose),
        wt::PoolCmd::Destroy {
            slot,
            repo,
            all,
            yes,
            include_leased,
            include_dirty,
            include_in_use,
        } => pool_destroy(
            cfg,
            slot,
            repo,
            all,
            yes,
            DestroyFlags {
                include_leased,
                include_dirty,
                include_in_use,
            },
        ),
    }
}

/// Append a `pool_get` / `pool_return` event to the shared event log, for `pool metrics`.
/// Best-effort — a logging failure never fails the pool operation.
fn log_pool_event(kind: &str, repo: &str, slot: u32, branch: &str, warm: Option<bool>) {
    let mut f = serde_json::Map::new();
    f.insert("repo".to_string(), repo.into());
    f.insert("slot".to_string(), slot.into());
    f.insert("branch".to_string(), branch.into());
    if let Some(w) = warm {
        f.insert("warm".to_string(), w.into());
    }
    let _ = bn_core::events::append(kind, f);
}

// --- pool size -------------------------------------------------------------

/// Pool size for `repo` — derived from the slots that exist and the leases held, never declared.
/// Delegates to [`bn_core::pool::pool_size`]; always a number, because every repo pools.
pub(crate) fn pool_size(cfg: &Config, repo: &str) -> usize {
    bn_core::pool::pool_size(cfg, repo)
}

// --- lease store (leases.json) ---------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Leases {
    slots: BTreeMap<u32, Lease>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Lease {
    branch: String,
    holder: String,
    /// PID of the process that took the lease — the *caller* (parent of the `bn` invocation, e.g.
    /// the agent or shell), not the ephemeral `bn` process which exits immediately. This is what
    /// makes the lease durable yet recoverable: a slot is only reclaimable once this process is
    /// gone. See [`caller_pid`].
    pid: u32,
    /// Epoch seconds when the lease was taken.
    at: i64,
    /// The boot id of the machine when the lease was taken (see [`current_boot_id`]). A lease whose
    /// id differs from the current boot's was taken *before* a restart (WSL shutdown / reboot), so
    /// its holder is provably gone even if the recycled `pid` now reads as alive — this is what lets
    /// the pool recover its slots after WSL dies and comes back. Empty for leases written before
    /// boot-scoping (treated as an unknown boot → falls back to the pid check), hence `default`.
    #[serde(default)]
    boot_id: String,
    /// A second worker slot reserved by this lease as a detached primary-branch comparison tree.
    /// The relationship is stored only on the owning feature lease, so it cannot become a
    /// half-written pair.
    #[serde(default)]
    comparison_slot: Option<u32>,
}

impl Leases {
    fn reserved_slots(&self) -> BTreeSet<u32> {
        self.slots
            .iter()
            .flat_map(|(&owner, lease)| [Some(owner), lease.comparison_slot])
            .flatten()
            .collect()
    }

    fn owner_slot(&self, n: u32) -> Option<u32> {
        self.slots.contains_key(&n).then_some(n).or_else(|| {
            self.slots
                .iter()
                .find_map(|(&owner, lease)| (lease.comparison_slot == Some(n)).then_some(owner))
        })
    }

    fn comparison_owner(&self, n: u32) -> Option<u32> {
        self.slots
            .iter()
            .find_map(|(&owner, lease)| (lease.comparison_slot == Some(n)).then_some(owner))
    }
}

/// This boot's unique id: a value that changes on every (re)boot, so a lease tagged with a
/// *different* id predates the current boot and its holder process cannot still exist — WSL
/// shutdown / a host reboot kills every process, then recycles their pids to new ones. Linux (and
/// WSL) expose a per-boot random UUID at `/proc/sys/kernel/random/boot_id`; if that's unreadable we
/// fall back to `/proc/stat`'s `btime` (the boot epoch-seconds, equally boot-unique). `None`
/// off-Linux (no `/proc`) — there we can't boot-scope, so liveness stays pid-only. `$BN_BOOT_ID`
/// overrides for deterministic tests (simulating a reboot by changing it).
fn current_boot_id() -> Option<String> {
    if let Ok(id) = std::env::var("BN_BOOT_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    // Fallback: the kernel boot time from /proc/stat (`btime <epoch_secs>`) — also boot-unique.
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .map(|s| format!("btime:{}", s.trim()))
        .filter(|s| s != "btime:")
}

/// A stored `boot_id` as an `Option` for [`bn_core::pool::lease_liveness`]: an empty string (a
/// pre-boot-scoping lease) is an *unknown* boot, not a real id to compare.
fn boot_opt(id: &str) -> Option<&str> {
    (!id.is_empty()).then_some(id)
}

/// The PID to record as the lease holder: the parent of this `bn` process (the long-lived
/// agent/shell), so liveness checks mean "is the holder still around?" rather than "is `bn` still
/// running?" (which is always false by the time anyone looks). Falls back to our own PID off-unix.
#[allow(
    unsafe_code,
    reason = "libc::getppid is the portable Unix API for identifying the lease holder"
)]
fn caller_pid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getppid is always safe and never fails.
        let ppid = unsafe { libc::getppid() };
        if ppid > 0 {
            return ppid as u32;
        }
    }
    std::process::id()
}

fn leases_path(pool_dir: &Path) -> PathBuf {
    pool_dir.join("leases.json")
}

/// Read the lease file, treating a missing or corrupt file as an empty pool (defensive, like
/// `resolver::ConfigFile::load`).
fn read_leases(pool_dir: &Path) -> Leases {
    std::fs::read_to_string(leases_path(pool_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Hold the pool's lease file for a read-modify-write.
///
/// `leases.json` is read, edited and written back by `acquire`, `return` and the release paths, and
/// two dispatches racing that sequence can hand the *same* slot to both — each reads a lease set
/// without the other's entry, picks the same lowest-free slot, and the second write erases the
/// first. Every mutation now takes this lock first, so slot allocation is serialized per repo.
/// [`bn_core::store::FileLock::acquire`] warns and proceeds unowned rather than blocking forever,
/// which keeps a stuck lock from wedging the pool.
fn lock_leases(pool_dir: &Path) -> bn_core::store::FileLock {
    let _ = std::fs::create_dir_all(pool_dir);
    bn_core::store::FileLock::acquire(&leases_path(pool_dir))
}

/// Write the lease file atomically (temp + rename) so a crash mid-write can't corrupt it.
fn write_leases(pool_dir: &Path, leases: &Leases) -> std::io::Result<()> {
    let path = leases_path(pool_dir);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(leases).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)
}

// --- paths -----------------------------------------------------------------

fn pool_dir(worktree_dir: &Path, repo: &str) -> PathBuf {
    worktree_dir.join(repo).join("_pool")
}

fn slot_path(pool_dir: &Path, n: u32) -> PathBuf {
    pool_dir.join(n.to_string())
}

/// `(repo, slot_n, pool_dir)` for a slot path that ends in `.../_pool/<n>`, else `None`.
fn slot_parts(slot: &Path) -> Option<(String, u32, PathBuf)> {
    let n: u32 = slot.file_name()?.to_string_lossy().parse().ok()?;
    let pdir = slot.parent()?;
    if pdir.file_name()? != "_pool" {
        return None;
    }
    let repo = pdir.parent()?.file_name()?.to_string_lossy().to_string();
    Some((repo, n, pdir.to_path_buf()))
}

/// Walk up from `start` to the enclosing `_pool/<n>` slot dir, if any.
fn find_slot_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|anc| {
            anc.parent()
                .and_then(|p| p.file_name())
                .map(|n| n == "_pool")
                .unwrap_or(false)
        })
        .map(|p| p.to_path_buf())
}

/// The enclosing `_pool/<n>` slot for a path, if the path is inside one.
pub(crate) fn pool_slot_root(start: &Path) -> Option<PathBuf> {
    find_slot_root(start)
}

// --- git helpers -----------------------------------------------------------

/// Run git with stdout suppressed (stderr inherited for progress). The pool's stdout contract is
/// "only the slot path / status text" — git's "HEAD is now at …" chatter must not leak into a
/// `pool get --path` capture. Returns whether it succeeded. Thin delegate to the shared
/// [`crate::git::git_run_quiet`] so its stderr progress still reaches the terminal.
fn git_quiet(args: &[&str]) -> bool {
    crate::git::git_run_quiet(args)
}

/// Like `git_quiet` but also suppresses stderr — for calls whose failure is expected and handled
/// (e.g. `worktree remove` on a broken/orphan registration, before the prune+rmdir fallback).
/// Delegates to the runner-backed [`crate::git::git_ok`]: capturing and discarding both streams is
/// observably identical to routing them to `/dev/null`.
fn git_silent(args: &[&str]) -> bool {
    crate::git::git_ok(args)
}

/// A valid base ref to create/reset slots against. Delegates to [`bn_core::gitbase::base_ref`],
/// the one resolver, scoped to the bare repo; `HEAD` when the repo offers no answer (a slot still
/// has to be cut from something).
fn default_base(bare_s: &str) -> String {
    bn_core::gitbase::base_ref(
        &bn_core::runner::RealRunner,
        bn_core::gitbase::BaseScope::BareRepo(std::path::Path::new(bare_s)),
    )
    .unwrap_or_else(|| "HEAD".to_string())
}

/// Create one warm slot as a detached worktree off the default base.
pub(crate) fn add_detached_slot(bare_s: &str, slot_s: &str) -> bool {
    // A fresh checkout has no deps, whatever the previous tenant of this slot number had built.
    if let Some((_, n, pdir)) = slot_parts(Path::new(slot_s)) {
        clear_warm_stamp(&pdir, n);
    }
    let base = default_base(bare_s);
    git_quiet(&[
        "--git-dir",
        bare_s,
        "worktree",
        "add",
        "--detach",
        slot_s,
        &base,
    ])
}

/// Reset a slot back to a clean warm-idle state: discard tracked changes and detach to the base,
/// then drop untracked files — but NEVER `-x`/`-X`, so ignored deps/build cache survive. The one
/// ignored subtree we do trim is `target/**/incremental`: warm builds already run with
/// `CARGO_INCREMENTAL=0`, so those dirs are dead weight once the slot is idle again.
fn warm_reset(slot_s: &str, bare_s: &str) {
    warm_reset_with(slot_s, bare_s, None)
}

/// [`warm_reset`] for a caller that has just probed the slot and knows its untracked paths —
/// `return` runs `git status` to refuse a dirty slot, and running it a second time here was the
/// bigger half of a 12 s return on a monorepo checkout. `None` probes.
fn warm_reset_with(slot_s: &str, bare_s: &str, known_untracked: Option<Vec<String>>) {
    // A lease can outlive its checkout (manual cleanup, a botched prune). There is nothing to reset
    // then, and running git against a missing directory only prints `fatal: cannot change to …` over
    // the caller's output — the lease still gets dropped by whoever called us.
    if !Path::new(slot_s).is_dir() {
        return;
    }
    // Never interleave with another bn process resetting or leasing this slot: the switch below
    // writes tracked files that a concurrent `status` + `clean -df` (reading the index from before
    // the switch) would delete as untracked. See `slot_lock`.
    let _lock =
        slot_parts(Path::new(slot_s)).and_then(|(_, n, pdir)| slot_lock::lock_slot(&pdir, n));
    warm_reset_unlocked(slot_s, bare_s, known_untracked)
}

/// [`warm_reset_with`] for a caller that already holds the slot's lock — a second `flock` from the
/// same process on a fresh descriptor would wait on itself.
fn warm_reset_unlocked(slot_s: &str, bare_s: &str, known_untracked: Option<Vec<String>>) {
    let base = default_base(bare_s);
    // Detach first — no tree change, so nothing can refuse it — then `reset --hard` to the base.
    // This used to be one `switch --detach --discard-changes <base>`, which git refuses whenever
    // an *untracked* file stands where the new base puts a tracked one (a beachball change file a
    // tenant left behind that upstream has since merged), leaving the slot on its old commit with
    // every tracked edit intact. `reset --hard` overwrites such files; on a detached HEAD it moves
    // no branch ref.
    let _ = git_quiet(&["-C", slot_s, "switch", "--quiet", "--detach"]);
    let _ = git_quiet(&["-C", slot_s, "reset", "--quiet", "--hard", &base]);
    // `clean -df` walks every directory of the tree (5-6 s on a monorepo) even when there is
    // nothing to remove. Ask status for the untracked paths instead — it uses the untracked cache
    // when the repo has one — and clean only those.
    let untracked = known_untracked.unwrap_or_else(|| wt::dirty_files(slot_s));
    if !untracked.is_empty() {
        let mut args: Vec<&str> = vec!["-C", slot_s, "clean", "-df", "--"];
        args.extend(untracked.iter().map(String::as_str));
        let _ = git_quiet(&args);
    }
    idle::settle(Path::new(slot_s));
}

/// Whether `slot` is a usable warm checkout — a *real* git worktree, not just a directory left
/// behind by an interrupted `worktree add` (killed mid-checkout leaves the files but no `.git`
/// link). Such a stub passes `is_dir()` yet every git command in it fails, so it must be recreated,
/// not reused. We require its own `.git` entry AND that git accepts it as a work tree.
pub(crate) fn is_live_slot(slot: &Path) -> bool {
    slot.join(".git").exists()
        && git_silent(&[
            "-C",
            &slot.to_string_lossy(),
            "rev-parse",
            "--is-inside-work-tree",
        ])
}

/// Forcibly tear down a slot's worktree registration and on-disk dir, tolerating a broken/orphan
/// state (interrupted `worktree add`, dangling ref): try `worktree remove --force`, else prune the
/// dangling ref and remove the dir. Shared by slot removal and by the recreate path in get/warm.
pub(crate) fn force_remove_worktree(bare_s: &str, slot: &Path) {
    let slot_s = slot.to_string_lossy().to_string();
    if !git_silent(&[
        "--git-dir",
        bare_s,
        "worktree",
        "remove",
        "--force",
        &slot_s,
    ]) {
        let _ = git_silent(&["--git-dir", bare_s, "worktree", "prune"]);
        let _ = std::fs::remove_dir_all(slot);
    }
}

// --- in-use detection ------------------------------------------------------

/// Whether a process with `pid` is alive — `/proc/<pid>` on Linux, `kill(pid, 0)` elsewhere.
///
/// This used to answer a flat `true` off Linux, which quietly disabled every "has the holder gone?"
/// decision on macOS: a lease could never read as abandoned, so `pool status` never flagged one,
/// the dead-holder sweep never freed one, and slots leaked until someone returned them by hand.
/// `kill(pid, 0)` is the portable existence check the rest of bn already uses.
fn pid_alive(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(crate::cmd::killport::process_exists)
}

pub(crate) mod occupancy;
use occupancy::any_proc_in;

// --- agent occupancy --------------------------------------------------------
//
// "In use" is a fact about *agents*, and it lives in the agent-lease registry and tmux — not in
// leases.json. The pool used to decide from its own ledger plus [`any_proc_in`], which was a
// `/proc` scan compiled out off Linux, so on macOS it had no process signal at all: a slot the
// ledger had lost track of (an in-place `bn agent launch`, or a kill that resolved the wrong
// worktree and handed the slot back) read as idle, was leased to the next branch, and
// `switch_into_slot` changed the branch under a running agent. Every decision below now asks the
// same oracle the one-agent-per-worktree launch guard asks, and [`occupancy::any_proc_in`] now
// answers on every platform and fails closed rather than guessing idle.

/// Live agents working in `slot`, oldest first. Empty for an idle slot.
fn slot_occupants(slot: &Path) -> Vec<crate::agent_lease::Occupant> {
    #[cfg(test)]
    if let Some(stubbed) = test_occupants(slot) {
        return stubbed;
    }
    crate::agent_lease::live_agents_in(slot)
}

/// The tenant label the ledger records for an occupant: its branch, except in the controller's
/// reserved slot, where the lease is keyed by the fixed [`CONTROLLER_LEASE_LABEL`] rather than by
/// whatever branch `bn control`'s checkout happens to sit on.
fn occupant_label(occupant: &crate::agent_lease::Occupant, n: u32) -> String {
    if n == bn_core::agent::MAIN_POOL_SLOT {
        return CONTROLLER_LEASE_LABEL.to_string();
    }
    occupant.label()
}

/// Make the ledger tell the truth about slots a live agent is working in, and report which slots
/// changed. The occupancy counterpart of [`reclaim_stale`]: that one frees slots whose holder is
/// provably gone, this one claims slots whose holder the ledger never knew about (or has since
/// mis-recorded), so neither the pick nor an idempotent warm hit can hand one out.
///
/// Deliberately *adopting* rather than refusing: a slot with a live agent is held, and saying so —
/// with the tenant's name — is what makes `wt pool status` legible and the next lease correct.
fn adopt_occupied_slots(ctx: &PoolCtx, leases: &mut Leases) -> Vec<u32> {
    let mut adopted = Vec::new();
    for n in 1..=ctx.size as u32 {
        // A comparison checkout is reserved by its feature lease but deliberately has no agent of
        // its own. Never turn activity inside it into a second, independent tenancy.
        if leases.comparison_owner(n).is_some() {
            continue;
        }
        let slot = slot_path(&ctx.pool_dir, n);
        if !slot.is_dir() {
            continue;
        }
        let occupants = slot_occupants(&slot);
        let labels: Vec<String> = occupants
            .iter()
            .map(|occupant| occupant_label(occupant, n))
            .collect();
        let recorded = leases
            .slots
            .get(&n)
            .map(|l| bn_core::agent::normalized_branch(&l.branch));
        let bn_core::pool::LedgerRepair::Adopt(tenant) =
            bn_core::pool::ledger_repair(recorded.as_deref(), &labels)
        else {
            continue;
        };
        let Some(occupant) = occupants
            .iter()
            .find(|occupant| occupant_label(occupant, n) == tenant)
        else {
            continue;
        };
        leases.slots.insert(
            n,
            Lease {
                branch: tenant,
                holder: format!("agent:{}", occupant.tmux_target),
                // The occupant's own pane, so the lease is exactly as live as the agent is.
                pid: crate::agent_lease::pane_pid(&occupant.tmux_target).unwrap_or_else(caller_pid),
                at: occupant.since.max(0),
                boot_id: current_boot_id().unwrap_or_default(),
                comparison_slot: None,
            },
        );
        adopted.push(n);
    }
    if !adopted.is_empty() {
        let _ = write_leases(&ctx.pool_dir, leases);
    }
    adopted
}

/// Test-only occupancy: `Some(..)` replaces the tmux/registry oracle wholesale, so slot occupancy
/// is exercisable without a tmux server.
#[cfg(test)]
fn test_occupants(slot: &Path) -> Option<Vec<crate::agent_lease::Occupant>> {
    TEST_OCCUPANTS.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|by_slot| by_slot.get(slot).cloned().unwrap_or_default())
    })
}

#[cfg(test)]
thread_local! {
    static TEST_OCCUPANTS: std::cell::RefCell<Option<BTreeMap<PathBuf, Vec<crate::agent_lease::Occupant>>>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) mod health;
pub(crate) use health::*;

mod idle;
pub(crate) mod slot_lock;
mod switch;
#[cfg(test)]
use switch::switch_into_slot;
use switch::switch_into_slot_from;

pub(crate) mod release;
pub(crate) use release::*;

pub(crate) mod warm;
pub(crate) use warm::*;

pub(crate) mod reclaim;
use reclaim::{
    leaked_lease_candidates, live_branch_slot, reclaim_leaked_leases, reclaim_stale, ReclaimReport,
    ReclaimScope, LEAKED_LEASE_GRACE_SECS,
};

// --- removal classification (pure, unit-tested) ----------------------------

/// What a slot is, for the removal decision. Filled by `classify_slot` (which does the I/O), then
/// handed to the pure deciders below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlotClass {
    /// Held in leases.json.
    leased: bool,
    /// Has uncommitted changes (ignored deps don't count — `wt::dirty_count`).
    dirty: bool,
    /// A live process is working in it.
    in_use: bool,
    /// Slot number exceeds the configured pool size (e.g. after the size was lowered).
    over_size: bool,
    /// On disk but git doesn't know it, or the bare repo is gone.
    orphan: bool,
}

#[derive(Clone, Copy, Default)]
struct DestroyFlags {
    include_leased: bool,
    include_dirty: bool,
    include_in_use: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Remove,
    Skip(&'static str),
}

/// `prune` removes ONLY genuinely-stale slots — over-size or orphaned — and never anything
/// leased/dirty/in-use. An in-size idle warm slot *is* the pool, so it's kept. Pure → testable.
fn prune_decision(c: SlotClass) -> Action {
    if c.leased {
        return Action::Skip("leased");
    }
    if c.in_use {
        return Action::Skip("in use");
    }
    if c.dirty {
        return Action::Skip("uncommitted changes");
    }
    if c.over_size || c.orphan {
        return Action::Remove;
    }
    Action::Skip("in-size warm slot")
}

/// `destroy` removes a *targeted* slot (the caller already scoped to a slot or `--all`), skipping
/// protected classes unless the matching `--include-*` flag opts in. Pure → testable.
fn destroy_decision(c: SlotClass, f: DestroyFlags) -> Action {
    if c.leased && !f.include_leased {
        return Action::Skip("leased (use --include-leased)");
    }
    if c.in_use && !f.include_in_use {
        return Action::Skip("in use (use --include-in-use)");
    }
    if c.dirty && !f.include_dirty {
        return Action::Skip("uncommitted changes (use --include-dirty)");
    }
    Action::Remove
}

// --- resolution ------------------------------------------------------------

struct PoolCtx {
    bare: PathBuf,
    repo: String,
    pool_dir: PathBuf,
    worktree_dir: PathBuf,
    size: usize,
}

/// Resolve repo → bare path + pool dir + size, or print why not and return `None`.
fn resolve_pool(cfg: &Config, repo_arg: Option<String>) -> Option<PoolCtx> {
    let (bare_dir, worktree_dir) = wt::layout(cfg);
    let bare = match repo_arg {
        Some(r) => {
            let b = bare_dir.join(format!("{r}.git"));
            if !b.is_dir() {
                ceprintln!("{RED}No bare repo '{r}' under {}{NC}", bare_dir.display());
                return None;
            }
            b
        }
        None => wt::resolve_bare_or_pick(&bare_dir)?,
    };
    let repo = wt::repo_name(&bare);
    let pool_dir = pool_dir(&worktree_dir, &repo);
    let size = pool_size(cfg, &repo);
    Some(PoolCtx {
        bare,
        repo,
        pool_dir,
        worktree_dir,
        size,
    })
}

/// Resolve the repo (canonical name + bare path) a `pool metrics` targets. Precedence: an explicit
/// filesystem `path` (a pool slot, any worktree checkout, or a `<repo>.git` bare dir) — so you can
/// point metrics at any repo path and get its pool health — then the `--repo` name, then the current
/// repo (or an fzf pick). Unlike [`resolve_pool`], this does NOT require the repo to be pooled: the
/// caller reports a clean "not pooled" so `metrics` answers "health of pools *if they exist*". Prints
/// and returns `None` only when nothing resolves to a real repo.
fn resolve_metrics_repo(
    cfg: &Config,
    path: Option<&str>,
    repo: Option<String>,
) -> Option<(String, PathBuf)> {
    let (bare_dir, _) = wt::layout(cfg);
    if let Some(p) = path {
        let bare = bare_from_path(p)?;
        return Some((wt::repo_name(&bare), bare));
    }
    if let Some(r) = repo {
        let bare = bare_dir.join(format!("{r}.git"));
        if !bare.is_dir() {
            ceprintln!("{RED}No bare repo '{r}' under {}{NC}", bare_dir.display());
            return None;
        }
        return Some((wt::repo_name(&bare), bare));
    }
    let bare = wt::resolve_bare_or_pick(&bare_dir)?;
    Some((wt::repo_name(&bare), bare))
}

/// Resolve a filesystem path to the bare repo backing it: a `<repo>.git` bare dir is itself the
/// answer; a pool slot or any worktree checkout maps through its `.git` pointer up to the bare (via
/// [`wt::find_bare_repo`]). Prints and returns `None` when the path is neither.
fn bare_from_path(path: &str) -> Option<PathBuf> {
    let raw = Path::new(path);
    let abs = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        wt::cwd().join(raw)
    };
    // A bare repo dir passed directly (`.../<repo>.git` holding a HEAD).
    if abs.extension().map(|e| e == "git").unwrap_or(false) && abs.join("HEAD").is_file() {
        return Some(abs);
    }
    // Inside a worktree or a `_pool/<n>` slot → walk the `.git` pointer up to the bare.
    if let Some(bare) = wt::find_bare_repo(&abs) {
        return Some(bare);
    }
    ceprintln!(
        "{RED}'{}' isn't a bare repo or inside a worktree — can't find its pool.{NC}",
        abs.display()
    );
    None
}

fn default_holder() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn now_epoch() -> i64 {
    // Delegate to the canonical clock so `$BN_NOW` (the deterministic-test override) is honored — a
    // raw SystemTime read silently ignored it.
    bn_core::util::now_epoch_secs()
}

// --- commands --------------------------------------------------------------

fn pool_get(
    cfg: &Config,
    branch: Option<String>,
    repo: Option<String>,
    holder: Option<String>,
    path_only: bool,
    no_tmux: bool,
) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let Some(branch) = branch else {
        ceprintln!("{RED}Usage: wt pool get <branch>{NC}");
        return false;
    };
    let Some(leased) = acquire_slot(&ctx, &branch, None, holder, SlotRole::Worker, false) else {
        return false;
    };
    finish_get(&ctx, &leased.slot, &branch, path_only, no_tmux)
}

/// The result of leasing a slot: its path, plus whether this was a *fresh* lease of `branch` (a
/// newly taken slot) versus an idempotent hand-back of the slot already leased to it. `bn dispatch`
/// maps `fresh` onto its "created" flag (seed the note / run setup only on a fresh lease).
struct Leased {
    slot: PathBuf,
    comparison: Option<PathBuf>,
    fresh: bool,
}

/// Which half of the pool a lease may draw from. Slot [`MAIN_POOL_SLOT`] is `bn control`'s
/// alone; everything else leases from the worker range, so the two can never collide.
///
/// [`MAIN_POOL_SLOT`]: bn_core::agent::MAIN_POOL_SLOT
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotRole {
    Worker,
    Controller,
}

impl SlotRole {
    fn owns(self, n: u32) -> bool {
        let controller = n == bn_core::agent::MAIN_POOL_SLOT;
        match self {
            SlotRole::Worker => !controller,
            SlotRole::Controller => controller,
        }
    }
}

/// Lease a warm slot for `branch` in an already-resolved pool. Reuses the slot already leased to
/// this branch (idempotent warm hit), else picks — and if the pool is full, reclaims an abandoned
/// — idle slot, creates it cold if it isn't warm yet, switches `branch` in, and persists the lease.
/// Returns `None` (message already printed) when the pool is full or a git op failed. Shared by
/// `wt pool get` and `bn dispatch --pool` so both lease identically. `class` decides which slots are
/// even candidates: a worker never sees the controller's slot, and vice versa.
fn acquire_slot(
    ctx: &PoolCtx,
    branch: &str,
    base: Option<&str>,
    holder: Option<String>,
    class: SlotRole,
    with_main: bool,
) -> Option<Leased> {
    // Held for the whole pick-and-persist sequence: two concurrent dispatches that each read the
    // lease set before either wrote would pick the same lowest-free slot and the second write would
    // erase the first, putting two agents in one worktree.
    let _lock = lock_leases(&ctx.pool_dir);
    let mut leases = read_leases(&ctx.pool_dir);

    // Self-heal first: reclaim any lease left by a *prior boot* (WSL shutdown / host reboot). Its
    // holder is provably gone, so the durable-until-return rule no longer applies — freeing these
    // pre-emptively is what lets the pool come back to life after a restart, and it may reveal the
    // very slot this call needs. Same-boot dead holders are left for the last-resort sweep below.
    reclaim_stale(ctx, &mut leases, ReclaimScope::PriorBootOnly);

    // Then the opposite self-heal: claim every slot a live agent is working in, so a checkout in
    // use can be neither picked below nor handed back as somebody else's warm hit.
    adopt_occupied_slots(ctx, &mut leases);

    // Already leased to this branch, in a slot of the right class, *and still tenanted*? Hand back
    // the same slot (idempotent, a warm hit). A lease whose tenancy is provably over is reclaimed
    // rather than honoured — see [`live_branch_slot`] for why answering "already running" from the
    // ledger alone wedges a branch permanently.
    let want = bn_core::agent::normalized_branch(branch);
    if let Some(n) = live_branch_slot(ctx, &mut leases, &want, class, now_epoch()) {
        let slot = slot_path(&ctx.pool_dir, n);
        // A lease can outlive its checkout: the slot dir may have been removed out-of-band (manual
        // cleanup, a botched prune) while the holder pid is still alive — so `reclaim_stale` leaves
        // the lease in place. Handing that back as a "warm hit" drops the agent into a missing or
        // broken directory. Repair the checkout first (the same recreate the pick path does), so an
        // idempotent hit is always a *live* slot. Still `fresh: false` — the branch's tenancy (and
        // its note) predates this call; only the checkout was rebuilt.
        let warm = is_live_slot(&slot);
        if !warm {
            let slot_s = slot.to_string_lossy().to_string();
            let bare_s = ctx.bare.to_string_lossy().to_string();
            if slot.exists() {
                ceprintln!("{YELLOW}Slot {n} checkout is broken — recreating…{NC}");
                force_remove_worktree(&bare_s, &slot);
            } else {
                ceprintln!("{YELLOW}Slot {n} checkout is missing — recreating…{NC}");
            }
            if !add_detached_slot(&bare_s, &slot_s) {
                ceprintln!("{RED}Failed to rebuild slot {n} for {}{NC}", ctx.repo);
                return None;
            }
            match class {
                SlotRole::Worker => {
                    if !switch_into_slot_from(&slot_s, &bare_s, branch, base) {
                        ceprintln!(
                            "{RED}Failed to switch '{branch}' into rebuilt slot {n} (branch checked out elsewhere?){NC}"
                        );
                        return None;
                    }
                }
                SlotRole::Controller => warm_reset(&slot_s, &bare_s),
            }
        }
        let comparison = if with_main {
            match ensure_comparison_slot(ctx, &mut leases, n, branch, true) {
                Some(slot) => Some(slot),
                None => return None,
            }
        } else {
            leases
                .slots
                .get(&n)
                .and_then(|lease| lease.comparison_slot)
                .map(|comparison| slot_path(&ctx.pool_dir, comparison))
        };
        if with_main && write_leases(&ctx.pool_dir, &leases).is_err() {
            ceprintln!("{RED}Failed to persist the paired pool lease.{NC}");
            return None;
        }
        log_pool_event("pool_get", &ctx.repo, n, branch, Some(warm));
        return Some(Leased {
            slot,
            comparison,
            fresh: false,
        });
    }

    // Pick the lowest idle slot of this class (pure policy in bn-core). Live-boot leases are durable
    // (held until `return`), so we DON'T reclaim them pre-emptively — only as a last resort when
    // every slot is leased, and only for slots whose holder is genuinely gone (dead pid, nothing
    // running in them).
    let leased_set = |l: &Leases| l.reserved_slots();
    // Built slots first: a checkout whose deps a warm actually installed is the only kind that
    // makes the lease near-instant. Slot order alone kept choosing the lowest idle slot, which in
    // a monorepo pool is exactly the one warm keeps skipping.
    // …and never a slot the last warm found dirty: leasing it would carry somebody's uncommitted
    // work onto the new branch. Both are files beside leases.json, so this costs no git call.
    let dirty = read_dirty_marks(&ctx.pool_dir);
    let built: BTreeSet<u32> = read_warm_stamps(&ctx.pool_dir)
        .difference(&dirty)
        .copied()
        .collect();
    let pick = |l: &Leases| match class {
        SlotRole::Worker => {
            // A dirty slot is off the table while anything else is free — even an unbuilt
            // checkout beats carrying a stranger's uncommitted work onto a new branch.
            let avoid: BTreeSet<u32> = leased_set(l).union(&dirty).copied().collect();
            bn_core::pool::pick_built_slot(ctx.size, &avoid, &built)
                .or_else(|| bn_core::pool::pick_built_slot(ctx.size, &leased_set(l), &built))
        }
        SlotRole::Controller => bn_core::pool::pick_controller_slot(&leased_set(l)),
    };
    let enough_worker_slots = |l: &Leases, needed: usize| {
        let mut reserved = l.reserved_slots();
        (0..needed).all(|_| {
            let Some(n) = bn_core::pool::pick_slot(ctx.size, &reserved) else {
                return false;
            };
            reserved.insert(n);
            true
        })
    };
    if with_main
        && class == SlotRole::Worker
        && !enough_worker_slots(&leases, 2)
        && reclaim_stale(ctx, &mut leases, ReclaimScope::AnyDeadHolder).changed()
    {
        // Re-evaluated below; this pass may have recovered both slots.
    }
    if with_main
        && class == SlotRole::Worker
        && !enough_worker_slots(&leases, 2)
        && !reclaim_leaked_leases(ctx, &mut leases, now_epoch(), false).is_empty()
    {
        // Re-evaluated below; this pass may have recovered both slots.
    }
    if with_main && class == SlotRole::Worker && !enough_worker_slots(&leases, 2) {
        ceprintln!(
            "{RED}Pool needs two free worker slots for --with-main.{NC} Return a slot or increase \
             this repo's pool size."
        );
        return None;
    }
    let mut n = pick(&leases);
    if n.is_none() && reclaim_stale(ctx, &mut leases, ReclaimScope::AnyDeadHolder).changed() {
        n = pick(&leases);
    }
    // Still nothing. The sweep above only frees a slot whose *holder pid* has died, and a dispatch's
    // recorded holder is the caller's parent — a tmux server (every popup command's parent) or a
    // login shell, neither of which ever exits. So a slot whose agent is long gone stays leased
    // forever, the pool reads full, and every later dispatch spills into a cold `worktree/<repo>/…`
    // checkout instead of the warm slot that is sitting right there, idle. Before declaring the pool
    // full, run the same leaked-lease sweep `wt pool prune` does: it asks who is actually in the
    // checkout rather than trusting the pid, and only frees a slot nobody occupies, with nothing
    // uncommitted in it, whose lease is past the startup grace.
    if n.is_none() && !reclaim_leaked_leases(ctx, &mut leases, now_epoch(), false).is_empty() {
        n = pick(&leases);
    }
    // Full pool? Grow by one rather than refuse. Creating slot `size + 1` *is* the growth: the
    // size is derived from the slots that exist, so the next `derived_size` reads the new dir back
    // and the bigger pool persists with nothing written down anywhere. Bounded by MAX_POOL_SIZE,
    // and workers only — the controller's slot is fixed, so "already leased" is its real answer.
    let n = n.or_else(|| {
        (class == SlotRole::Worker && ctx.size < bn_core::pool::MAX_POOL_SIZE)
            .then(|| ctx.size as u32 + 1)
    });
    let Some(n) = n else {
        match class {
            SlotRole::Controller => ceprintln!(
                "{RED}Controller slot {} is already leased.{NC} Stop the running controller with \
                 `bn control stop <id>`, or hand the slot back with `wt pool return`.",
                bn_core::agent::MAIN_POOL_SLOT
            ),
            SlotRole::Worker => ceprintln!(
                "{RED}Pool full at the {} slot ceiling: {}/{} worker slots leased.{NC} \
                 Return one with `wt pool return`.",
                bn_core::pool::MAX_POOL_SIZE,
                leases
                    .reserved_slots()
                    .into_iter()
                    .filter(|&n| class.owns(n))
                    .count(),
                ctx.size.saturating_sub(1)
            ),
        }
        return None;
    };

    let slot = slot_path(&ctx.pool_dir, n);
    let slot_s = slot.to_string_lossy().to_string();
    let bare_s = ctx.bare.to_string_lossy().to_string();

    // Warm hit = the slot already holds a *live* checkout (we skip the cold checkout); cold =
    // created. A bare dir left by an interrupted `worktree add` is NOT warm — tear it down and
    // recreate, else the `git switch` below fails with "not a git repository".
    let warm = is_live_slot(&slot);
    if !warm {
        if slot.exists() {
            ceprintln!("{YELLOW}Slot {n} checkout is broken — recreating…{NC}");
            force_remove_worktree(&bare_s, &slot);
        }
        ceprintln!("{BLUE}Creating warm slot {n} for {}…{NC}", ctx.repo);
        if !add_detached_slot(&bare_s, &slot_s) {
            ceprintln!("{RED}Failed to create slot {n}{NC}");
            return None;
        }
    }
    match class {
        SlotRole::Worker => {
            // Hold the slot across the switch so a concurrent warm cannot reset it under us.
            let _slot_lock = slot_lock::lock_slot(&ctx.pool_dir, n);
            if !switch_into_slot_from(&slot_s, &bare_s, branch, base) {
                // An idle slot nobody occupies has no work anyone is keeping, so a refused
                // switch is leftover state — a tenant's uncommitted edits, a reset cut short —
                // and the caller can do nothing about it. Reset and try once more instead of
                // failing the lease over a slot's dirt; the retry still fails honestly when the
                // branch is checked out elsewhere.
                let recoverable = slot_occupants(&slot).is_empty();
                if recoverable {
                    ceprintln!(
                        "{YELLOW}Slot {n} refused '{branch}' — resetting it and retrying…{NC}"
                    );
                    warm_reset_unlocked(&slot_s, &bare_s, None);
                }
                if !recoverable || !switch_into_slot_from(&slot_s, &bare_s, branch, base) {
                    ceprintln!(
                        "{RED}Failed to switch '{branch}' into slot {n} (branch checked out elsewhere?){NC}"
                    );
                    return None;
                }
            }
        }
        // The controller coordinates and ships nothing, so its slot stays **detached** at the base
        // rather than checking a branch out: `main` is already checked out in the main worktree, and
        // git refuses the same branch twice. A reset also clears whatever a prior tenant left.
        SlotRole::Controller => warm_reset(&slot_s, &bare_s),
    }

    leases.slots.insert(
        n,
        Lease {
            branch: branch.to_string(),
            holder: holder.unwrap_or_else(default_holder),
            pid: caller_pid(),
            at: now_epoch(),
            boot_id: current_boot_id().unwrap_or_default(),
            comparison_slot: None,
        },
    );
    let comparison = if with_main {
        match ensure_comparison_slot(ctx, &mut leases, n, branch, false) {
            Some(slot) => Some(slot),
            None => {
                warm_reset(&slot_s, &bare_s);
                leases.slots.remove(&n);
                return None;
            }
        }
    } else {
        None
    };
    if let Err(error) = write_leases(&ctx.pool_dir, &leases) {
        warm_reset(&slot_s, &bare_s);
        if let Some(comparison) = &comparison {
            warm_reset(&comparison.to_string_lossy(), &bare_s);
        }
        leases.slots.remove(&n);
        let _ = write_leases(&ctx.pool_dir, &leases);
        ceprintln!("{RED}Failed to persist lease for slot {n}: {error}{NC}");
        return None;
    }
    log_pool_event("pool_get", &ctx.repo, n, branch, Some(warm));
    Some(Leased {
        slot,
        comparison,
        fresh: true,
    })
}

/// Add or refresh the comparison half of a feature lease while the caller holds the pool lock.
fn ensure_comparison_slot(
    ctx: &PoolCtx,
    leases: &mut Leases,
    owner: u32,
    branch: &str,
    allow_reclaim: bool,
) -> Option<PathBuf> {
    let bare_s = ctx.bare.to_string_lossy().to_string();
    if let Some(n) = leases
        .slots
        .get(&owner)
        .and_then(|lease| lease.comparison_slot)
    {
        let slot = slot_path(&ctx.pool_dir, n);
        // This pair already belongs to a live tenancy. Looking it up must be non-destructive: the
        // caller may still refuse the duplicate dispatch, and resetting here would erase work in
        // the comparison checkout before that refusal.
        return Some(slot);
    }

    let mut reserved = leases.reserved_slots();
    let mut comparison = bn_core::pool::pick_slot(ctx.size, &reserved);
    if allow_reclaim
        && comparison.is_none()
        && reclaim_stale(ctx, leases, ReclaimScope::AnyDeadHolder).changed()
    {
        reserved = leases.reserved_slots();
        comparison = bn_core::pool::pick_slot(ctx.size, &reserved);
    }
    if allow_reclaim
        && comparison.is_none()
        && !reclaim_leaked_leases(ctx, leases, now_epoch(), false).is_empty()
    {
        reserved = leases.reserved_slots();
        comparison = bn_core::pool::pick_slot(ctx.size, &reserved);
    }
    let Some(n) = comparison else {
        ceprintln!(
            "{RED}Pool needs two free worker slots for --with-main, but only one is available.{NC} \
             Return a slot or increase this repo's pool size."
        );
        return None;
    };
    let slot = slot_path(&ctx.pool_dir, n);
    if !is_live_slot(&slot) {
        if slot.exists() {
            force_remove_worktree(&bare_s, &slot);
        }
        ceprintln!("{BLUE}Creating comparison slot {n} for {}…{NC}", ctx.repo);
        if !add_detached_slot(&bare_s, &slot.to_string_lossy()) {
            return None;
        }
    }
    warm_reset(&slot.to_string_lossy(), &bare_s);
    leases.slots.get_mut(&owner)?.comparison_slot = Some(n);
    log_pool_event(
        "pool_get",
        &ctx.repo,
        n,
        &format!("main for {branch}"),
        Some(true),
    );
    Some(slot)
}

/// Lease a warm pool slot for a `bn dispatch` of `branch` in `repo` (#457) — the same primitive as
/// `wt pool get`, minus the tmux-window / stdout side effects: dispatch owns the window (it launches
/// the seeded agent there) and needs the slot path *returned*, not printed. Returns `(slot_path,
/// fresh)` where `fresh` is true for a new lease of `branch` (dispatch seeds the note only then).
/// `None` when the repo has no bn-managed bare clone or the pool is full — the caller falls back to
/// a cold worktree. Leasing is the default dispatch path, so this pools *every* repo (see
/// [`resolve_dispatch_pool`]).
pub fn lease_for_dispatch(cfg: &Config, repo: &str, branch: &str) -> Option<(PathBuf, bool)> {
    lease_for_dispatch_mode(cfg, repo, branch, None, false).map(|(slot, _, fresh)| (slot, fresh))
}

/// Outcome of a direct human request to open a branch.
///
/// Unlike dispatch, a direct open has no work item to schedule, so a pool that cannot lease must
/// remain a visible capacity failure rather than silently cutting a cold checkout. There is no
/// longer an `Unpooled` outcome: every repo with a bare clone pools, so the only ways this fails
/// are "no such clone" and "no capacity".
pub(crate) enum DirectLease {
    Leased(PathBuf),
    MissingRepo,
    Unavailable,
}

pub(crate) fn lease_for_direct_open(cfg: &Config, repo: &str, branch: &str) -> DirectLease {
    let (bare_dir, _) = wt::layout(cfg);
    if !bare_dir.join(format!("{repo}.git")).is_dir() {
        return DirectLease::MissingRepo;
    }
    match lease_for_dispatch(cfg, repo, branch) {
        Some((slot, _fresh)) => DirectLease::Leased(slot),
        None => DirectLease::Unavailable,
    }
}

pub fn lease_for_dispatch_mode(
    cfg: &Config,
    repo: &str,
    branch: &str,
    base: Option<&str>,
    with_main: bool,
) -> Option<(PathBuf, Option<PathBuf>, bool)> {
    let ctx = resolve_dispatch_pool(cfg, repo)?;
    if with_main
        && !git_quiet(&[
            "--git-dir",
            &ctx.bare.to_string_lossy(),
            "fetch",
            "--quiet",
            "origin",
        ])
    {
        ceprintln!(
            "{RED}Could not refresh origin before leasing --with-main for {repo}.{NC} The paired \
             comparison must represent the latest remote primary branch."
        );
        return None;
    }
    if with_main {
        let _ = git_quiet(&[
            "--git-dir",
            &ctx.bare.to_string_lossy(),
            "remote",
            "set-head",
            "origin",
            "--auto",
        ]);
    }
    let leased = acquire_slot(&ctx, branch, base, None, SlotRole::Worker, with_main)?;
    // Taking a slot may have taken the *last* one. Refill behind ourselves so the next dispatch
    // finds capacity already waiting instead of discovering the pool is full.
    ensure_spare_slot(&ctx);
    Some((leased.slot, leased.comparison, leased.fresh))
}

/// The branch a pool slot is currently leased to, or `None` for a path that is not a leased slot.
///
/// A slot's tenant isn't in its path the way a per-branch worktree's is (`_pool/3` names a slot,
/// not a branch), so anything that identifies an agent by *where it is running* — the fleet reaper
/// discovering `repo:branch` from a cwd — has to ask the ledger. Falling back to the checkout's own
/// HEAD covers a slot occupied without a lease.
pub(crate) fn slot_branch(slot: &Path) -> Option<String> {
    let (_repo, n, pdir) = slot_parts(slot)?;
    if let Some(lease) = read_leases(&pdir).slots.get(&n) {
        return Some(lease.branch.clone());
    }
    wt::git_out(&[
        "-C",
        &slot.to_string_lossy(),
        "rev-parse",
        "--abbrev-ref",
        "HEAD",
    ])
    .map(|head| head.trim().to_string())
    .filter(|head| !head.is_empty() && head != "HEAD")
}

/// When the current tenancy of `worktree` began (epoch seconds), or `None` when it isn't a leased
/// pool slot.
///
/// A slot is a *stable directory reused by many branches*, and the harnesses key their session
/// history on the directory: `~/.claude/projects/<encoded-cwd>/` and Copilot's `workspace.yaml`
/// both say "sessions that ran in this path". So typing `claude` in a returned, idle slot resumed
/// whatever the previous tenant was doing — the directory remembered, even though the lease had
/// changed hands. Anything resolving a resume target in a slot asks this first and ignores history
/// older than the current lease: a slot's memory starts when its lease does.
///
/// A conventional per-branch worktree returns `None` — there the directory *is* the tenant, so its
/// whole history is legitimately yours.
pub fn slot_lease_started_at(worktree: &Path) -> Option<i64> {
    let (_repo, n, pdir) = slot_parts(worktree)?;
    let leases = read_leases(&pdir);
    let owner = leases.owner_slot(n)?;
    leases.slots.get(&owner).map(|lease| lease.at)
}

/// Refresh the lease on the slot an agent is being *relaunched* into.
///
/// A relaunch respawns into the same worktree, so the slot must stay leased — but the recorded
/// holder pid belongs to the process that just got killed, and [`reclaim_stale`] would read that as
/// an abandoned slot and hand it to someone else mid-relaunch. Re-stamping `pid`/`at`/`boot_id`
/// keeps the lease provably live. No-op for a cold worktree or an unleased slot.
pub fn touch_dispatch_lease(cfg: &Config, worktree: &Path) -> bool {
    let _ = cfg;
    let Some((_repo, n, pdir)) = slot_parts(worktree) else {
        return false;
    };
    let _lock = lock_leases(&pdir);
    let mut leases = read_leases(&pdir);
    let Some(owner) = leases.owner_slot(n) else {
        return false;
    };
    let Some(lease) = leases.slots.get_mut(&owner) else {
        return false;
    };
    lease.pid = caller_pid();
    lease.at = now_epoch();
    lease.boot_id = current_boot_id().unwrap_or_default();
    write_leases(&pdir, &leases).is_ok()
}

/// What [`ensure_spare_slot`] found when it looked one lease ahead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpareSlot {
    /// An idle slot already holds a live checkout — the next dispatch is an instant warm hit.
    Ready(u32),
    /// An idle slot number is free but has no checkout yet; a warm was scheduled to build it.
    Warming(u32),
    /// Every slot is leased and the pool is at [`bn_core::pool::MAX_POOL_SIZE`]. Reported, not
    /// papered over — `bn dispatch --on-pool-full` decides what happens next.
    Full(usize),
}

/// Keep a **warm** slot ready for the next dispatch, so leasing the last idle one is not a cliff.
///
/// Slots are otherwise built lazily, at the moment they're leased: the dispatch that finds a cold
/// slot pays for the checkout and the dep install before its agent starts. Looking one step ahead
/// after each lease moves that cost off the critical path.
///
/// It does not grow the pool itself — [`acquire_slot`] does, at the moment a lease actually finds
/// no free slot, so growth is paid for by demand rather than by a look-ahead that guesses at it.
/// This only reports [`SpareSlot::Full`], which now means "at the ceiling" rather than "at the
/// size somebody configured".
///
/// Best-effort: it never fails a dispatch, it only makes the next one cheaper.
fn ensure_spare_slot(ctx: &PoolCtx) -> SpareSlot {
    let leased: BTreeSet<u32> = {
        let _lock = lock_leases(&ctx.pool_dir);
        read_leases(&ctx.pool_dir).reserved_slots()
    };
    let Some(n) = bn_core::pool::pick_slot(ctx.size, &leased) else {
        return SpareSlot::Full(ctx.size);
    };
    if is_live_slot(&slot_path(&ctx.pool_dir, n)) {
        return SpareSlot::Ready(n);
    }
    schedule_warm(&ctx.repo);
    SpareSlot::Warming(n)
}

/// Start a repo-scoped `pool-warm` in the background, so building a slot's checkout and deps never
/// blocks the dispatch that revealed the need. Detached and silent: this is a best-effort top-up,
/// its failure is not the caller's problem, and the run records its own `cron_run` outcome.
fn schedule_warm(repo: &str) {
    // Never under `cargo test`: `current_exe()` there is the *test binary*, so spawning it with
    // `run pool-warm` argv would re-run the test suite as a subprocess. The Phase-8 integration
    // test drives the real `bn` binary out-of-process, where this is `false` and the warm runs.
    if cfg!(test) {
        return;
    }
    // A spawn failure just means no warm — the next lease still creates the slot cold.
    let _ = bn_core::bn_subprocess::spawn_detached(&[
        "run",
        "pool-warm",
        "--repo",
        repo,
        "--branch",
        "main",
    ]);
}

/// The lease label the controller's slot carries in `leases.json` and `wt pool status`. It is not a
/// branch — the slot sits detached at the base — but the lease store is keyed by this string, so a
/// stable label makes re-leasing idempotent.
pub const CONTROLLER_LEASE_LABEL: &str = "control";

/// Lease the controller's reserved slot (`_pool/1`) for `bn control start`. Returns the slot path
/// and whether the lease is fresh, or `None` when the repo has no bn-managed bare clone or the
/// controller slot is already held (message printed). The controller never rotates through the
/// worker range, so this can only ever hand back slot [`bn_core::agent::MAIN_POOL_SLOT`].
pub fn lease_controller_slot(cfg: &Config, repo: &str) -> Option<(PathBuf, bool)> {
    let ctx = resolve_dispatch_pool(cfg, repo)?;
    let leased = acquire_slot(
        &ctx,
        CONTROLLER_LEASE_LABEL,
        None,
        Some("bn-control".into()),
        SlotRole::Controller,
        false,
    )?;
    Some((leased.slot, leased.fresh))
}

/// Resolve the pool for a dispatch. Identical to [`resolve_pool`] except in how the repo is named:
/// this takes it directly (the cron and `bn dispatch` both know it), while `resolve_pool` resolves
/// the cwd or an fzf pick. Neither consults a registry any more — there isn't one — so the only way
/// this returns `None` is a repo with no bn-managed bare clone, the one case a dispatch genuinely
/// can't pool, and the caller cuts a cold worktree instead.
fn resolve_dispatch_pool(cfg: &Config, repo: &str) -> Option<PoolCtx> {
    let (_, worktree_dir) = wt::layout(cfg);
    // Case-insensitive, and the pool goes by the clone's spelling: a job's `--repo BigMono` arrives
    // lowercased, and `bigmono.git` read as "no bare clone — nothing to warm" on a case-sensitive fs.
    let bare = wt::bare_for(cfg, repo)?;
    let repo = bare.file_stem()?.to_string_lossy().to_string();
    let size = pool_size(cfg, &repo);
    let pool_dir = pool_dir(&worktree_dir, &repo);
    Some(PoolCtx {
        bare,
        repo,
        pool_dir,
        worktree_dir,
        size,
    })
}

/// Open the slot's tmux window (unless suppressed) and print its path on stdout for the shell
/// wrapper / agent to `cd` into.
fn finish_get(ctx: &PoolCtx, slot: &Path, branch: &str, path_only: bool, no_tmux: bool) -> bool {
    if !path_only && !no_tmux {
        wt::open_worktree_window(&ctx.repo, &wt::sanitize_branch(branch), slot, false);
    }
    println!("{}", slot.display());
    true
}

fn pool_status(cfg: &Config, repo: Option<String>) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let mut leases = read_leases(&ctx.pool_dir);
    // Self-heal on read: reclaim any slot left leased by a prior boot (WSL/host restart) so the
    // status reflects a recovered pool, not phantom leases pinned by recycled pids.
    let recovered = reclaim_stale(&ctx, &mut leases, ReclaimScope::PriorBootOnly);
    // And the other direction: claim any slot a live agent is working in, so the status is the
    // ledger reconciled against reality rather than the ledger's own opinion. Status is where you
    // look when the pool is confusing, so it is also where the record catches up.
    let adopted = adopt_occupied_slots(&ctx, &mut leases);
    cprintln!(
        "{BLUE}{} pool — {} slots ({} worker + 1 controller){NC}",
        ctx.repo,
        ctx.size,
        ctx.size.saturating_sub(1)
    );
    if !recovered.prior_boot.is_empty() {
        cprintln!(
            "  {GREEN}recovered {} slot(s) from a prior boot: {}{NC}",
            recovered.prior_boot.len(),
            slot_list(&recovered.prior_boot)
        );
    }
    if !adopted.is_empty() {
        cprintln!(
            "  {YELLOW}adopted {} slot(s) held by a live agent the ledger didn't name: {}{NC}",
            adopted.len(),
            slot_list(&adopted)
        );
    }
    let now = now_epoch();
    let current_boot = current_boot_id();
    let built = read_warm_stamps(&ctx.pool_dir);
    let dirty_marks = read_dirty_marks(&ctx.pool_dir);
    for n in 1..=ctx.size as u32 {
        // Slot 1 is `bn control`'s alone — label it so an idle controller slot doesn't read as
        // spare worker capacity.
        let role = if n == bn_core::agent::MAIN_POOL_SLOT {
            "  (controller)"
        } else {
            ""
        };
        // Who is *actually* in the slot, beside what the ledger claims — the two disagreeing is
        // the failure this view exists to make visible.
        let agents = slot_occupants(&slot_path(&ctx.pool_dir, n));
        let occupancy = match agents.first() {
            Some(occupant) => format!("  agent={}", occupant.tmux_target),
            None => String::new(),
        };
        let crowded = if agents.len() > 1 {
            format!("  {RED}⚠ {} agents in one checkout{NC}", agents.len())
        } else {
            String::new()
        };
        let owner = leases.owner_slot(n);
        match owner.and_then(|owner| leases.slots.get(&owner).map(|lease| (owner, lease))) {
            Some((owner, l)) => {
                // Flag a stuck slot so it is visible even though the durable-lease policy leaves it
                // for `prune`/reuse. Two ways a lease stops meaning anything: its holder pid died,
                // or nobody ever turned up in the checkout and the lease has aged out — the common
                // case, since the recorded holder (a tmux server, a login shell) usually outlives
                // every agent it launched. A slot with a live agent is never abandoned, whatever
                // the recorded pid says.
                // Boot-scoped liveness folded in, so a lease from before a reboot reads as dead even
                // if its recycled pid now looks alive.
                let holder_dead = !matches!(
                    bn_core::pool::lease_liveness(
                        boot_opt(&l.boot_id),
                        current_boot.as_deref(),
                        pid_alive(l.pid),
                    ),
                    bn_core::pool::LeaseLiveness::Live
                );
                // Lazy on git: the facts probe runs `git status` only for a lease that tmux and
                // the process table already call abandoned, and the count is fetched only for the
                // one line that prints it. Status used to pay two full status runs per leased
                // slot — minutes on a monorepo pool.
                let facts = reclaim::lease_occupancy(&ctx, n, l, now, !holder_dead);
                // Say what actually frees the slot: a dirty abandoned slot is deliberately *not*
                // reclaimable, so pointing at `prune` there promises something it will refuse to do.
                let flag = if owner != n {
                    "  (paired main comparison)".to_string()
                } else if bn_core::pool::lease_abandoned(facts, LEAKED_LEASE_GRACE_SECS) {
                    // Occupancy only — no git here. Whether uncommitted work holds it is for
                    // `prune` (which refuses) or `return --force` (which discards) to decide.
                    format!(
                        "  (nobody home — `wt pool prune` reclaims it if clean, `wt pool return \
                         {n} --force` regardless)"
                    )
                } else {
                    String::new()
                };
                cprintln!(
                    "  {YELLOW}[{n}] leased{NC}{role}  {}  holder={} age={}{flag}{occupancy}{crowded}",
                    if owner == n {
                        l.branch.clone()
                    } else {
                        format!("main (for {})", l.branch)
                    },
                    l.holder,
                    fmt_age(now - l.at)
                );
            }
            None => {
                // "warm" is only true once a build has completed in the slot; a bare checkout
                // still costs the full install on lease, so say so.
                let state = if !slot_path(&ctx.pool_dir, n).is_dir() {
                    "—"
                } else if dirty_marks.contains(&n) {
                    "idle (dirty — warm skips it; commit the work or `wt pool return N --force`)"
                } else if built.contains(&n) {
                    "idle (warm)"
                } else {
                    "idle (checkout only — not built; `wt pool warm`)"
                };
                cprintln!("  {GREEN}[{n}] {state}{NC}{role}{occupancy}{crowded}");
            }
        }
    }
    true
}

/// Render a slot-number list as `[1] [3]` for status/health lines.
fn slot_list(slots: &[u32]) -> String {
    slots
        .iter()
        .map(|n| format!("[{n}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pool_metrics(cfg: &Config, path: Option<String>, repo: Option<String>) -> bool {
    let Some((repo_name, bare)) = resolve_metrics_repo(cfg, path.as_deref(), repo) else {
        return false;
    };
    let size = pool_size(cfg, &repo_name);
    let (_, worktree_dir) = wt::layout(cfg);
    let ctx = PoolCtx {
        bare,
        pool_dir: pool_dir(&worktree_dir, &repo_name),
        repo: repo_name,
        worktree_dir,
        size,
    };
    let mut leases = read_leases(&ctx.pool_dir);
    // Self-heal on read: recover slots pinned by a prior boot (WSL/host restart) before reporting,
    // so `metrics` both *shows* and *maintains* the pool's health in one call.
    let recovered = reclaim_stale(&ctx, &mut leases, ReclaimScope::PriorBootOnly);
    let events = bn_core::events::read_all();
    let slot_of = |e: &bn_core::events::Event| e.fields.get("slot").and_then(|v| v.as_u64());

    let (mut gets, mut warm, mut returns, mut recoveries) = (0u64, 0u64, 0u64, 0u64);
    let mut per_slot: BTreeMap<u64, u64> = BTreeMap::new();
    // Pair each return with the most recent prior get on the same slot to estimate lease duration.
    let mut open_get: BTreeMap<u64, i64> = BTreeMap::new();
    let mut durations: Vec<i64> = Vec::new();

    for e in events
        .iter()
        .filter(|e| e.fields.get("repo").and_then(|v| v.as_str()) == Some(ctx.repo.as_str()))
    {
        match e.kind.as_str() {
            "pool_get" => {
                gets += 1;
                if e.fields
                    .get("warm")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    warm += 1;
                }
                if let Some(s) = slot_of(e) {
                    *per_slot.entry(s).or_default() += 1;
                    open_get.insert(s, e.ts);
                }
            }
            "pool_return" => {
                returns += 1;
                if let Some(s) = slot_of(e) {
                    if let Some(g) = open_get.remove(&s) {
                        if e.ts >= g {
                            durations.push(e.ts - g);
                        }
                    }
                }
            }
            // A slot auto-reclaimed after a boot changed under it — the restart-recovery counter.
            "pool_recover" => {
                recoveries += 1;
                // A recover ends an open lease without a matching return; don't leave it dangling.
                if let Some(s) = slot_of(e) {
                    open_get.remove(&s);
                }
            }
            _ => {}
        }
    }

    let leased = leases.reserved_slots().len();
    let idle = ctx.size.saturating_sub(leased);
    let cold = gets.saturating_sub(warm);
    let warm_pct = if gets > 0 {
        (warm as f64 / gets as f64 * 100.0).round() as u64
    } else {
        0
    };
    let avg_lease = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<i64>() / durations.len() as i64
    };

    cprintln!("{BLUE}{} pool metrics — {} slots{NC}", ctx.repo, ctx.size);
    println!("  utilization : {leased}/{} leased, {idle} idle", ctx.size);
    println!("  gets        : {gets}  ({warm} warm / {cold} cold · {warm_pct}% warm-hit)");
    println!("  returns     : {returns}");
    println!("  avg lease   : {}", fmt_age(avg_lease));
    if gets > 0 {
        let churn: Vec<String> = (1..=ctx.size as u64)
            .map(|n| format!("[{n}]{}", per_slot.get(&n).copied().unwrap_or(0)))
            .collect();
        println!("  per-slot    : {}", churn.join(" "));
    }
    print_pool_health(&ctx, &leases, &recovered, recoveries);
    true
}

// --- removal: prune + destroy ----------------------------------------------

/// Slot numbers present on disk (`_pool/<n>` dirs) — including over-size ones beyond the config.
fn existing_slot_numbers(pool_dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(pool_dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Ok(n) = e.file_name().to_string_lossy().parse::<u32>() {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// Every slot that actually exists for this pool (a `_pool/<n>` dir and/or a lease), sorted.
fn scan_slots(ctx: &PoolCtx, leases: &Leases) -> Vec<u32> {
    let mut set: BTreeMap<u32, ()> = BTreeMap::new();
    for n in existing_slot_numbers(&ctx.pool_dir) {
        set.insert(n, ());
    }
    for n in leases.slots.keys() {
        set.insert(*n, ());
    }
    for lease in leases.slots.values() {
        if let Some(n) = lease.comparison_slot {
            set.insert(n, ());
        }
    }
    set.into_keys().collect()
}

/// Slot numbers git has registered for this pool (from `worktree list --porcelain`) — anything on
/// disk but missing here is an orphan.
fn registered_slots(ctx: &PoolCtx) -> std::collections::HashSet<u32> {
    let listing = wt::git_out(&[
        "--git-dir",
        &ctx.bare.to_string_lossy(),
        "worktree",
        "list",
        "--porcelain",
    ])
    .unwrap_or_default();
    let mut out = std::collections::HashSet::new();
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some((_, n, pdir)) = slot_parts(Path::new(p)) {
                if pdir == ctx.pool_dir {
                    out.insert(n);
                }
            }
        }
    }
    out
}

/// Classify a slot for the removal decision (the I/O; the pure deciders take the result).
fn classify_slot(
    ctx: &PoolCtx,
    leases: &Leases,
    registered: &std::collections::HashSet<u32>,
    n: u32,
) -> SlotClass {
    let slot = slot_path(&ctx.pool_dir, n);
    let exists = slot.is_dir();
    let group = leases
        .owner_slot(n)
        .and_then(|owner| leases.slots.get(&owner).map(|lease| (owner, lease)))
        .map(|(owner, lease)| {
            [Some(owner), lease.comparison_slot]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![n]);
    SlotClass {
        leased: leases.owner_slot(n).is_some(),
        dirty: group.iter().any(|slot_n| {
            let slot = slot_path(&ctx.pool_dir, *slot_n);
            slot.is_dir() && wt::dirty_count(&slot.to_string_lossy()) > 0
        }),
        // A live agent in the slot is the in-use signal that matters; the cwd scan is the backstop
        // that also catches a plain shell or a build somebody left in the checkout.
        in_use: group.iter().any(|slot_n| {
            let slot = slot_path(&ctx.pool_dir, *slot_n);
            !slot_occupants(&slot).is_empty()
                || crate::agent_lease::any_live_pane_in(&slot)
                || any_proc_in(&slot)
        }),
        over_size: n > ctx.size as u32,
        orphan: !ctx.bare.is_dir() || (exists && !registered.contains(&n)),
    }
}

/// SIGTERM any process whose cwd is inside `slot` (graceful — parity with treehouse).
///
/// Answers on every platform. This used to read `/proc` directly and compile to an empty body off
/// Linux, so on macOS a removed slot kept its processes: the checkout went away under a still-live
/// Nx daemon, which then held a cwd in a directory git had already removed. Reuses
/// [`crate::reap::process_cwds`], the same cross-platform oracle occupancy asks, rather than
/// keeping a second Linux-only copy.
#[allow(
    unsafe_code,
    reason = "libc::kill is required to signal processes discovered through the cwd table"
)]
fn kill_procs_in(slot: &Path) {
    for (pid, cwd) in crate::reap::process_cwds() {
        if cwd.starts_with(slot) {
            // SAFETY: kill(2) with SIGTERM is always safe; an ESRCH race is ignored.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

/// Remove one slot: SIGTERM its processes, step our cwd out if we're inside it, `git worktree
/// remove --force` (with an orphan fallback), drop its lease (caller persists), tidy the parent.
fn remove_slot(ctx: &PoolCtx, n: u32, leases: &mut Leases) -> bool {
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let owner = leases.owner_slot(n);
    let branch = owner
        .and_then(|owner| leases.slots.get(&owner))
        .map(|l| l.branch.clone())
        .unwrap_or_default();

    if let Some(owner) = owner {
        let comparison = leases
            .slots
            .get(&owner)
            .and_then(|lease| lease.comparison_slot);
        for slot_n in [Some(owner), comparison].into_iter().flatten() {
            let slot = slot_path(&ctx.pool_dir, slot_n);
            kill_procs_in(&slot);
            if wt::cwd().starts_with(&slot) {
                let _ = std::env::set_current_dir(&ctx.pool_dir);
            }
            force_remove_worktree(&bare_s, &slot);
            wt::rmdir_if_empty_repo(&slot, &ctx.worktree_dir);
        }
        leases.slots.remove(&owner);
    } else {
        let slot = slot_path(&ctx.pool_dir, n);
        kill_procs_in(&slot);
        if wt::cwd().starts_with(&slot) {
            let _ = std::env::set_current_dir(&ctx.pool_dir);
        }
        force_remove_worktree(&bare_s, &slot);
        wt::rmdir_if_empty_repo(&slot, &ctx.worktree_dir);
    }
    log_pool_event("pool_destroy", &ctx.repo, n, &branch, None);
    true
}

/// `branch=<x>` for a leased slot, else `idle`.
fn slot_tag(leases: &Leases, n: u32) -> String {
    match leases.owner_slot(n) {
        Some(owner) if owner == n => leases
            .slots
            .get(&owner)
            .map(|l| format!("branch={}", l.branch))
            .unwrap_or_else(|| "idle".to_string()),
        Some(owner) => leases
            .slots
            .get(&owner)
            .map(|l| format!("main-for={}", l.branch))
            .unwrap_or_else(|| "idle".to_string()),
        None => "idle".to_string(),
    }
}

/// Print one slot's planned/applied action.
fn print_action(leases: &Leases, n: u32, class: &SlotClass, action: &Action, executed: bool) {
    let tag = slot_tag(leases, n);
    match action {
        Action::Remove => {
            let reason = if class.over_size {
                "over-size"
            } else if class.orphan {
                "orphan"
            } else {
                "targeted"
            };
            let verb = if executed {
                "removed     "
            } else {
                "would remove"
            };
            cprintln!("  {YELLOW}[{n}] {verb}{NC}  {tag}  ({reason})");
        }
        Action::Skip(why) => cprintln!("  {GREEN}[{n}] keep        {NC}  {tag}  ({why})"),
    }
}

fn pool_prune(cfg: &Config, repo: Option<String>, yes: bool, all: bool, verbose: bool) -> bool {
    let targets: Vec<Option<String>> = if all {
        let repos = bn_core::pool::pooled_repos(cfg);
        if repos.is_empty() {
            ceprintln!("{YELLOW}No bn-managed bare clones — nothing to prune.{NC}");
            return true;
        }
        repos.into_iter().map(Some).collect()
    } else {
        vec![repo]
    };
    let mut ok = true;
    for t in targets {
        let Some(ctx) = resolve_pool(cfg, t) else {
            ok = false;
            continue;
        };
        prune_one(&ctx, yes, verbose);
    }
    ok
}

fn prune_one(ctx: &PoolCtx, yes: bool, verbose: bool) {
    let mut leases = read_leases(&ctx.pool_dir);
    let (repo, size) = (&ctx.repo, ctx.size);
    cprintln!("{BLUE}{repo} pool prune — {size} slots configured{NC}");
    // First make the ledger honest — claim slots a live agent holds, hand back the ones nobody
    // does — so the removal pass below classifies against reality rather than a stale record.
    adopt_occupied_slots(ctx, &mut leases);
    let leaked = if yes {
        reclaim_leaked_leases(ctx, &mut leases, now_epoch(), true)
    } else {
        let candidates = leaked_lease_candidates(ctx, &leases, now_epoch());
        for n in &candidates {
            cprintln!(
                "  {YELLOW}[{n}] would reclaim leaked lease ({}) — nobody is in it{NC}",
                slot_tag(&leases, *n)
            );
        }
        candidates
    };
    let registered = registered_slots(ctx);
    let mut to_remove = Vec::new();
    for n in scan_slots(ctx, &leases) {
        // Already reported above as a leaked lease — don't also list it as a kept slot.
        if leaked.contains(&n) {
            continue;
        }
        let class = classify_slot(ctx, &leases, &registered, n);
        let action = prune_decision(class);
        match action {
            Action::Remove => {
                to_remove.push(n);
                print_action(&leases, n, &class, &action, yes);
            }
            // Quietly keep in-size warm slots unless --verbose; always show protected skips.
            Action::Skip("in-size warm slot") if !verbose => {}
            Action::Skip(_) => print_action(&leases, n, &class, &action, yes),
        }
    }
    // Reclaimed leases and removed slots are different outcomes — a reclaimed slot stays warm and
    // leasable, a pruned one is gone — so the summary counts them apart.
    let reclaimed = match leaked.len() {
        0 => String::new(),
        n => format!(
            " · {n} leaked lease(s) {}",
            if yes { "reclaimed" } else { "to reclaim" }
        ),
    };
    if yes {
        for n in &to_remove {
            remove_slot(ctx, *n, &mut leases);
        }
        let _ = write_leases(&ctx.pool_dir, &leases);
        cprintln!("{GREEN}Pruned {} slot(s){reclaimed}.{NC}", to_remove.len());
    } else {
        cprintln!(
            "{YELLOW}Dry run — {} slot(s) would be pruned{reclaimed}. Re-run with --yes.{NC}",
            to_remove.len()
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn pool_destroy(
    cfg: &Config,
    slot: Option<u32>,
    repo: Option<String>,
    all: bool,
    yes: bool,
    flags: DestroyFlags,
) -> bool {
    if flags.include_leased && all {
        let msg = "--include-leased can't be combined with --all — target an exact slot.";
        ceprintln!("{RED}{msg}{NC}");
        return false;
    }
    match (slot, all) {
        (None, false) => {
            ceprintln!("{RED}Specify a slot number or --all.{NC}");
            return false;
        }
        (Some(_), true) => {
            ceprintln!("{RED}Pass either a slot number or --all, not both.{NC}");
            return false;
        }
        _ => {}
    }
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let mut leases = read_leases(&ctx.pool_dir);
    let registered = registered_slots(&ctx);
    let targets: Vec<u32> = match slot {
        Some(n) => vec![n],
        None => scan_slots(&ctx, &leases),
    };
    if targets.is_empty() {
        cprintln!(
            "{YELLOW}{} pool is empty — nothing to destroy.{NC}",
            ctx.repo
        );
        return true;
    }
    let (repo, size) = (&ctx.repo, ctx.size);
    cprintln!("{BLUE}{repo} pool destroy — {size} slots configured{NC}");
    let mut to_remove = Vec::new();
    for n in &targets {
        // A specific slot that doesn't exist at all → report and skip.
        if !slot_path(&ctx.pool_dir, *n).is_dir() && leases.owner_slot(*n).is_none() {
            cprintln!("  {YELLOW}[{n}] no such slot{NC}");
            continue;
        }
        let class = classify_slot(&ctx, &leases, &registered, *n);
        let action = destroy_decision(class, flags);
        if action == Action::Remove {
            to_remove.push(*n);
        }
        print_action(&leases, *n, &class, &action, yes);
    }
    if yes {
        for n in &to_remove {
            remove_slot(&ctx, *n, &mut leases);
        }
        let _ = write_leases(&ctx.pool_dir, &leases);
        cprintln!("{GREEN}Destroyed {} slot(s).{NC}", to_remove.len());
    } else {
        cprintln!(
            "{YELLOW}Dry run — {} slot(s) would be destroyed. Re-run with --yes.{NC}",
            to_remove.len()
        );
    }
    true
}

/// Compact human age: `42s`, `5m`, `3h`, `2d`.
fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

#[cfg(test)]
mod pool_tests;

/// Real-git integration tests for `pool get`/`pool warm`: each builds a throwaway bare repo in a
/// tempdir and drives the actual `git` binary, so a regression in how a slot is created, switched,
/// reset or torn down is caught here rather than in a broken pool on someone's box. Split in two
/// when the single file crossed the 1000-line ratchet — slot mechanics here, acquire/lease next.
#[cfg(test)]
mod git_integration_tests;
#[cfg(test)]
mod pool_acquire_tests;
