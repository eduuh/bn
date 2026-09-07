//! May a dispatch take over a pool slot that is already leased to its branch?
//!
//! The lease returns the same-branch slot with `fresh = false` when someone already holds it.
//! Two callers may go ahead anyway, and the lease below then evicts the prior occupant:
//!
//! * `--fresh` recovery, which is a person saying so.
//! * A slot whose only occupants have reported done — a Copilot sitting at its prompt after
//!   `bn agent done`, its window still open. That is not an occupant in any sense that should stop
//!   the next stage of the same task from starting here; it is the slot waiting to be taken over.
//!   The queue refused this for hours on end, backing off toward the same answer, while the
//!   pipeline's own next agent for that branch was the thing being refused.
//!
//! An unstamped pane — a shell someone has open — still refuses.
use std::path::Path;

/// The cold path's rule, and the plain half of [`verdict`]: an occupied worktree is reusable only
/// for explicit `--fresh` recovery of an agent launch.
pub(super) fn may_reuse_occupied_worktree(fresh: bool, launching_agent: bool) -> bool {
    fresh && launching_agent
}

/// `Some(replace_prior)` to proceed, `None` to refuse. `replace_prior` is what the agent lease
/// needs to know: whether to evict whoever is there.
pub(super) fn verdict(fresh: bool, launching_agent: bool, slot: &Path, branch: &str) -> Option<bool> {
    if !launching_agent {
        return None;
    }
    if fresh {
        return Some(true);
    }
    if crate::agent_lease::only_finished_agents_in(slot) {
        eprintln!(
            "bn dispatch: reusing pooled worktree {} — its previous agent on '{branch}' has finished",
            slot.display()
        );
        return Some(true);
    }
    None
}

/// The refusal, worded so the person can act on it.
pub(super) fn refusal(slot: &Path, branch: &str) -> String {
    format!(
        "bn dispatch: refusing to reuse occupied pooled worktree {} for branch '{branch}' — choose \
         another --branch or omit --branch to allocate a unique dispatch branch",
        slot.display()
    )
}
