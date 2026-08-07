use bn_core::queue::tick::Dispatch;
use serde_json::{Map, Value};

/// The `queue_finish` payload, shared by inline execution and heavy work's callback so the two
/// cannot describe the same event differently.
pub(super) fn finish_fields(repo: &str, id: &str, exit_code: i32) -> Map<String, Value> {
    Map::from_iter([
        ("repo".to_string(), Value::String(repo.to_string())),
        ("id".to_string(), Value::String(id.to_string())),
        ("exit_code".to_string(), Value::from(exit_code)),
        (
            "outcome".to_string(),
            Value::String(if exit_code == 0 { "completed" } else { "failed" }.to_string()),
        ),
    ])
}

pub(super) fn launch_failure_fields(repo: &str, id: &str, error: &str) -> Map<String, Value> {
    Map::from_iter([
        ("repo".to_string(), Value::String(repo.to_string())),
        ("id".to_string(), Value::String(id.to_string())),
        ("outcome".to_string(), Value::String("failed".to_string())),
        ("phase".to_string(), Value::String("launch".to_string())),
        ("error".to_string(), Value::String(error.to_string())),
    ])
}

pub(super) fn dispatch_fields(d: &Dispatch, machine: &str) -> Map<String, Value> {
    let mut fields = Map::from_iter([
        ("repo".to_string(), Value::String(d.repo.clone())),
        ("id".to_string(), Value::String(d.id.clone())),
        ("item".to_string(), Value::String(d.item.clone())),
        // `work_kind`, not `kind`: the event envelope owns `kind`, and a payload field of that name
        // shadows it (see `bn_core::events::unshadowed`).
        (
            "work_kind".to_string(),
            Value::String(d.kind.label().to_string()),
        ),
        ("exec".to_string(), Value::String(d.exec.label().to_string())),
        ("target".to_string(), Value::String(d.target.label())),
        (
            "node".to_string(),
            Value::String(if d.target.is_local() {
                machine.to_string()
            } else {
                d.target.label()
            }),
        ),
        (
            "lane".to_string(),
            Value::String(d.work_class.label().to_string()),
        ),
        ("priority".to_string(), Value::from(d.priority)),
        (
            "rationale".to_string(),
            Value::String(d.rationale.clone()),
        ),
    ]);
    if let Some(harness) = &d.harness {
        fields.insert("harness".to_string(), Value::String(harness.clone()));
    }
    fields
}
