//! `bn release` — the release-on-idle companion (#627). The pure, testable core of cutting a
//! GitHub release end-to-end: pick/bump a semver from the last tag, roll release notes up from the
//! PRs merged since that tag, and decide whether a release is *allowed* right now (idle fleet +
//! green `main`, clean tree, not behind, not already released for this commit).
//!
//! Everything here reads git/GitHub through the injectable [`crate::runner::CommandRunner`] seam,
//! so the CLI drives it with [`crate::runner::RealRunner`] while tests replay fixtures with
//! [`crate::runner::FakeRunner`]. The actual side-effects (creating/pushing the tag, `gh release
//! create`) live at the CLI call site — this module only computes the plan and the gate.

use crate::runner::CommandRunner;

/// Which component of the semver to bump. Default is [`Bump::Patch`], matching the issue's
/// "default patch, configurable" contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bump {
    /// Breaking change: `x.y.z` → `(x+1).0.0`.
    Major,
    /// Feature: `x.y.z` → `x.(y+1).0`.
    Minor,
    /// Fix / default: `x.y.z` → `x.y.(z+1)`.
    #[default]
    Patch,
}

impl Bump {
    /// Parse a bump level from a flag value (case-insensitive), or `None` when unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "major" => Some(Bump::Major),
            "minor" => Some(Bump::Minor),
            "patch" => Some(Bump::Patch),
            _ => None,
        }
    }
}

/// A minimal `major.minor.patch` semantic version — enough for tag bumping. Pre-release/build
/// metadata is intentionally out of scope (single-repo release, patch-default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemVer {
    /// Major component.
    pub major: u64,
    /// Minor component.
    pub minor: u64,
    /// Patch component.
    pub patch: u64,
}

impl SemVer {
    /// The `v0.0.0` starting point when a repo has no tags yet.
    pub const ZERO: SemVer = SemVer {
        major: 0,
        minor: 0,
        patch: 0,
    };

    /// Parse a tag/version string like `v1.2.3` or `1.2.3` (a leading `v` is optional; any
    /// pre-release/build suffix after the patch is ignored). Returns `None` when the three core
    /// numeric components can't be read.
    pub fn parse(tag: &str) -> Option<SemVer> {
        let t = tag.trim();
        let t = t
            .strip_prefix('v')
            .or_else(|| t.strip_prefix('V'))
            .unwrap_or(t);
        // Drop any pre-release/build metadata after the core version.
        let core = t.split(['-', '+']).next().unwrap_or(t);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(SemVer {
            major,
            minor,
            patch,
        })
    }

    /// Apply a [`Bump`], zeroing the lower components as semver requires.
    pub fn bump(self, level: Bump) -> SemVer {
        match level {
            Bump::Major => SemVer {
                major: self.major + 1,
                minor: 0,
                patch: 0,
            },
            Bump::Minor => SemVer {
                major: self.major,
                minor: self.minor + 1,
                patch: 0,
            },
            Bump::Patch => SemVer {
                major: self.major,
                minor: self.minor,
                patch: self.patch + 1,
            },
        }
    }

