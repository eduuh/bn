//! What a pool slot *is*, derived from the world rather than read from a ledger.
//!
//! Three things already answer it and cannot lie about themselves: the checkout's `HEAD` (which
//! branch it holds), its working tree (whether there is work to lose), and tmux plus the
//! agent-lease registry (who is in it). They also survive a reboot, which a recorded pid and boot
//! id only pretended to.
//!
//! The pool used to keep its own durable ledger of the same facts, which drifted from them
//! constantly — and then needed four separate reconcilers to catch up: a prior-boot sweep, a
//! dead-pid sweep, a leaked-lease sweep, and an "adopt whoever is actually in there" pass. Each was
//! added after the ledger lied in a new way, and the ways it lied are in the git history: a branch
//! that retried once a minute for eight hours against a slot nobody was in; leases that could never
//! read as abandoned off Linux, so slots leaked until returned by hand; a fresh branch switched
//! into a checkout somebody was working in.
//!
//! Deriving the answer removes the thing they were reconciling. What cannot be derived is the gap
//! between a dispatch taking a slot and its agent appearing in tmux — for those few seconds nothing
//! observable says the slot is spoken for. That, and only that, is what a reservation covers.
//!
//! Split from [`crate::pool`] because it is a self-contained rule with its own vocabulary: the
//! parent module decides which repos pool and how big; this one decides whether a given slot can be
//! handed to a new tenant.

use std::collections::BTreeMap;

/// How long a reservation holds a slot that has nothing observable in it yet.
///
/// It exists solely to cover the dispatch → agent-appears gap, so it is a timeout, not a tenancy:
/// once the agent is up it is visible on its own and the reservation stops mattering. Matching the
/// fleet's own idle grace keeps one notion of "long enough for something to show up" in bn.
pub const RESERVATION_TTL_SECS: i64 = crate::fleet_gc::DEFAULT_MIN_IDLE_SECS;

/// What one slot is, derived from [`SlotFacts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    /// No usable checkout on disk — never created, or left broken by an interrupted `worktree add`.
    /// Acquiring it means building it first.
    Cold,
    /// A live checkout with nobody in it, nothing uncommitted, and no live reservation. Free.
    Idle,
    /// Spoken for. `branch` is what the checkout holds (`None` while detached at base).
    Busy {
        branch: Option<String>,
        why: BusyReason,
    },
}

/// Why a slot is unavailable, strongest evidence first — this is also the precedence
/// [`slot_state`] applies, so a message names the most concrete reason it could.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusyReason {
    /// Somebody is in it: a registered agent, a harness pane, or a shell. Carries their label.
    Occupied(String),
    /// Uncommitted work. Never reset out from under anyone, whatever else is true.
    Dirty(usize),
    /// The detached comparison half of slot `owner`'s tenancy — idle by design, not free.
    Paired(u32),
    /// Reserved by an acquisition whose agent has not appeared yet.
    Reserved,
}

/// What the caller observed about one slot. The I/O (git, tmux, the registry, the reservation
/// file) belongs to `bn-cli`; the rule for reading it lives here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotFacts {
    /// A real git worktree is on disk (not a directory left by an interrupted checkout).
    pub live: bool,
    /// The branch `HEAD` names, or `None` when detached — a warm slot at rest is detached.
    pub branch: Option<String>,
    /// Live agents, harness panes and processes in the checkout, oldest first.
    pub occupants: Vec<String>,
    /// Uncommitted tracked+untracked entries.
    pub dirty: usize,
    /// A reservation for this slot was taken less than [`RESERVATION_TTL_SECS`] ago.
    pub reserved: bool,
    /// This slot is the comparison half of slot `owner`'s tenancy.
    pub paired_to: Option<u32>,
}

/// Read one slot's facts as a state. The whole occupancy rule, in one place.
pub fn slot_state(f: &SlotFacts) -> SlotState {
    if !f.live {
        return SlotState::Cold;
    }
    let busy = |why| SlotState::Busy {
        branch: f.branch.clone(),
        why,
    };
    if let Some(who) = f.occupants.first() {
        return busy(BusyReason::Occupied(who.clone()));
    }
    if f.dirty > 0 {
        return busy(BusyReason::Dirty(f.dirty));
    }
    if let Some(owner) = f.paired_to {
        return busy(BusyReason::Paired(owner));
    }
    if f.reserved {
        return busy(BusyReason::Reserved);
    }
    SlotState::Idle
}

impl SlotState {
    /// Whether this slot can be handed to a new tenant.
    pub fn free(&self) -> bool {
        matches!(self, SlotState::Idle | SlotState::Cold)
    }

    /// The branch the checkout holds, if any — `None` for a cold or detached slot.
    pub fn branch(&self) -> Option<&str> {
        match self {
            SlotState::Busy { branch, .. } => branch.as_deref(),
            _ => None,
        }
    }
}

/// Which slot an acquisition should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// This slot already holds the branch. Reuse it: the idempotent same-branch hit, and the
    /// warmest possible one — the checkout is already on the branch, so nothing is switched.
    Reuse(u32),
    /// Take this free slot and switch the branch into it. A [`SlotState::Cold`] slot is built first.
    Take(u32),
    /// Every worker slot is busy.
    Full,
}

