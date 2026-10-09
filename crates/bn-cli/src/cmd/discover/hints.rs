//! Validate hint argv against the same grouping and clap definitions used by main, never run it.

use super::shell_word;
use std::ffi::OsString;

pub(super) fn bn(args: &[&str]) -> Result<String, String> {
    let argv = std::iter::once("bn")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let crate::regroup::Plan::Parse(argv) = crate::regroup::plan(argv) else {
        return Err("discovery command hint does not resolve to a command".to_owned());
    };
    crate::regroup::command()
        .try_get_matches_from(argv)
        .map_err(|e| format!("invalid discovery command hint: {e}"))?;
    Ok(std::iter::once("bn".to_owned())
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" "))
}

pub(super) fn skill_read(
    cfg: &crate::Config,
    cwd: &std::path::Path,
    name: &str,
) -> Result<String, String> {
    let skill = bn_core::skills::load_skill(cfg, cwd, name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("discovery workflow source no longer exists: {name}"))?;
    let mut args = vec!["skills".to_owned(), "render".to_owned(), name.to_owned()];
    for arg in skill.arguments.iter().filter(|arg| arg.required) {
        args.extend(["--arg".to_owned(), format!("{}=<{}>", arg.name, arg.name)]);
    }
    bn(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::discover::tests::{fixture, repo, snapshot};
    use bn_core::test_support::env_lock;
    use std::path::Path;

    fn check_emitted(data: &serde_json::Value) {
        let commands = data["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap());
        let reads = data["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["read_command"].as_str())
            .filter(|command| command.starts_with("bn "));
        for command in commands.chain(reads) {
            // These fixtures emit single-word arguments and quoted one-word placeholders.
            let words = command
                .split_whitespace()
                .map(|s| s.trim_matches('\''))
                .collect::<Vec<_>>();
            assert_eq!(words[0], "bn");
            let args = &words[1..];
            assert_eq!(bn(args).unwrap(), command);
        }
    }

    #[test]
    fn nonexistent_and_obsolete_command_paths_or_flags_cannot_be_hints() {
        for args in [
            vec!["not-a-command"],
            vec!["skills", "discover", "crash"],
            vec!["handoff", "load"],
            vec!["discover", "--repo", "project"],
            vec!["skills", "render", "debug", "--obsolete-flag"],
            vec!["fleet", "not-a-command"],
        ] {
            assert!(bn(&args).is_err(), "invalid hint accepted: {args:?}");
        }
        assert!(bn(&["agent", "handoff", "read"]).is_ok());
        assert!(bn(&["skills", "render", "quoted skill"]).is_ok());
    }

    #[test]
    fn emitted_repo_and_global_hints_parse_without_executing_or_changing_notes() {
        let _lock = env_lock();
        let (dir, cfg, _env) = fixture();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let repo = repo(dir.path());
        let before = snapshot(dir.path());
        for cwd in [&repo, &outside] {
            for task in ["", "fix a crash", "zyxwvu"] {
                check_emitted(&crate::cmd::discover::discover(&cfg, cwd, task).unwrap());
            }
        }
        assert_eq!(snapshot(dir.path()), before);

        let ctx = crate::resolver::resolve(&cfg, &repo).unwrap();
        let note = cfg.note_file(&ctx);
        std::fs::create_dir_all(note.parent().unwrap()).unwrap();
        std::fs::write(&note, "---\nrepo: project\nbranch: feature\ncreated: 2026-10-09\nstatus: active\n---\n## Goal\n\nFix a crash\n").unwrap();
        let handoff = bn_core::handoff::json_path(&cfg, &ctx);
        write_handoff(&handoff);
        let before = snapshot(dir.path());
        let data = crate::cmd::discover::discover(&cfg, &repo, "fix a crash").unwrap();
        assert!(data["commands"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("bn --cat")));
        assert!(data["commands"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("bn handoff read")));
        check_emitted(&data);
        assert_eq!(snapshot(dir.path()), before);
    }

    fn write_handoff(path: &Path) {
        let note = bn_core::model::Note::new(
            bn_core::resolver::RepoBranch {
                repo: "project",
                branch: "feature",
            },
            "2026-10-09",
            "testhost",
        );
        let packet = bn_core::handoff::compose(
            &note,
            "2026-10-09",
            &bn_core::handoff::HandoffInput::default(),
        );
        std::fs::write(path, serde_json::to_vec(&packet).unwrap()).unwrap();
    }

    #[test]
    fn named_workflow_reads_require_a_current_source_and_supply_required_arguments() {
        let _lock = env_lock();
        let (dir, cfg, _env) = fixture();
        let cwd = dir.path().join("outside");
        std::fs::create_dir(&cwd).unwrap();
        assert!(skill_read(&cfg, &cwd, "missing-workflow").is_err());
        let source = dir.path().join("shared/argument-workflow.md");
        std::fs::write(&source, "---\ndescription: Read an argument workflow\narguments:\n- name: request\n  required: true\n---\n{{request}}\n").unwrap();
        assert_eq!(
            skill_read(&cfg, &cwd, "argument-workflow").unwrap(),
            "bn skills render argument-workflow --arg 'request=<request>'",
        );
        std::fs::remove_file(&source).unwrap();
        assert!(skill_read(&cfg, &cwd, "argument-workflow").is_err());
    }

    #[test]
    fn native_sources_do_not_emit_a_missing_external_reader() {
        let _lock = env_lock();
        let (dir, cfg, _env) = fixture();
        let cwd = dir.path().join("outside");
        std::fs::create_dir(&cwd).unwrap();
        let source = dir.path().join(".agents/skills/merge-upstream/SKILL.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "---\nname: merge-upstream\ndescription: Resolve merge conflicts\n---\nNative body\n",
        )
        .unwrap();
        let catalog = crate::cmd::discover::native::catalog(&cfg, &cwd, dir.path()).unwrap();
        assert!(catalog["merge-upstream"]
            .read_command_on_path(&cfg, &cwd, "")
            .unwrap()
            .is_none());
        let data = crate::cmd::discover::discover(&cfg, &cwd, "resolve merge conflicts").unwrap();
        let workflow = &data["workflows"][0];
        assert_eq!(workflow["name"], "merge-upstream");
        assert_eq!(workflow["invoke"]["copilot"], "/merge-upstream");
        assert_eq!(workflow["source"], source.display().to_string());
    }
}
