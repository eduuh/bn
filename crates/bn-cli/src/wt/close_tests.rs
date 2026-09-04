//! Pure decision tests for `bn wt close` — no tmux, no git. Facts are constructed directly, so
//! every branch of the `prefix x` decision is pinned without a server or a checkout.

use super::*;

/// A clean, conventional (non-pool) managed worktree — the baseline each test perturbs.
fn facts() -> CloseFacts {
    CloseFacts {
        managed: true,
        reserved: false,
        slot: None,
        branch: "feat-auth".to_string(),
        dirty: 0,
        unpushed: 0,
        occupant: None,
        confirmed: false,
    }
}

#[test]
fn an_unmanaged_pane_just_closes() {
    let f = CloseFacts {
        managed: false,
        ..facts()
    };
    assert!(matches!(close_plan(&f), ClosePlan::KillPaneOnly(_)));
}

#[test]
fn the_main_worktree_is_never_reclaimed() {
    let f = CloseFacts {
        reserved: true,
        branch: "main".to_string(),
        ..facts()
    };
    assert!(matches!(close_plan(&f), ClosePlan::KillPaneOnly(_)));
}

#[test]
fn the_controller_slot_is_never_released() {
    let f = CloseFacts {
        slot: Some(bn_core::agent::MAIN_POOL_SLOT),
        ..facts()
    };
    assert!(
        matches!(close_plan(&f), ClosePlan::KillPaneOnly(_)),
        "prefix x must not become a quieter way past the orchestrator guard"
    );
}

#[test]
fn a_clean_pool_slot_hands_its_lease_back() {
    let f = CloseFacts {
        slot: Some(3),
        ..facts()
    };
    assert_eq!(
        close_plan(&f),
        ClosePlan::ReleaseSlot {
            slot: 3,
            discard: false
        }
    );
}

#[test]
fn a_dirty_slot_asks_first_then_discards() {
    let dirty = CloseFacts {
        slot: Some(3),
        dirty: 2,
        ..facts()
    };
    assert_eq!(
        close_plan(&dirty),
        ClosePlan::Confirm {
            branch: "feat-auth".to_string(),
            dirty: 2,
            unpushed: 0,
            occupant: None,
        }
    );
    let confirmed = CloseFacts {
        confirmed: true,
        ..dirty
    };
    assert_eq!(
        close_plan(&confirmed),
        ClosePlan::ReleaseSlot {
            slot: 3,
            discard: true
        }
    );
}

#[test]
fn unpushed_commits_ask_even_when_the_tree_is_clean() {
    let f = CloseFacts {
        slot: Some(3),
        dirty: 0,
        unpushed: 2,
        ..facts()
    };
    assert!(
        matches!(close_plan(&f), ClosePlan::Confirm { unpushed: 2, .. }),
        "commits that exist only here are work too"
    );
}

#[test]
fn confirming_a_conventional_worktree_never_forces() {
    // The guard that stops a future refactor turning prefix x into a branch-deleter: outside a
    // reusable pool slot, a confirmation buys no extra power — cleanup still runs its own gates.
    let f = CloseFacts {
        slot: None,
        dirty: 4,
        unpushed: 1,
        confirmed: true,
        ..facts()
    };
    assert_eq!(close_plan(&f), ClosePlan::CleanupWorktree);
}

#[test]
fn a_slot_with_a_live_agent_asks_even_when_clean() {
    // The kill this pins: an agent mid-prompt has a clean tree, so the dirty/unpushed gates said
    // nothing and `prefix x` took its windows on the keystroke. A live occupant is work in flight.
    let occupied = CloseFacts {
        slot: Some(4),
        occupant: Some("claude agent 46ea27bd in bn:claude-priority-list".to_string()),
        ..facts()
    };
    assert!(
        matches!(close_plan(&occupied), ClosePlan::Confirm { occupant: Some(_), .. }),
        "closing over a live agent must prompt, never kill silently"
    );
    assert!(needs_confirm(&close_plan(&occupied)));
    // Confirmed, it proceeds — the human saw the agent named in the prompt and said yes.
    let confirmed = CloseFacts {
        confirmed: true,
        ..occupied
    };
    assert_eq!(
        close_plan(&confirmed),
        ClosePlan::ReleaseSlot {
            slot: 4,
            discard: true
        }
    );
}

#[test]
fn check_is_exactly_the_confirm_branch() {
    // tmux.conf branches on this exit status; if the two definitions drifted, the prompt would
    // appear for the wrong cases.
    let clean = CloseFacts {
        slot: Some(3),
        ..facts()
    };
    let dirty = CloseFacts { dirty: 1, ..clean.clone() };
    assert!(!needs_confirm(&close_plan(&clean)));
    assert!(needs_confirm(&close_plan(&dirty)));
    assert!(!needs_confirm(&close_plan(&CloseFacts {
        managed: false,
        ..dirty
    })));
}