    /// Render as a `vX.Y.Z` tag (bn's tags are `v`-prefixed).
    pub fn to_tag(self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A merged PR harvested from the commit log since the last tag, for the notes rollup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedPr {
    /// PR number (without the `#`).
    pub number: u64,
    /// PR title (the commit subject with the trailing `(#N)` stripped).
    pub title: String,
}

impl MergedPr {
    /// The conventional-commit type prefix of the title (e.g. `feat`, `fix`, `docs`), lowercased,
    /// or `None` when the title has no `type:`/`type(scope):` prefix.
    pub fn conventional_type(&self) -> Option<String> {
        let (head, _rest) = self.title.split_once(':')?;
        // Strip an optional `(scope)` and a trailing `!` (breaking-change marker).
        let ty = head
            .split('(')
            .next()
            .unwrap_or(head)
            .trim_end_matches('!')
            .trim();
        if ty.is_empty() || ty.contains(char::is_whitespace) {
            return None;
        }
        Some(ty.to_ascii_lowercase())
    }
}

/// The last tag reachable from HEAD via `git describe --tags --abbrev=0`, or `None` when the repo
/// has no tags yet (first release).
pub fn last_tag(runner: &dyn CommandRunner) -> Option<String> {
    runner
        .git(&["describe", "--tags", "--abbrev=0"])
        .ok_stdout()
}

/// The commit SHA at HEAD, or `None` when git can't resolve it.
pub fn head_sha(runner: &dyn CommandRunner) -> Option<String> {
    runner.git(&["rev-parse", "HEAD"]).ok_stdout()
}

/// The commit a tag points at (`git rev-list -n 1 <tag>`), or `None` when the tag is unknown.
pub fn tag_commit(runner: &dyn CommandRunner, tag: &str) -> Option<String> {
    runner.git(&["rev-list", "-n", "1", tag]).ok_stdout()
}

/// Whether the working tree is clean (`git status --porcelain` empty). A failed/absent git call is
/// treated as *not clean* (conservative: never release on an unknown state).
pub fn working_tree_clean(runner: &dyn CommandRunner) -> bool {
    let out = runner.git(&["status", "--porcelain"]);
    out.success && out.stdout.trim().is_empty()
}

/// How many commits HEAD is behind `origin/main` (`git rev-list --count HEAD..origin/main`). A
/// failed call yields `None` (unknown — the gate refuses on unknown, see [`evaluate_gate`]).
pub fn behind_count(runner: &dyn CommandRunner) -> Option<u64> {
    runner
        .git(&["rev-list", "--count", "HEAD..origin/main"])
        .ok_stdout()
        .and_then(|s| s.trim().parse().ok())
}

/// Enumerate the PRs merged since `since_tag` (or the whole history when `None`) by scanning commit
/// subjects for a trailing `(#N)` — the shape GitHub's squash-merge writes — plus `Merge pull
/// request #N` merge-commit subjects. Newest first, de-duplicated by PR number. Driven through the
/// runner (`git log`) so it's fully testable.
pub fn merged_prs_since(runner: &dyn CommandRunner, since_tag: Option<&str>) -> Vec<MergedPr> {
    let range = since_tag.map(|t| format!("{t}..HEAD"));
    let mut full: Vec<&str> = vec!["log", "--format=%s"];
    if let Some(r) = range.as_deref() {
        full.push(r);
    }
    let out = match runner.git(&full).ok_stdout() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut prs = Vec::new();
    for line in out.lines() {
        if let Some(pr) = parse_pr_subject(line) {
            if seen.insert(pr.number) {
                prs.push(pr);
            }
        }
    }
    prs
}

/// Parse a single commit subject into a [`MergedPr`] when it carries a PR number, handling both
/// squash-merge (`title (#123)`) and merge-commit (`Merge pull request #123 from …`) shapes.
fn parse_pr_subject(subject: &str) -> Option<MergedPr> {
    let s = subject.trim();
    // Merge-commit shape: "Merge pull request #123 from owner/branch".
    if let Some(rest) = s.strip_prefix("Merge pull request #") {
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let number = num.parse().ok()?;
        return Some(MergedPr {
            number,
            title: s.to_string(),
        });
    }
    // Squash-merge shape: trailing "(#123)".
    let open = s.rfind("(#")?;
    let close = s[open..].find(')')? + open;
    let digits = &s[open + 2..close];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse().ok()?;
    let title = s[..open].trim().to_string();
    Some(MergedPr {
        number,
        title: if title.is_empty() {
            s.to_string()
        } else {
            title
        },
    })
}

/// Render Markdown release notes from the merged PRs, grouped by conventional-commit type when the
/// titles carry one, with an "Other" bucket for the rest. Empty input yields a short placeholder so
/// a release is never notes-less.
pub fn render_notes(tag: &str, prs: &[MergedPr]) -> String {
    if prs.is_empty() {
        return format!("## {tag}\n\n_No merged pull requests since the previous release._\n");
    }
    // Stable, human-friendly ordering of the common conventional types; everything else → Other.
    const GROUPS: &[(&str, &str)] = &[
        ("feat", "Features"),
        ("fix", "Fixes"),
        ("perf", "Performance"),
        ("refactor", "Refactoring"),
        ("docs", "Documentation"),
        ("test", "Tests"),
        ("build", "Build"),
        ("ci", "CI"),
        ("chore", "Chores"),
    ];
    let mut out = format!("## {tag}\n\n");
    let mut rendered = std::collections::HashSet::new();
    for (ty, heading) in GROUPS {
        let bucket: Vec<&MergedPr> = prs
            .iter()
            .filter(|p| p.conventional_type().as_deref() == Some(ty))
            .collect();
        if bucket.is_empty() {
            continue;
        }
        out.push_str(&format!("### {heading}\n"));
        for pr in bucket {
            out.push_str(&format!("- {} (#{})\n", strip_type(&pr.title), pr.number));
            rendered.insert(pr.number);
        }
        out.push('\n');
    }
    let other: Vec<&MergedPr> = prs
        .iter()
        .filter(|p| !rendered.contains(&p.number))
        .collect();
    if !other.is_empty() {
        out.push_str("### Other\n");
        for pr in other {
            out.push_str(&format!("- {} (#{})\n", pr.title, pr.number));
        }
        out.push('\n');
    }
    out
}

/// Strip a leading conventional-commit `type:`/`type(scope):` prefix from a title for cleaner
/// per-group bullets (the group heading already names the type).
fn strip_type(title: &str) -> String {
    match title.split_once(':') {
        Some((head, rest)) if !head.contains(char::is_whitespace) => rest.trim().to_string(),
        _ => title.to_string(),
    }
}

/// The outcome of the release gate: either allowed (with the plan) or refused (with a reason). The
/// pure decision lives in [`decide`]; [`evaluate_gate`] gathers the live inputs through the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// A release is allowed; carries the computed plan.
    Release(ReleasePlan),
    /// A release is refused; carries a human-readable reason.
    Skip(String),
}

/// The concrete plan for a release: the tag to create and the notes to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePlan {
    /// The previous tag (`None` on the very first release).
    pub prev_tag: Option<String>,
    /// The new tag to create/push.
    pub next_tag: String,
    /// The commit the tag will point at.
    pub commit: String,
    /// The rolled-up release notes (Markdown).
    pub notes: String,
}

