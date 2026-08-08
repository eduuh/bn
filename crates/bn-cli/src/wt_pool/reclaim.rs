//! Handing pool slots back — the two sweeps that decide a lease no longer means anything.
//!
//! A lease is durable until `return` on purpose, so nothing here guesses: [`reclaim_stale`] frees a
//! slot whose *holder process* is provably gone (the reboot/WSL recovery), and
//! [`reclaim_leaked_leases`] frees a slot whose *checkout* is provably empty. The second exists
//! because the first is not enough on its own — the recorded holder is the caller's parent, a tmux
//! server or login shell that outlives every agent it launches, so a pid check alone frees nothing
//! in ordinary use and the pool silently fills with leases whose agents ended hours ago.
//!
//! Both keep the checkout: reclaiming returns a slot to *warm idle*, it does not destroy it.

use super::*;

/// How aggressively [`reclaim_stale`] frees abandoned slots.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReclaimScope {
    /// Only leases from a *prior boot* (holder provably dead — WSL shutdown / host reboot). Safe to
    /// run on every access, so the pool self-heals the moment anything touches it after a restart.
    PriorBootOnly,
    /// Any dead-holder lease — prior-boot *or* a same-boot lease whose pid is gone. The last-resort
    /// sweep when the pool is otherwise full, honoring the durable-lease policy for a live boot
    /// (a same-boot lease is only reclaimed here, not pre-emptively).
    AnyDeadHolder,
}

/// What a [`reclaim_stale`] pass freed, split by cause so callers can report the WSL/reboot
/// recovery (`prior_boot`) apart from an ordinary dead-holder reclaim (`this_boot`).
#[derive(Debug, Default, Clone)]
pub(super) struct ReclaimReport {
    /// Slots reclaimed because their lease predated the current boot (a restart recovery).
    pub(super) prior_boot: Vec<u32>,
    /// Slots reclaimed because a same-boot holder pid had died.
    pub(super) this_boot: Vec<u32>,
}

impl ReclaimReport {
    pub(super) fn changed(&self) -> bool {
        !self.prior_boot.is_empty() || !self.this_boot.is_empty()
    }
}

/// Drop leases whose holder is gone, resetting each reclaimed slot to warm-idle, and persist the
/// lease store when anything changed. A slot is reclaimed only when its holder is gone (per `scope`
/// and the boot-aware [`bn_core::pool::lease_liveness`]) AND nothing is running in it AND it has no
/// uncommitted work to lose — a dirty abandoned slot is left leased (`return --force` frees it).
/// Each prior-boot reclaim logs a `pool_recover` event so `pool metrics` can show restart recoveries.
pub(super) fn reclaim_stale(
    ctx: &PoolCtx,
    leases: &mut Leases,
    scope: ReclaimScope,
) -> ReclaimReport {
    use bn_core::pool::LeaseLiveness;
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let current_boot = current_boot_id();
    let mut report = ReclaimReport::default();

    let candidates: Vec<(u32, bool)> = leases
        .slots
        .iter()
        .filter_map(|(n, l)| {
            let slot = slot_path(&ctx.pool_dir, *n);
            let liveness = bn_core::pool::lease_liveness(
                boot_opt(&l.boot_id),
                current_boot.as_deref(),
                pid_alive(l.pid),
            );
            let prior_boot = matches!(liveness, LeaseLiveness::StalePriorBoot);
            let dead_holder = match scope {
                ReclaimScope::PriorBootOnly => prior_boot,
                ReclaimScope::AnyDeadHolder => !matches!(liveness, LeaseLiveness::Live),
            };
            // Never clobber a live agent, a live process's cwd, or uncommitted work — even for a
            // prior-boot lease: the working tree survives a reboot, so a dirty slot is preserved
            // and surfaced by `pool metrics` (commit or `return --force`) rather than silently
            // discarded. The occupancy check is the load-bearing one off Linux, where
            // `any_proc_in` cannot see anything.
            let reclaimable = dead_holder
                && slot_occupants(&slot).is_empty()
                && !any_proc_in(&slot)
                && wt::dirty_count(&slot.to_string_lossy()) == 0;
            reclaimable.then_some((*n, prior_boot))
        })
        .collect();

    for (n, prior_boot) in candidates {
        let slot = slot_path(&ctx.pool_dir, n);
        let branch = leases
            .slots
            .get(&n)
            .map(|l| l.branch.clone())
            .unwrap_or_default();
        warm_reset(&slot.to_string_lossy(), &bare_s);
        leases.slots.remove(&n);
        if prior_boot {
            log_pool_event("pool_recover", &ctx.repo, n, &branch, None);
            report.prior_boot.push(n);
        } else {
            report.this_boot.push(n);
        }
    }
    if report.changed() {
        let _ = write_leases(&ctx.pool_dir, leases);
    }
    report
}

/// How long a lease with nobody in its slot is given before it counts as leaked. Long enough to
/// cover the gap between `bn dispatch` taking a slot and its agent appearing in tmux; short enough
/// that a day's work doesn't silently fill the pool. Same grace the fleet GC gives a quiet agent.
pub(super) const LEAKED_LEASE_GRACE_SECS: i64 = bn_core::fleet_gc::DEFAULT_MIN_IDLE_SECS;

