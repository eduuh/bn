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


