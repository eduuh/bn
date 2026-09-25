//! The one place the SMART bar is enforced (#998, EPIC #993).
//!
//! [`crate::smart::check`] decides *whether* a task is ready. This module is the only thing that
//! acts on the answer, and it is called from exactly one place: [`super::stage::transition`], on
//! entry to [`Stage::Investigate`]. Everything that starts work — `bn dispatch`, `bn attn start`,
//! `bn pipeline set` — records its stage through `transition`, so all of them are gated by
//! construction rather than by remembering to call a checker.
//!
//! **Do not add a second call site.** Two copies of a gate drift, and the drift is
//! security-relevant once #1003 lets tasks launch unattended agents.
//!
//! # What refusal does
//!
//! Stamps the verdict onto the task (`smart:fail`, `smart_at:`, `smart_tries:`) and raises one
//! idempotent attention item keyed `attn:smart:<task-id>` carrying the gap codes and their fixes.
//! The enhance agent (#1002) picks that up, rewrites the context doc, and the next transition
//! re-checks.
//!
//! # The backstop
//!
//! After [`MAX_IMPROVE_TRIES`] failed attempts the task is stamped `smart:stuck` and the raised
//! item becomes **non-auto**, so it waits for a person. Without this, a task the model cannot fix
//! consumes an idle lane every ten minutes, all night, forever. A gap the model *cannot* close by
//! construction ([`crate::smart::SmartVerdict::needs_human`]) skips straight there.

use super::stage::Stage;
use crate::resolver::Config;
use crate::smart::{self, SmartVerdict};
use crate::todo::{Task, TodoFile};

/// How many times the enhance agent may try before a task is handed to a person.
pub const MAX_IMPROVE_TRIES: i64 = 3;

/// Meta keys the gate writes onto a task.
pub const SMART_KEY: &str = "smart";
pub const SMART_AT_KEY: &str = "smart_at";
pub const SMART_TRIES_KEY: &str = "smart_tries";

/// What the gate decided about one transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Not a gated transition, or the task passed.
    Admitted,
    /// No task governs this worktree, so there is nothing to judge.
    ///
    /// Admitted, deliberately: refusing would break every worktree that predates the task store.
    /// #1001 closes this hole from the other side, by making it impossible for work to enter the
    /// orchestrate backlog without a `task:<id>`.
    Ungoverned,
    /// The task is not ready. The stamp and the attention item have already been written.
    Refused {
        task_id: String,
        verdict: SmartVerdict,
        tries: i64,
        /// True when this task has exhausted its improve attempts (or needs a human anyway) and
        /// is now waiting on a person.
        stuck: bool,
    },
}

impl GateDecision {
    /// Whether the transition may proceed.
    pub fn admits(&self) -> bool {
        !matches!(self, GateDecision::Refused { .. })
    }

    /// A one-line explanation for a CLI to print on refusal.
    pub fn refusal_line(&self) -> Option<String> {
        match self {
            GateDecision::Refused {
                task_id,
                verdict,
                tries,
                stuck,
            } => Some(format!(
                "{task_id} is not ready to execute ({}){}",
                verdict.codes().join(", "),
                if *stuck {
                    " — needs you; the enhance agent has given up".to_string()
                } else {
                    format!(" — enhance attempt {tries}/{MAX_IMPROVE_TRIES} queued")
                }
            )),
            _ => None,
        }
    }
}

/// Whether entering `stage` needs SMART judgement. Only the step into real work does.
///
/// Deliberately keyed on the *destination* rather than the (from, to) pair: `transition` records a
/// stage without knowing where the worktree came from, and gating every entry into `investigate`
/// is both simpler and stricter than gating one specific edge.
///
/// This is the *machine* gate — it asks whether the task is well-formed enough to work on, and a
/// refusal is handed to the enhance agent, not to a person. The human's per-stage `hold` policy is
/// a separate question asked after this one; see [`super::stage::transition`] for the ordering.
pub fn is_smart_gated(stage: Stage) -> bool {
    stage == Stage::Investigate
}

