//! `bn hooks` — git-hook integration (#248). `install` drops a `prepare-commit-msg` shim into
//! the directory git actually consults (`core.hooksPath`, else the common dir's `hooks/` — so
//! one install covers every worktree of a bare repo). The shim chains any pre-existing hook,
//! then delegates to `bn hooks run-prepare-commit-msg`, whose logic lives here in Rust:
//! stamp `BN-Note:` / `BN-Decision:` trailers when the branch has a note, silently do nothing
//! otherwise. The bn side can never block a commit — the shim exits 0 no matter what bn does.

use bn_core::resolver::{self, Config};
use std::path::{Path, PathBuf};

/// Identifies our script on re-install; bump the version if the shim's contract ever changes.
const MARKER: &str = "bn-hook v1";

/// The installed shim. `command -v bn || exit 0` keeps commits working in a clone without bn;
/// the chained pre-existing hook may still block (its exit code propagates), the bn stamp
/// never does.
const HOOK_SCRIPT: &str = r#"#!/bin/sh
# bn-hook v1 — stamps BN-Note trailers from the branch note (see `bn why`).
# Installed by `bn hooks install`; safe to delete. A pre-existing hook chained as
# prepare-commit-msg.pre-bn runs first and may still block the commit.
dir=$(dirname "$0")
if [ -x "$dir/prepare-commit-msg.pre-bn" ]; then
  "$dir/prepare-commit-msg.pre-bn" "$@" || exit $?
fi
command -v bn >/dev/null 2>&1 || exit 0
bn hooks run-prepare-commit-msg "$@" >/dev/null 2>&1 || :
exit 0
"#;

/// The hooks dir git consults for the repo containing `dir`: `core.hooksPath` when set
/// (`--path` so git expands `~`; a relative value resolves against the worktree top-level,
/// matching git), else `<git-common-dir>/hooks` — shared across a bare repo's worktrees.
fn hooks_dir(dir: &Path) -> Option<PathBuf> {
    if let Some(p) =
        crate::git::git_capture(&["config", "--path", "core.hooksPath"], Some(dir), true)
    {
        if !p.is_empty() {
            let path = PathBuf::from(&p);
            if path.is_absolute() {
                return Some(path);
            }
            let top = crate::git::git_capture(&["rev-parse", "--show-toplevel"], Some(dir), true)?;
            return Some(PathBuf::from(top).join(path));
        }
    }
    let common = crate::git::git_capture(&["rev-parse", "--git-common-dir"], Some(dir), true)?;
    let common_path = PathBuf::from(&common);
    let resolved = if common_path.is_absolute() {
        common_path
    } else {
        dir.join(common_path)
    };
    Some(resolved.join("hooks"))
}

