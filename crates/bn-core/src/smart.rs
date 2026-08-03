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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::TodoFile;

    const TODAY: &str = "2026-08-03";

    fn repos() -> HashSet<String> {
        ["bn".to_string(), "atlas".to_string()]
            .into_iter()
            .collect()
    }

    fn task(line: &str) -> Task {
        TodoFile::parse(line).tasks().next().unwrap().clone()
    }

    fn doc(body: &str) -> TaskDoc {
        TaskDoc {
            path: std::path::PathBuf::from("/tmp/t1.md"),
            text: body.to_string(),
        }
    }

    /// A task that passes every dimension — the baseline each test perturbs one field of.
    fn good_task() -> Task {
        task("fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:2h day:2026-08-03\n")
    }

    fn good_doc() -> TaskDoc {
        doc("---\ntask: t1\n---\n\n## Why\nIt drops the dashboard.\n\n## Done When\n- [ ] reconnect backs off\n")
    }

    fn codes(v: &SmartVerdict) -> Vec<&'static str> {
        let mut c = v.codes();
        c.sort_unstable();
        c
    }

    #[test]
    fn a_fully_scoped_task_passes() {
        let v = check(&good_task(), Some(&good_doc()), &repos(), TODAY);
        assert!(v.ok, "unexpected gaps: {:?}", codes(&v));
        assert!(v.gaps.is_empty());
        assert!(!v.needs_human());
    }

    #[test]
    fn a_task_with_no_done_when_is_refused() {
        // The headline case: this is what stops an agent being handed an unscoped job.
        let v = check(&good_task(), None, &repos(), TODAY);
        assert!(!v.ok);
        assert!(codes(&v).contains(&"no_done_when"));
    }

    #[test]
    fn an_untouched_template_does_not_satisfy_measurable() {
        let v = check(
            &good_task(),
            Some(&doc("## Done When\n- [ ]\n")),
            &repos(),
            TODAY,
        );
        assert!(
            codes(&v).contains(&"no_done_when"),
            "placeholder must not count"
        );
    }

    #[test]
    fn specific_requires_a_known_repo_and_a_real_title() {
        let v = check(
            &task("fix it id:t1 est:2h day:2026-08-03\n"),
            Some(&good_doc()),
            &repos(),
            TODAY,
        );
        let c = codes(&v);
        assert!(c.contains(&"no_repo"));
        assert!(c.contains(&"too_terse"));

        let v = check(
            &task(
                "fix the SSE reconnect storm on flaky LAN id:t1 repo:nope est:2h day:2026-08-03\n",
            ),
            Some(&good_doc()),
            &repos(),
            TODAY,
        );
        assert!(codes(&v).contains(&"unknown_repo"));
    }

    #[test]
    fn an_unknown_repo_is_not_flagged_when_no_repo_set_is_supplied() {
        // Callers that cannot enumerate repos (a bare unit test, an early boot) must not have
        // every task fail on `unknown_repo`.
        let v = check(
            &task("fix the SSE reconnect storm on flaky LAN id:t1 repo:whatever est:2h day:2026-08-03\n"),
            Some(&good_doc()),
            &HashSet::new(),
            TODAY,
        );
        assert!(!codes(&v).contains(&"unknown_repo"));
    }

    #[test]
    fn a_vague_opener_is_allowed_when_the_doc_says_why() {
        let vague =
            task("investigate the SSE reconnect storm id:t1 repo:bn est:2h day:2026-08-03\n");

        let without = check(
            &vague,
            Some(&doc("## Done When\n- [ ] find the cause\n")),
            &repos(),
            TODAY,
        );
        assert!(codes(&without).contains(&"vague_verb"));

        let with = check(&vague, Some(&good_doc()), &repos(), TODAY);
        assert!(
            !codes(&with).contains(&"vague_verb"),
            "a stated Why earns the vague opener: {:?}",
            codes(&with)
        );
    }

    #[test]
    fn achievable_needs_an_estimate_inside_a_day() {
        let none = check(
            &task("fix the SSE reconnect storm on flaky LAN id:t1 repo:bn day:2026-08-03\n"),
            Some(&good_doc()),
            &repos(),
            TODAY,
        );
        assert!(codes(&none).contains(&"no_estimate"));

        let huge = check(
            &task("fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:3d day:2026-08-03\n"),
            Some(&good_doc()),
            &repos(),
            TODAY,
        );
        assert!(codes(&huge).contains(&"too_large"));
    }

    #[test]
    fn the_estimate_ceiling_is_inclusive() {
        let at = task(
            "fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:480m day:2026-08-03\n",
        );
        assert!(
            check(&at, Some(&good_doc()), &repos(), TODAY).ok,
            "480 is allowed"
        );

        let over = task(
            "fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:481m day:2026-08-03\n",
        );
        assert!(codes(&check(&over, Some(&good_doc()), &repos(), TODAY)).contains(&"too_large"));
    }

    #[test]
    fn a_blocker_needs_a_person_not_the_improve_agent() {
        // An agent cannot unblock a task; looping it through improve would burn a lane every ten
        // minutes until the backstop gave up.
        let v = check(
            &good_task(),
            Some(&doc(
                "## Done When\n- [ ] it works\n\n## Blockers\nWaiting on the API team.\n",
            )),
            &repos(),
            TODAY,
        );
        assert!(codes(&v).contains(&"blocked"));
        assert!(v.needs_human(), "must route to a human");
        let blocked = v.gaps.iter().find(|g| g.code == "blocked").unwrap();
        assert!(blocked.human_only);
    }

    #[test]
    fn relevance_accepts_an_issue_a_quarter_goal_or_a_near_date() {
        let base = "fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:2h";

        // Dated far out: time-bound is satisfied, relevance is not.
        let far = task(&format!("{base} day:2027-01-01\n"));
        assert!(codes(&check(&far, Some(&good_doc()), &repos(), TODAY)).contains(&"not_relevant"));

        // A linked issue rescues it.
        let sourced = task(&format!(
            "{base} day:2027-01-01 src:https://github.com/eduuh/bn/issues/963\n"
        ));
        assert!(
            !codes(&check(&sourced, Some(&good_doc()), &repos(), TODAY)).contains(&"not_relevant")
        );

        // So does a quarter goal.
        let aligned = task(&format!("{base} day:2027-01-01 q:2026-Q3\n"));
        assert!(
            !codes(&check(&aligned, Some(&good_doc()), &repos(), TODAY)).contains(&"not_relevant")
        );
    }

    #[test]
    fn time_bound_rejects_missing_malformed_and_past_dates() {
        let base =
            "fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:2h src:https://x.test/1";

        let undated = task(&format!("{base}\n"));
        assert!(codes(&check(&undated, Some(&good_doc()), &repos(), TODAY)).contains(&"no_date"));

        let malformed = task(&format!("{base} day:soon\n"));
        assert!(codes(&check(&malformed, Some(&good_doc()), &repos(), TODAY)).contains(&"no_date"));

        let past = task(&format!("{base} day:2020-01-01\n"));
        assert!(codes(&check(&past, Some(&good_doc()), &repos(), TODAY)).contains(&"stale_date"));
    }

    #[test]
    fn today_itself_is_not_stale() {
        let t =
            task("fix the SSE reconnect storm on flaky LAN id:t1 repo:bn est:2h day:2026-08-03\n");
        assert!(!codes(&check(&t, Some(&good_doc()), &repos(), TODAY)).contains(&"stale_date"));
    }

    #[test]
    fn every_gap_carries_a_fix_and_a_stable_code() {
        let v = check(&task("x id:t1\n"), None, &repos(), TODAY);
        assert!(!v.gaps.is_empty());
        for g in &v.gaps {
            assert!(!g.code.is_empty());
            assert!(!g.message.is_empty(), "{:?} has no message", g.code);
            assert!(!g.suggested_fix.is_empty(), "{:?} has no fix", g.code);
        }
    }

    #[test]
    fn estimates_parse_in_every_accepted_shape() {
        assert_eq!(parse_est_minutes("90"), Some(90));
        assert_eq!(parse_est_minutes("90m"), Some(90));
        assert_eq!(parse_est_minutes("2h"), Some(120));
        assert_eq!(parse_est_minutes("1h30m"), Some(90));
        assert_eq!(parse_est_minutes("1h30"), Some(90));
        assert_eq!(parse_est_minutes("1d"), Some(1440));
        assert_eq!(parse_est_minutes(""), None);
        assert_eq!(parse_est_minutes("soon"), None);
        assert_eq!(parse_est_minutes("0"), None);
    }
}
