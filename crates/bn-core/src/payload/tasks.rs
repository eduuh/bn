//! Task-store payloads shared by the web dashboard and any future CLI consumers.

use serde_json::Value;

/// Filters for the complete task-store ledger.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskLedgerFilter<'a> {
    pub query: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub smart: Option<&'a str>,
    pub stage: Option<&'a str>,
    pub day: Option<&'a str>,
    pub state: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

fn status_name(status: crate::todo::Status) -> &'static str {
    match status {
        crate::todo::Status::Open => "open",
        crate::todo::Status::Wip => "wip",
        crate::todo::Status::Snoozed => "snoozed",
        crate::todo::Status::Done => "done",
        crate::todo::Status::Cancelled => "cancelled",
    }
}

/// The complete task store, newest-first, with bounded rows and lifecycle counts.
///
/// The task file never purges terminal rows, so this is the durable all-time work ledger. Counts
/// are computed after text/repo/gate filters but before the lifecycle filter, letting a UI switch
/// tabs without losing the population behind the other tabs.
pub fn task_ledger_data(
    file: &crate::todo::TodoFile,
    today: &str,
    filter: TaskLedgerFilter<'_>,
) -> Value {
    let query_terms: Vec<String> = filter
        .query
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();

    let mut tasks: Vec<&crate::todo::Task> = file
        .tasks()
        .filter(|task| {
            filter.repo.is_none_or(|want| {
                task.meta("repo")
                    .is_some_and(|repo| repo.eq_ignore_ascii_case(want))
            })
        })
        .filter(|task| {
            filter.stage.is_none_or(|want| {
                task.meta("stage")
                    .is_some_and(|stage| stage.eq_ignore_ascii_case(want))
            })
        })
        .filter(|task| match filter.smart {
            None => true,
            Some("unjudged") => task.meta("smart").is_none(),
            Some(want) => task
                .meta("smart")
                .is_some_and(|smart| smart.eq_ignore_ascii_case(want)),
        })
        .filter(|task| filter.day.is_none_or(|want| task.meta("day") == Some(want)))
        .filter(|task| {
            if query_terms.is_empty() {
                return true;
            }
            let haystack = task.render().to_ascii_lowercase();
            query_terms.iter().all(|term| haystack.contains(term))
        })
        .collect();

    tasks.sort_by(|a, b| {
        b.modified()
            .cmp(&a.modified())
            .then_with(|| b.id().cmp(&a.id()))
    });

    let mut counts = serde_json::Map::from_iter([
        ("open".to_string(), serde_json::json!(0)),
        ("wip".to_string(), serde_json::json!(0)),
        ("snoozed".to_string(), serde_json::json!(0)),
        ("done".to_string(), serde_json::json!(0)),
        ("cancelled".to_string(), serde_json::json!(0)),
    ]);
    for task in &tasks {
        let key = status_name(task.status(today));
        let next = counts[key].as_u64().unwrap_or_default() + 1;
        counts.insert(key.to_string(), serde_json::json!(next));
    }

    if let Some(state) = filter.state {
        tasks.retain(|task| status_name(task.status(today)) == state);
    }

    let total = tasks.len();
    let limit = filter.limit.clamp(1, 200);
    let rows = tasks
        .into_iter()
        .skip(filter.offset)
        .take(limit)
        .map(|task| task.to_json(today))
        .collect::<Vec<_>>();

    serde_json::json!({
        "today": today,
        "all": true,
        "status": "all",
        "query": filter.query,
        "repo": filter.repo,
        "smart": filter.smart,
        "stage": filter.stage,
        "day": filter.day,
        "state": filter.state,
        "offset": filter.offset,
        "limit": limit,
        "total": total,
        "count": rows.len(),
        "counts": counts,
        "tasks": rows,
    })
}
