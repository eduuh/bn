//! Handing a pool slot back — **the** way a lease ends, and the one place that decides whether it
//! may.
//!
//! There were two implementations of this. One structured and automatic ([`release_dispatch_slot`],
//! for an agent that died), one human and `--force`-capable ([`return_slot`], for `wt pool return`).
//! They checked the same three things — leased, occupied, dirty — in different orders, with
//! different answers, and drifted apart over time. That is a bad thing to have two of: this is the
//! code that decides whether a checkout somebody is working in may be reset underneath them.
//!
//! So there is one core ([`release_slot_core`]) and everything else here is a report of it. The
//! guards are refusals, never silent discards, and `--force` is the single axis that relaxes them —
//! and even then only through [`evict_occupant`], which evicts an agent solely when the fleet
//! reaper's own policy calls its work finished.
//!
//! Releasing keeps the checkout. A returned slot is *warm idle*, ready for its next tenant — which
//! is the entire point of the pool, and why this is a different operation from removing a worktree.

use super::*;
/// What handing a slot back did — the honest outcomes of [`release_dispatch_slot`], so a caller can
/// report "returned" separately from "left alone because it holds work".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SlotRelease {
    /// The slot was leased and is now reset and back in the pool.
    Returned {
        repo: String,
        slot: u32,
        branch: String,
    },
    /// The slot holds uncommitted changes — left leased on purpose. Never discard work implicitly;
    /// `wt pool return <n> --force` is the deliberate discard.
    Dirty {
        repo: String,
        slot: u32,
        dirty: usize,
    },
    /// Another live agent is still working in the slot — left leased. Returning it would reset a
    /// checkout somebody is using and then hand it to the next dispatch, which is precisely how a
    /// pool slot came to host two agents at once.
    Occupied {
        repo: String,
        slot: u32,
        holder: String,
    },
    /// The path is not a `_pool/<n>` slot (a cold worktree) — nothing to return.
    NotASlot,
    /// The slot was already free. Idempotent success: releasing twice is not an error.
    NotLeased { repo: String, slot: u32 },
    /// The lease file could not be written — the slot is still leased and the caller must say so.
    Failed {
        repo: String,
        slot: u32,
        error: String,
    },
}

impl SlotRelease {
    /// A one-line human summary, or `None` for a path that was never a pool slot.
    pub fn message(&self) -> Option<String> {
        match self {
            SlotRelease::Returned { repo, slot, .. } => {
                Some(format!("returned pool slot {slot} ({repo})"))
            }
            SlotRelease::Dirty { repo, slot, dirty } => Some(format!(
                "pool slot {slot} ({repo}) has {dirty} uncommitted change(s) — left leased; \
                 inspect it, then `wt pool return {slot} --force`"
            )),
            SlotRelease::Occupied { repo, slot, holder } => Some(format!(
                "pool slot {slot} ({repo}) is still held by a live {holder} — left leased; \
                 returning it would reset a checkout in use"
            )),
            SlotRelease::NotASlot => None,
            SlotRelease::NotLeased { repo, slot } => {
                Some(format!("pool slot {slot} ({repo}) was already free"))
            }
            SlotRelease::Failed { repo, slot, error } => Some(format!(
                "could not return pool slot {slot} ({repo}): {error} — the slot is still leased"
            )),
        }
    }
}

/// Hand a dispatch slot back when the agent holding it is gone (`bn agent kill`). `gone_session` is
/// the session the caller just killed, so its own (already dead) window never reads as an occupant.
///
/// Leasing a slot is durable-until-returned, and until this existed *nothing* returned a worker
/// slot: every killed agent leaked one, so a pool filled up over a day's work and every later
/// dispatch silently fell back to cutting a cold worktree. Safe by construction — a slot with
/// uncommitted changes is left leased rather than reset, a slot that isn't leased is a no-op, and a
/// failed lease write is *reported* instead of swallowed, because "the slot came back" and "the
/// write failed" are the same picture on disk otherwise. Takes the pool lock, so a concurrent
/// dispatch cannot lease the slot mid-reset.
pub fn release_dispatch_slot(
    cfg: &Config,
    worktree: &Path,
    gone_session: Option<&str>,
) -> SlotRelease {
    release_slot_core(
        cfg,
        worktree,
        ReleasePolicy {
            force: false,
            gone_session,
        },
    )
}

