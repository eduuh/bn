use super::*;
use bn_core::test_support::{env_lock, EnvGuard};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) fn fixture() -> (tempfile::TempDir, Config, Vec<EnvGuard>) {
    let dir = tempfile::Builder::new()
        .prefix(".discover-test-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    // Stop git walking into the enclosing checkout without changing process-global ceilings.
    std::fs::write(dir.path().join(".git"), "not a git repository\n").unwrap();
    let shared = dir.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let env = vec![
        EnvGuard::set("BN_MCP_SKILLS_DIR", &shared),
        EnvGuard::set("BN_MCP_PERSONAL_SKILLS_DIR", dir.path().join("personal")),
        EnvGuard::set("HOME", dir.path()),
    ];
    for (name, description, keywords) in [
        ("debug", "Reproduce and fix a crash", "crash, regression"),
        ("blog-polish", "Polish a blog post", "blog, prose"),
        (
            "browser-test",
            "Test a browser application",
            "browser, screenshot",
        ),
        ("build", "Build the application", "build, application"),
        ("explore", "Explore an application", "application"),
    ] {
        std::fs::write(
            shared.join(format!("{name}.md")),
            format!(
                "---\ndescription: {description}\nkeywords: [{keywords}]\n---\nBody not for discovery.\n"
            ),
        )
        .unwrap();
    }
    let cfg = bn_core::test_support::config(&dir.path().join("notes"));
    (dir, cfg, env)
}

pub(super) fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.insert(path.clone(), Vec::new());
                walk(&path, result);
            } else {
                result.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, &mut result);
    result
}

pub(super) fn repo(root: &Path) -> PathBuf {
    let repo = root.join("project");
    std::fs::create_dir(&repo).unwrap();
    let out = std::process::Command::new("git")
        .args(["init", "-q", "--initial-branch=feature"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    repo
}

#[test]
fn discover_without_task_or_repo_is_safe_and_does_not_list_workflows() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let before = snapshot(dir.path());
    let data = discover(&cfg, &outside, "  ").unwrap();
    assert!(data["context"].is_null());
    assert_eq!(data["task"], "");
    assert_eq!(data["workflows"], json!([]));
    assert_eq!(data["workflows_filtered"], false);
    assert_eq!(data["commands"], json!(["bn discover '<task>'"]));
    assert!(render(&data).contains("No branch context"));
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn discover_task_selects_relevant_effective_workflows_and_real_commands() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let repo = repo(dir.path());
    let local = repo.join(".bn/skills");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(
        local.join("debug.md"),
        "---\ndescription: Local crash workflow\nkeywords: [crash]\n---\nLocal body\n",
    )
    .unwrap();
    let before = snapshot(dir.path());
    let data = discover(&cfg, &repo, "fix a crash").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(data["workflows"][0]["name"], "debug");
    assert_eq!(data["workflows"][0]["scope"], "repo-shared");
    assert_eq!(
        data["workflows"][0]["read_command"],
        "bn skills render debug"
    );
    assert_eq!(data["workflows"][0]["invoke"]["copilot"], "/debug");
    let output = render(&data);
    assert!(output.contains("Local crash workflow"));
    assert!(!output.contains("blog-polish"));
    assert!(!output.contains("browser-test"));
    assert!(!output.contains("Local body"));
    assert_eq!(snapshot(dir.path()), before);
    assert!(!cfg.personal_root.exists());
    let empty = discover(&cfg, &repo, "").unwrap();
    assert_eq!(empty["context"]["note_exists"], false);
    assert_eq!(
        empty["context"]["counts"],
        json!({"open_todos": 0, "blockers": 0})
    );
    assert_eq!(empty["commands"][0], "bn goal '<goal>'");
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn discover_orients_existing_note_without_writing_or_guessing_from_goal() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let repo = repo(dir.path());
    let ctx = resolver::resolve(&cfg, &repo).unwrap();
    let note_path = cfg.note_file(&ctx);
    std::fs::create_dir_all(note_path.parent().unwrap()).unwrap();
    std::fs::write(
        &note_path,
        "---\nrepo: project\nbranch: feature\ncreated: 2026-10-08\nstatus: active\ntodos:\n\
         - {id: t1, text: Fix crash, created: 2026-10-08, done: false}\n\
         - {id: t2, text: Completed, created: 2026-10-08, done: true}\n\
         blockers:\n- {id: b1, text: Await access, created: 2026-10-08}\n\
         ---\n## Goal\n\nFix a crash\n",
    )
    .unwrap();
    let before = snapshot(dir.path());
    let data = discover(&cfg, &repo, "").unwrap();
    assert_eq!(data["context"]["note_exists"], true);
    assert_eq!(data["context"]["goal"], "Fix a crash");
    assert_eq!(data["context"]["counts"]["open_todos"], 1);
    assert_eq!(data["context"]["counts"]["blockers"], 1);
    assert_eq!(data["context"]["todos"][0]["id"], "t1");
    assert_eq!(data["context"]["blockers"][0]["id"], "b1");
    assert!(data["context"].get("help").is_none());
    assert_eq!(data["workflows"], json!([]));
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn discover_unknown_task_does_not_offer_irrelevant_guidance() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let data = discover(&cfg, &outside, "zyxwvu").unwrap();
    assert_eq!(data["workflows"], json!([]));
    assert_eq!(data["workflows_filtered"], false);
    assert_eq!(data["workflows_truncated"], false);
    assert_eq!(data["commands"], json!(["bn skills"]));
    assert!(!render(&data).contains("bn goal"));
    assert!(!render(&data).contains("bn --cat"));
}

#[test]
fn discover_task_results_and_text_are_bounded() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let data = discover(&cfg, &outside, "application").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 2);
    assert_eq!(data["workflows"][0]["name"], "build");
    assert_eq!(data["workflows"][1]["name"], "explore");
    assert_eq!(data["workflows_truncated"], false);
    for name in ["application-one", "application-two", "application-three"] {
        native_skill(dir.path(), ".agents/skills", name, "Work on an application");
    }
    let data = discover(&cfg, &outside, "application").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 3);
    assert_eq!(data["workflows_truncated"], true);
    assert_eq!(compact(&"é".repeat(200)).chars().count(), TEXT_LIMIT + 1);
    assert_eq!(compact("first\n  second"), "first second");
}

