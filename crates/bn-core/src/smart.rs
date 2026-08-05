//! The SMART gate's predicate (#998, EPIC #993) — pure, so the policy is testable in isolation.
//!
//! Vague tasks are the main cause of wasted autonomous runs. An agent handed "improve the
//! dashboard" burns a worktree, a model budget and an hour producing something nobody asked for.
//! So a task must be **S**pecific, **M**easurable, **A**chievable, **R**elevant and **T**ime-bound
//! before it may enter execution, and failing the bar routes it to the improve agent rather than
//! to an executor.
//!
//! # Codes, not prose
//!
//! Every gap carries a stable [`SmartGap::code`]. Routing switches on the code — the improve agent
//! needs a machine-readable reason, and matching on a human sentence would break the moment
//! someone improves the wording.
//!
//! # Two kinds of gap
//!
//! Most gaps an agent can close: write a done-when, add an estimate, name the repo. One cannot —
//! a task blocked on someone else is not un-*scoped*, it is waiting, and handing it to the improve
//! agent would burn a lane every ten minutes until the 3-strike backstop gave up. Those carry
//! [`SmartGap::human_only`], and the caller routes them to a person instead.
//!
//! # Enforcement lives elsewhere
//!
//! This module decides *whether* a task is ready. [`crate::pipeline::gate`] owns the single place
//! that acts on the answer. Keeping the two apart is what lets the policy be unit-tested without a
//! store, and what keeps a second copy of the check from drifting into `bn dispatch`.

use crate::task_doc::TaskDoc;
use crate::todo::Task;
use std::collections::HashSet;

/// The five dimensions, for grouping and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dim {
    Specific,
    Measurable,
    Achievable,
    Relevant,
    TimeBound,
}

impl Dim {
    pub fn label(self) -> &'static str {
        match self {
            Dim::Specific => "Specific",
            Dim::Measurable => "Measurable",
            Dim::Achievable => "Achievable",
            Dim::Relevant => "Relevant",
            Dim::TimeBound => "Time-bound",
        }
    }
}

/// One reason a task is not ready, and what would fix it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SmartGap {
    pub dimension: Dim,
    /// Stable identifier — what routing switches on. Never match on `message`.
    pub code: &'static str,
    pub message: String,
    pub suggested_fix: String,
    /// True when no agent can close this gap, so it must go to a person.
    pub human_only: bool,
}

/// The verdict: ready, or a list of specific reasons why not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SmartVerdict {
    pub ok: bool,
    pub gaps: Vec<SmartGap>,
}

impl SmartVerdict {
    /// Whether any gap needs a person rather than the improve agent.
    pub fn needs_human(&self) -> bool {
        self.gaps.iter().any(|g| g.human_only)
    }

    /// The gap codes, for a terse log line or an agent prompt.
    pub fn codes(&self) -> Vec<&'static str> {
        self.gaps.iter().map(|g| g.code).collect()
    }
}

/// The largest estimate a single task may carry, in minutes. Beyond a day of work it is an epic,
/// and an agent handed an epic produces an unreviewable diff.
pub const MAX_EST_MINUTES: i64 = 480;

/// The minimum number of words a title needs to name a concrete change.
pub const MIN_TITLE_WORDS: usize = 5;

/// Openers that describe an *activity* rather than an outcome. Allowed when the doc explains why —
/// "investigate the SSE reconnect storm" is a fine task if `## Why` says what's at stake.
const VAGUE_OPENERS: [&str; 8] = [
    "look into",
    "look at",
    "investigate",
    "improve",
    "clean up",
    "cleanup",
    "fix stuff",
    "tidy",
];

fn gap(
    dimension: Dim,
    code: &'static str,
    message: impl Into<String>,
    suggested_fix: impl Into<String>,
) -> SmartGap {
    SmartGap {
        dimension,
        code,
        message: message.into(),
        suggested_fix: suggested_fix.into(),
        human_only: false,
    }
}

fn human_gap(
    dimension: Dim,
    code: &'static str,
    message: impl Into<String>,
    suggested_fix: impl Into<String>,
) -> SmartGap {
    SmartGap {
        human_only: true,
        ..gap(dimension, code, message, suggested_fix)
    }
}

/// Parse an `est:` value into minutes. Accepts `90m`, `2h`, `1h30m`, or a bare number of minutes.
pub fn parse_est_minutes(raw: &str) -> Option<i64> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return (n > 0).then_some(n);
    }
    let mut total = 0i64;
    let mut digits = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = digits.parse().ok()?;
        digits.clear();
        match c {
            'h' => total += n * 60,
            'm' => total += n,
            'd' => total += n * 60 * 24,
            _ => return None,
        }
        saw_unit = true;
    }
    // A trailing bare number after a unit ("1h30") reads as minutes.
    if !digits.is_empty() {
        total += digits.parse::<i64>().ok()?;
    }
    (saw_unit && total > 0).then_some(total)
}

/// Is `date` (YYYY-MM-DD) within seven days after `today`? Used by the *Relevant* check, which
/// asks whether the task belongs to the near horizon at all.
fn within_a_week(date: &str, today: &str) -> bool {
    match (crate::util::epoch_day(date), crate::util::epoch_day(today)) {
        (Some(d), Some(t)) => (d - t).abs() <= 7,
        _ => false,
    }
}

