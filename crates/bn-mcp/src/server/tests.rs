use super::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn worktree(root: &Path, repo: &str, branch: &str) -> PathBuf {
    let dir = root.join(repo).join(branch);
    std::fs::create_dir_all(&dir).expect("create fixture worktree");
    git(&dir, &["init", "--quiet", "--initial-branch", branch]);
    git(
        &dir,
        &[
            "-c",
            "user.name=bn-test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    dir
}

#[test]
fn each_server_resolves_only_its_launch_worktree() {
    let root = tempfile::tempdir().expect("tempdir");
    let worktrees = root.path().join("worktrees");
    let cfg = Config {
        worktree_dir: worktrees.clone(),
        ..bn_core::test_support::config(root.path())
    };
    let first = BnServer::new(cfg.clone(), worktree(&worktrees, "first", "topic"), vec![]);
    let second = BnServer::new(cfg, worktree(&worktrees, "second", "main"), vec![]);

    let here = first.resolve().expect("first scope");
    assert_eq!((&*here.repo, &*here.branch), ("first", "topic"));
    let there = second.resolve().expect("second scope");
    assert_eq!((&*there.repo, &*there.branch), ("second", "main"));

    git(&first.worktree, &["checkout", "--quiet", "-b", "next"]);
    assert_eq!(first.resolve().expect("updated branch").branch, "next");
    assert_eq!(second.resolve().expect("unchanged scope").branch, "main");
}

#[test]
fn missing_worktree_and_detached_head_fail_closed() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = bn_core::test_support::config(root.path());
    let missing = BnServer::new(cfg.clone(), root.path().to_path_buf(), vec![]);
    assert!(missing.resolve().is_err());

    let dir = worktree(&root.path().join("worktrees"), "repo", "main");
    git(&dir, &["checkout", "--quiet", "--detach"]);
    let detached = BnServer::new(cfg, dir, vec![]);
    assert!(detached.resolve().is_err());
}