#[test]
fn discover_dominant_merge_conflict_match_excludes_derivative_workflows() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    for (name, description) in [
        ("resolving-merge-conflicts", "Handle merge conflicts"),
        ("dep-prs", "Resolve merge conflicts in dependency PRs"),
        (
            "mine-review-standards",
            "Review merged pull request standards",
        ),
    ] {
        native_skill(dir.path(), ".agents/skills", name, description);
    }
    let catalog = native::catalog(&cfg, &outside, dir.path()).unwrap();
    let infos = catalog.values().map(|s| s.info.clone()).collect::<Vec<_>>();
    let scores = bn_core::skills::rank(&infos, "resolve merge conflicts")
        .into_iter()
        .map(|s| (s.name, s.score.unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        scores,
        [
            ("resolving-merge-conflicts".to_owned(), 14),
            ("dep-prs".to_owned(), 6),
            ("mine-review-standards".to_owned(), 2),
        ]
    );
    let data = discover(&cfg, &outside, "resolve merge conflicts").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(data["workflows"][0]["name"], "resolving-merge-conflicts");
    assert_eq!(data["workflows"][0]["score"], 14);
    assert_eq!(data["workflows_filtered"], true);
    assert_eq!(data["workflows_truncated"], false);
    assert!(!render(&data).contains("dep-prs"));
    assert!(!render(&data).contains("mine-review-standards"));

    // A confidence exclusion is not a result-cap truncation, even with >3 raw matches.
    for name in ["reports", "history"] {
        native_skill(
            dir.path(),
            ".agents/skills",
            name,
            "Review merged pull requests",
        );
    }
    let data = discover(&cfg, &outside, "resolve merge conflicts").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(data["workflows_filtered"], true);
    assert_eq!(data["workflows_truncated"], false);
}

#[test]
fn discover_cli_accepts_multiword_task_and_json_anywhere() {
    use clap::{CommandFactory, FromArgMatches};
    for args in [
        vec!["bn", "--json", "discover", "fix", "a", "crash"],
        vec!["bn", "discover", "fix", "a", "crash", "--json"],
    ] {
        let cli =
            Cli::from_arg_matches(&Cli::command().try_get_matches_from(args).unwrap()).unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Some(Command::Coord(CmdCoord::Discover { task })) if task == ["fix", "a", "crash"]
        ));
    }
}

