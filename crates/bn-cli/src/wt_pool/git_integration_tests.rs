//! Pool tests that drive real git.
//!
//! Every test here holds [`bn_core::test_support::env_lock`], because they all reach production
//! code that resolves `git` from `$PATH` — and a sibling test (`cmd::task_workflow`) legitimately
//! points `PATH` at an empty temp directory to prove a harness is found via the job-path prelude
//! rather than the ambient path. Spawning `git` by bare name during that window is an *unlocked
//! read* of the same global: the spawn fails and the assertion reports something unrelated, like
//! `add_detached_slot` returning false.
//!
//! That is why these failed only as part of the full parallel run and always passed alone. Taking
//! the lock costs about two seconds of wall time across the suite and buys a deterministic result.
//! The local [`git`] helper additionally resolves an absolute binary, which covers the spawns this
//! file makes itself; production correctly keeps using `$PATH`, so the lock is the only cover for
//! the rest.

use super::*;
use std::process::Command;

/// `git`, resolved to an absolute path that does not depend on `$PATH`.
///
/// These tests spawn real git, and a sibling test (`cmd::task_workflow`) legitimately points `PATH`
/// at a temp directory while holding `env_lock`. Spawning by the bare name `git` is an *unlocked
/// read* of that same global: during the sibling's window there is no git on `PATH`, the spawn
/// fails, and `git init` comes back false — which is how seven of these tests failed only ever as
/// part of the full parallel run, and always passed alone.
///
/// Resolving from a fixed candidate list rather than `PATH` removes the shared mutable state
/// instead of serialising against it, so these keep running in parallel. Falls back to the bare
/// name so a host with git somewhere unusual still works.
pub(super) fn git_bin() -> &'static str {
    static BIN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        [
            "/usr/bin/git",
            "/bin/git",
            "/usr/local/bin/git",
            "/opt/homebrew/bin/git",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("git")
    })
}

pub(super) fn git(args: &[&str]) -> bool {
    Command::new(git_bin())
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build a one-commit repo on `main` (with a `feat` branch and a committed `.gitignore`) under
/// `srcdir`, then bare-clone it to `bare`. Shared by the tempdir and staged-layout setups.
pub(super) fn build_repo_into(srcdir: &Path, bare: &Path) {
    std::fs::create_dir_all(srcdir).unwrap();
    let s = srcdir.to_string_lossy().to_string();
    assert!(git(&["init", "-q", &s]));
    assert!(git(&["-C", &s, "config", "user.email", "t@t"]));
    assert!(git(&["-C", &s, "config", "user.name", "t"]));
    std::fs::write(srcdir.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(srcdir.join("a.txt"), "hi\n").unwrap();
    assert!(git(&["-C", &s, "add", "-A"]));
    assert!(git(&["-C", &s, "commit", "-qm", "init"]));
    // Normalize the default branch name across git versions.
    assert!(git(&["-C", &s, "branch", "-M", "main"]));
    assert!(git(&["-C", &s, "branch", "feat"]));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    assert!(git(&["clone", "-q", "--bare", &s, &bare.to_string_lossy()]));
}

/// A bare clone of a one-commit repo on `main`, with a second branch `feat` and a committed
/// `.gitignore` (`target/`). Returns `(tempdir, bare_path_string)`.
pub(super) fn mk_bare() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare.git");
    build_repo_into(&tmp.path().join("src"), &bare);
    (tmp, bare.to_string_lossy().to_string())
}

pub(super) fn head_ref(slot: &Path) -> String {
    let out = Command::new("git")
        .args([
            "-C",
            &slot.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn default_base_prefers_main() {
    let _env = bn_core::test_support::env_lock();
    let (_tmp, bare) = mk_bare();
    // A bare clone copies heads (refs/heads/main) but sets up no origin remote, so `main`
    // resolves and `origin/main` does not — default_base must land on `main`.
    assert_eq!(default_base(&bare), "main");
}

#[test]
fn add_detached_slot_creates_a_live_worktree() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(is_live_slot(&slot));
    // Detached at the base — abbrev-ref reads "HEAD".
    assert_eq!(head_ref(&slot), "HEAD");
    assert!(slot.join("a.txt").exists());
}

#[test]
fn switch_reuses_an_existing_branch() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
    assert_eq!(head_ref(&slot), "feat");
}

#[test]
fn switch_creates_a_new_branch_when_absent() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(
        &slot.to_string_lossy(),
        &bare,
        "brand-new"
    ));
    assert_eq!(head_ref(&slot), "brand-new");
}

#[test]
fn switch_fails_when_branch_checked_out_elsewhere() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    // `feat` is checked out in another worktree; git refuses to check it out a second time.
    let other = tmp.path().join("other");
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "add",
        &other.to_string_lossy(),
        "feat"
    ]));
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(!switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
}

#[test]
fn is_live_slot_rejects_a_bare_directory() {
    let _env = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("not-a-worktree");
    std::fs::create_dir_all(&dir).unwrap();
    // A plain dir (an interrupted `worktree add` leaves files but no `.git`) is not reusable.
    assert!(!is_live_slot(&dir));
    assert!(!is_live_slot(&tmp.path().join("missing")));
}

