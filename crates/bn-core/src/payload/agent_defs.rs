//! Markdown agent-roster payloads — the *definition* side of agents, as `/agents` reads them.
//!
//! Kept apart from the triggerable catalog in [`crate::payload::catalog_data`], which answers a
//! different question ("what can the automation fire?") and therefore folds in plain cron jobs and
//! drops specialists. These builders answer "what agents are defined, and what do they say?".

use serde_json::{json, Value};

/// One Markdown agent definition, as the roster view needs it.
///
/// Hand-built rather than `#[derive(Serialize)]` on [`crate::agents::AgentDef`]: the def carries
/// its whole prompt body, and a roster that shipped every body would be megabytes of JSON to
/// render one table. The body is fetched per agent by the `/api/agent-defs/{name}` route instead.
fn agent_def_row(a: &crate::agents::AgentDef) -> Value {
    json!({
        "name": a.name,
        "kind": a.kind,
        "scope": a.scope.label(),
        "path": a.path,
        "description": a.description,
        "schedule": a.on_calendar,
        "startup": a.is_startup(),
        "on_demand": a.on_calendar.is_none() && !a.is_startup(),
        "specialist": a.is_specialist(),
        "harness": a.harness,
        "host_repo": a.host_repo,
        "host_session": a.host_session,
        "host_window": a.host_window,
        "model": a.model,
        "run": a.run,
        "gather": a.gather,
        "repo": a.repo,
        "network": a.network,
        "catch_up": a.catch_up,
        "queue": a.queue,
        "retry_after_secs": a.retry_after_secs,
        "skills": a.skills,
        "mcps": a.mcps,
        "arguments": a.arguments.iter().map(|arg| json!({
            "name": arg.name,
            "description": arg.description,
            "required": arg.required,
        })).collect::<Vec<_>>(),
    })
}

/// `GET /api/agent-defs` → the Markdown agent roster: every agent definition discovered across the
/// tiers, with the metadata a human needs to tell them apart, but without their prompt bodies.
///
/// Unlike [`crate::payload::catalog_data`] this is the *definition* roster rather than the
/// triggerable one, so it deliberately includes specialists (installable, not runnable) — you can
/// read one even though nothing can fire it. `count` makes an empty roster definitive rather than a
/// missing field.
pub fn agent_defs_data(agents: &[crate::agents::AgentDef]) -> Value {
    let rows: Vec<Value> = agents.iter().map(agent_def_row).collect();
    json!({ "count": rows.len(), "agents": rows })
}

/// `GET /api/agent-defs/{name}` → one agent definition plus the verbatim `.md` file behind it.
///
/// `source` is the file as written — frontmatter and all — not [`crate::agents::AgentDef::body`],
/// which has already been stripped of its frontmatter and trimmed. The point of the view is to
/// read what is actually on disk.
pub fn agent_def_data(agent: &crate::agents::AgentDef, source: &str) -> Value {
    json!({ "agent": agent_def_row(agent), "source": source })
}
