//! Pool **release** integration tests: `wt-cleanup` handing a merged slot back. Split out of
//! `pool_acquire_tests` at the 1000-line ratchet; the git helpers live in `git_integration_tests`.

use super::super::git_integration_tests::build_repo_into;
use super::super::*;

/// The `wt-cleanup` sweep's merged-slot release: a stray uncommitted file must not pin the slot
/// forever, and must not be discarded either. It lands as a commit on the slot's branch, which
/// outlives the reset in the bare repo.
#[test]
fn release_merged_slot_rescues_dirty_work_onto_the_branch_then_returns() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let cfg = bn_core::test_support::config(root);
    let ctx = PoolCtx {
        bare: bare.clone(),
        repo: "bn".to_string(),
        pool_dir: root.join("worktree").join("bn").join("_pool"),
        worktree_dir: root.join("worktree"),
        size: 2,
    };
    let leased =
        acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false).expect("a free slot");
    std::fs::write(leased.slot.join("leftover.txt"), "keep me\n").unwrap();

    assert!(matches!(
        crate::wt_pool::release_slot(&cfg, &leased.slot, false),
        SlotRelease::Dirty { .. }
    ));
    assert!(matches!(
        crate::wt_pool::release_merged_slot(&cfg, &leased.slot),
        SlotRelease::Returned { .. }
    ));
    assert!(read_leases(&ctx.pool_dir).slots.is_empty());
    assert!(
        !leased.slot.join("leftover.txt").exists(),
        "the checkout is reset"
    );
    assert_eq!(
        wt::git_out(&[
            "--git-dir",
            &bare.to_string_lossy(),
            "show",
            "feat:leftover.txt"
        ])
        .as_deref(),
        Some("keep me"),
        "the rescued file lives on the branch"
    );
}
