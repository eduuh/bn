//! Seeding a **freshly created worktree** with this repo's repo-tier skills.
//!
//! [`super::sync_native_skills`] deliberately refuses to touch a repo's native skill roots unless
//! one already holds a bn-managed skill: a dashboard edit must not start writing generated files
//! into a project that never opted in. That rule is right, and it leaves a hole — a brand-new
//! `bn dispatch` worktree has no `.claude/skills` at all, so it can never satisfy the
//! already-installed test, and the agent dispatched into it silently runs without the repo's own
//! skills. Only the global tiers (`~/.claude/skills`) reach it.
//!
//! This closes that hole for the one moment it is safe to: worktree creation, where there is no
//! prior state to clobber. Two preconditions, both of which must hold:
//!
//! 1. **The repo actually has repo-tier skills.** Otherwise there is nothing the worktree is
//!    missing, and creating empty native roots would opt a project in by accident.
//! 2. **The native root is gitignored in that worktree.** Generated skills are not the branch's
//!    work. Untracked, they turn every `git status` noisy and every PR's file list wrong; tracked,
//!    they commit machine-local content into shared history. A repo that has not ignored the path
//!    has not opted in, and is skipped with a reason rather than written to.

// `harness_target` and the `HarnessTarget` trait are private to `skills`, and a child module can
// see its ancestors' private items — so the native root layout stays defined in exactly one place
// rather than being restated here and drifting.
use super::{
    harness_target, install_native_skills, skill_dirs, NativeHarness, NativeScope,
    NativeSkillChange, SkillScope, NATIVE_HARNESSES,
};
use crate::resolver::Config;
use std::path::{Path, PathBuf};

/// Why one harness root was left alone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkipReason {
    pub harness: NativeHarness,
    pub path: PathBuf,
    pub reason: String,
}

/// What seeding a worktree did, and what it deliberately did not do.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeSkillsReport {
    pub changes: Vec<NativeSkillChange>,
    pub skipped: Vec<SkipReason>,
    /// True when the repo has no repo-tier skills, so there was nothing to seed at all.
    pub nothing_to_seed: bool,
}

impl WorktreeSkillsReport {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Whether a native skill root is ignored by git inside `worktree`.
///
/// `git check-ignore` exits 0 for an ignored path and 1 for a tracked-or-untracked one, so the
/// status *is* the answer. A git that cannot run at all reports "not ignored", which fails safe:
/// the caller then skips rather than writing files it might not be able to clean up.
///
/// The probe path is the subtle part. The natural `.gitignore` entry for a directory is written
/// with a trailing slash (`.claude/skills/`), and git only matches such a pattern against a path
/// it can see *is* a directory — which, in a worktree created seconds ago, it is not, because
/// nothing has created it yet. Asking about the bare root would therefore answer "not ignored"
/// for exactly the repos that did opt in. Asking about a file *inside* the root sidesteps that:
/// both `.claude/skills/` and `.claude/skills` ignore everything beneath them, and the answer no
/// longer depends on whether the directory happens to exist yet.
fn root_is_gitignored(worktree: &Path, root: &Path) -> bool {
    let probe = root.join("bn-probe").join("SKILL.md");
    std::process::Command::new("git")
        .args(["-C", &worktree.to_string_lossy(), "check-ignore", "-q"])
        .arg(&probe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether any *repo* tier holds at least one skill file.
///
/// Reads the tier directories rather than [`super::discover`] so a single malformed skill
/// elsewhere in the repo cannot make this answer "no" and silently skip the seed.
fn has_repo_tier_skills(cfg: &Config, worktree: &Path) -> bool {
    skill_dirs(cfg, worktree)
        .into_iter()
        .filter(|(scope, _)| {
            !matches!(scope, SkillScope::GlobalShared | SkillScope::GlobalPersonal)
        })
        .any(|(_, dir)| {
            std::fs::read_dir(dir).is_ok_and(|mut entries| {
                entries
                    .any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|ext| ext == "md")))
            })
        })
}

/// Materialize this repo's skills into a freshly created `worktree`, honouring both preconditions
/// above. Idempotent and safe to call on an existing worktree — it delegates the writes to
/// [`install_native_skills`], which preflights every path and refuses to overwrite a same-named
/// skill bn does not own.
pub fn seed_worktree_skills(cfg: &Config, worktree: &Path, home: &Path) -> WorktreeSkillsReport {
    let mut report = WorktreeSkillsReport::default();
    if !has_repo_tier_skills(cfg, worktree) {
        report.nothing_to_seed = true;
        return report;
    }

    for harness in NATIVE_HARNESSES {
        let root = harness_target(harness).root(NativeScope::Repo, worktree, home);
        if !root_is_gitignored(worktree, &root) {
            report.skipped.push(SkipReason {
                harness,
                path: root,
                reason: "not gitignored in this repo — add it to .gitignore to opt in".to_string(),
            });
            continue;
        }
        // Each root installs independently: a name clash in one harness is not a reason to leave
        // the other unseeded (the same rule `sync_native_skills` learned the hard way).
        match install_native_skills(cfg, worktree, home, NativeScope::Repo, &[harness]) {
            Ok(ok) => report.changes.extend(ok.changes),
            Err(error) => report.skipped.push(SkipReason {
                harness,
                path: root,
                reason: error,
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_with_no_repo_tier_skills_seeds_nothing() {
        let temp = tempfile::tempdir().expect("temp");
        let report = seed_worktree_skills(&Config::default(), temp.path(), temp.path());
        assert!(report.nothing_to_seed);
        assert!(report.changes.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn a_path_outside_any_repo_is_not_reported_as_ignored() {
        // The fail-safe direction: when git cannot answer, the caller must skip rather than write.
        let temp = tempfile::tempdir().expect("temp");
        assert!(!root_is_gitignored(
            temp.path(),
            &temp.path().join(".claude/skills")
        ));
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?} failed");
    }

    /// The regression this probe exists for: a directory-form ignore rule matches nothing until
    /// the directory exists, and in a freshly created worktree it never does. Answering from the
    /// bare root would skip exactly the repos that opted in.
    #[test]
    fn a_directory_rule_is_honoured_before_the_directory_exists() {
        let temp = tempfile::tempdir().expect("temp");
        git(temp.path(), &["init", "-q"]);
        std::fs::write(
            temp.path().join(".gitignore"),
            ".claude/skills/
",
        )
        .expect("write");

        let root = temp.path().join(".claude").join("skills");
        assert!(!root.exists(), "the directory must not exist for this test");
        assert!(root_is_gitignored(temp.path(), &root));
    }

    #[test]
    fn a_repo_that_never_ignored_the_root_is_not_seeded_into() {
        let temp = tempfile::tempdir().expect("temp");
        git(temp.path(), &["init", "-q"]);
        std::fs::write(
            temp.path().join(".gitignore"),
            "target
",
        )
        .expect("write");

        let root = temp.path().join(".claude").join("skills");
        assert!(!root_is_gitignored(temp.path(), &root));
    }
}