/// The task governing `(repo, branch)`, if the store has one.
///
/// Matched on `branch:` first — a worktree is one task's — falling back to nothing. A repo-only
/// match would be wrong: several tasks share a repo, and judging the wrong one is worse than
/// judging none.
pub fn governing_task(cfg: &Config, repo: &str, branch: &str) -> Option<Task> {
    let file = crate::task_store::task_store(cfg).load().ok()?;
    governing_in(&file, repo, branch).cloned()
}

/// [`governing_task`] over a list already loaded, for a caller that goes on to write through the
/// same store.
pub fn governing_in<'a>(file: &'a TodoFile, repo: &str, branch: &str) -> Option<&'a Task> {
    file.tasks().find(|t| {
        t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(repo))
            && t.meta("branch")
                .is_some_and(|b| b.eq_ignore_ascii_case(branch))
    })
}

/// Judge a transition, and realize its consequences.
///
/// The only caller is [`super::stage::transition`]. Returns [`GateDecision::Admitted`] for any
/// ungated stage, so it is safe to call unconditionally.
pub fn check_entry(
    cfg: &Config,
    repo: &str,
    branch: &str,
    stage: Stage,
    today: &str,
) -> GateDecision {
    if !is_smart_gated(stage) {
        return GateDecision::Admitted;
    }
    let Some(task) = governing_task(cfg, repo, branch) else {
        return GateDecision::Ungoverned;
    };
    judge(cfg, &task, today)
}

/// Judge one task directly, and realize the same consequences [`check_entry`] does.
///
/// The gate's body, reachable without a worktree. `check_entry` asks "may this worktree enter real
/// work?" and has to find the task first; the `today` producer asks "is this task ready to be
/// started?" and already holds it — a task accepted on Today has no branch yet, so there is no
/// `(repo, branch)` to look it up by.
///
/// One body rather than two, because the consequences are the load-bearing half: the `smart:` stamp,
/// the try counter, and the attention item raised at the three-strike backstop. A second copy of the
/// check that skipped those would let a task fail the gate forever without ever being handed to the
/// enhance agent or to a person.
pub fn judge(cfg: &Config, task: &Task, today: &str) -> GateDecision {
    let Some(id) = task.id().map(str::to_string) else {
        return GateDecision::Ungoverned;
    };

    // Machine-minted work (`auto:1`) is not judged. The bar exists so that a *person's* vague
    // sentence is sharpened before an agent burns an hour on it, and refusal routes the task to the
    // enhance agent to be rewritten. Neither half applies here: a repo setup agent's prompt already
    // states its done-when in full, and there is no author to hand it back to — the machine wrote
    // it. Gating it would mean the enhance agent rewriting a generated prompt three times and then
    // raising an attention item asking you to scope a chore you never chose. The work simply never
    // runs, which is the failure this admits.
    if crate::task_owner::is_machine(task) {
        return GateDecision::Admitted;
    }

    let known = crate::issues::known_repos(cfg).into_iter().collect();
    let doc = crate::task_doc::read(cfg, task);
    let verdict = smart::check(task, doc.as_ref(), &known, today);

    if verdict.ok {
        mark_ok(cfg, &id);
        return GateDecision::Admitted;
    }

    // `smart:stuck` is terminal: the task is already waiting on a person, and re-judging cannot
    // change that. Repeat the refusal from the recorded counter without touching the store.
    //
    // Without this short-circuit every re-offer of a stuck task re-stamps `tasks.txt`. A producer
    // that keeps re-accepting the same suggestion therefore rewrites the task store forever —
    // observed on a stuck dojo-quiz task at roughly one write every 23s and 3,899 accumulated
    // tries, which also kept the notes repo permanently dirty and fed the commit-notes churn. The
    // attention item is keyed and already raised, so repeating it buys nothing either.
    if task.meta(SMART_KEY) == Some("stuck") {
        return GateDecision::Refused {
            task_id: id,
            verdict,
            tries: task
                .meta(SMART_TRIES_KEY)
                .and_then(|v| v.parse().ok())
                .unwrap_or(MAX_IMPROVE_TRIES),
            stuck: true,
        };
    }

    let tries = task
        .meta(SMART_TRIES_KEY)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        + 1;
    let stuck = verdict.needs_human() || tries >= MAX_IMPROVE_TRIES;
    record_refusal(cfg, &id, &verdict, tries, stuck, today);

    GateDecision::Refused {
        task_id: id,
        verdict,
        tries,
        stuck,
    }
}