#[test]
fn force_remove_tears_down_a_slot() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(slot.is_dir());
    force_remove_worktree(&bare, &slot);
    assert!(!slot.exists());
    // The worktree registration is gone too, so the slot can be cleanly recreated.
    let list = Command::new("git")
        .args(["--git-dir", &bare, "worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&list.stdout).contains("slot2"));
}

#[test]
fn force_remove_recovers_a_broken_checkout() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    // Simulate an interrupted create: drop the `.git` link so it reads as broken.
    std::fs::remove_file(slot.join(".git")).unwrap();
    assert!(!is_live_slot(&slot));
    force_remove_worktree(&bare, &slot);
    // The recreate path (used by both get and warm) then rebuilds a live slot.
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(is_live_slot(&slot));
}

#[test]
fn warm_reset_discards_tracked_but_keeps_ignored_deps() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
    // A tracked edit, an untracked scratch file, and an ignored build artifact.
    std::fs::write(slot.join("a.txt"), "DIRTY\n").unwrap();
    std::fs::write(slot.join("untracked.txt"), "scratch\n").unwrap();
    std::fs::create_dir_all(slot.join("target")).unwrap();
    std::fs::write(slot.join("target/build-cache"), "expensive\n").unwrap();

    warm_reset(&slot.to_string_lossy(), &bare);

    // Tracked change reverted, untracked scratch cleaned…
    assert_eq!(std::fs::read_to_string(slot.join("a.txt")).unwrap(), "hi\n");
    assert!(!slot.join("untracked.txt").exists());
    // …but the ignored deps/build cache — the entire point of the pool — survive.
    assert!(slot.join("target/build-cache").exists());
    // And the slot is detached (ready for the controller / next lease).
    assert_eq!(head_ref(&slot), "HEAD");
}

#[test]
fn direct_open_leases_a_configured_warm_slot_and_never_spills_cold() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("src"), &bare);
    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let pool_dir = root.join("worktree").join("bn").join("_pool");
    let slot = pool_dir.join("2");
    assert!(add_detached_slot(
        &bare.to_string_lossy(),
        &slot.to_string_lossy()
    ));
    let cfg = bn_core::test_support::config(root);

    match lease_for_direct_open(&cfg, "bn", "feat") {
        DirectLease::Leased(path) => assert_eq!(path, slot),
        _ => panic!("a direct open should lease the warm worker slot"),
    }
    assert_eq!(head_ref(&slot), "feat");
    assert!(!root.join("worktree").join("bn").join("feat").exists());

    // A second branch does NOT spill to a cold worktree beside the pool — it takes another slot.
    // Slots 2..=4 exist by derivation (the floor), so this is an ordinary pick, not growth.
    match lease_for_direct_open(&cfg, "bn", "second") {
        DirectLease::Leased(path) => assert_eq!(path, pool_dir.join("3")),
        _ => panic!("a direct open should lease a second slot rather than cut a cold worktree"),
    }
    assert!(!root.join("worktree").join("bn").join("second").exists());
}

/// The pool grows to fit demand instead of refusing, and stops at the ceiling. With the config
/// gone this is the only thing that bounds a pool, so it has to hold at both ends.
#[test]
fn a_full_pool_grows_by_one_until_it_hits_the_ceiling() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("src"), &bare);
    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));
    let cfg = bn_core::test_support::config(root);
    let pool_dir = root.join("worktree").join("bn").join("_pool");

    // Slot 1 is the controller's, so the workers are 2..=MAX — every one of them leasable.
    let max = bn_core::pool::MAX_POOL_SIZE;
    for n in 2..=max {
        match lease_for_direct_open(&cfg, "bn", &format!("b{n}")) {
            DirectLease::Leased(path) => assert_eq!(path, pool_dir.join(n.to_string())),
            _ => panic!("slot {n} should have been leased (grown into if need be)"),
        }
        assert_eq!(bn_core::pool::pool_size(&cfg, "bn"), n.max(4));
    }

    // At the ceiling, full is genuinely full — growth is not an escape hatch from capacity.
    assert!(matches!(
        lease_for_direct_open(&cfg, "bn", "one-too-many"),
        DirectLease::Unavailable
    ));
    assert_eq!(bn_core::pool::pool_size(&cfg, "bn"), max);
}

/// With no config there are only two ways a direct open fails, and "no clone" is one of them.
#[test]
fn direct_open_reports_a_missing_repo() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", tmp.path());
    let cfg = bn_core::test_support::config(tmp.path());

    assert!(matches!(
        lease_for_direct_open(&cfg, "missing", "feat"),
        DirectLease::MissingRepo
    ));
}

#[test]
fn wt_cleanup_inside_pool_slot_returns_the_slot() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let pdir = root.join("worktree").join("bn").join("_pool");
    let slot = pdir.join("2");
    std::fs::create_dir_all(&pdir).unwrap();
    assert!(git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "add",
        "-q",
        &slot.to_string_lossy(),
        "main"
    ]));
    assert!(switch_into_slot(
        &slot.to_string_lossy(),
        &bare.to_string_lossy(),
        "feat"
    ));
    let nested = slot.join("nested");
    std::fs::create_dir_all(&nested).unwrap();

    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "test".into(),
            pid: std::process::id(),
            at: now_epoch(),
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let cfg = bn_core::test_support::config(root);
    assert!(crate::wt::cleanup_one(&cfg, &nested, &root.join("worktree"), false).is_ok());

    assert!(slot.is_dir(), "cleanup should keep the warm slot checkout");
    assert!(read_leases(&pdir).slots.is_empty());
    assert_eq!(head_ref(&slot), "HEAD");
}
