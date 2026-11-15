use super::*;
use std::process::Command;

#[test]
fn consolidated_skills_install_short_entries_and_keep_runtime_contracts() {
    let _lock = crate::test_support::env_lock();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = tempfile::tempdir().unwrap();
    let _shared =
        crate::test_support::EnvGuard::set("BN_MCP_SKILLS_DIR", root.join("config/prompts"));
    let _personal = crate::test_support::EnvGuard::set(
        "BN_MCP_PERSONAL_SKILLS_DIR",
        temp.path().join("personal"),
    );
    let cfg = Config::default();
    let stale = temp.path().join(".copilot/skills/execute/SKILL.md");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "<!-- bn-managed-skill: execute -->\nold workflow").unwrap();

    install_native_skills(
        &cfg,
        &root,
        temp.path(),
        NativeScope::Global,
        &[NativeHarness::Copilot, NativeHarness::Claude],
    )
    .unwrap();

    assert!(!stale.exists());
    let discovered = discover(&cfg, &root).unwrap();
    assert_eq!(
        rank(&discovered, "implement a small feature")[0].name,
        "ship-solo"
    );
    for name in [
        "implement",
        "execute",
        "investigate-and-plan",
        "subagent-orchestration",
    ] {
        assert!(!discovered.iter().any(|skill| skill.name == name));
        let skill = load_skill(&cfg, &root, name).unwrap().unwrap();
        assert!(!skill.render_checked(None).unwrap().is_empty());
        assert!(!temp
            .path()
            .join(format!(".claude/skills/{name}/SKILL.md"))
            .exists());
    }
    for (name, reference) in [
        ("coding-principles", "coding-principles-reference"),
        ("testing-principles", "testing-principles-reference"),
        ("coordinate-agents", "coordinate-agents-reference"),
        ("ship-solo", "ship-solo-workflow"),
        ("build-from-plan", "build-from-plan-workflow"),
        ("plan", "plan-workflow"),
        ("stow", "stow-workflow"),
        ("write-like-me", "write-like-me-workflow"),
    ] {
        let entry = load_skill(&cfg, &root, name).unwrap().unwrap();
        let detail = load_skill(&cfg, &root, reference).unwrap().unwrap();
        assert!(detail.internal);
        assert!(entry
            .body
            .contains(&format!("bn skills render {reference}")));
        assert!(!detail.render_checked(None).unwrap().is_empty());
        for harness in [".copilot", ".claude"] {
            let native = temp
                .path()
                .join(format!("{harness}/skills/{name}/SKILL.md"));
            let content = std::fs::read_to_string(native).unwrap();
            assert!(content.contains(&format!("bn skills render {reference}")));
            assert!(!temp
                .path()
                .join(format!("{harness}/skills/{reference}/SKILL.md"))
                .exists());
        }
    }
}

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
    let root = std::fs::canonicalize(&root).unwrap();
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
