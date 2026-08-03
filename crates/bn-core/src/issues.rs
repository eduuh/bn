//! Cross-repo GitHub issues — the reservoir `bn task pull` refills from (#999, EPIC #993).
//!
//! # The failure this module exists to fix
//!
//! [`crate::palette::github_issues_repo`] shells `gh issue list` and ends in `.unwrap_or_default()`,
//! so **every** failure — `gh` missing, not authenticated, rate-limited, offline, repo unreachable,
//! or simply hung — collapses to an empty `Vec` that is *indistinguishable from "this repo has zero
//! open issues"*. The dashboard duly renders "no open issues" and you believe it.
//!
//! Three properties fix that, and callers depend on all three:
//!
//! 1. **Honest degradation.** A failure never returns a bare empty list. It returns the last good
//!    cache with [`RepoIssues::stale`] set and [`RepoIssues::error`] populated, or — when there is
//!    no cache — an empty list *with* `error`, which a caller can tell apart from a genuinely empty
//!    backlog.
//! 2. **Bounded.** Fetches go through [`CommandRunner::run_with_timeout`]. The old path used the
//!    unbounded `.output()`, so a hung `gh` hung the caller (in bn-web, an axum worker) forever.
//! 3. **Cached with a TTL.** Not per-day. [`crate::scout::with_cache`] keys on the date, which is
//!    right for a Saturday radar and wrong here: an issue filed at 10:00 would be invisible to a
//!    "what can I do today" triage until tomorrow. This keys on `fetched_at + ttl`.
//!
//! # Report, never omit
//!
//! A repo whose `owner/name` slug cannot be resolved (no local bare clone — see
//! [`crate::remote::github_slug`]) still appears in [`all`], carrying `slug: None` and a reason.
//! Dropping it would be the same lie in a different shape: a caller cannot act on a repo it never
//! hears about.

use crate::remote::github_slug;
use crate::resolver::Config;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// How long a cached fetch stays fresh. Short enough that an issue filed this morning reaches
/// today's triage; long enough that a dashboard refresh loop is not a `gh` call per repo per load.
pub const DEFAULT_TTL_SECS: i64 = 600;

/// Wall-clock budget for one `gh issue list`. Exceeding it is a *failure with a reason*, not a hang.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How many open issues to drain per repo.
pub const DEFAULT_LIMIT: usize = 100;

/// One open issue, as much of it as the reservoir needs to rank and convert into a task.
///
/// Wider than [`crate::palette::IssueHit`]'s four fields: `labels` drive the exclusion policy that
/// `bn task pull` reuses from [`crate::orchestrate::plan_keep_warm`], `updated_at` orders staleness,
/// and `is_assigned` keeps already-owned work out of the pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRef {
    /// The bn repo name this issue belongs to (not the GitHub slug).
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// ISO-8601 last-update stamp from GitHub, when it reported one.
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    /// Convenience mirror of `!assignees.is_empty()` — the pull filter's actual question.
    #[serde(default)]
    pub is_assigned: bool,
}

/// One repo's issues **and how much to trust them**.
///
/// `stale` and `error` are the point of the type. `issues` alone cannot distinguish "no open
/// issues" from "we could not ask", which is exactly the bug this module replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIssues {
    pub repo: String,
    /// `owner/name`, or `None` when no local bare clone resolved a remote.
    pub slug: Option<String>,
    /// Epoch seconds the `issues` were actually fetched — *not* when they were served.
    pub fetched_at: i64,
    /// True when this is cached data served because a live fetch failed.
    pub stale: bool,
    /// Why the live fetch failed, when it did. `None` on a clean fetch or a fresh cache hit.
    pub error: Option<String>,
    pub issues: Vec<IssueRef>,
}

impl RepoIssues {
    /// An empty, failed result — the honest "we could not ask, and have nothing cached" shape.
    fn failed(repo: &str, slug: Option<String>, now: i64, error: String) -> Self {
        RepoIssues {
            repo: repo.to_string(),
            slug,
            fetched_at: now,
            stale: false,
            error: Some(error),
            issues: Vec::new(),
        }
    }

    /// Whether this carries data a caller can act on, even if imperfect.
    pub fn usable(&self) -> bool {
        !self.issues.is_empty()
    }
}

