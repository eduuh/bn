//! Sync retained Workspan stores without staging historical branch notes or session history.
use crate::*;
use std::path::{Component, Path, PathBuf};

fn retained_path(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|part| {
        matches!(
            *part,
            "note.md"
                | "note.yaml"
                | "handoff.md"
                | "handoff.json"
                | "handoff.yaml"
                | "archive"
                | "archives"
                | "_archive"
                | ".entire"
        )
    }) {
        return false;
    }
    matches!(
        parts.as_slice(),
        ["tasks.txt" | "links.md" | "config.toml" | ".gitattributes"]
            | [
                "tasks" | "references" | "triage" | "config" | "skills" | "cmds" | "jobs" | "agents",
                ..
            ]
            | [_, "daily" | "track" | "metrics" | "insights" | "state", ..]
            | [_, "inbox.md"]
            | [_, "config.toml"]
            | [_, _, "reference" | "skills", ..]
            | [_, _, _, "plans" | "proof" | "cmds" | "jobs" | "skills" | "agents", ..]
    )
}

fn sync_root(root: &Path) -> bool {
    let Some(staged) = crate::git::git_capture(
        &["diff", "--cached", "--name-only", "-z"],
        Some(root),
        false,
    ) else {
        return false;
    };
    if staged
        .split('\0')
        .filter(|path| !path.is_empty())
        .any(|path| !retained_path(path))
    {
        native_job_line(
            "sync-state: unexpected staged paths; index left untouched",
            true,
        );
        return false;
    }
    let Some(status) = crate::git::git_capture(
        &[
            "status",
            "--porcelain",
            "-z",
            "--no-renames",
            "--untracked-files=all",
        ],
        Some(root),
        false,
    ) else {
        return false;
    };
    let paths: Vec<_> = status
        .split('\0')
        .filter_map(|line| {
            if line.len() < 4 || !line.is_char_boundary(3) {
                return None;
            }
            let status = &line[..2];
            let path = &line[3..];
            (!status.contains(['R', 'C'])
                && !path.starts_with('"')
                && retained_path(path)
                && Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))))
            .then_some(path)
        })
        .collect();
    if !paths.is_empty() {
        let mut add = vec!["add", "-A", "--"];
        add.extend(&paths);
        if !crate::git::git_result(&add, Some(root)).0 {
            return false;
        }
        // --only excludes anything somebody else already staged, including historical notes.
        let mut commit = vec![
            "commit",
            "--only",
            "-q",
            "-m",
            "chore: sync Workspan state",
            "--",
        ];
        commit.extend(&paths);
        let (ok, _, err) = crate::git::git_result(&commit, Some(root));
        if !ok {
            native_job_line(&format!("sync-state: commit failed: {err}"), true);
            return false;
        }
    }
    let Some(upstream) =
        crate::git::git_capture(&["rev-parse", "--abbrev-ref", "@{u}"], Some(root), true)
    else {
        return true;
    };
    // Never stash or rebase a tree containing excluded historical changes.
    if !crate::git::git_capture(&["status", "--porcelain"], Some(root), true)
        .is_some_and(|text| text.is_empty())
    {
        native_job_line(
            &format!(
                "sync-state: local state saved; dirty tree left untouched: {}",
                root.display()
            ),
            false,
        );
        return true;
    }
    let Some((remote, _)) = upstream.split_once('/') else {
        return false;
    };
    if !crate::git::git_result(&["fetch", "--quiet", remote], Some(root)).0 {
        return false;
    }
    if !crate::git::git_result(&["rebase", &upstream], Some(root)).0 {
        let _ = crate::git::git_result(&["rebase", "--abort"], Some(root));
        native_job_line("sync-state: rebase failed; local commit retained", true);
        return false;
    }
    crate::git::git_result(&["push", "-q"], Some(root)).0
}