/// Install (idempotently) the prepare-commit-msg shim for the repo containing `dir`. A
/// pre-existing non-bn hook is chained by renaming it to `prepare-commit-msg.pre-bn`; a
/// previous bn shim is simply rewritten. Returns the installed path and what happened:
/// `installed` | `refreshed` | `chained`.
pub(crate) fn install(dir: &Path) -> Result<(PathBuf, &'static str), String> {
    let hooks = hooks_dir(dir).ok_or_else(|| "not a git repo".to_string())?;
    std::fs::create_dir_all(&hooks).map_err(|e| format!("create {}: {e}", hooks.display()))?;
    let target = hooks.join("prepare-commit-msg");

    let mut outcome = "installed";
    if target.exists() {
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        if existing.contains(MARKER) {
            outcome = "refreshed";
        } else {
            let pre = hooks.join("prepare-commit-msg.pre-bn");
            if pre.exists() {
                return Err(format!(
                    "can't chain: both {} and {} already exist — resolve manually",
                    target.display(),
                    pre.display()
                ));
            }
            std::fs::rename(&target, &pre).map_err(|e| format!("chain existing hook: {e}"))?;
            outcome = "chained";
        }
    }

    std::fs::write(&target, HOOK_SCRIPT).map_err(|e| format!("write {}: {e}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    Ok((target, outcome))
}

/// Best-effort hook install on worktree creation — new worktrees get the trailer stamp for
/// free (issue #248). Never fails the caller; quiet unless something actually changed.
pub(crate) fn install_on_worktree_create(wt: &Path) {
    if let Ok((path, outcome)) = install(wt) {
        if outcome != "refreshed" {
            println!("bn hook {outcome}: {}", path.display());
        }
    }
}

/// The `prepare-commit-msg` body: append `BN-Note:` (+ newest `BN-Decision:`) trailers to
/// `msg_file` when `dir`'s branch has a note (`dir` is the worktree top-level — git's cwd for
/// hooks). Skips merge/squash/amend sources and `fixup!`/`squash!` subjects. Every failure
/// path is a silent no-op — a hook must never break `git commit`.
pub(crate) fn run_prepare_commit_msg(
    cfg: &Config,
    dir: &Path,
    msg_file: &Path,
    source: Option<&str>,
) {
    if matches!(source, Some("merge") | Some("squash") | Some("commit")) {
        return;
    }
    let Ok(ctx) = resolver::resolve(cfg, dir) else {
        return;
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return;
    };
    let Ok(msg) = std::fs::read_to_string(msg_file) else {
        return;
    };
    let subject = msg.lines().next().unwrap_or("");
    if subject.starts_with("fixup!") || subject.starts_with("squash!") {
        return;
    }
    let head = crate::git::git_capture(&["log", "-1", "--format=%B"], Some(dir), false);
    let decision = bn_core::why::decision_to_stamp(&note, head.as_deref());
    let note_ref = format!("{}/{}", ctx.repo, ctx.branch);
    if let Some(new_msg) = bn_core::why::append_trailers(&msg, &note_ref, decision.as_deref()) {
        let _ = std::fs::write(msg_file, new_msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = crate::git::git_capture(args, Some(dir), false).is_some();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
    }

    #[test]
    fn install_is_idempotent_and_chains_existing_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        // Fresh install.
        let (path, outcome) = install(&repo).unwrap();
        assert_eq!(outcome, "installed");
        assert!(path.ends_with(".git/hooks/prepare-commit-msg"));
        assert!(std::fs::read_to_string(&path).unwrap().contains(MARKER));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "hook must be executable");
        }

        // Re-install refreshes in place, never chains itself.
        let (_, outcome) = install(&repo).unwrap();
        assert_eq!(outcome, "refreshed");
        assert!(!path.with_file_name("prepare-commit-msg.pre-bn").exists());

        // A pre-existing foreign hook is chained.
        std::fs::write(&path, "#!/bin/sh\necho husky\n").unwrap();
        let (_, outcome) = install(&repo).unwrap();
        assert_eq!(outcome, "chained");
        let pre = path.with_file_name("prepare-commit-msg.pre-bn");
        assert_eq!(
            std::fs::read_to_string(&pre).unwrap(),
            "#!/bin/sh\necho husky\n"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains(MARKER));

        // Chained + another foreign hook appears again → refuse rather than clobber.
        std::fs::write(&path, "#!/bin/sh\necho other\n").unwrap();
        assert!(install(&repo).is_err());
    }

    #[test]
    fn install_respects_core_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        git(&repo, &["config", "core.hooksPath", ".husky"]);

        let (path, outcome) = install(&repo).unwrap();
        assert_eq!(outcome, "installed");
        // Compare canonicalized: git resolves /var → /private/var symlinks on macOS.
        let expect = repo
            .canonicalize()
            .unwrap()
            .join(".husky/prepare-commit-msg");
        assert_eq!(path.canonicalize().unwrap(), expect.canonicalize().unwrap());
    }

    #[test]
    fn install_outside_a_repo_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(install(tmp.path()).is_err());
    }

    /// Trailer writing against a fixture repo (#248 AC): a noted branch gets stamped, and the
    /// skip cases (merge/amend sources, fixup! subjects, note-less branches) stay untouched.
    #[test]
    fn run_prepare_commit_msg_stamps_noted_branches_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let cfg = Config {
            personal_root: tmp.path().join("notes"),
            work_root: tmp.path().join("notes"),
            worktree_dir: PathBuf::from("/nonexistent"),
            hostname: "testhost".into(),
            ..Default::default()
        };

        let msg = repo.join("COMMIT_EDITMSG");
        let write_msg = |m: &str| std::fs::write(&msg, m).unwrap();
        let read_msg = || std::fs::read_to_string(&msg).unwrap();

        // No note yet → untouched.
        write_msg("feat: x\n");
        run_prepare_commit_msg(&cfg, &repo, &msg, None);
        assert_eq!(read_msg(), "feat: x\n");

        // Note with a decision → both trailers land.
        let ctx = resolver::resolve(&cfg, &repo).unwrap();
        bn_core::commands::add(&cfg, &ctx, "decision", "ship it simple", None, "2026-07-05")
            .unwrap();
        run_prepare_commit_msg(&cfg, &repo, &msg, None);
        assert_eq!(
            read_msg(),
            format!(
                "feat: x\n\nBN-Note: {}/{}\nBN-Decision: ship it simple\n",
                ctx.repo, ctx.branch
            )
        );

        // Re-run (amended file already stamped) → idempotent.
        run_prepare_commit_msg(&cfg, &repo, &msg, None);
        assert_eq!(read_msg().matches("BN-Note:").count(), 1);

        // Merge/squash/amend sources and fixup! subjects → untouched.
        for (m, src) in [
            ("Merge branch 'x'\n", Some("merge")),
            ("squashed\n", Some("squash")),
            ("amended\n", Some("commit")),
            ("fixup! feat: x\n", None),
        ] {
            write_msg(m);
            run_prepare_commit_msg(&cfg, &repo, &msg, src);
            assert_eq!(read_msg(), m, "source {src:?} must not be stamped");
        }
    }
}
