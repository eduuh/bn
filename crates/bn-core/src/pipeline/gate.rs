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
use crate::todo::Task;

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
    let file = crate::todo::load(&cfg.tasks_file()).ok()?;
    let found = file.tasks().find(|t| {
        t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(repo))
            && t.meta("branch")
                .is_some_and(|b| b.eq_ignore_ascii_case(branch))
    });
    found.cloned()
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
/// queued?" and already holds it — a task accepted on Today has no branch yet, so there is no
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

/// Stamp a passing verdict, clearing any stale failure bookkeeping.
fn mark_ok(cfg: &Config, id: &str) {
    let path = cfg.tasks_file();
    let _lock = crate::todo::lock(&path);
    let Ok(mut file) = crate::todo::load(&path) else {
        return;
    };
    let Some(task) = file.task_mut(id) else {
        return;
    };
    task.set_meta(SMART_KEY, "ok");
    // Reset rather than delete, so a task that failed twice and then passed doesn't carry a
    // stale attempt count into its next judgement.
    task.set_meta(SMART_TRIES_KEY, "0");
    let _ = crate::todo::save(&path, &file);
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
    let path = cfg.tasks_file();
    {
        let _lock = crate::todo::lock(&path);
        if let Ok(mut file) = crate::todo::load(&path) {
            if let Some(task) = file.task_mut(id) {
                task.set_meta(SMART_KEY, if stuck { "stuck" } else { "fail" });
                task.set_meta(SMART_TRIES_KEY, &tries.to_string());
                task.set_meta(SMART_AT_KEY, &crate::util::now_epoch_secs().to_string());
                let _ = crate::todo::save(&path, &file);
            }
        }
    }

    let task = crate::todo::load(&path)
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
