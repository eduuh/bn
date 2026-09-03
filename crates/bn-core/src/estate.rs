//! Offline collection of **estate rot** — aging TODO/FIXME markers, stale dependencies, flaky
//! verify history, worktree/branch cruft, and note hygiene.
//!
//! This is a collector, not an agent. It answers "what does the estate currently look like?" and
//! nothing else: no thresholds, no ranking, no landing. Whoever reads the packet decides what is
//! worth raising — today that is the `sentinel` agent's prompt (`config/agents/sentinel.md`), and
//! the module neither knows nor cares.
//!
//! Every collector degrades independently. A repo whose git errors contributes nothing, an absent
//! dependency cache yields `None` for that section alone, and an empty event log simply means no
//! verify history — one broken source never sinks the gather. The whole thing is offline: no
//! network, so it can run on a scheduler that has no credentials.

use crate::resolver::Config;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// Default age (days) below which a TODO/FIXME marker is not worth reporting. Applied at
/// collection so the packet stays small; the reader still decides what to do with what survives.
pub const DEFAULT_TODO_AGE_DAYS: i64 = 30;

/// How many markers (each individually `git blame`d for age) one gather will inspect — a hard
/// bound so a nightly job stays cheap regardless of how much debt a repo carries.
pub const MARKER_SCAN_CAP: usize = 80;

/// A file with at least this many commits in the recent window counts as "hot".
const HOT_FILE_COMMITS: usize = 3;

/// How far back the verify-history collector looks.
const VERIFY_WINDOW_DAYS: i64 = 7;

// --- collected shapes ---------------------------------------------------------------------------

/// A TODO/FIXME marker located by `git blame`, with the age of its introducing commit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoMarker {
    pub repo: String,
    pub file: String,
    pub line: u32,
    pub text: String,
    pub age_days: i64,
    /// The marker sits in a "hot" file (many recent commits).
    #[serde(default)]
    pub hot: bool,
}

/// A dependency's staleness, from the daily registry cache vs. the lockfile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DepStatus {
    pub repo: String,
    pub name: String,
    pub current: String,
    pub latest: String,
    pub majors_behind: u32,
    #[serde(default)]
    pub vuln: bool,
}

/// One verify-gate outcome for a labelled job/test in the trailing window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyRun {
    pub repo: String,
    /// The job/test label (e.g. a cron job name).
    pub label: String,
    pub passed: bool,
}

/// A worktree/branch that looks like reclaimable cruft.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeState {
    pub repo: String,
    pub branch: String,
    /// Upstream is `[gone]` (the PR merged/branch deleted) — reclaimable.
    #[serde(default)]
    pub gone: bool,
    /// A stale non-main worktree `wt cleanup` would refuse (dirty / unmerged), so it lingers.
    #[serde(default)]
    pub cleanup_refused: bool,
    #[serde(default)]
    pub reason: String,
}

/// A branch note's hygiene state. Overlaps what `bn doctor` reports, which is why the packet
/// marks it — a reader that raises it unconditionally is duplicating the doctor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoteState {
    pub repo: String,
    pub branch: String,
    /// Days the oldest open blocker has been open (0 = none / not blocked).
    pub blocked_days: i64,
    /// An active note with no goal set.
    #[serde(default)]
    pub goal_less: bool,
}

/// Which repos to sweep, and how old a marker must be to be worth collecting.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Bare repo names the sweep may touch. Empty ⇒ nothing is collected (the opt-in boundary).
    pub repos: Vec<String>,
    pub todo_age_days: i64,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            repos: Vec::new(),
            todo_age_days: DEFAULT_TODO_AGE_DAYS,
        }
    }
}

impl Scope {
    pub fn allows(&self, repo: &str) -> bool {
        self.repos.iter().any(|r| r.eq_ignore_ascii_case(repo))
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }
}

/// Everything one sweep saw. `deps: None` means the dependency cache was absent, which is
/// materially different from "no dependency is stale" and is reported as such.
#[derive(Debug, Clone, Default)]
pub struct Estate {
    pub markers: Vec<TodoMarker>,
    pub deps: Option<Vec<DepStatus>>,
    pub verify_runs: Vec<VerifyRun>,
    pub worktrees: Vec<WorktreeState>,
    pub notes: Vec<NoteState>,
}