/// Stamp `smart:ok` on every open task that already passes, and report how many were stamped.
///
/// The free half of the gate. [`smart::check`] is pure and the stamp is one file write, so judging
/// the whole store costs no model time at all — which matters because a task that already passes is
/// otherwise invisible to every producer: it is never stamped, so it never becomes startable, never
/// leaves the enhance queue, and is re-picked by `--due` on the next run forever.
///
/// Only *passing* tasks are touched. A refusal increments the try counter and raises an attention
/// item, and both mean "an enhance attempt happened" — a sweep is not an attempt. Judging the whole
/// store through [`judge`] would therefore march every unscoped task to `smart:stuck` in
/// [`MAX_IMPROVE_TRIES`] passes and raise an attention item for each, without an agent having tried
/// once. Failing tasks are left exactly as they were, for the enhance agent to work through.
pub fn stamp_passing(cfg: &Config, today: &str) -> usize {
    let known: std::collections::HashSet<String> =
        crate::issues::known_repos(cfg).into_iter().collect();
    let store = crate::task_store::task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
        return 0;
    };

    // Judge against the store as read, then apply — `task_mut` borrows mutably, so the ids are
    // collected first. Machine-minted work is exempt from the gate, so it is exempt here too.
    let passing: Vec<String> = file
        .tasks()
        .filter(|t| t.is_open(today))
        .filter(|t| t.meta(SMART_KEY) != Some("ok"))
        .filter(|t| !crate::task_owner::is_machine(t))
        .filter(|t| {
            let doc = crate::task_doc::read(cfg, t);
            smart::check(t, doc.as_ref(), &known, today).ok
        })
        .filter_map(|t| t.id().map(str::to_string))
        .collect();

    for id in &passing {
        if let Some(task) = file.task_mut(id) {
            task.set_meta(SMART_KEY, "ok");
            task.set_meta(SMART_TRIES_KEY, "0");
        }
    }
    if !passing.is_empty() {
        let _ = store.save(&file);
    }
    passing.len()
}

/// Stamp a passing verdict on one task by id — the single-task half of [`stamp_passing`].
pub fn mark_passing(cfg: &Config, id: &str) {
    mark_ok(cfg, id);
}

/// Stamp a passing verdict, clearing any stale failure bookkeeping.
fn mark_ok(cfg: &Config, id: &str) {
    let store = crate::task_store::task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
        return;
    };
    let Some(task) = file.task_mut(id) else {
        return;
    };
    task.set_meta(SMART_KEY, "ok");
    // Reset rather than delete, so a task that failed twice and then passed doesn't carry a
    // stale attempt count into its next judgement.
    task.set_meta(SMART_TRIES_KEY, "0");
    let _ = store.save(&file);
}

