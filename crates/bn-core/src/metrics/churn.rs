//! Commit-derived signals: the feature-vs-maintenance impact mix, re-revision
//! churn, and agent-vs-hand attribution windows.

use super::flow::DAY;
use crate::events::Event;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// How a commit's work splits between building new capability and keeping the lights on. Derived
/// from the conventional-commit type prefix (`feat:` → feature; `fix`/`chore`/`refactor`/`docs`/
/// `perf`/`style`/`test`/`build`/`ci` → maintenance); anything without a recognisable prefix lands
/// in `unclassified` rather than skewing the ratio. This is the DX Core 4 *Impact* axis — "am I
/// mostly building or mostly maintaining?" — read straight from commit messages, no new input.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ImpactMix {
    /// `feat`-prefixed commits — new capability.
    pub feature: usize,
    /// `fix`/`chore`/`refactor`/`docs`/… commits — maintenance.
    pub maintenance: usize,
    /// Commits with no recognisable conventional-commit type.
    pub unclassified: usize,
    /// feature / (feature + maintenance) — the building-vs-keeping ratio. `None` when neither
    /// bucket has a commit (nothing classified), so the surface can stay quiet.
    pub feature_ratio: Option<f64>,
}

impl ImpactMix {
    /// Total classified-or-not commits the window saw.
    pub fn total(&self) -> usize {
        self.feature + self.maintenance + self.unclassified
    }
}

/// Maintenance conventional-commit types — everything that keeps the lights on rather than adding
/// capability. `feat` (the one feature type) is matched separately; an unrecognised prefix is
/// neither, so it falls through to the unclassified bucket.
const MAINTENANCE_TYPES: &[&str] = &[
    "fix", "chore", "refactor", "docs", "perf", "style", "test", "build", "ci", "revert",
];

/// Classify a commit subject's conventional-commit type: `Some(true)` = feature (`feat`),
/// `Some(false)` = maintenance, `None` = no recognisable type. The type is the leading run of
/// ASCII letters before an optional `(scope)` and a `:` (`feat:`, `fix(core):`); a `!` breaking
/// marker is tolerated. Anything else (no colon, non-CC prefix) is unclassified.
fn classify_commit(subject: &str) -> Option<bool> {
    let (prefix, _) = subject.split_once(':')?;
    let prefix = prefix.trim();
    // Strip an optional `(scope)` and a trailing `!` breaking marker, leaving the bare type.
    let kind = prefix
        .split('(')
        .next()
        .unwrap_or(prefix)
        .trim_end_matches('!');
    // A real type is lowercase ASCII letters, and the prefix is either the bare type or
    // `type(scope)` / `type!` — not free-form text that happened to contain a colon.
    let well_formed = prefix == kind
        || prefix == format!("{kind}!")
        || (prefix.starts_with(kind) && prefix.ends_with(')'));
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) || !well_formed {
        return None;
    }
    if kind == "feat" {
        Some(true)
    } else if MAINTENANCE_TYPES.contains(&kind) {
        Some(false)
    } else {
        None
    }
}

/// Classify a slice of commit subjects into [`ImpactMix`]. Pure over the subjects so it's trivially
/// testable; [`impact_mix`] gathers the subjects through the runner first.
pub fn impact_mix_of(subjects: &[String]) -> ImpactMix {
    let mut m = ImpactMix::default();
    for s in subjects {
        match classify_commit(s) {
            Some(true) => m.feature += 1,
            Some(false) => m.maintenance += 1,
            None => m.unclassified += 1,
        }
    }
    let classified = m.feature + m.maintenance;
    m.feature_ratio = (classified > 0).then(|| m.feature as f64 / classified as f64);
    m
}