/// Everything [`bn_core::pool::lease_abandoned`] needs to judge one slot, gathered from tmux and
/// git. Shared by the sweep and by `pool status`, so the ledger and the view never disagree about
/// which slots are actually free.
pub(super) fn lease_facts(
    ctx: &PoolCtx,
    n: u32,
    lease: &Lease,
    now: i64,
) -> bn_core::pool::LeaseFacts {
    let slot = slot_path(&ctx.pool_dir, n);
    bn_core::pool::LeaseFacts {
        holder_alive: pid_alive(lease.pid),
        age_secs: now - lease.at,
        // "Someone is here" is a fact about tmux, not about the ledger: an agent that registered, an
        // unregistered harness pane, or just a shell somebody left open in the checkout.
        occupied: !slot_occupants(&slot).is_empty()
            || crate::agent_lease::any_live_pane_in(&slot)
            || any_proc_in(&slot),
        dirty: wt::dirty_count(&slot.to_string_lossy()) > 0,
    }
}

/// The slots whose leases are leaked — nobody in the checkout, nothing uncommitted, and either a
/// dead holder or a lease old enough that an agent would long since have shown up.
pub(super) fn leaked_lease_candidates(ctx: &PoolCtx, leases: &Leases, now: i64) -> Vec<u32> {
    leases
        .slots
        .iter()
        .filter(|(n, lease)| {
            bn_core::pool::lease_abandoned(
                lease_facts(ctx, **n, lease, now),
                LEAKED_LEASE_GRACE_SECS,
            )
        })
        .map(|(n, _)| *n)
        .collect()
}

/// Every pool that exists on this machine, as `(repo, ctx)` — one per `<worktree>/<repo>/_pool`
/// directory with a lease store in it.
///
/// Enumerated from disk rather than from `~/.config/wt/pools` on purpose: a plain `bn dispatch`
/// pools *every* repo at [`bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE`] whether or not it is listed,
/// so the config would miss exactly the unregistered pools nobody is watching.
fn existing_pools(cfg: &Config) -> Vec<PoolCtx> {
    let (_, worktree_dir) = wt::layout(cfg);
    let Ok(entries) = std::fs::read_dir(&worktree_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(repo) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !leases_path(&entry.path().join("_pool")).is_file() {
            continue;
        }
        if let Some(ctx) = resolve_dispatch_pool(cfg, &repo) {
            out.push(ctx);
        }
    }
    out
}

/// Make every pool's ledger match who is actually in its slots — the queue tick's pool counterpart
/// to its backlog-lease reclaim.
///
/// A slot is leased by `bn dispatch` and handed back by `bn agent kill`, and the second half only
/// happens on that one path: an agent whose tmux window simply closes leaves its lease behind
/// forever. Nothing noticed, because the recorded holder is the caller's parent — a tmux server or
/// login shell that never exits — so a pool would sit at "7/7 worker slots leased" with three
/// agents actually running, and every dispatch spilled into a cold worktree.
///
/// This runs at the head of a tick pass, before `realize` dispatches anything, so the pass that
/// frees a slot is the pass that uses it. A separate cron would leave a window in which the
/// dispatcher plans against a pool it wrongly believes is full and spills cold anyway — the drift
/// has to be gone *before* the dispatch in the same pass, not merely bounded by some cadence.
///
/// It is **not** the reclamation `config/crons.toml` deliberately keeps manual: it deletes no
/// branch, removes no worktree, kills no session. It edits `leases.json` and detaches slots that
/// are provably empty — and `warm_reset`'s `clean -df` cannot delete anything there, since a single
/// untracked file would have made the slot dirty and excluded it.
pub(crate) fn reconcile_pools(cfg: &Config, now: i64) {
    for ctx in existing_pools(cfg) {
        let _lock = lock_leases(&ctx.pool_dir);
        let mut leases = read_leases(&ctx.pool_dir);
        // Claim first, hand back second: a slot whose live agent the ledger never named must be
        // adopted before the sweep looks at it, or it reads as an unleased stranger.
        let adopted = adopt_occupied_slots(&ctx, &mut leases);
        let freed = reclaim_leaked_leases(&ctx, &mut leases, now, false);
        // Reported, not silent: a slot coming back means something ended without saying so, and a
        // machine that keeps leaking them is worth noticing.
        if !adopted.is_empty() {
            eprintln!(
                "bn queue tick: adopted {} slot(s) {adopted:?} held by a live agent the ledger \
                 didn't name",
                ctx.repo
            );
        }
        if !freed.is_empty() {
            eprintln!(
                "bn queue tick: returned {} pool slot(s) {freed:?} — leased but nobody in them",
                ctx.repo
            );
        }
    }
}

/// Reclaim every leaked lease in `leases`, returning the slots freed. `announce` prints a line per
/// slot — `wt pool prune` reports its work, while the sweep inside a lease acquisition is silent
/// (dispatch owns that stdout).
pub(super) fn reclaim_leaked_leases(
    ctx: &PoolCtx,
    leases: &mut Leases,
    now: i64,
    announce: bool,
) -> Vec<u32> {
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let leaked = leaked_lease_candidates(ctx, leases, now);

    for n in &leaked {
        let slot = slot_path(&ctx.pool_dir, *n);
        let branch = leases
            .slots
            .get(n)
            .map(|l| l.branch.clone())
            .unwrap_or_default();
        warm_reset(&slot.to_string_lossy(), &bare_s);
        leases.slots.remove(n);
        log_pool_event("pool_return", &ctx.repo, *n, &branch, None);
        if announce {
            println!("  {GREEN}[{n}] reclaimed leaked lease ({branch}){NC}");
        }
    }
    if !leaked.is_empty() {
        let _ = write_leases(&ctx.pool_dir, leases);
    }
    leaked
}