/// The refusal as one line for a Forge comment: each gap's message, and who acts next.
fn refusal_reason(verdict: &SmartVerdict, stuck: bool) -> String {
    let gaps = verdict
        .gaps
        .iter()
        .map(|g| g.message.trim_end_matches('.').to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if stuck {
        format!("{gaps}. bn cannot fill this in by itself; please add it to the task.")
    } else {
        format!("{gaps}. bn will try to fill this in.")
    }
}

/// Stamp the failure and raise the (idempotent) attention item that gets it fixed.
fn record_refusal(
    cfg: &Config,
    id: &str,
    verdict: &SmartVerdict,
    tries: i64,
    stuck: bool,
    today: &str,
) {
    let store = crate::task_store::task_store(cfg);
    {
        let _lock = store.lock();
        if let Ok(mut file) = store.load() {
            if let Some(task) = file.task_mut(id) {
                task.set_meta(SMART_KEY, if stuck { "stuck" } else { "fail" });
                task.set_meta(SMART_TRIES_KEY, &tries.to_string());
                task.set_meta(SMART_AT_KEY, &crate::util::now_epoch_secs().to_string());
                let _ = store.save(&file);
            }
        }
    }

    let task = store
        .load()
        .ok()
        .and_then(|f| f.tasks().find(|t| t.id() == Some(id)).cloned());
    let (repo, branch) = task
        .as_ref()
        .map(|t| {
            (
                t.meta("repo").map(str::to_string),
                t.meta("branch").map(str::to_string),
            )
        })
        .unwrap_or((None, None));
    let summary = task.as_ref().map(|t| t.summary()).unwrap_or_default();

    // The person who assigned the task in Forge hears why it has not started. The file store has
    // nowhere to put this, so it never gets here.
    if store.kind() == crate::task_store::TaskProviderKind::ForgeApp {
        let text = format!("needs more detail: {}", refusal_reason(verdict, stuck));
        crate::task_store::note_best_effort(store.as_ref(), id, &text);
    }

    let detail = verdict
        .gaps
        .iter()
        .map(|g| format!("- [{}] {}\n  fix: {}", g.code, g.message, g.suggested_fix))
        .collect::<Vec<_>>()
        .join("\n");

    let title = if stuck {
        format!("Task {id} cannot be made ready automatically: {summary}")
    } else {
        format!("Task {id} is not ready to execute: {summary}")
    };
    // A bn-authored prompt that points at the durable record, following `promote_todo`'s
    // discipline — the agent reads the task and its doc rather than being handed their contents.
    let prompt = format!(
        "Run `bn task enhance {id}`. It failed the SMART gate on: {}. \
         Read the task and its context doc — including whatever the operator captured under \
         `## Context` and `## Links`, and any files `bn task attach {id}` resolves — then close \
         each gap and re-check.",
        verdict.codes().join(", ")
    );

    let key = format!("attn:smart:{id}");

    // `add_new` refreshes a keyed item's title/severity/detail in place — but **not its `mode`**.
    // So a task that was raised as `auto` (fixable by the enhance agent) and has now gone `stuck`
    // would keep its auto flag and stay eligible for unattended pickup: the loop would keep
    // grabbing a task nobody can fix. Retire the old record first so the replacement is genuinely
    // manual. Terminal removal also frees the key, which is what lets the fresh add through.
    if stuck {
        if let Some(existing) = crate::attention::live_by_key(&key) {
            if existing.mode() != "manual" {
                let _ = crate::attention::remove(&existing.id);
            }
        }
    }

    let new = crate::attention::NewAttn {
        // NOT "manual" (#1003): a task may have been pulled from a public issue, so its content
        // is not self-authored. The narrower `task` tier re-checks smart:ok, a known repo, and
        // that this prompt is bn-authored before anything runs unattended.
        source: crate::attention::SOURCE_TASK,
        title: &title,
        severity: if stuck { "warn" } else { "info" },
        repo: repo.as_deref(),
        branch: branch.as_deref(),
        link: None,
        key: Some(&key),
        detail: Some(&detail),
        // A stuck task waits for a person; a fixable one is fair game for the enhance agent.
        mode: Some(if stuck { "manual" } else { "auto" }),
        prompt: Some(&prompt),
        task_type: Some("planning"),
        worktree_path: None,
        uncommitted_files: Vec::new(),
    };
    let _ = crate::attention::add_new(&new, today);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::Task;

    const TODAY: &str = "2026-08-11";

    /// A config whose store lives in `root`, and which knows one repo (`bn`) so a task naming it
    /// does not trip the `unknown_repo` gap.
    fn cfg(root: &std::path::Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            managed_repos: vec!["https://github.com/me/bn.git".to_string()],
            ..Default::default()
        }
    }

    /// A task that passes every SMART dimension: known repo, sized, dated, and relevant via `q:`.
    /// Its `## Done When` is written by [`with_doc`].
    fn passing(id: &str) -> Task {
        let mut task = Task::new_todo(
            id,
            "collapse the goal write side onto tasks",
            None,
            "",
            "web",
            TODAY,
        );
        task.set_meta("repo", "bn");
        task.set_meta("est", "45m");
        task.set_meta("day", TODAY);
        task.set_meta("q", "ship the pipeline");
        task
    }

    fn with_doc(cfg: &Config, task: &Task) {
        let path =
            crate::task_doc::staging_path(cfg, &crate::task_doc::staging_name(task.id().unwrap()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "## Done When\n- [ ] the write side is gone\n").unwrap();
    }

    fn write_store(cfg: &Config, tasks: &[Task]) {
        std::fs::create_dir_all(&cfg.personal_root).unwrap();
        let mut file = crate::todo::TodoFile::parse("");
        for task in tasks {
            file.push_task(task.clone());
        }
        crate::todo::save(&cfg.tasks_file(), &file).unwrap();
    }

    fn reload(cfg: &Config, id: &str) -> Task {
        crate::todo::load(&cfg.tasks_file())
            .unwrap()
            .tasks()
            .find(|t| t.id() == Some(id))
            .cloned()
            .unwrap()
    }

    #[test]
    fn a_task_that_already_passes_is_stamped_ready() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let task = passing("t1");
        write_store(&cfg, std::slice::from_ref(&task));
        with_doc(&cfg, &task);

        assert_eq!(stamp_passing(&cfg, TODAY), 1);
        assert_eq!(reload(&cfg, "t1").meta(SMART_KEY), Some("ok"));
    }

    /// The gate does not judge machine-minted work, so neither may the sweep — otherwise every
    /// cron chore acquires a verdict the gate itself would never give it.
    #[test]
    fn machine_minted_work_is_never_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut task = passing("t2");
        crate::task_owner::mark_machine(&mut task);
        write_store(&cfg, std::slice::from_ref(&task));
        with_doc(&cfg, &task);

        assert_eq!(stamp_passing(&cfg, TODAY), 0);
        assert_eq!(reload(&cfg, "t2").meta(SMART_KEY), None);
    }

    /// The invariant that keeps the sweep honest: a refusal means "an enhance attempt happened",
    /// and a sweep is not an attempt. Judging every failing task would march the whole backlog to
    /// `smart:stuck` in [`MAX_IMPROVE_TRIES`] passes and raise an attention item for each, without
    /// an agent having tried once.
    #[test]
    fn a_failing_task_is_left_completely_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        // No doc, no est, no date — fails on several dimensions at once.
        let mut task = Task::new_todo(
            "t3",
            "look into the reconnect storm",
            None,
            "",
            "web",
            TODAY,
        );
        task.set_meta("repo", "bn");
        write_store(&cfg, &[task]);

        assert_eq!(stamp_passing(&cfg, TODAY), 0);
        let after = reload(&cfg, "t3");
        assert_eq!(after.meta(SMART_KEY), None, "no verdict may be stamped");
        assert_eq!(
            after.meta(SMART_TRIES_KEY),
            None,
            "no attempt may be counted"
        );
    }

    /// A task already handed to a person is terminal. Re-judging it must not count another
    /// attempt or rewrite the store: a producer that keeps re-offering the same stuck task would
    /// otherwise rewrite `tasks.txt` on every pass forever (observed at ~1 write/23s and 3,899
    /// accumulated tries on one dojo-quiz task), keeping the notes repo permanently dirty.
    #[test]
    fn a_stuck_task_is_not_regraded() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut task = Task::new_todo("t9", "quiz · explain something", None, "", "web", TODAY);
        task.set_meta("repo", "bn");
        task.set_meta(SMART_KEY, "stuck");
        task.set_meta(SMART_TRIES_KEY, "3");
        task.set_meta(SMART_AT_KEY, "1787768779");
        write_store(&cfg, std::slice::from_ref(&task));

        let decision = judge(&cfg, &task, TODAY);
        assert!(
            matches!(decision, GateDecision::Refused { stuck: true, .. }),
            "a stuck task still refuses admission"
        );

        let after = reload(&cfg, "t9");
        assert_eq!(
            after.meta(SMART_TRIES_KEY),
            Some("3"),
            "re-judging must not count another attempt"
        );
        assert_eq!(
            after.meta(SMART_AT_KEY),
            Some("1787768779"),
            "re-judging must not restamp the task store"
        );
    }

    /// A store with both kinds: only the passing human task moves.
    #[test]
    fn a_sweep_stamps_only_what_passes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let good = passing("t4");
        let mut bad = passing("t5");
        bad.set_meta("est", ""); // unsized → no_estimate
        write_store(&cfg, &[good.clone(), bad]);
        with_doc(&cfg, &good);

        assert_eq!(stamp_passing(&cfg, TODAY), 1);
        assert_eq!(reload(&cfg, "t4").meta(SMART_KEY), Some("ok"));
        assert_eq!(reload(&cfg, "t5").meta(SMART_KEY), None);
    }
}