/// The live inputs the gate needs, gathered so [`decide`] can stay a pure predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateInputs {
    /// No coding lanes in flight (spare capacity).
    pub idle_fleet: bool,
    /// Latest local gate/verify signal is green.
    pub green_main: bool,
    /// Working tree is clean.
    pub clean: bool,
    /// Commits behind `origin/main`, or `None` when unknown.
    pub behind: Option<u64>,
    /// The current tag already points at HEAD (already released this commit).
    pub already_released: bool,
}

/// The pure gating predicate (#627 acceptance): a release is allowed only on **spare capacity**
/// (idle fleet) **and** green `main`, with a clean tree that isn't behind `origin/main`, and only
/// when this commit hasn't already been released (idempotence). Any failing condition yields a
/// [`GateDecision::Skip`] naming the reason; a clean pass returns [`GateDecision::Release`] with the
/// supplied `plan`.
pub fn decide(inputs: &GateInputs, plan: ReleasePlan) -> GateDecision {
    if inputs.already_released {
        return GateDecision::Skip("already released for this commit".to_string());
    }
    if !inputs.idle_fleet {
        return GateDecision::Skip("fleet busy — coding lanes in flight".to_string());
    }
    if !inputs.green_main {
        return GateDecision::Skip(
            "main not green — latest local gate/verify not PASS".to_string(),
        );
    }
    if !inputs.clean {
        return GateDecision::Skip("working tree is dirty".to_string());
    }
    match inputs.behind {
        None => return GateDecision::Skip("behind-main state unknown".to_string()),
        Some(n) if n > 0 => {
            return GateDecision::Skip(format!("HEAD is {n} commit(s) behind origin/main"))
        }
        Some(_) => {}
    }
    GateDecision::Release(plan)
}