pub(crate) fn job_sync_state(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut ok = true;
    for dir in [&cfg.personal_root, &cfg.work_root] {
        if dir.as_os_str().is_empty() || !dir.is_dir() {
            continue;
        }
        let Some(top) = crate::git::git_capture(&["rev-parse", "--show-toplevel"], Some(dir), true)
        else {
            continue;
        };
        let root = PathBuf::from(top);
        if seen.contains(&root) {
            continue;
        }
        seen.push(root.clone());
        ok &= sync_root(&root);
    }
    i32::from(!ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_regression_sync_includes_resolver_config_and_agent_tiers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| crate::git::git_result(args, Some(root));
        assert!(git(&["init", "-q"]).0);
        assert!(git(&["config", "user.name", "Test"]).0);
        assert!(git(&["config", "user.email", "test@example.invalid"]).0);
        for path in [
            "agents/reviewer.md",
            "machine/config.toml",
            "machine/repo/main/agents/reviewer.md",
            "machine/repo/main/agents/note.md",
            "machine/repo/main/note.md",
            "machine/repo/main/task.md",
            "agents/archive/historical.md",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "preserved\n").unwrap();
        }
        assert!(sync_root(root));
        assert_eq!(
            git(&["ls-tree", "-r", "--name-only", "HEAD"]).1,
            "agents/reviewer.md\nmachine/config.toml\nmachine/repo/main/agents/reviewer.md",
        );
    }

    #[test]
    fn sync_commits_tasks_but_preserves_untracked_history() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| crate::git::git_result(args, Some(root));
        assert!(git(&["init", "-q"]).0);
        assert!(git(&["config", "user.name", "Test"]).0);
        assert!(git(&["config", "user.email", "test@example.invalid"]).0);
        std::fs::write(root.join("tasks.txt"), "t1 preserved\n").unwrap();
        std::fs::create_dir_all(root.join("tasks/context")).unwrap();
        std::fs::write(root.join("tasks/context/t1.md"), "## Why\n\nTask context\n").unwrap();
        std::fs::create_dir_all(root.join("machine/repo/reference")).unwrap();
        std::fs::write(root.join("machine/repo/reference/build.md"), "cargo test\n").unwrap();
        let historical = root.join("machine/repo/branch");
        std::fs::create_dir_all(&historical).unwrap();
        std::fs::write(historical.join("note.md"), "historical note\n").unwrap();
        std::fs::write(historical.join("task.md"), "historical task context\n").unwrap();
        std::fs::write(historical.join("handoff.md"), "historical handoff\n").unwrap();
        std::fs::create_dir_all(historical.join("plans")).unwrap();
        std::fs::write(historical.join("plans/state.json"), "{}\n").unwrap();
        std::fs::create_dir_all(historical.join("proof")).unwrap();
        std::fs::write(historical.join("proof/p1.md"), "verified\n").unwrap();
        std::fs::write(root.join("unknown.md"), "not retained\n").unwrap();
        assert!(sync_root(root));
        let (_, committed, _) = git(&["ls-tree", "-r", "--name-only", "HEAD"]);
        assert_eq!(committed, "machine/repo/branch/plans/state.json\nmachine/repo/branch/proof/p1.md\nmachine/repo/reference/build.md\ntasks.txt\ntasks/context/t1.md");
        assert_eq!(
            std::fs::read_to_string(historical.join("note.md")).unwrap(),
            "historical note\n"
        );
        assert_eq!(
            std::fs::read_to_string(historical.join("handoff.md")).unwrap(),
            "historical handoff\n"
        );
    }

    #[test]
    fn sync_refuses_staged_history_without_changing_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| crate::git::git_result(args, Some(root));
        assert!(git(&["init", "-q"]).0);
        std::fs::write(root.join("tasks.txt"), "t1 preserved\n").unwrap();
        std::fs::create_dir_all(root.join("archive")).unwrap();
        std::fs::write(root.join("archive/historical.md"), "historical note\n").unwrap();
        assert!(git(&["add", "archive/historical.md"]).0);
        assert!(!sync_root(root));
        assert_eq!(
            git(&["diff", "--cached", "--name-only"]).1,
            "archive/historical.md"
        );
        assert!(!git(&["rev-parse", "--verify", "HEAD"]).0);
    }
}