/// Feature-vs-maintenance mix over the trailing window, read from the commit log through the
/// runner (`git log --since=<date> --pretty=%s`). Best-effort: a git failure (not a repo, git
/// absent) yields an all-zero mix rather than an error. `since` is the window's start date as
/// `YYYY-MM-DD`. Surfaces only the bn-cli/metrics layer — stays out of `reports.rs`.
pub fn impact_mix(runner: &dyn CommandRunner, since: &str) -> ImpactMix {
    let since_arg = format!("--since={since}");
    let out = runner.git(&["log", &since_arg, "--no-merges", "--pretty=format:%s"]);
    let subjects: Vec<String> = out
        .ok_stdout()
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    impact_mix_of(&subjects)
}

/// The AI-era quality axis — code churn / turnover, split by who wrote it (#128). "Churn" is lines
/// re-revised soon after they were written (the rework that AI-assisted code does measurably more
/// of); we split it agent-assisted vs hand-written from the agent-session telemetry so the
/// "agents made me faster" feeling is checkable. Healthy is < 1.5× hand-written churn.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Churn {
    /// Window (days) within which a re-touch of a file counts as churn.
    pub churn_days: i64,
    /// Total lines changed across the commit window (added + removed).
    pub total_lines: i64,
    /// Of those, lines in commits that re-revised a recently-touched file (the churn).
    pub churned_lines: i64,
    /// churned / total — your overall churn rate. `None` when nothing changed.
    pub churn_rate: Option<f64>,
    /// Churned lines attributed to an agent session.
    pub agent_churned: i64,
    /// Churned lines with no agent session over them (hand-written).
    pub hand_churned: i64,
    /// Total agent-attributed lines (the churn denominator for the agent rate).
    pub agent_lines: i64,
    /// Total hand-written lines.
    pub hand_lines: i64,
    /// Agent churn-rate ÷ hand churn-rate — the turnover ratio. > 1 means agent code is reworked
    /// more. `None` when either side has no lines to rate. Healthy reference is < 1.5.
    pub agent_vs_hand_ratio: Option<f64>,
    /// False when the cwd repo is one the scorecard excludes ([`crate::metrics::is_excluded_repo`])
    /// — churn was not measured at all, as opposed to measured and found to be zero.
    ///
    /// Churn is the one metric read from `git log` in the current directory rather than from the
    /// repo-tagged event log, so it was the one metric the exclusion never reached: standing in
    /// `bn` reported `bn`'s own tooling churn as if it were project work, which is precisely the
    /// number [`crate::metrics::EXCLUDED_REPOS`] exists to keep out of the scorecard.
    #[serde(default = "counted_default")]
    pub counted: bool,
}

/// Older snapshots have no `counted` field; they were all real measurements.
fn counted_default() -> bool {
    true
}

/// A single commit's footprint, parsed from `git log --numstat`: when it landed and which files it
/// touched with how many lines. The pure churn input, so [`churn_of`] needs no git.
#[derive(Debug, Clone)]
pub struct CommitChurn {
    pub ts: i64,
    /// `(path, lines_changed)` — added + removed for that file in this commit.
    pub files: Vec<(String, i64)>,
}

/// A half-open `[start, end]` time window an agent session covered — the attribution key for
/// churn. A commit whose timestamp falls in any window is agent-assisted.
#[derive(Debug, Clone, Copy)]
pub struct AgentWindow {
    pub start: i64,
    pub end: i64,
}

impl AgentWindow {
    fn covers(&self, ts: i64) -> bool {
        ts >= self.start && ts <= self.end
    }
}

/// Parse `git log --numstat --pretty=format:'C%H%x09%ct'` output into per-commit footprints. Each
/// commit is a `C<sha>\t<ct>` header line followed by `added\tremoved\tpath` rows (binary files
/// show `-`/`-`, skipped). Order-independent; blank lines tolerated. Pure — the read path passes
/// the runner's stdout straight in.
pub fn parse_numstat(stdout: &str) -> Vec<CommitChurn> {
    let mut commits: Vec<CommitChurn> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('C') {
            // header: <sha>\t<ct>
            if let Some((_sha, ct)) = rest.split_once('\t') {
                if let Ok(ts) = ct.trim().parse::<i64>() {
                    commits.push(CommitChurn {
                        ts,
                        files: Vec::new(),
                    });
                }
            }
            continue;
        }
        // numstat row: added\tremoved\tpath
        let mut parts = line.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let added: i64 = a.trim().parse().unwrap_or(0);
        let removed: i64 = r.trim().parse().unwrap_or(0);
        let lines = added + removed;
        if let Some(c) = commits.last_mut() {
            c.files.push((path.trim().to_string(), lines));
        }
    }
    commits
}

