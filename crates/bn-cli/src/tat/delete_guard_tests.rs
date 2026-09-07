//! The `ctrl-d` guard, pinned: every refusal has a test rather than a hope.
use super::delete_block;

/// The `worktree/<repo>/main` checkout is not a row you can pick, but the `rmworktree` stage is
/// reachable directly — and it once removed exactly that checkout when asked. Never again.
#[test]
fn the_main_checkout_is_never_removable() {
    for name in ["main", "master", "Main", " main "] {
        let err = delete_block("worktree", name, true, 0, 0).unwrap_err();
        assert!(err.contains("main checkout"), "{name:?}: {err}");
    }
}

#[test]
fn a_clean_pushed_worktree_may_go() {
    assert_eq!(delete_block("worktree", "feature-x", true, 0, 0), Ok(()));
}

#[test]
fn the_other_refusals_still_stand() {
    assert!(delete_block("window", "1", true, 0, 0)
        .unwrap_err()
        .contains("window row"));
    assert!(delete_block("", "x", true, 0, 0)
        .unwrap_err()
        .contains("that row"));
    assert!(delete_block("worktree", "x", false, 0, 0)
        .unwrap_err()
        .contains("already gone"));
    assert!(delete_block("worktree", "x", true, 2, 0)
        .unwrap_err()
        .contains("2 uncommitted changes"));
    assert!(delete_block("worktree", "x", true, 0, 1)
        .unwrap_err()
        .contains("1 unpushed commit "));
}