#[test]
fn discover_reports_an_unreadable_note_instead_of_claiming_it_is_missing() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let repo = repo(dir.path());
    let ctx = resolver::resolve(&cfg, &repo).unwrap();
    let note_path = cfg.note_file(&ctx);
    std::fs::create_dir_all(note_path.parent().unwrap()).unwrap();
    std::fs::write(&note_path, "---\ntodos: [invalid yaml\n---\n").unwrap();
    let before = snapshot(dir.path());
    assert!(discover(&cfg, &repo, "")
        .unwrap_err()
        .contains("cannot read branch note"));
    assert_eq!(snapshot(dir.path()), before);
}

fn native_skill(root: &Path, relative: &str, name: &str, description: &str) {
    let dir = root.join(relative).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {description}\narguments: [task]\n\
             ---\nNative {{{{example}}}}.\n"
        ),
    )
    .unwrap();
}

#[test]
fn discover_installed_upstream_skills_deduplicates_and_uses_native_invocations() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    for relative in [".agents/skills", ".copilot/skills", ".claude/skills"] {
        native_skill(
            dir.path(),
            relative,
            "resolving-merge-conflicts",
            "Resolve git merge conflicts",
        );
    }
    let before = snapshot(dir.path());
    let data = discover(&cfg, &outside, "resolve merge conflicts").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 1);
    let skill = &data["workflows"][0];
    assert_eq!(skill["name"], "resolving-merge-conflicts");
    assert_eq!(skill["invoke"]["copilot"], "/resolving-merge-conflicts");
    assert_eq!(skill["invoke"]["claude"], "/resolving-merge-conflicts");
    assert!(skill["read_command"]
        .as_str()
        .unwrap()
        .starts_with("cat -- "));
    assert!(!render(&data).contains("/bn:resolving"));
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn discover_owned_effective_source_wins_over_generated_native_copies() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    native_skill(
        dir.path(),
        ".copilot/skills",
        "debug",
        "Outdated generated crash copy",
    );
    native_skill(
        dir.path(),
        ".claude/skills",
        "debug",
        "Outdated generated crash copy",
    );
    let data = discover(&cfg, &outside, "crash").unwrap();
    assert_eq!(data["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["workflows"][0]["description"],
        "Reproduce and fix a crash"
    );
    assert_eq!(data["workflows"][0]["invoke"]["claude"], "/bn:debug");
    assert_eq!(
        data["workflows"][0]["read_command"],
        "bn skills render debug"
    );
}

#[test]
fn discover_agents_sources_are_available_in_copilot_not_assumed_in_claude() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    native_skill(
        dir.path(),
        ".agents/skills",
        "upstream",
        "Resolve git merge conflicts",
    );
    let data = discover(&cfg, &outside, "merge conflicts").unwrap();
    assert_eq!(
        data["workflows"][0]["invoke"],
        json!({"copilot": "/upstream", "claude": null})
    );
    assert!(render(&data).contains("not installed"));
}

#[test]
fn discover_native_metadata_errors_name_the_source() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let source = dir.path().join(".copilot/skills/broken/SKILL.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "---\ndescription: [broken yaml\n---\n").unwrap();
    let error = discover(&cfg, &outside, "merge conflicts").unwrap_err();
    assert!(error.contains("broken/SKILL.md"));
    assert!(error.contains("invalid frontmatter"));
}

#[test]
fn discover_project_claude_skills_are_available_in_both_harnesses() {
    let _lock = env_lock();
    let (dir, cfg, _env) = fixture();
    let repo = repo(dir.path());
    native_skill(
        &repo,
        ".claude/skills",
        "merge-local",
        "Resolve local merge conflicts",
    );
    let data = discover(&cfg, &repo, "merge conflicts").unwrap();
    assert_eq!(data["workflows"][0]["name"], "merge-local");
    assert_eq!(
        data["workflows"][0]["invoke"],
        json!({"copilot": "/merge-local", "claude": "/merge-local"})
    );
    assert_eq!(data["workflows"][0]["scope"], "repo-shared");
}