/// How hard a release is allowed to push. The only axis on which the two release surfaces differ —
/// everything else about handing a slot back is [`release_slot_core`].
struct ReleasePolicy<'a> {
    /// Evict a provably-finished occupant, discard uncommitted changes, and reset a slot that isn't
    /// leased at all. The `wt pool return --force` valve; never set by an automatic path.
    force: bool,
    /// A session the caller has just killed, discounted when deciding whether anyone still occupies
    /// the slot. `None` means "trust the occupancy probe as-is".
    gone_session: Option<&'a str>,
}

/// Hand a pool slot back: the one implementation, under the pool lock.
///
/// Both release surfaces are this function — [`release_dispatch_slot`] (structured, automatic) and
/// [`return_slot`] (human, `--force`-capable). They were separate copies that had drifted into
/// checking the same three things in different orders with different answers, which is precisely the
/// bug you cannot afford in the code that decides whether a checkout somebody is working in may be
/// reset. The guards, in order, and each one is a refusal rather than a discard:
///
/// 1. **Leased?** An unleased slot has nothing to hand back (`--force` resets it anyway).
/// 2. **Occupied?** A live agent's checkout is not ours to reset, whatever the ledger says.
///    `--force` routes through [`evict_occupant`], which evicts only a *provably finished* agent.
/// 3. **Dirty?** Uncommitted work is never silently discarded (`--force` discards).
///
/// Only then: warm-reset the checkout, drop the lease, persist, log. A failed lease write is
/// reported rather than swallowed — "the slot came back" and "the write failed" look identical on
/// disk otherwise.
fn release_slot_core(cfg: &Config, worktree: &Path, policy: ReleasePolicy) -> SlotRelease {
    let Some((repo, n, pdir)) = slot_parts(worktree) else {
        return SlotRelease::NotASlot;
    };
    let (bare_dir, _) = wt::layout(cfg);
    let bare = bare_dir.join(format!("{repo}.git"));

    let _lock = lock_leases(&pdir);
    let mut leases = read_leases(&pdir);
    let lease = leases.slots.get(&n).cloned();
    if lease.is_none() && !policy.force {
        return SlotRelease::NotLeased { repo, slot: n };
    }
    // Whose slot is it *really*? A kill knows which agent it stopped, not which checkout that agent
    // shared — and the worktree it resolves can belong to somebody else entirely (a stale lease
    // entry, or git's answer for a branch that has since moved). Anyone still working here keeps it.
    if let Some(holder) = slot_occupants(worktree).into_iter().find(|occupant| {
        match (occupant.session_id.as_deref(), policy.gone_session) {
            // Only the session we just killed is discounted. An occupant with no session id
            // registered nothing and can't be the one that was killed — it still holds the slot.
            (Some(seen), Some(gone)) => seen != gone,
            _ => true,
        }
    }) {
        // `--force` may evict, but only an agent `fleet_gc::classify` calls finished. If it won't
        // go, the slot is still held and must report as held.
        if !policy.force || !evict_occupant(cfg, worktree, &holder, n, &repo) {
            return SlotRelease::Occupied {
                repo,
                slot: n,
                holder: holder.describe(),
            };
        }
    }
    // The occupant probe above only sees agents it can *name* — registered leases, stamped
    // windows, harness-titled panes. A live pane it couldn't classify (a shell someone left open,
    // a harness whose process title none of the probes match) still means a warm reset would pull
    // the checkout out from under someone. Same cwd-based rule the leaked-lease sweep's
    // `lease_facts` already applies, and like there it is the load-bearing check off Linux, where
    // no `/proc` scan can answer. Not consulted under `--force`: the human aiming `wt pool return
    // --force` at a slot is standing somewhere, possibly in it.
    if !policy.force && crate::agent_lease::any_live_pane_in(worktree) {
        return SlotRelease::Occupied {
            repo,
            slot: n,
            holder: "tmux pane rooted in the slot".to_string(),
        };
    }
    if !policy.force {
        let dirty = wt::dirty_count(&worktree.to_string_lossy());
        if dirty > 0 {
            return SlotRelease::Dirty {
                repo,
                slot: n,
                dirty,
            };
        }
    }
    let branch = lease.map(|l| l.branch).unwrap_or_default();
    warm_reset(&worktree.to_string_lossy(), &bare.to_string_lossy());
    leases.slots.remove(&n);
    if let Err(e) = write_leases(&pdir, &leases) {
        return SlotRelease::Failed {
            repo,
            slot: n,
            error: e.to_string(),
        };
    }
    log_pool_event("pool_return", &repo, n, &branch, None);
    SlotRelease::Returned {
        repo,
        slot: n,
        branch,
    }
}

