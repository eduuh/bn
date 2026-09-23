use super::*;
use std::path::Path;
use std::process::Command;

fn g(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// An origin on `master`, its bare clone, and that branch checked out in a dir named `main`.
fn master_in_main() -> (tempfile::TempDir, std::path::PathBuf, Config) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    g(&src, &["init", "-q"]);
    std::fs::write(src.join("f"), "one\n").unwrap();
    g(&src, &["add", "f"]);
    g(&src, &["commit", "-qm", "one"]);
    let bare = tmp.path().join("repo.git");
    g(tmp.path(), &["clone", "-q", "--bare", "src", "repo.git"]);
    let wt = tmp.path().join("wt");
    let main = wt.join("repo").join("main");
    g(tmp.path(), &["--git-dir", "repo.git", "worktree", "add", "-q", &main.to_string_lossy(), "master"]);
    let cfg = Config {
        worktree_dir: wt,
        ..Config::default()
    };
    (tmp, bare, cfg)
}

#[test]
fn a_default_branch_checked_out_under_another_dir_name_is_reused() {
    let _env = bn_core::test_support::env_lock();
    let (_tmp, bare, cfg) = master_in_main();

    let base = ensure_dispatch_base(&cfg, "repo", &bare, None).expect("reuses repo/main");

    assert_eq!(base.branch, "master");
    assert!(!base.bootstrapped);
    assert!(!cfg.worktree_dir.join("repo").join("master").exists());
}

#[test]
fn a_dirty_base_checkout_is_left_alone_and_does_not_block_dispatch() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare, cfg) = master_in_main();
    let src = tmp.path().join("src");
    std::fs::write(src.join("f"), "two\n").unwrap();
    g(&src, &["commit", "-qam", "two"]);
    let main = cfg.worktree_dir.join("repo").join("main");
    std::fs::write(main.join("f"), "my local edit\n").unwrap();

    let base = ensure_dispatch_base(&cfg, "repo", &bare, None).expect("dirty base is advice");

    assert!(base.stale_checkout.is_some());
    assert_eq!(std::fs::read_to_string(main.join("f")).unwrap(), "my local edit\n");
    let msg = dispatch_base_bootstrap_message("repo", &base).unwrap();
    assert!(msg.contains("forking from origin/master"), "{msg}");
}
