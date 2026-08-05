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
        ..Default::default()
    };

    for c in &commits {
        let agent = is_agent(c.ts);
        for (path, lines) in &c.files {
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
    use crate::metrics::ev;
    use crate::runner::FakeRunner;
    use serde_json::json;

    // ---- #109 feature-vs-maintenance impact mix ----

    #[test]
    fn impact_mix_classifies_by_conventional_type() {
        let subjects: Vec<String> = [
            "feat: add the cockpit panel",
            "feat(nvim): colour the counts",
            "fix: flaky test",
            "chore(deps): bump serde",
            "refactor!: rename the trait", // breaking marker tolerated
            "docs: tidy the readme",
            "wip half-done thing",     // no colon → unclassified
            "Merge branch 'main'",     // no CC type → unclassified
            "random: not a real type", // colon but unknown type → unclassified
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = impact_mix_of(&subjects);
        assert_eq!(m.feature, 2, "two feat commits");
        assert_eq!(m.maintenance, 4, "fix/chore/refactor/docs");
        assert_eq!(m.unclassified, 3, "no-colon, merge, unknown type");
        // ratio is over classified only (feature + maintenance), not the unclassified bucket.
        assert_eq!(m.feature_ratio, Some(2.0 / 6.0));
        assert_eq!(m.total(), 9);
    }

    #[test]
    fn impact_mix_empty_has_no_ratio() {
        let m = impact_mix_of(&[]);
        assert_eq!(m.feature_ratio, None, "nothing classified → quiet");
        assert_eq!(m.total(), 0);
    }

    #[test]
    fn impact_mix_reads_commits_through_the_runner() {
        // The mix is driven entirely through the CommandRunner seam — no real git here.
        let log = "feat: a\nfix: b\nchore: c\n";
        let r = FakeRunner::new().on(
            "git",
            &[
                "log",
                "--since=2026-05-27",
                "--no-merges",
                "--pretty=format:%s",
            ],
            log,
        );
        let m = impact_mix(&r, "2026-05-27");
        assert_eq!(m.feature, 1);
        assert_eq!(m.maintenance, 2);
        assert_eq!(m.feature_ratio, Some(1.0 / 3.0));
        // A runner with no fixture (git absent / not a repo) yields an all-zero mix, not a panic.
        assert_eq!(impact_mix(&FakeRunner::new(), "2026-05-27").total(), 0);
    }

    // ---- #128 churn / turnover split agent vs hand ----

    #[test]
    fn parse_numstat_groups_files_under_each_commit() {
        let out = "Cabc\t1000\n10\t2\tsrc/a.rs\n5\t0\tsrc/b.rs\n\nCdef\t2000\n3\t1\tsrc/a.rs\n-\t-\tbin.png\n";
        let commits = parse_numstat(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].ts, 1000);
        assert_eq!(
            commits[0].files,
            vec![("src/a.rs".into(), 12), ("src/b.rs".into(), 5)]
        );
        // Binary row (`-`/`-`) parses to 0 lines, still recorded for the path.
        assert_eq!(
            commits[1].files,
            vec![("src/a.rs".into(), 4), ("bin.png".into(), 0)]
        );
    }

    #[test]
    fn churn_counts_re_revision_within_the_window() {
        let day = 86_400;
        // a.rs written day0 (10 lines), re-touched day3 (4 lines) → the day3 lines are churn.
        // b.rs written once → never churn. c.rs re-touched after 30 days → outside the 14d window.
        let commits = vec![
            CommitChurn {
                ts: 0,
                files: vec![("a.rs".into(), 10), ("b.rs".into(), 5)],
            },
            CommitChurn {
                ts: 3 * day,
                files: vec![("a.rs".into(), 4)],
            },
            CommitChurn {
                ts: 0,
                files: vec![("c.rs".into(), 7)],
            },
            CommitChurn {
                ts: 30 * day,
                files: vec![("c.rs".into(), 2)],
            },
        ];
        let c = churn_of(commits, &[], 14 * day, 14);
        assert_eq!(c.total_lines, 10 + 5 + 4 + 7 + 2);
        assert_eq!(c.churned_lines, 4, "only a.rs's day3 re-touch is churn");
        assert_eq!(c.churn_rate, Some(4.0 / 28.0));
    }

    #[test]
    #[allow(clippy::identity_op)]
    fn churn_splits_agent_vs_hand_and_ratios() {
        let day = 86_400;
        // Agent session covers day10..day11. Two files each written then re-revised:
        //   agent.rs: written day10 (in window) 100 lines, re-revised day11 (in window) 50 → churn.
        //   hand.rs:  written day0 (no window) 100 lines, re-revised day1 100 → churn but hand.
        let commits = vec![
            CommitChurn {
                ts: 0,
                files: vec![("hand.rs".into(), 100)],
            },
            CommitChurn {
                ts: 1 * day,
                files: vec![("hand.rs".into(), 20)],
            },
            CommitChurn {
                ts: 10 * day,
                files: vec![("agent.rs".into(), 100)],
            },
            CommitChurn {
                ts: 10 * day + 3600,
                files: vec![("agent.rs".into(), 60)],
            },
        ];
        let windows = vec![AgentWindow {
            start: 10 * day,
            end: 11 * day,
        }];
        let c = churn_of(commits, &windows, 14 * day, 14);
        // agent: 160 lines, 60 churned → rate 60/160 = 0.375
        // hand:  120 lines, 20 churned → rate 20/120 = 0.1667
        assert_eq!(c.agent_lines, 160);
        assert_eq!(c.agent_churned, 60);
        assert_eq!(c.hand_lines, 120);
        assert_eq!(c.hand_churned, 20);
        let ratio = c.agent_vs_hand_ratio.unwrap();
        assert!(
            (ratio - (0.375 / (20.0 / 120.0))).abs() < 1e-9,
            "agent churns ~2.25× hand"
        );
    }

    #[test]
    fn churn_reads_through_the_runner() {
        let log = "Cabc\t1000\n10\t0\ta.rs\n\nCdef\t2000\n4\t0\ta.rs\n";
        let r = FakeRunner::new().on(
            "git",
            &[
                "log",
                "--since=2026-05-27",
                "--no-merges",
                "--numstat",
                "--pretty=format:C%H%x09%ct",
            ],
            log,
        );
        // ts 1000 → 2000 is 1000s apart, well within 14 days → the 4-line re-touch is churn.
        let c = churn(&r, &[], "2026-05-27", 14);
        assert_eq!(c.total_lines, 14);
        assert_eq!(c.churned_lines, 4);
        // A runner with no git fixture yields an all-zero churn, not a panic.
        assert_eq!(
            churn(&FakeRunner::new(), &[], "2026-05-27", 14).total_lines,
            0
        );
    }

    #[test]
    fn agent_windows_match_repo_basename() {
        let day = 86_400;
        let evs = vec![
            ev(
                "agent_session",
                10 * day,
                &[
                    ("repo", json!("bn")),
                    ("started_ts", json!(10 * day - 1800)),
                    ("duration_secs", json!(1800)),
                ],
            ),
            // A different repo → not a window for `bn`.
            ev(
                "agent_session",
                11 * day,
                &[("repo", json!("atlas")), ("started_ts", json!(11 * day))],
            ),
        ];
        let w = agent_windows_for_repo(&evs, "eduuh/bn");
        assert_eq!(w.len(), 1, "only the bn session matches by basename");
        assert_eq!(w[0].start, 10 * day - 1800);
        assert_eq!(w[0].end, 10 * day);
    }
}