/// Hand the controller's slot back when `bn control stop` tears its window down, so the next
/// `bn control start` can lease it instead of reporting the slot held by a controller that is gone.
/// Quiet and idempotent: a slot that isn't leased (or a repo with no pool) is simply a no-op, since
/// this runs on every stop including controllers that predate slot leasing. Dirty slots are left
/// alone — `wt pool return --force` is the deliberate discard.
pub fn release_controller_slot(cfg: &Config, repo: &str) {
    let Some(ctx) = resolve_dispatch_pool(cfg, repo) else {
        return;
    };
    let n = bn_core::agent::CONTROLLER_POOL_SLOT;
    let _lock = lock_leases(&ctx.pool_dir);
    let mut leases = read_leases(&ctx.pool_dir);
    if !leases.slots.contains_key(&n) {
        return;
    }
    let slot = slot_path(&ctx.pool_dir, n);
    if wt::dirty_count(&slot.to_string_lossy()) > 0 {
        eprintln!(
            "{YELLOW}Controller slot {n} ({}) has uncommitted changes — leaving it leased. \
             Inspect it, then `wt pool return {n} --force`.{NC}",
            ctx.repo
        );
        return;
    }
    warm_reset(&slot.to_string_lossy(), &ctx.bare.to_string_lossy());
    leases.slots.remove(&n);
    let _ = write_leases(&ctx.pool_dir, &leases);
    log_pool_event("pool_return", &ctx.repo, n, CONTROLLER_LEASE_LABEL, None);
}

pub(super) fn pool_return(cfg: &Config, target: Option<String>, force: bool) -> bool {
    let (_, worktree_dir) = wt::layout(cfg);
    let slot = match target {
        Some(t) if Path::new(&t).is_absolute() => PathBuf::from(t),
        Some(t) if t.chars().all(|c| c.is_ascii_digit()) => {
            let Some(bare) = wt::find_bare_repo(&wt::cwd()) else {
                eprintln!("{RED}A slot number needs a repo — run from inside the repo or pass a path.{NC}");
                return false;
            };
            let repo = wt::repo_name(&bare);
            slot_path(&pool_dir(&worktree_dir, &repo), t.parse().unwrap_or(0))
        }
        Some(t) => {
            eprintln!("{RED}Invalid target '{t}' — pass a slot number or an absolute path.{NC}");
            return false;
        }
        None => match find_slot_root(&wt::cwd()) {
            Some(s) => s,
            None => {
                eprintln!("{RED}Not inside a pool slot — pass a slot number or path.{NC}");
                return false;
            }
        },
    };
    return_slot(cfg, &slot, force)
}

/// Hand a concrete slot back and report structurally — [`release_slot_core`] with a `force` policy.
/// What [`crate::wt::cleanup_one`] uses, so the one reclaim path can tell "already free" (nothing to
/// do) apart from "refused" (someone is working in it) instead of collapsing both to a bool.
pub(crate) fn release_slot(cfg: &Config, slot: &Path, force: bool) -> SlotRelease {
    release_slot_core(
        cfg,
        slot,
        ReleasePolicy {
            force,
            gone_session: None,
        },
    )
}

/// Reset and release a concrete pool slot path — the human-facing skin over [`release_slot_core`].
///
/// Adds exactly two things to the core, and nothing else: a `--force` policy, and the wording. Every
/// guard, and the order they run in, lives in the core, so `wt pool return` and an automatic release
/// can no longer disagree about whether a slot is safe to reset.
pub(crate) fn return_slot(cfg: &Config, slot: &Path, force: bool) -> bool {
    match release_slot_core(
        cfg,
        slot,
        ReleasePolicy {
            force,
            gone_session: None,
        },
    ) {
        SlotRelease::Returned { repo, slot: n, .. } => {
            println!("{GREEN}Returned slot {n} ({repo}) to the pool.{NC}");
            true
        }
        SlotRelease::NotASlot => {
            eprintln!("{RED}Not a pool slot: {}{NC}", slot.display());
            false
        }
        SlotRelease::NotLeased { repo, slot: n } => {
            eprintln!(
                "{YELLOW}Slot {n} ({repo}) isn't leased. Use --force to reset it anyway.{NC}"
            );
            false
        }
        SlotRelease::Occupied {
            repo,
            slot: n,
            holder,
        } => {
            // Under `--force` the core already tried to evict and `evict_occupant` said why.
            if !force {
                eprintln!(
                    "{RED}Slot {n} ({repo}) is in use by a live {holder}.{NC} Steer or stop it \
                     first, or `wt pool return {n} --force` to evict it once its work has landed."
                );
            }
            false
        }
        SlotRelease::Dirty {
            repo,
            slot: n,
            dirty,
        } => {
            eprintln!(
                "{RED}Slot {n} ({repo}) has {dirty} uncommitted change(s).{NC} Commit/stash first, or `wt pool return {n} --force` to discard."
            );
            false
        }
        SlotRelease::Failed {
            repo,
            slot: n,
            error,
        } => {
            eprintln!("{RED}Could not return slot {n} ({repo}): {error}{NC}");
            false
        }
    }
}