/// The cache TTL, overridable for tests and slow links via `$BN_ISSUES_TTL_SECS`.
fn ttl_secs() -> i64 {
    std::env::var("BN_ISSUES_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn cache_dir() -> PathBuf {
    crate::resolver::state_dir().join("issues")
}

/// Cache file for one repo. Repo names are already `[A-Za-z0-9._-]`-shaped by
/// [`crate::resolver`], but sanitize anyway so a hand-passed name can never escape the directory.
fn cache_path(repo: &str) -> PathBuf {
    let safe: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    cache_dir().join(format!("{safe}.json"))
}

fn cache_read(repo: &str) -> Option<RepoIssues> {
    let text = std::fs::read_to_string(cache_path(repo)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist a *successful* fetch only. A failure is never cached — caching it would turn one flaky
/// minute into a TTL-long outage, and would overwrite the last good data the stale path serves.
fn cache_write(value: &RepoIssues) {
    if std::fs::create_dir_all(cache_dir()).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(cache_path(&value.repo), text);
    }
}

/// `gh issue list` args for one repo. The slug is the only caller-supplied token.
///
/// Widened past the legacy `number,title,url,state` — see [`IssueRef`] for why each field earns
/// its place.
pub fn issue_list_args(slug: &str, limit: usize) -> Vec<String> {
    vec![
        "issue".into(),
        "list".into(),
        "-R".into(),
        slug.into(),
        "--state".into(),
        "open".into(),
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,url,state,labels,updatedAt,assignees".into(),
    ]
}

/// The `gh --json` row shape, kept private so [`IssueRef`] stays the module's vocabulary.
#[derive(Debug, Deserialize)]
struct GhRow {
    number: i64,
    title: String,
    url: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<GhNamed>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(default)]
    assignees: Vec<GhLogin>,
}

#[derive(Debug, Deserialize)]
struct GhNamed {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct GhLogin {
    #[serde(default)]
    login: String,
}

impl GhRow {
    fn into_ref(self, repo: &str) -> IssueRef {
        let assignees: Vec<String> = self
            .assignees
            .into_iter()
            .map(|a| a.login)
            .filter(|l| !l.is_empty())
            .collect();
        IssueRef {
            repo: repo.to_string(),
            number: self.number,
            title: self.title,
            url: self.url,
            state: self.state,
            labels: self
                .labels
                .into_iter()
                .map(|l| l.name)
                .filter(|n| !n.is_empty())
                .collect(),
            updated_at: self.updated_at.filter(|s| !s.is_empty()),
            is_assigned: !assignees.is_empty(),
            assignees,
        }
    }
}

/// A short, human reason from a failed subprocess: its stderr tail, else a generic note.
fn failure_reason(out: &crate::runner::CommandOutput, what: &str) -> String {
    let tail = out.stderr.trim();
    if tail.is_empty() {
        format!("{what} failed")
    } else {
        let tail: String = tail.chars().take(300).collect();
        format!("{what} failed: {}", tail.replace('\n', " "))
    }
}

/// Resolve a repo's GitHub slug from its local bare clone, or say why not.
fn resolve_slug(cfg: &Config, runner: &dyn CommandRunner, repo: &str) -> Result<String, String> {
    let git_dir = cfg.bare_dir().join(format!("{repo}.git"));
    if !git_dir.exists() {
        return Err(format!(
            "no local bare clone at {} — cannot resolve a GitHub remote",
            git_dir.display()
        ));
    }
    github_slug(runner, &git_dir).ok_or_else(|| "no GitHub remote resolved for origin".to_string())
}

/// One repo's open issues: a fresh cache hit, a live fetch, or the last good cache annotated.
///
/// Never returns a bare empty list on failure — see the module docs. `now` is epoch seconds,
/// passed in so the TTL decision is deterministic under test.
pub fn for_repo(cfg: &Config, runner: &dyn CommandRunner, repo: &str, now: i64) -> RepoIssues {
    let cached = cache_read(repo);
    if let Some(hit) = cached
        .as_ref()
        .filter(|c| now.saturating_sub(c.fetched_at) < ttl_secs())
    {
        return hit.clone();
    }

    // Serve the last good cache, annotated, when the live path fails at any step.
    let degrade = |slug: Option<String>, error: String| -> RepoIssues {
        match cached.clone() {
            Some(mut stale) => {
                stale.stale = true;
                stale.error = Some(error);
                // Keep the cache's own `slug`/`fetched_at`: they describe the data being served,
                // and overwriting `fetched_at` with `now` would make stale data look fresh.
                stale
            }
            None => RepoIssues::failed(repo, slug, now, error),
        }
    };

    let slug = match resolve_slug(cfg, runner, repo) {
        Ok(slug) => slug,
        Err(why) => return degrade(None, why),
    };

    let args = issue_list_args(&slug, DEFAULT_LIMIT);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    // Bounded — the whole point. A hung `gh` must not hang the caller.
    let out = runner.run_with_timeout("gh", &argv, FETCH_TIMEOUT);
    if !out.success {
        return degrade(Some(slug), failure_reason(&out, "gh issue list"));
    }
    let rows: Vec<GhRow> = match serde_json::from_str(&out.stdout) {
        Ok(rows) => rows,
        Err(e) => {
            return degrade(
                Some(slug),
                format!("gh issue list returned unreadable JSON: {e}"),
            )
        }
    };

    let fresh = RepoIssues {
        repo: repo.to_string(),
        slug: Some(slug),
        fetched_at: now,
        stale: false,
        error: None,
        issues: rows.into_iter().map(|r| r.into_ref(repo)).collect(),
    };
    cache_write(&fresh);
    fresh
}

/// Every repo bn knows about, each with its issues and its own freshness.
///
/// A repo that cannot be reached is **present with a reason**, never omitted. Sorted by repo name
/// so callers and tests see a stable order.
pub fn all(cfg: &Config, runner: &dyn CommandRunner, now: i64) -> Vec<RepoIssues> {
    known_repos(cfg)
        .into_iter()
        .map(|repo| for_repo(cfg, runner, &repo, now))
        .collect()
}

/// The repos to sweep: everything bn manages, plus everything it has a branch note for. The union
/// matters — a repo can be managed before it has notes, and can have notes without being managed.
pub fn known_repos(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = cfg
        .managed_repos
        .iter()
        .filter_map(|url| repo_name_from_url(url))
        .chain(
            crate::rollups::all_notes(cfg)
                .iter()
                .map(|n| n.repo.to_ascii_lowercase()),
        )
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The bn repo name for a clone URL: the last path segment, minus `.git`, lowercased.
fn repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last).trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{CommandOutput, FakeRunner};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NOW: i64 = 1_785_000_000;

    fn rows_json() -> String {
        serde_json::json!([
            {
                "number": 963,
                "title": "Dispatched agents stall",
                "url": "https://github.com/eduuh/bn/issues/963",
                "state": "OPEN",
                "labels": [{"name": "bug"}, {"name": "P1"}],
                "updatedAt": "2026-08-01T09:00:00Z",
                "assignees": []
            },
            {
                "number": 869,
                "title": "Consolidate observation stores",
                "url": "https://github.com/eduuh/bn/issues/869",
                "state": "OPEN",
                "labels": [],
                "updatedAt": "2026-07-20T09:00:00Z",
                "assignees": [{"login": "eduuh"}]
            }
        ])
        .to_string()
    }

    /// A config whose state dir (the cache) and bare dir live under one temp root.
    fn fixture() -> (tempfile::TempDir, Config, crate::test_support::EnvGuard) {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let guard = crate::test_support::EnvGuard::set("BN_STATE_DIR", &state);
        // `bare_dir()` is `<worktree_dir>/../bare`.
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(bare.join("bn.git")).unwrap();
        let cfg = Config {
            personal_root: dir.path().join("notes"),
            work_root: dir.path().join("notes-work"),
            worktree_dir: dir.path().join("worktree"),
            hostname: "testhost".into(),
            ..Default::default()
        };
        (dir, cfg, guard)
    }

    fn runner_with_issues() -> FakeRunner {
        FakeRunner::new()
            .on_program("git", "git@github.com:eduuh/bn.git\n")
            .on_program("gh", &rows_json())
    }

    #[test]
    fn a_clean_fetch_populates_labels_assignees_and_updated_at() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let got = for_repo(&cfg, &runner_with_issues(), "bn", NOW);

        assert_eq!(got.slug.as_deref(), Some("eduuh/bn"));
        assert!(!got.stale);
        assert_eq!(got.error, None);
        assert_eq!(got.issues.len(), 2);

        let bug = &got.issues[0];
        assert_eq!(bug.number, 963);
        assert_eq!(bug.labels, vec!["bug", "P1"]);
        assert_eq!(bug.updated_at.as_deref(), Some("2026-08-01T09:00:00Z"));
        assert!(!bug.is_assigned);
        assert_eq!(bug.repo, "bn");

        let taken = &got.issues[1];
        assert!(taken.is_assigned, "an assigned issue is marked as such");
        assert_eq!(taken.assignees, vec!["eduuh"]);
    }

    #[test]
    fn a_second_call_inside_the_ttl_runs_no_subprocess() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();

        /// Counts every call so a cache hit that secretly re-shells is caught.
        struct Counting {
            inner: FakeRunner,
            calls: AtomicUsize,
        }
        impl CommandRunner for Counting {
            fn run(&self, program: &str, args: &[&str]) -> CommandOutput {
                self.calls.fetch_add(1, Ordering::Relaxed);
                self.inner.run(program, args)
            }
        }

        let runner = Counting {
            inner: runner_with_issues(),
            calls: AtomicUsize::new(0),
        };
        let first = for_repo(&cfg, &runner, "bn", NOW);
        let after_first = runner.calls.load(Ordering::Relaxed);
        assert!(after_first > 0, "the first call must actually fetch");

        let second = for_repo(&cfg, &runner, "bn", NOW + 60);
        assert_eq!(
            runner.calls.load(Ordering::Relaxed),
            after_first,
            "a hit inside the TTL must not shell out at all"
        );
        assert_eq!(second.issues, first.issues);
        assert!(!second.stale, "a fresh cache hit is not stale");
        assert_eq!(second.error, None);

        // Past the TTL it fetches again.
        let third = for_repo(&cfg, &runner, "bn", NOW + DEFAULT_TTL_SECS + 1);
        assert!(runner.calls.load(Ordering::Relaxed) > after_first);
        assert_eq!(third.issues.len(), 2);
    }

    #[test]
    fn a_failed_fetch_serves_the_last_good_cache_marked_stale() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let good = for_repo(&cfg, &runner_with_issues(), "bn", NOW);
        assert_eq!(good.issues.len(), 2);

        // `gh` is now unusable — not authenticated. Registered on the exact argv the fetch builds,
        // which also pins that `for_repo` calls `gh` the way `issue_list_args` says it does.
        let args = issue_list_args("eduuh/bn", DEFAULT_LIMIT);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let broken = FakeRunner::new()
            .on_program("git", "git@github.com:eduuh/bn.git\n")
            .on_output(
                "gh",
                &argv,
                CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: "gh: To get started with GitHub CLI, please run: gh auth login".into(),
                },
            );

        let degraded = for_repo(&cfg, &broken, "bn", NOW + DEFAULT_TTL_SECS + 1);
        assert!(degraded.stale, "served from cache after a failure");
        assert!(
            degraded
                .error
                .as_deref()
                .is_some_and(|e| e.contains("auth")),
            "the reason survives: {:?}",
            degraded.error
        );
        assert_eq!(
            degraded.issues, good.issues,
            "the last good data is served, NOT an empty list"
        );
        assert_eq!(
            degraded.fetched_at, good.fetched_at,
            "stale data keeps the time it was actually fetched"
        );
    }

    #[test]
    fn a_failure_with_no_cache_is_empty_but_carries_a_reason() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let broken = FakeRunner::new().on_program("git", "git@github.com:eduuh/bn.git\n");

        let got = for_repo(&cfg, &broken, "bn", NOW);
        assert!(got.issues.is_empty());
        assert!(
            got.error.is_some(),
            "an empty list without an error is the bug this module replaces"
        );
        assert!(!got.usable());
    }

    #[test]
    fn a_failure_is_never_cached() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let broken = FakeRunner::new().on_program("git", "git@github.com:eduuh/bn.git\n");
        let _ = for_repo(&cfg, &broken, "bn", NOW);

        // A cached failure would turn one flaky minute into a TTL-long outage.
        let recovered = for_repo(&cfg, &runner_with_issues(), "bn", NOW + 1);
        assert_eq!(recovered.issues.len(), 2);
        assert_eq!(recovered.error, None);
    }

    #[test]
    fn the_fetch_is_bounded_and_a_timeout_is_reported() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();

        /// Fails the test if the issue fetch ever uses the *unbounded* `run`, and models a
        /// timeout on the bounded path.
        struct Bounded {
            inner: FakeRunner,
        }
        impl CommandRunner for Bounded {
            fn run(&self, program: &str, args: &[&str]) -> CommandOutput {
                assert_ne!(
                    program, "gh",
                    "the issue fetch must go through run_with_timeout, not the unbounded run"
                );
                self.inner.run(program, args)
            }
            fn run_with_timeout(
                &self,
                program: &str,
                _args: &[&str],
                timeout: Duration,
            ) -> CommandOutput {
                assert_eq!(program, "gh");
                assert_eq!(timeout, FETCH_TIMEOUT);
                CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("timed out after {}s", timeout.as_secs()),
                }
            }
        }

        let runner = Bounded {
            inner: FakeRunner::new().on_program("git", "git@github.com:eduuh/bn.git\n"),
        };
        let got = for_repo(&cfg, &runner, "bn", NOW);
        assert!(got
            .error
            .as_deref()
            .is_some_and(|e| e.contains("timed out")));
        assert!(got.issues.is_empty());
    }

    #[test]
    fn a_repo_with_no_bare_clone_is_reported_not_omitted() {
        let _lock = crate::test_support::env_lock();
        let (_d, mut cfg, _g) = fixture();
        cfg.managed_repos = vec![
            "git@github.com:eduuh/bn.git".into(),
            "https://github.com/eduuh/atlas.git".into(), // no bare clone in the fixture
        ];

        let all = all(&cfg, &runner_with_issues(), NOW);
        let names: Vec<&str> = all.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(names, vec!["atlas", "bn"], "sorted, and nothing dropped");

        let atlas = &all[0];
        assert_eq!(atlas.slug, None);
        assert!(
            atlas
                .error
                .as_deref()
                .is_some_and(|e| e.contains("no local bare clone")),
            "the reason is specific: {:?}",
            atlas.error
        );
        assert!(all[1].error.is_none(), "the reachable repo still works");
    }

    #[test]
    fn unreadable_json_degrades_rather_than_panicking() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let garbage = FakeRunner::new()
            .on_program("git", "git@github.com:eduuh/bn.git\n")
            .on_program("gh", "not json at all");

        let got = for_repo(&cfg, &garbage, "bn", NOW);
        assert!(got.issues.is_empty());
        assert!(got
            .error
            .as_deref()
            .is_some_and(|e| e.contains("unreadable JSON")));
    }

    #[test]
    fn issues_round_trip_through_the_cache_with_every_field() {
        let _lock = crate::test_support::env_lock();
        let (_d, cfg, _g) = fixture();
        let fresh = for_repo(&cfg, &runner_with_issues(), "bn", NOW);
        // Re-read via the cache path only (a runner that would fail if consulted).
        let cached = for_repo(&cfg, &FakeRunner::new(), "bn", NOW + 1);
        assert_eq!(cached.issues, fresh.issues);
        assert_eq!(cached.issues[0].labels, vec!["bug", "P1"]);
        assert_eq!(
            cached.issues[0].updated_at.as_deref(),
            Some("2026-08-01T09:00:00Z")
        );
        assert!(cached.issues[1].is_assigned);
    }

    #[test]
    fn issue_list_args_ask_for_the_wider_field_set() {
        let args = issue_list_args("eduuh/bn", 50);
        assert!(args.windows(2).any(|w| w[0] == "-R" && w[1] == "eduuh/bn"));
        assert!(args.contains(&"50".to_string()));
        let fields = args.last().unwrap();
        for want in [
            "number",
            "title",
            "url",
            "state",
            "labels",
            "updatedAt",
            "assignees",
        ] {
            assert!(fields.contains(want), "missing {want} in {fields}");
        }
    }

    #[test]
    fn repo_names_come_off_any_clone_url_shape() {
        assert_eq!(
            repo_name_from_url("git@github.com:eduuh/BN.git").as_deref(),
            Some("bn")
        );
        assert_eq!(
            repo_name_from_url("https://github.com/eduuh/atlas.git").as_deref(),
            Some("atlas")
        );
        assert_eq!(
            repo_name_from_url("https://github.com/eduuh/tracker/").as_deref(),
            Some("tracker")
        );
        assert_eq!(repo_name_from_url("   ").as_deref(), None);
    }
}