/// Pick a worker slot for `branch` given every slot's state, keyed by slot number.
///
/// Reuse beats take, so re-dispatching a branch lands back in its own checkout instead of opening a
/// second one on it — including when that checkout has gone quiet, where the old ledger drew the
/// line between "already running" and "ran here once" and wedged a repo whenever it guessed wrong.
/// Here the branch is either checked out somewhere or it isn't, and either way the answer is the
/// same one the next call will give.
///
/// Slot [`crate::agent::MAIN_POOL_SLOT`] is never picked: it is the reserved main slot.
pub fn pick_for(branch: &str, states: &BTreeMap<u32, SlotState>, size: usize) -> Pick {
    let mut workers = crate::agent::MAIN_POOL_SLOT + 1..=size as u32;
    if let Some(n) = workers
        .clone()
        .find(|n| states.get(n).and_then(SlotState::branch) == Some(branch))
    {
        return Pick::Reuse(n);
    }
    // A slot missing from `states` has never been created — free, and built on demand.
    match workers.find(|n| states.get(n).is_none_or(SlotState::free)) {
        Some(n) => Pick::Take(n),
        None => Pick::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(branch: Option<&str>) -> SlotFacts {
        SlotFacts {
            live: true,
            branch: branch.map(str::to_string),
            ..SlotFacts::default()
        }
    }

    #[test]
    fn a_detached_empty_checkout_is_idle() {
        assert_eq!(slot_state(&live(None)), SlotState::Idle);
    }

    #[test]
    fn a_missing_checkout_is_cold_and_still_free() {
        assert_eq!(slot_state(&SlotFacts::default()), SlotState::Cold);
        assert!(SlotState::Cold.free());
    }

    #[test]
    fn a_branch_nobody_is_in_and_has_not_dirtied_is_free() {
        // The case the old ledger got wrong for eight hours: an agent finished, the checkout is
        // clean, nothing is running. It is free — and `pick_for` still reuses it for that branch.
        assert_eq!(slot_state(&live(Some("feat"))), SlotState::Idle);
    }

    #[test]
    fn an_occupant_outranks_every_other_reason() {
        let mut f = live(Some("feat"));
        f.occupants = vec!["feat".to_string()];
        f.dirty = 3;
        f.reserved = true;
        assert_eq!(
            slot_state(&f),
            SlotState::Busy {
                branch: Some("feat".to_string()),
                why: BusyReason::Occupied("feat".to_string()),
            }
        );
    }

    #[test]
    fn uncommitted_work_holds_a_slot_with_nobody_in_it() {
        let mut f = live(Some("feat"));
        f.dirty = 2;
        assert!(!slot_state(&f).free());
        assert_eq!(
            slot_state(&f),
            SlotState::Busy {
                branch: Some("feat".to_string()),
                why: BusyReason::Dirty(2),
            }
        );
    }

    #[test]
    fn a_comparison_half_is_held_by_its_owner_not_by_anything_in_it() {
        let mut f = live(None);
        f.paired_to = Some(3);
        assert_eq!(
            slot_state(&f),
            SlotState::Busy {
                branch: None,
                why: BusyReason::Paired(3),
            }
        );
    }

    #[test]
    fn a_reservation_holds_a_slot_nothing_has_appeared_in_yet() {
        // The one fact that cannot be observed: the gap between a dispatch taking a slot and its
        // agent showing up in tmux.
        let mut f = live(Some("feat"));
        f.reserved = true;
        assert_eq!(
            slot_state(&f),
            SlotState::Busy {
                branch: Some("feat".to_string()),
                why: BusyReason::Reserved,
            }
        );
    }

    fn busy_on(branch: &str) -> SlotState {
        SlotState::Busy {
            branch: Some(branch.to_string()),
            why: BusyReason::Occupied(branch.to_string()),
        }
    }

    #[test]
    fn a_branch_already_checked_out_is_reused_wherever_it_sits() {
        let states = BTreeMap::from([(2, SlotState::Idle), (3, busy_on("feat"))]);
        assert_eq!(pick_for("feat", &states, 4), Pick::Reuse(3));
    }

    #[test]
    fn a_quiet_slot_still_on_the_branch_is_reused_rather_than_switched() {
        // Reuse is decided from the checkout, not from whether anyone is home — so a re-dispatch
        // lands back in its own warm checkout instead of switching the branch into a fresh slot
        // and leaving two checkouts on it.
        let states = BTreeMap::from([
            (
                2,
                SlotState::Busy {
                    branch: Some("feat".to_string()),
                    why: BusyReason::Dirty(1),
                },
            ),
            (3, SlotState::Idle),
        ]);
        assert_eq!(pick_for("feat", &states, 4), Pick::Reuse(2));
    }

    #[test]
    fn otherwise_the_lowest_free_worker_is_taken() {
        let states = BTreeMap::from([(2, busy_on("other")), (3, SlotState::Idle)]);
        assert_eq!(pick_for("feat", &states, 4), Pick::Take(3));
    }

    #[test]
    fn a_slot_that_was_never_built_is_free() {
        // Nothing on disk yet is the emptiest a slot can be; the caller creates it.
        assert_eq!(pick_for("feat", &BTreeMap::new(), 4), Pick::Take(2));
    }

    #[test]
    fn the_main_slot_is_never_picked_for_a_worker() {
        // Free and lowest-numbered, and still not a candidate: slot 1 is the reserved main slot.
        let states = BTreeMap::from([(1, SlotState::Idle), (2, busy_on("other"))]);
        assert_eq!(pick_for("feat", &states, 2), Pick::Full);

        // …not even when it is the slot holding the very branch being asked for. Slot 2 has never
        // been built, so building it is the right answer — reusing slot 1 would not be.
        let holding = BTreeMap::from([(1, busy_on("feat"))]);
        assert_eq!(pick_for("feat", &holding, 2), Pick::Take(2));
    }

    #[test]
    fn every_worker_busy_is_a_full_pool() {
        let states = BTreeMap::from([(2, busy_on("a")), (3, busy_on("b"))]);
        assert_eq!(pick_for("c", &states, 3), Pick::Full);
    }
}