/// Gather the live inputs and compute the plan, then run the pure [`decide`]. `idle_fleet` and
/// `green_main` come from the caller (the orchestrator knows in-flight lanes; the CLI/gate knows
/// the last verify result); everything else is read from git through `runner`.
pub fn evaluate_gate(
    runner: &dyn CommandRunner,
    bump: Bump,
    idle_fleet: bool,
    green_main: bool,
) -> GateDecision {
    let prev_tag = last_tag(runner);
    let current = prev_tag
        .as_deref()
        .and_then(SemVer::parse)
        .unwrap_or(SemVer::ZERO);
    let next = current.bump(bump);
    let commit = head_sha(runner).unwrap_or_default();
    let prs = merged_prs_since(runner, prev_tag.as_deref());
    let notes = render_notes(&next.to_tag(), &prs);
    let plan = ReleasePlan {
        prev_tag: prev_tag.clone(),
        next_tag: next.to_tag(),
        commit: commit.clone(),
        notes,
    };
    // Idempotence: if the last tag already points at HEAD, this commit is already released.
    let already_released = match (&prev_tag, commit.is_empty()) {
        (Some(t), false) => tag_commit(runner, t).as_deref() == Some(commit.as_str()),
        _ => false,
    };
    let inputs = GateInputs {
        idle_fleet,
        green_main,
        clean: working_tree_clean(runner),
        behind: behind_count(runner),
        already_released,
    };
    decide(&inputs, plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[test]
    fn semver_parse_handles_v_prefix_and_metadata() {
        assert_eq!(
            SemVer::parse("v1.2.3"),
            Some(SemVer {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            SemVer::parse("1.2.3"),
            Some(SemVer {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            SemVer::parse("v0.2.0-rc1"),
            Some(SemVer {
                major: 0,
                minor: 2,
                patch: 0
            })
        );
        assert_eq!(
            SemVer::parse("v1.2.3+build"),
            Some(SemVer {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(SemVer::parse("v1.2"), None);
        assert_eq!(SemVer::parse("nonsense"), None);
    }

    #[test]
    fn semver_bump_zeroes_lower_components() {
        let v = SemVer {
            major: 1,
            minor: 4,
            patch: 7,
        };
        assert_eq!(v.bump(Bump::Patch).to_tag(), "v1.4.8");
        assert_eq!(v.bump(Bump::Minor).to_tag(), "v1.5.0");
        assert_eq!(v.bump(Bump::Major).to_tag(), "v2.0.0");
    }

    #[test]
    fn bump_parse_is_case_insensitive() {
        assert_eq!(Bump::parse("MAJOR"), Some(Bump::Major));
        assert_eq!(Bump::parse(" minor "), Some(Bump::Minor));
        assert_eq!(Bump::parse("patch"), Some(Bump::Patch));
        assert_eq!(Bump::parse("wat"), None);
        assert_eq!(Bump::default(), Bump::Patch);
    }

    #[test]
    fn parses_both_squash_and_merge_pr_subjects() {
        let squash = parse_pr_subject("feat: add release agent (#627)").unwrap();
        assert_eq!(squash.number, 627);
        assert_eq!(squash.title, "feat: add release agent");
        let merge = parse_pr_subject("Merge pull request #42 from eduuh/x").unwrap();
        assert_eq!(merge.number, 42);
        assert!(parse_pr_subject("no pr number here").is_none());
        assert!(parse_pr_subject("weird (#notnum)").is_none());
    }

    #[test]
    fn merged_prs_since_dedups_and_reads_through_runner() {
        let log = "feat: a (#3)\nfix: b (#2)\nfeat: a again (#3)\nchore: c (#1)\n";
        let r = FakeRunner::new().on("git", &["log", "--format=%s", "v0.2.0..HEAD"], log);
        let prs = merged_prs_since(&r, Some("v0.2.0"));
        assert_eq!(
            prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        // No tag → whole history (no range arg).
        let r2 = FakeRunner::new().on("git", &["log", "--format=%s"], "fix: only (#9)\n");
        assert_eq!(
            merged_prs_since(&r2, None),
            vec![MergedPr {
                number: 9,
                title: "fix: only".to_string()
            }]
        );
    }

    #[test]
    fn render_notes_groups_by_conventional_type() {
        let prs = vec![
            MergedPr {
                number: 3,
                title: "feat: cut releases".to_string(),
            },
            MergedPr {
                number: 2,
                title: "fix: gate refuses dirty".to_string(),
            },
            MergedPr {
                number: 1,
                title: "misc cleanup".to_string(),
            },
        ];
        let notes = render_notes("v0.3.0", &prs);
        assert!(notes.contains("## v0.3.0"));
        assert!(notes.contains("### Features\n- cut releases (#3)"));
        assert!(notes.contains("### Fixes\n- gate refuses dirty (#2)"));
        assert!(notes.contains("### Other\n- misc cleanup (#1)"));
    }

    #[test]
    fn render_notes_empty_has_placeholder() {
        let notes = render_notes("v0.3.0", &[]);
        assert!(notes.contains("No merged pull requests"));
    }

    fn plan() -> ReleasePlan {
        ReleasePlan {
            prev_tag: Some("v0.2.0".to_string()),
            next_tag: "v0.2.1".to_string(),
            commit: "abc".to_string(),
            notes: "notes".to_string(),
        }
    }

    fn ok_inputs() -> GateInputs {
        GateInputs {
            idle_fleet: true,
            green_main: true,
            clean: true,
            behind: Some(0),
            already_released: false,
        }
    }

    #[test]
    fn decide_allows_on_idle_green_clean_current() {
        assert_eq!(decide(&ok_inputs(), plan()), GateDecision::Release(plan()));
    }

    #[test]
    fn decide_refuses_each_failing_condition() {
        let busy = GateInputs {
            idle_fleet: false,
            ..ok_inputs()
        };
        assert!(matches!(decide(&busy, plan()), GateDecision::Skip(r) if r.contains("busy")));

        let red = GateInputs {
            green_main: false,
            ..ok_inputs()
        };
        assert!(matches!(decide(&red, plan()), GateDecision::Skip(r) if r.contains("not green")));

        let dirty = GateInputs {
            clean: false,
            ..ok_inputs()
        };
        assert!(matches!(decide(&dirty, plan()), GateDecision::Skip(r) if r.contains("dirty")));

        let behind = GateInputs {
            behind: Some(2),
            ..ok_inputs()
        };
        assert!(matches!(decide(&behind, plan()), GateDecision::Skip(r) if r.contains("behind")));

        let unknown = GateInputs {
            behind: None,
            ..ok_inputs()
        };
        assert!(matches!(decide(&unknown, plan()), GateDecision::Skip(r) if r.contains("unknown")));

        let dup = GateInputs {
            already_released: true,
            ..ok_inputs()
        };
        assert!(
            matches!(decide(&dup, plan()), GateDecision::Skip(r) if r.contains("already released"))
        );
    }

    #[test]
    fn evaluate_gate_is_idempotent_when_tag_points_at_head() {
        // Last tag v0.2.0 points at HEAD → already released → Skip.
        let r = FakeRunner::new()
            .on("git", &["describe", "--tags", "--abbrev=0"], "v0.2.0")
            .on("git", &["rev-parse", "HEAD"], "deadbeef")
            .on("git", &["rev-list", "-n", "1", "v0.2.0"], "deadbeef")
            .on("git", &["log", "--format=%s", "v0.2.0..HEAD"], "")
            .on("git", &["status", "--porcelain"], "")
            .on("git", &["rev-list", "--count", "HEAD..origin/main"], "0");
        match evaluate_gate(&r, Bump::Patch, true, true) {
            GateDecision::Skip(reason) => assert!(reason.contains("already released"), "{reason}"),
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_gate_plans_next_patch_and_notes() {
        let r = FakeRunner::new()
            .on("git", &["describe", "--tags", "--abbrev=0"], "v0.2.0")
            .on("git", &["rev-parse", "HEAD"], "cafe123")
            .on("git", &["rev-list", "-n", "1", "v0.2.0"], "oldersha")
            .on(
                "git",
                &["log", "--format=%s", "v0.2.0..HEAD"],
                "feat: x (#5)\nfix: y (#4)\n",
            )
            .on("git", &["status", "--porcelain"], "")
            .on("git", &["rev-list", "--count", "HEAD..origin/main"], "0");
        match evaluate_gate(&r, Bump::Patch, true, true) {
            GateDecision::Release(plan) => {
                assert_eq!(plan.next_tag, "v0.2.1");
                assert_eq!(plan.prev_tag.as_deref(), Some("v0.2.0"));
                assert_eq!(plan.commit, "cafe123");
                assert!(plan.notes.contains("### Features\n- x (#5)"));
                assert!(plan.notes.contains("### Fixes\n- y (#4)"));
            }
            other => panic!("expected Release, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_gate_first_release_starts_from_zero() {
        // No tags → describe fails → start from v0.0.0, bump patch → v0.0.1.
        let r = FakeRunner::new()
            .on("git", &["rev-parse", "HEAD"], "sha0")
            .on("git", &["log", "--format=%s"], "feat: first (#1)\n")
            .on("git", &["status", "--porcelain"], "")
            .on("git", &["rev-list", "--count", "HEAD..origin/main"], "0");
        match evaluate_gate(&r, Bump::Patch, true, true) {
            GateDecision::Release(plan) => {
                assert_eq!(plan.next_tag, "v0.0.1");
                assert_eq!(plan.prev_tag, None);
            }
            other => panic!("expected Release, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_gate_refuses_when_behind_or_dirty() {
        let dirty = base_with("M file.rs", "0");
        assert!(matches!(dirty, GateDecision::Skip(r) if r.contains("dirty")));
        let is_behind = base_with("", "3");
        assert!(matches!(is_behind, GateDecision::Skip(r) if r.contains("behind")));
    }

    // Helper: build a full runner with a given status/behind and evaluate the gate.
    fn base_with(status_out: &str, behind_out: &str) -> GateDecision {
        let r = FakeRunner::new()
            .on("git", &["describe", "--tags", "--abbrev=0"], "v0.2.0")
            .on("git", &["rev-parse", "HEAD"], "cafe123")
            .on("git", &["rev-list", "-n", "1", "v0.2.0"], "oldersha")
            .on("git", &["log", "--format=%s", "v0.2.0..HEAD"], "")
            .on("git", &["status", "--porcelain"], status_out)
            .on(
                "git",
                &["rev-list", "--count", "HEAD..origin/main"],
                behind_out,
            );
        evaluate_gate(&r, Bump::Patch, true, true)
    }
}