/// Whether a path is a generated/vendored artefact rather than authored code, and so contributes
/// nothing meaningful to a rework signal.
///
/// Matched on the path, not on content: lockfiles and vendored trees are regenerated wholesale, so
/// every refresh reads as a total rewrite of a very large file. Measured against the real repo,
/// `package-lock.json` alone accounted for 10,730 changed lines in 30 days — more than any authored
/// source file — which is how an unfiltered churn rate reached 72% against an industry norm nearer
/// 6%.
pub fn is_generated_path(path: &str) -> bool {
    let p = path.trim_start_matches("./");
    let base = p.rsplit('/').next().unwrap_or(p);
    // Vendored / build output anywhere in the path.
    if p.split('/').any(|seg| {
        matches!(
            seg,
            "node_modules" | "vendor" | "target" | "dist" | "build" | ".venv" | "__pycache__"
        )
    }) {
        return true;
    }
    // Dependency lockfiles — regenerated wholesale, never hand-authored.
    if matches!(
        base,
        "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "Cargo.lock"
            | "poetry.lock"
            | "go.sum"
            | "composer.lock"
            | "Gemfile.lock"
    ) {
        return true;
    }
    // Minified/compiled assets and source maps.
    base.ends_with(".min.js")
        || base.ends_with(".min.css")
        || base.ends_with(".map")
        || base.ends_with(".lock")
}

/// Compute churn from parsed commits + agent windows (pure). A file touched again within
/// `churn_window_secs` of an earlier touch contributes that later touch's lines as churn; each
/// commit's lines are attributed agent vs hand by whether its timestamp falls in any agent window.
/// The split lets the turnover ratio answer "does my agent-assisted code churn more?".
pub fn churn_of(
    mut commits: Vec<CommitChurn>,
    agent_windows: &[AgentWindow],
    churn_window_secs: i64,
    churn_days: i64,
) -> Churn {
    // Oldest first so "re-touched a recent file" looks only backwards.
    commits.sort_by_key(|c| c.ts);
    let is_agent = |ts: i64| agent_windows.iter().any(|w| w.covers(ts));

    // Last time each path was touched, to detect a re-revision within the window.
    let mut last_touch: HashMap<String, i64> = HashMap::new();
    let mut out = Churn {
        churn_days,
        counted: true,
        ..Default::default()
    };

    for c in &commits {
        let agent = is_agent(c.ts);
        for (path, lines) in &c.files {
            // Generated artefacts are not craft, and they dwarf everything that is: one
            // `package-lock.json` refresh outweighs a week of hand-written diffs, so leaving them
            // in reports a churn rate that describes the lockfile rather than the author.
            if is_generated_path(path) {
                continue;
            }
            out.total_lines += lines;
            if agent {
                out.agent_lines += lines;
            } else {
                out.hand_lines += lines;
            }
            let churned = last_touch
                .get(path)
                .is_some_and(|&prev| c.ts - prev <= churn_window_secs);
            if churned {
                out.churned_lines += lines;
                if agent {
                    out.agent_churned += lines;
                } else {
                    out.hand_churned += lines;
                }
            }
            // Record this touch as the most recent for the path.
            last_touch.insert(path.clone(), c.ts);
        }
    }

    out.churn_rate =
        (out.total_lines > 0).then(|| out.churned_lines as f64 / out.total_lines as f64);
    let agent_rate =
        (out.agent_lines > 0).then(|| out.agent_churned as f64 / out.agent_lines as f64);
    let hand_rate = (out.hand_lines > 0).then(|| out.hand_churned as f64 / out.hand_lines as f64);
    out.agent_vs_hand_ratio = match (agent_rate, hand_rate) {
        (Some(a), Some(h)) if h > f64::EPSILON => Some(a / h),
        _ => None,
    };
    out
}

