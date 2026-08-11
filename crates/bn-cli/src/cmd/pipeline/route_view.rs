//! `bn pipeline route` — what pipeline each task's complexity would buy it.
//!
//! Read-only on purpose. [`bn_core::pipeline::route`] decides a route and nothing consumes the
//! answer yet, so this is the whole feature for now: a table to look at, disagree with, and tune
//! the thresholds against, before a route is allowed to change what actually runs.
use crate::*;
use bn_core::pipeline::route::{self, RoutePlan};
use bn_core::todo::Task;

/// One task's line in the report.
struct Row {
    id: String,
    summary: String,
    plan: RoutePlan,
}

/// `bn pipeline route [--task <id>] [--all]`.
pub(crate) fn emit_pipeline_route(cfg: &Config, task: Option<&str>, all: bool, as_json: bool) {
    let path = crate::cmd::task::task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn pipeline route: {e}")));
    let today = today();

    let selected: Vec<Task> = match task {
        Some(id) => vec![file
            .tasks()
            .find(|t| t.id() == Some(id))
            .cloned()
            .unwrap_or_else(|| fail(&format!("bn pipeline route: no task {id}")))],
        None => file
            .tasks()
            .filter(|t| t.is_open(&today))
            // The default view is the work routing would actually see: what the sweep admits.
            // `--all` widens to everything open, machine bookkeeping included, for tuning against
            // the whole store rather than the handful currently through the gate.
            .filter(|t| all || !bn_core::task_owner::is_machine(t))
            .filter(|t| all || t.meta("smart") == Some("ok"))
            .cloned()
            .collect(),
    };

    let rows: Vec<Row> = selected
        .into_iter()
        .map(|t| Row {
            id: t.id().unwrap_or("?").to_string(),
            summary: t.summary(),
            plan: route::resolve(cfg, &t),
        })
        .collect();

    if as_json {
        let data = serde_json::json!({
            "count": rows.len(),
            "tasks": rows.iter().map(|r| serde_json::json!({
                "id": r.id,
                "summary": r.summary,
                "route": r.plan.route.as_str(),
                "guarded": r.plan.guarded,
                "stages": r.plan.stages.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "fanout_cap": r.plan.fanout_cap,
                "reviewers": r.plan.reviewers,
                "critic_rounds": r.plan.critic_rounds,
                "gate": r.plan.gate.as_str(),
                "reasons": r.plan.reasons,
            })).collect::<Vec<_>>(),
        });
        println!("{}", Envelope::success("pipeline_route", data).to_json());
        return;
    }

    if rows.is_empty() {
        println!("No tasks to route — nothing open has passed the SMART gate (try --all).");
        return;
    }

    println!("{:<7} {:<11} {:<38} REASONS", "ID", "ROUTE", "STAGES");
    for row in &rows {
        let stages = row
            .plan
            .stages
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        let label = if row.plan.guarded {
            format!("{}!", row.plan.route.as_str())
        } else {
            row.plan.route.as_str().to_string()
        };
        let reasons = row
            .plan
            .reasons
            .iter()
            .map(|r| r.code)
            .collect::<Vec<_>>()
            .join(", ");
        println!("{:<7} {label:<11} {stages:<38} {reasons}", row.id);
    }

    // The summary is the point of the whole command: a distribution you can argue with. A run
    // where every task lands in one bucket means the thresholds are not yet saying anything.
    let mut counts = [0usize; 3];
    for row in &rows {
        counts[row.plan.route as usize] += 1;
    }
    let guarded = rows.iter().filter(|r| r.plan.guarded).count();
    println!(
        "\n{} tasks — {} mechanical · {} standard · {} deep · {guarded} guarded",
        rows.len(),
        counts[route::Route::Mechanical as usize],
        counts[route::Route::Standard as usize],
        counts[route::Route::Deep as usize],
    );
    println!("Reporting only — no route changes what runs yet.");
}
