use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn detached_checkout_keeps_repository_skill_tiers_and_local_precedence() {
    let _lock = crate::test_support::env_lock();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    git(&root, &["checkout", "--quiet", "--detach"]);
    let shared = root.join(".bn/skills");
    let local = root.join(".bn/skills.local");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(
        shared.join("detached-fixture.md"),
        "---\ndescription: Shared fixture\n---\nshared body\n",
    )
    .unwrap();
    std::fs::write(
        local.join("detached-fixture.md"),
        "---\ndescription: Local fixture\n---\nlocal body\n",
    )
    .unwrap();
    let cfg = crate::test_support::config_with_roots(
        temp.path(),
        temp.path(),
        temp.path().join("worktrees"),
        "testhost",
    );

    let dirs = skill_dirs(&cfg, &root);
    assert!(dirs.contains(&(SkillScope::RepoShared, shared)));
    assert!(dirs.contains(&(SkillScope::RepoPersonalLocal, local)));
    let skill = load_skill(&cfg, &root, "detached-fixture")
        .unwrap()
        .expect("repository skill in detached checkout");
    assert_eq!(skill.render_checked(None).unwrap().trim(), "local body");
}