/// Read churn over the trailing window from git through the runner, attributed against the agent
/// windows. `since` is the window start (`YYYY-MM-DD`); `churn_days` is the re-revision horizon.
/// Best-effort: a git failure yields an all-zero churn, not an error.
pub fn churn(
    runner: &dyn CommandRunner,
    agent_windows: &[AgentWindow],
    since: &str,
    churn_days: i64,
) -> Churn {
    let since_arg = format!("--since={since}");
    let out = runner.git(&[
        "log",
        &since_arg,
        "--no-merges",
        "--numstat",
        "--pretty=format:C%H%x09%ct",
    ]);
    let commits = out
        .ok_stdout()
        .map(|s| parse_numstat(&s))
        .unwrap_or_default();
    churn_of(commits, agent_windows, churn_days * DAY, churn_days)
}

/// The agent-session windows for `repo` from the metrics events, for churn attribution. Each
/// `agent_session` event carries `started_ts` + `duration_secs`; the window is `[start, end]`.
/// `repo` is matched on the bn-canonical basename (lowercased) the sessions are stamped with.
pub fn agent_windows_for_repo(events: &[Event], repo: &str) -> Vec<AgentWindow> {
    let want = repo.rsplit('/').next().unwrap_or(repo).to_lowercase();
    crate::agent::latest_session_events(events)
        .into_iter()
        .filter(|e| {
            e.fields
                .get("repo")
                .and_then(Value::as_str)
                .map(|r| r.rsplit('/').next().unwrap_or(r).to_lowercase() == want)
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let start = e.fields.get("started_ts").and_then(Value::as_i64)?;
            let dur = e
                .fields
                .get("duration_secs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some(AgentWindow {
                start,
                end: start + dur.max(0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_artefacts_are_not_authored_code() {
        for p in [
            "crates/bn-web/ui/package-lock.json",
            "Cargo.lock",
            "ui/node_modules/react/index.js",
            "target/debug/build/x.rs",
            "web/dist/app.min.js",
            "web/static/app.min.css",
            "web/static/app.js.map",
            "vendor/github.com/pkg/errors.go",
            "go.sum",
        ] {
            assert!(is_generated_path(p), "{p} should be generated");
        }
    }

    #[test]
    fn authored_code_is_kept() {
        for p in [
            "crates/bn-core/src/metrics/churn.rs",
            "crates/bn-web/ui/src/index.css",
            "config/agents/standup.md",
            "scripts/check-file-size.sh",
            // Not a lockfile: a source file that merely mentions one.
            "crates/bn-cli/src/lockfile_parser.rs",
        ] {
            assert!(!is_generated_path(p), "{p} should be authored");
        }
    }

    /// The real shape of the bug: one lockfile refresh outweighing a week of authored work.
    #[test]
    fn a_lockfile_refresh_does_not_dominate_the_rate() {
        let day = 86_400;
        let commits = vec![
            CommitChurn {
                ts: 0,
                files: vec![
                    ("src/a.rs".into(), 10),
                    ("package-lock.json".into(), 10_000),
                ],
            },
            CommitChurn {
                ts: day,
                files: vec![
                    ("src/a.rs".into(), 10),
                    ("package-lock.json".into(), 10_000),
                ],
            },
        ];
        let c = churn_of(commits, &[], 14 * day, 14);
        assert_eq!(c.total_lines, 20, "only the authored lines count");
        assert_eq!(
            c.churned_lines, 10,
            "the second touch of a.rs is the rework"
        );
        assert_eq!(c.churn_rate, Some(0.5));
        assert!(c.counted);
    }
}