/// Judge a task against the five dimensions.
///
/// Pure — `known_repos` (lowercased) and `today` are handed in rather than read, so the whole
/// policy is unit-testable without a config, a store or a clock. `doc` is `None` when the task has
/// no context doc yet, which is itself a *Measurable* failure.
pub fn check(
    task: &Task,
    doc: Option<&TaskDoc>,
    known_repos: &HashSet<String>,
    today: &str,
) -> SmartVerdict {
    let mut gaps = Vec::new();
    let summary = task.summary();
    let lower = summary.to_ascii_lowercase();

    // --- Specific -------------------------------------------------------------------------
    match task.meta("repo").map(str::trim).filter(|r| !r.is_empty()) {
        None => gaps.push(gap(
            Dim::Specific,
            "no_repo",
            "the task names no repo, so nothing can route it",
            "add `repo:<name>` — the repo whose code changes",
        )),
        Some(repo)
            if !known_repos.is_empty() && !known_repos.contains(&repo.to_ascii_lowercase()) =>
        {
            gaps.push(gap(
                Dim::Specific,
                "unknown_repo",
                format!("`repo:{repo}` is not a repo bn knows about"),
                "correct the repo name, or add it to [managed_repos].repos",
            ))
        }
        Some(_) => {}
    }

    if summary.split_whitespace().count() < MIN_TITLE_WORDS {
        gaps.push(gap(
            Dim::Specific,
            "too_terse",
            format!(
                "the title is {} words — too short to name a concrete change",
                summary.split_whitespace().count()
            ),
            format!(
                "rewrite it in at least {MIN_TITLE_WORDS} words, naming what changes and where"
            ),
        ));
    }

    let has_why = doc.and_then(|d| d.why()).is_some();
    if let Some(opener) = VAGUE_OPENERS.iter().find(|o| lower.starts_with(**o)) {
        if !has_why {
            gaps.push(gap(
                Dim::Specific,
                "vague_verb",
                format!(
                    "the title opens with \"{opener}\", which names an activity, not an outcome"
                ),
                "either state the outcome in the title, or fill in `## Why` in the context doc",
            ));
        }
    }

    // --- Measurable -----------------------------------------------------------------------
    if !doc.is_some_and(|d| d.has_done_when()) {
        gaps.push(gap(
            Dim::Measurable,
            "no_done_when",
            "no `## Done When` criteria — nothing states when this is finished",
            "run `bn task doc <id>` and list at least one checkable acceptance criterion",
        ));
    }

    // --- Achievable -----------------------------------------------------------------------
    match task.meta("est").and_then(parse_est_minutes) {
        None => gaps.push(gap(
            Dim::Achievable,
            "no_estimate",
            "no `est:` — an unsized task cannot be scheduled or bounded",
            "add `est:2h` (or `90m`) once the work is scoped",
        )),
        Some(mins) if mins > MAX_EST_MINUTES => gaps.push(gap(
            Dim::Achievable,
            "too_large",
            format!(
                "`est:` is {mins} minutes, past the {MAX_EST_MINUTES}-minute ceiling for one task"
            ),
            "split it into tasks that each fit inside a day",
        )),
        Some(_) => {}
    }

    // A blocked task is not un-scoped, it is waiting — no agent can fix that, so it goes to a
    // person rather than looping through the improve agent until the backstop gives up.
    if doc
        .and_then(|d| d.section("Blockers"))
        .is_some_and(|b| !b.trim().is_empty())
    {
        gaps.push(human_gap(
            Dim::Achievable,
            "blocked",
            "the context doc records an open blocker",
            "clear the blocker, or snooze the task until it can move",
        ));
    }

    // --- Relevant -------------------------------------------------------------------------
    // The valve that stops autonomously-pulled issue noise from reaching execution: a task must
    // trace to *something* — an issue, a quarter goal, or a date on the near horizon.
    let has_src = task.meta("src").is_some_and(|s| s.starts_with("http"));
    let has_quarter = task.meta("q").is_some_and(|q| !q.trim().is_empty());
    let near_date = ["day", "due"]
        .iter()
        .filter_map(|k| task.meta(k))
        .any(|d| within_a_week(d, today));
    if !has_src && !has_quarter && !near_date {
        gaps.push(gap(
            Dim::Relevant,
            "not_relevant",
            "nothing ties this task to an issue, a quarter goal, or the coming week",
            "link it with `src:<issue url>`, align it with `q:<quarter goal>`, or schedule it with `day:`",
        ));
    }

    // --- Time-bound -----------------------------------------------------------------------
    let dated: Vec<(&str, &str)> = ["day", "due"]
        .iter()
        .filter_map(|k| task.meta(k).map(|v| (*k, v)))
        .collect();
    if dated.is_empty() {
        gaps.push(gap(
            Dim::TimeBound,
            "no_date",
            "no `day:` or `due:` — the task is not scheduled",
            "stamp `day:<YYYY-MM-DD>` for when you intend to do it",
        ));
    } else if let Some((key, value)) = dated
        .iter()
        .find(|(_, v)| crate::util::epoch_day(v).is_none())
    {
        gaps.push(gap(
            Dim::TimeBound,
            "no_date",
            format!("`{key}:{value}` is not a YYYY-MM-DD date"),
            "correct it to a real date",
        ));
    } else if dated.iter().all(|(_, v)| *v < today) {
        gaps.push(gap(
            Dim::TimeBound,
            "stale_date",
            "every date on this task is in the past",
            "reschedule it — stamp `day:` for when you will actually pick it up",
        ));
    }

    SmartVerdict {
        ok: gaps.is_empty(),
        gaps,
    }
}
