//! Minimal execution controls and fan-out state in the existing pipeline event stream.
use crate::events;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DispatchPolicy {
    pub skill: Option<String>,
    pub verify: bool,
    pub plan_review: bool,
}

fn matches(event: &events::Event, repo: &str, branch: &str) -> bool {
    event.fields.get("repo").and_then(|v| v.as_str()) == Some(repo)
        && event.fields.get("branch").and_then(|v| v.as_str()) == Some(branch)
}

pub fn record_policy(repo: &str, branch: &str, policy: &DispatchPolicy) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("repo".into(), json!(repo));
    fields.insert("branch".into(), json!(branch));
    fields.insert("policy".into(), json!(policy));
    events::append("pipeline_dispatch", fields)
}

pub fn policy(repo: &str, branch: &str) -> DispatchPolicy {
    events::read_all()
        .into_iter()
        .rev()
        .find(|event| event.kind == "pipeline_dispatch" && matches(event, repo, branch))
        .and_then(|event| serde_json::from_value(event.fields.get("policy")?.clone()).ok())
        .unwrap_or_default()
}

pub fn record_fanout(repo: &str, branch: &str, token: &str, status: &str) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("repo".into(), json!(repo));
    fields.insert("branch".into(), json!(branch));
    fields.insert("token".into(), json!(token));
    fields.insert("status".into(), json!(status));
    events::append("pipeline_fanout", fields)
}

pub fn fanouts(repo: &str, branch: &str) -> BTreeMap<String, String> {
    let mut state = BTreeMap::new();
    for event in events::read_all()
        .into_iter()
        .filter(|event| event.kind == "pipeline_fanout" && matches(event, repo, branch))
    {
        if let (Some(token), Some(status)) = (
            event.fields.get("token").and_then(|v| v.as_str()),
            event.fields.get("status").and_then(|v| v.as_str()),
        ) {
            state.insert(token.to_string(), status.to_string());
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_controls_and_stage_events_survive_reload_without_notes() {
        crate::test_support::with_state_dir_path(|root| {
            let cfg = crate::test_support::config(&root.join("stores"));
            let ctx = crate::resolver::Resolved {
                repo: "repo".into(),
                branch: "work".into(),
            };
            record_policy(
                "repo",
                "work",
                &DispatchPolicy {
                    skill: Some("ship-solo".into()),
                    verify: true,
                    plan_review: true,
                },
            )
            .unwrap();
            record_policy("other", "work", &DispatchPolicy::default()).unwrap();
            record_fanout("repo", "work", "child", "pending").unwrap();
            record_fanout("repo", "work", "child", "done").unwrap();
            record_fanout("repo", "sibling", "other", "blocked").unwrap();
            let stage = crate::pipeline::stage::Stage::Validate;
            assert!(crate::pipeline::stage::transition(&cfg, &ctx, stage)
                .unwrap()
                .admits());
            let loaded = policy("repo", "work");
            assert_eq!(loaded.skill.as_deref(), Some("ship-solo"));
            assert!(loaded.verify && loaded.plan_review);
            assert_eq!(
                fanouts("repo", "work"),
                BTreeMap::from([("child".into(), "done".into())])
            );
            assert_eq!(crate::pipeline::stage::current("repo", "work"), Some(stage));
            assert!(!cfg.artifact_dir(&ctx).exists());
        });
    }
}
