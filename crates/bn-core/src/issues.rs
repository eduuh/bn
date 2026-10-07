//! Cross-repo GitHub issues — the reservoir `bn task pull` refills from (#999, EPIC #993).
//!
//! # The failure this module exists to fix
//!
//! An issue fetch that ends in `.unwrap_or_default()` collapses **every** failure:
//! `gh` missing, not authenticated, rate-limited, offline, repo unreachable,
//! or simply hung — collapses to an empty `Vec` that is *indistinguishable from "this repo has zero
//! open issues"*.
//!
//! Three properties fix that, and callers depend on all three:
//!
//! 1. **Honest degradation.** A failure never returns a bare empty list. It returns the last good
//!    cache with [`RepoIssues::stale`] set and [`RepoIssues::error`] populated, or — when there is
//!    no cache — an empty list *with* `error`, which a caller can tell apart from a genuinely empty
//!    backlog.
//! 2. **Bounded.** Fetches go through [`CommandRunner::run_with_timeout`]. The old path used the
//!    unbounded `.output()`, so a hung `gh` hung the caller forever.
//! 3. **Cached with a TTL.** Not per-day. `crate::scout::with_cache` keys on the date, which is
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
/// today's triage; long enough that repeated CLI reads do not call `gh` per repo each time.
pub const DEFAULT_TTL_SECS: i64 = 600;

/// Wall-clock budget for one `gh issue list`. Exceeding it is a *failure with a reason*, not a hang.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How many open issues to drain per repo.
pub const DEFAULT_LIMIT: usize = 100;

/// One open issue, as much of it as the reservoir needs to rank and convert into a task.
///
/// Includes the `labels` that drive the exclusion policy so
/// `bn task pull` reuses from [`crate::keep_warm::plan_keep_warm`], `updated_at` orders staleness,
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
///
/// Sequential, and deliberately so for an injected runner: [`CommandRunner`] carries no `Sync`
/// bound, so a fixture runner cannot be shared across threads. Callers driving the *real* runner
/// over many repos want [`all_parallel`].
pub fn all(cfg: &Config, runner: &dyn CommandRunner, now: i64) -> Vec<RepoIssues> {
    known_repos(cfg)
        .into_iter()
        .map(|repo| for_repo(cfg, runner, &repo, now))
        .collect()
}

/// How many `gh issue list` calls may be in flight at once.
///
/// The work is network latency, not CPU, so the useful width is set by what GitHub and the local
/// `gh` will tolerate rather than by core count. Eight turns a dozen repos into two waves while
/// staying far below anything that reads as abuse.
pub const MAX_PARALLEL_FETCHES: usize = 8;

/// [`all`] with the per-repo fetches run concurrently against the real runner.
///
/// The sequential version is a *timeout* per repo, not a budget for the sweep: each repo may spend
/// [`FETCH_TIMEOUT`] (20s), and a failed fetch is never cached, so a handful of unreachable repos
/// re-pay that cost on every call. Fetching independent repos concurrently bounds the sweep
/// instead of making callers wait for each repo's timeout.
///
/// Each worker builds its own [`RealRunner`] (a unit struct) rather than sharing one, which is what
/// lets this parallelize without widening [`CommandRunner`] with a `Sync` bound that every fixture
/// runner would then have to honour.
///
/// **Order and completeness are preserved**: results are re-sorted into `known_repos` order, and a
/// repo whose worker panicked comes back with an error rather than going missing — the same promise
/// the sequential path makes, since a caller cannot act on a repo it never hears about.
pub fn all_parallel(cfg: &Config, now: i64) -> Vec<RepoIssues> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let repos = known_repos(cfg);
    if repos.len() <= 1 {
        return all(cfg, &crate::runner::RealRunner, now);
    }

    let cursor = AtomicUsize::new(0);
    let workers = repos.len().min(MAX_PARALLEL_FETCHES);
    let repos_ref = &repos;

    let done: Vec<(usize, RepoIssues)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let runner = crate::runner::RealRunner;
                    let mut mine = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some(repo) = repos_ref.get(i) else { break };
                        mine.push((i, for_repo(cfg, &runner, repo, now)));
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .flatten()
            .collect()
    });

    merge_in_order(&repos, done, now)
}

/// Put concurrent results back into `repos` order, and account for every repo.
///
/// Pure, and separate, because this is the step that can silently lose one. Workers finish out of
/// order and a panicking worker returns nothing at all; a merge that simply collected whatever came
/// back would hand callers a short list with no indication anything was missing — the precise lie
/// this module exists to remove, since an absent repo is indistinguishable from one with an empty
/// backlog.
fn merge_in_order(repos: &[String], done: Vec<(usize, RepoIssues)>, now: i64) -> Vec<RepoIssues> {
    let mut slots: Vec<Option<RepoIssues>> = repos.iter().map(|_| None).collect();
    for (i, fetched) in done {
        if let Some(slot) = slots.get_mut(i) {
            *slot = Some(fetched);
        }
    }
    repos
        .iter()
        .zip(slots)
        .map(|(repo, slot)| {
            slot.unwrap_or_else(|| {
                RepoIssues::failed(
                    repo,
                    None,
                    now,
                    "the worker fetching this repo did not finish".to_string(),
                )
            })
        })
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
pub(crate) fn repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last).trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn fetched(repo: &str) -> RepoIssues {
        RepoIssues {
            repo: repo.to_string(),
            slug: Some(format!("owner/{repo}")),
            fetched_at: 100,
            stale: false,
            error: None,
            issues: Vec::new(),
        }
    }

    /// Workers finish in whatever order the network allows; callers and tests see `known_repos`
    /// order regardless.
    #[test]
    fn out_of_order_results_are_restored_to_repo_order() {
        let repos = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let done = vec![
            (2, fetched("gamma")),
            (0, fetched("alpha")),
            (1, fetched("beta")),
        ];
        let merged = merge_in_order(&repos, done, 200);
        assert_eq!(
            merged.iter().map(|r| r.repo.as_str()).collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
        assert!(merged.iter().all(|r| r.error.is_none()));
    }

    /// A repo whose worker died comes back **present, with a reason**. Dropping it would let a
    /// crashed fetch read as an empty backlog.
    #[test]
    fn a_repo_no_worker_reported_is_present_with_an_error() {
        let repos = vec!["alpha".to_string(), "beta".to_string()];
        let merged = merge_in_order(&repos, vec![(0, fetched("alpha"))], 200);

        assert_eq!(merged.len(), 2, "every repo is accounted for");
        assert_eq!(merged[1].repo, "beta");
        assert!(
            merged[1].error.is_some(),
            "a repo nobody fetched must say so, not look empty"
        );
        assert!(merged[1].issues.is_empty());
    }
}
