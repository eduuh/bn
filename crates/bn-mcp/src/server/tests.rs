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

#[cfg(unix)]
#[tokio::test]
async fn bn_cmd_lists_and_runs_a_branch_command_with_its_args() {
    let root = tempfile::tempdir().expect("tempdir");
    let worktrees = root.path().join("worktrees");
    let cfg = Config {
        worktree_dir: worktrees.clone(),
        ..bn_core::test_support::config(root.path())
    };
    let server = BnServer::new(cfg, worktree(&worktrees, "repo", "topic"), vec![]);
    let dir = server
        .cfg
        .note_dir(&server.resolve().expect("scope"))
        .join("cmds");
    std::fs::create_dir_all(&dir).expect("cmds dir");
    std::fs::write(
        dir.join("greet.sh"),
        "# description: Say hi\necho \"hi $1 on $BN_CMD_BRANCH\"\n",
    )
    .expect("write cmd");

    let call = |name: Option<&str>, args: &[&str]| {
        server.bn_cmd(Parameters(CmdArgs {
            name: name.map(str::to_string),
            args: args.iter().map(|a| a.to_string()).collect(),
            timeout_secs: None,
        }))
    };
    let listed = call(None, &[])
        .await
        .expect("list")
        .structured_content
        .expect("json");
    let greet = listed["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|c| c["name"] == "greet")
        .expect("greet listed");
    assert_eq!(
        (&greet["scope"], &greet["description"]),
        (&json!("branch"), &json!("Say hi"))
    );

    let ran = call(Some("greet"), &["you"])
        .await
        .expect("run")
        .structured_content
        .expect("json");
    assert_eq!(
        (&ran["exit_code"], &ran["stdout"]),
        (&json!(0), &json!("hi you on topic\n"))
    );
    assert!(call(Some("nope"), &[]).await.is_err());
}

#[tokio::test]
async fn note_items_can_be_estimated_repaired_and_links_searched() {
    let root = tempfile::tempdir().expect("tempdir");
    let worktrees = root.path().join("worktrees");
    let cfg = Config {
        worktree_dir: worktrees.clone(),
        ..bn_core::test_support::config(root.path())
    };
    let server = BnServer::new(cfg, worktree(&worktrees, "repo", "topic"), vec![]);
    let data = |r: Result<CallToolResult, ErrorData>| {
        r.expect("tool ok").structured_content.expect("json")
    };

    // #1528: reading before the first note exists is a state, not an io error.
    let cat = data(server.bn_cat(Parameters(EmptyArgs {})).await);
    assert_eq!(cat["rendered"], json!("repo/topic: no note"));

    let add = |text: &str, est: Option<&str>| {
        server.bn_add(Parameters(AddArgs {
            section: "todo".into(),
            text: text.into(),
            if_absent: None,
            est: est.map(str::to_string),
            q: Some("ship-v1".into()),
        }))
    };
    data(add("write the parser", Some("2h")).await);
    assert!(add("bad estimate", Some("soon")).await.is_err());
    let note = bn_core::store::read(&server.cfg.note_file(&server.resolve().unwrap())).unwrap();
    let t1 = &note.front.todos[0];
    assert_eq!(
        (t1.est.as_deref(), t1.q.as_deref()),
        (Some("2h"), Some("ship-v1"))
    );

    let item = |q: &str| Parameters(ItemArgs { query: q.into() });
    let reworded = data(
        server
            .bn_reword(Parameters(RewordArgs {
                query: "t1".into(),
                text: "write the lexer".into(),
            }))
            .await,
    );
    assert_eq!(reworded["item"]["previous"], json!("write the parser"));
    data(
        server
            .bn_done(Parameters(DoneArgs {
                query: "t1".into(),
                pct: None,
            }))
            .await,
    );
    assert_eq!(
        data(server.bn_reopen(item("lexer")).await)["item"]["id"],
        json!("t1")
    );
    data(server.bn_drop(item("t1")).await);
    assert!(server.bn_drop(item("t1")).await.is_err());

    links::add(
        &server.cfg,
        "https://example.com/dash",
        Some("perf dashboard"),
        &["ops".into()],
        None,
        None,
        "2026-10-10",
    )
    .unwrap();
    links::add(
        &server.cfg,
        "https://example.com/docs",
        Some("api docs"),
        &[],
        None,
        None,
        "2026-10-10",
    )
    .unwrap();
    let search = |query: Option<&str>, tag: Option<&str>| {
        server.bn_links_search(Parameters(LinksSearchArgs {
            query: query.map(str::to_string),
            tag: tag.map(str::to_string),
            limit: None,
        }))
    };
    let found = data(search(Some("DASH"), None).await);
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["name"], json!("perf dashboard"));
    assert_eq!(
        data(search(None, Some("ops")).await)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        data(search(None, None).await)["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