// --- the sweep ----------------------------------------------------------------------------------

/// Sweep the allowlisted repos offline. Each section is independent — see the module note.
pub fn collect(cfg: &Config, runner: &dyn CommandRunner, scope: &Scope, today: &str) -> Estate {
    Estate {
        markers: collect_markers(cfg, runner, scope),
        deps: load_dep_cache(today),
        verify_runs: collect_verify_runs(VERIFY_WINDOW_DAYS),
        worktrees: collect_worktrees(cfg, runner, scope),
        notes: collect_notes(cfg, today),
    }
}

/// The read-only packet a reader gets: the sweep, the scope it ran under, and — when `carried_from`
/// names an attention source — that source's currently-open keyed items.
///
/// `carried` is what makes a trend sentence possible without a private snapshot file: the
/// attention store already remembers what was raised last time, keyed by subject, so a reader can
/// tell a finding it has been carrying for a week from one that appeared tonight. The source name
/// is a parameter rather than a constant because this module does not know which agent is asking.
pub fn gather_packet(
    cfg: &Config,
    runner: &dyn CommandRunner,
    scope: &Scope,
    carried_from: Option<&str>,
    today: &str,
) -> Value {
    let estate = collect(cfg, runner, scope, today);
    let carried: Vec<Value> = carried_from
        .map(|source| {
            crate::attention::open_items(today)
                .into_iter()
                .filter(|i| i.source == source)
                .map(|i| {
                    serde_json::json!({
                        // The bare key, as the agent wrote it. The store namespaces keys by source
                        // (`attn:staff-sentinel:todo:…`) and echoing that form back meant every
                        // "have I raised this before" test compared a namespaced key against a
                        // bare one and was silently false — so nothing was ever carried, the trend
                        // counts were fiction, and `replace` acked and re-raised the same board
                        // every night.
                        "key": i.key.as_deref().map(strip_source_namespace),
                        "title": i.title,
                        "severity": i.severity,
                        "repo": i.repo,
                        // `detail` carries the measurement the last run reported ("blocked 7d"),
                        // which is the only baseline a reader has for "has this got worse?".
                        // `mode` says whether the item is already queued for an unattended worker,
                        // so a reader does not promote the same finding twice.
                        "detail": i.detail,
                        "mode": i.mode,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "date": today,
        "scope": {
            "repos": scope.repos,
            "todo_age_days": scope.todo_age_days,
        },
        "markers": estate.markers,
        // Distinguish "the cache was missing" from "nothing is stale": a reader must not report a
        // clean dependency bill of health it never actually saw.
        "deps": estate.deps,
        "deps_degraded": estate.deps.is_none(),
        "verify_runs": estate.verify_runs,
        "worktrees": estate.worktrees,
        "notes": estate.notes,
        "carried": carried,
    })
}

/// A stored attention key (`attn:<source>:<key>`) reduced to the key its producer chose.
///
/// Pure and total: a key in some other shape is returned unchanged, because guessing at one would
/// be worse than handing back what is actually there.
fn strip_source_namespace(key: &str) -> &str {
    key.strip_prefix("attn:")
        .and_then(|rest| rest.split_once(':'))
        .map_or(key, |(_source, k)| k)
}

// --- collectors ---------------------------------------------------------------------------------

/// Verify labels to blame: cron jobs whose name looks like a verify/test gate.
fn is_verify_job(job: &str) -> bool {
    job.contains("verify") || job.contains("test")
}

/// Note-hygiene state from the branch notes (the same source `bn doctor` reads): a note with open
/// work carries a `blocked_days` proxy (idle days while it has open blockers) and a `goal_less`
/// flag. Only notes with something worth escalating are returned.
pub fn collect_notes(cfg: &Config, today: &str) -> Vec<NoteState> {
    crate::rollups::all_notes(cfg)
        .into_iter()
        .filter_map(|bn| {
            let blockers = bn.note.front.blockers.len();
            let open_todos = bn
                .note
                .front
                .todos
                .iter()
                .filter(|t| t.is_open_todo())
                .count();
            if blockers == 0 && open_todos == 0 {
                return None; // no open work — nothing to nag about (matches the doctor)
            }
            let goal_less = crate::render::goal_text(&bn.note.body).is_none();
            let idle_days =
                crate::util::idle_days_since(&crate::doctor::last_activity(&bn.note), today);
            let blocked_days = if blockers > 0 { idle_days } else { 0 };
            if blocked_days == 0 && !goal_less {
                return None;
            }
            Some(NoteState {
                repo: bn.repo,
                branch: bn.branch,
                blocked_days,
                goal_less,
            })
        })
        .collect()
}

/// Verify-gate history from the machine-local event log: every `cron_run` for a verify/test job in
/// the trailing `window_days`, as pass/fail rows. Offline (event log). The per-*test* granularity
/// arrives with the flake ledger; until then this blames whole verify jobs.
pub fn collect_verify_runs(window_days: i64) -> Vec<VerifyRun> {
    let cutoff = crate::util::now_epoch_secs() - window_days * 86_400;
    crate::events::read_all()
        .into_iter()
        .filter(|e| e.kind == "cron_run" && e.ts >= cutoff)
        .filter_map(|e| {
            let job = e.fields.get("job").and_then(Value::as_str)?;
            if !is_verify_job(job) {
                return None;
            }
            let passed = e
                .fields
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| e.fields.get("exit_code").and_then(Value::as_i64) == Some(0));
            let repo = e
                .fields
                .get("repo")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(VerifyRun {
                repo,
                label: job.to_string(),
                passed,
            })
        })
        .collect()
}

/// Branch/worktree cruft from the worktrees rollup: `[gone]` upstreams (merged/deleted) and stale
/// non-main worktrees `wt cleanup` would refuse (dirty). One cheap `git status -sb` per known
/// worktree path; anything unreadable is skipped.
pub fn collect_worktrees(
    cfg: &Config,
    runner: &dyn CommandRunner,
    scope: &Scope,
) -> Vec<WorktreeState> {
    crate::rollups::worktrees(cfg)
        .into_iter()
        .filter(|w| scope.allows(&w.repo))
        .filter_map(|w| {
            let path = w.worktree.clone()?;
            let out = runner
                .git(&["-C", &path, "status", "-sb"])
                .ok_stdout()
                .unwrap_or_default();
            let mut lines = out.lines();
            let header = lines.next().unwrap_or("");
            let gone = header.contains("[gone]");
            let dirty = lines.any(|l| !l.trim().is_empty());
            let is_main = crate::agent::is_reserved_branch(&w.branch);
            let cleanup_refused = dirty && !is_main;
            if !gone && !cleanup_refused {
                return None;
            }
            Some(WorktreeState {
                repo: w.repo,
                branch: w.branch,
                gone,
                cleanup_refused,
                reason: if dirty {
                    "uncommitted changes".to_string()
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

/// TODO/FIXME markers across the allowlisted repos' checked-out worktrees, aged by `git blame` and
/// flagged hot when their file churns. Bounded by [`MARKER_SCAN_CAP`]; all git is best-effort, so a
/// repo whose git errors simply contributes nothing. Markers younger than `scope.todo_age_days`
/// are dropped here rather than shipped and filtered downstream — the cap is a budget on `git
/// blame` calls, so spending it on markers nobody wants would starve the ones they do.
pub fn collect_markers(cfg: &Config, runner: &dyn CommandRunner, scope: &Scope) -> Vec<TodoMarker> {
    let mut out: Vec<TodoMarker> = Vec::new();
    let mut seen_repo = HashSet::new();
    for w in crate::rollups::worktrees(cfg) {
        if out.len() >= MARKER_SCAN_CAP {
            break;
        }
        if !scope.allows(&w.repo) || !seen_repo.insert(w.repo.clone()) {
            continue;
        }
        let Some(path) = w.worktree else { continue };
        let budget = MARKER_SCAN_CAP - out.len();
        out.extend(
            markers_in_repo(runner, &w.repo, &path, budget)
                .into_iter()
                .filter(|m| m.age_days >= scope.todo_age_days),
        );
    }
    out
}

fn markers_in_repo(
    runner: &dyn CommandRunner,
    repo: &str,
    path: &str,
    budget: usize,
) -> Vec<TodoMarker> {
    let grep = match runner
        .git(&["-C", path, "grep", "-nI", "-E", "TODO|FIXME"])
        .ok_stdout()
    {
        Some(s) => s,
        None => return Vec::new(), // no markers (or git errored) — degrade quietly
    };
    let hot = hot_files(runner, path);
    let now = crate::util::now_epoch_secs();
    grep.lines()
        .filter_map(parse_grep_line)
        .take(budget)
        .filter_map(|(file, line, text)| {
            let age_days = blame_age_days(runner, path, &file, line, now)?;
            Some(TodoMarker {
                repo: repo.to_string(),
                hot: hot.contains(&file),
                file,
                line,
                text,
                age_days,
            })
        })
        .collect()
}

/// Parse one `git grep -n` row (`path:line:content`) into its parts.
fn parse_grep_line(l: &str) -> Option<(String, u32, String)> {
    let mut it = l.splitn(3, ':');
    let file = it.next()?.to_string();
    let line: u32 = it.next()?.parse().ok()?;
    let text = it.next()?.trim().to_string();
    Some((file, line, text))
}

/// The age (days) of the commit that introduced `file:line`, via a single `git blame` on that line.
fn blame_age_days(
    runner: &dyn CommandRunner,
    path: &str,
    file: &str,
    line: u32,
    now: i64,
) -> Option<i64> {
    let spec = format!("{line},{line}");
    let out = runner
        .git(&[
            "-C",
            path,
            "blame",
            "-L",
            &spec,
            "--line-porcelain",
            "--",
            file,
        ])
        .ok_stdout()?;
    let ts: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("author-time "))
        .and_then(|s| s.trim().parse().ok())?;
    Some(((now - ts).max(0)) / 86_400)
}

/// Files with many recent commits (a churn/"hot" proxy), from one `git log` over the window.
fn hot_files(runner: &dyn CommandRunner, path: &str) -> HashSet<String> {
    let out = runner
        .git(&[
            "-C",
            path,
            "log",
            "--since=90.days.ago",
            "--name-only",
            "--pretty=format:",
        ])
        .ok_stdout()
        .unwrap_or_default();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(5000)
    {
        *counts.entry(f.to_string()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= HOT_FILE_COMMITS)
        .map(|(f, _)| f)
        .collect()
}

/// The daily dependency-staleness cache written by the (network-bound) registry-refresh job. Only a
/// cache dated *today* is trusted — a missing or stale cache degrades this section alone (returns
/// `None`), never the whole sweep.
pub fn load_dep_cache(today: &str) -> Option<Vec<DepStatus>> {
    #[derive(Deserialize)]
    struct DepCache {
        #[serde(default)]
        date: String,
        #[serde(default)]
        deps: Vec<DepStatus>,
    }
    let path = crate::resolver::state_dir()
        .join("estate")
        .join("deps.json");
    let cache: DepCache = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (cache.date == today).then_some(cache.deps)
}

#[cfg(test)]
mod tests {
    use super::strip_source_namespace;

    /// The bug this exists for: the store namespaces a producer's key by source, and echoing that
    /// form back into the packet meant every "have I raised this before" test compared a
    /// namespaced key against the bare one the agent writes — always false. Nothing was ever
    /// carried, the trend counts were fiction, and `replace` acked and re-raised the same board
    /// every night.
    #[test]
    fn a_stored_key_comes_back_as_the_producer_wrote_it() {
        assert_eq!(
            strip_source_namespace("attn:staff-sentinel:todo:bn:src/parse.rs:42"),
            "todo:bn:src/parse.rs:42"
        );
    }

    /// Total by construction: a key in some other shape is handed back unchanged, because guessing
    /// at one would be worse than returning what is actually there.
    #[test]
    fn a_key_in_any_other_shape_is_left_alone() {
        for key in ["todo:bn:x", "attn:no-second-colon", "", "attn:"] {
            assert_eq!(strip_source_namespace(key), key, "key {key:?}");
        }
    }
}