/// Evict the live agent holding a slot, for `wt pool return --force` — but only when its work is
/// provably finished. Returns whether the slot is now free to reset.
///
/// "Provably finished" is not a new rule: it is [`bn_core::fleet_gc::classify`], the same policy
/// The stale-agent reaper already applies to every agent on the box — its PR merged or closed, its window
/// or worktree gone, and past the idle grace with nothing uncommitted. Anything else (mid-turn,
/// dirty, freshly launched) is kept, because a forced *slot* return should never be a quieter way
/// to kill a working agent than `bn agent kill` is.
fn evict_occupant(
    cfg: &Config,
    slot: &Path,
    occupant: &crate::agent_lease::Occupant,
    n: u32,
    repo: &str,
) -> bool {
    use bn_core::fleet_gc::{classify, Disposition, PrStatus, ReapPolicy};

    let mut observation = crate::reap::observe_agent(
        cfg,
        slot,
        occupant.repo.as_deref().unwrap_or(repo),
        occupant.branch.as_deref().unwrap_or_default(),
        &occupant.tmux_target,
    );
    // The controller slot is `bn control`'s, and the orchestrator guard is the one gate
    // `fleet_gc` never lets anything override.
    observation.is_orchestrator = n == bn_core::agent::CONTROLLER_POOL_SLOT;

    // A registered agent can leave behind a live tmux window after its session is gone. If its PR
    // landed/closed and the checkout is clean, `pool return --force` may reap that stale window even
    // when the pane classifier still reads it as "working" from leftover terminal output.
    let registered = occupant.session_id.is_some();
    let stale_pr = matches!(observation.pr, PrStatus::Merged | PrStatus::Closed);
    if registered && stale_pr && observation.has_uncommitted {
        eprintln!(
            "{RED}Slot {n} ({repo}) belongs to a finished {} but has uncommitted work — left leased.{NC}\n  \
             Commit/stash it, or inspect and discard deliberately with `wt pool return {n} --force` \
             after closing the stale window.",
            occupant.describe()
        );
        return false;
    }
    let session_live = occupant.session_id.as_ref().is_some_and(|session_id| {
        bn_core::agent::supervise(
            cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        )
        .agents
        .iter()
        .any(|agent| agent.session_id == *session_id)
    });
    let stale_registered_window =
        registered && !session_live && stale_pr && !observation.is_orchestrator;
    let policy = ReapPolicy {
        force: stale_registered_window,
        ..ReapPolicy::default()
    };

    if let Disposition::Keep { reason } = classify(&observation, &policy) {
        eprintln!(
            "{RED}Slot {n} ({repo}) is held by a live {} that is still needed ({}).{NC}\n  \
             Force returns a slot whose agent has finished — its PR merged or closed, or it went \
             quiet with a clean tree.\n  To take it anyway, stop the agent first: bn agent kill {}",
            occupant.describe(),
            reason.label(),
            match (&occupant.repo, &occupant.branch) {
                (Some(r), Some(b)) => format!("{r}:{b}"),
                _ => occupant.tmux_target.clone(),
            }
        );
        return false;
    }

    eprintln!(
        "{YELLOW}Evicting finished {} from slot {n} ({repo})…{NC}",
        occupant.describe()
    );
    if let Some(window) = crate::agent_lease::window_id_for_target(&occupant.tmux_target) {
        crate::reap::kill_window_trees(std::slice::from_ref(&window));
        let _ = crate::proc::tmux_ok(&["kill-window", "-t", &window]);
    }
    if let Some(session) = &occupant.session_id {
        let _ = crate::agent_lease::release(session);
    }
    // Verify rather than trust: if anything is still alive in there, leave the slot leased.
    if let Some(still) = slot_occupants(slot).into_iter().next() {
        eprintln!(
            "{RED}Slot {n} ({repo}) still hosts a live {} after the eviction — left leased.{NC}",
            still.describe()
        );
        return false;
    }
    true
}
