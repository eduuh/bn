use super::*;

const ALL_KINDS: [(StuckLaneKind, &str); 4] = [
    (StuckLaneKind::InteractivePrompt, "interactive-prompt"),
    (StuckLaneKind::McpLoading, "mcp-loading"),
    (StuckLaneKind::WorkingIdle, "working-idle"),
    (StuckLaneKind::IdleWithWip, "idle-with-wip"),
];

fn lane(kind: StuckLaneKind, reason: &str) -> StuckLane {
    StuckLane {
        kind,
        reason: reason.into(),
        suggested_unblock: "answer the prompt".into(),
    }
}

#[test]
fn labels_match_the_serialized_kebab_case() {
    for (kind, label) in ALL_KINDS {
        assert_eq!(kind.label(), label);
        assert_eq!(serde_json::to_value(kind).unwrap(), label);
        let back: StuckLaneKind = serde_json::from_value(label.into()).unwrap();
        assert_eq!(back, kind);
    }
}

/// Each distinct prompt is its own stuck episode, so the key carries the reason; every other kind
/// is one episode however its reason text drifts.
#[test]
fn only_an_interactive_prompt_keys_on_its_reason() {
    assert_eq!(
        lane(StuckLaneKind::InteractivePrompt, "trust menu").key(),
        "interactive-prompt:trust menu"
    );
    assert_ne!(
        lane(StuckLaneKind::InteractivePrompt, "trust menu").key(),
        lane(StuckLaneKind::InteractivePrompt, "question").key()
    );
    for (kind, label) in &ALL_KINDS[1..] {
        assert_eq!(lane(*kind, "idle 12m").key(), *label);
        assert_eq!(lane(*kind, "a").key(), lane(*kind, "b").key());
    }
}

#[test]
fn summary_names_the_lane_reason_and_unblock() {
    assert_eq!(
        lane(StuckLaneKind::WorkingIdle, "no output for 12m").summary("bn:feat-x"),
        "bn:feat-x is blocked: no output for 12m; suggested unblock: answer the prompt"
    );
}

#[test]
fn a_stuck_lane_round_trips_through_json() {
    let original = lane(StuckLaneKind::McpLoading, "mcp servers still loading");
    let text = serde_json::to_string(&original).unwrap();
    assert!(text.contains("\"kind\":\"mcp-loading\""));
    assert_eq!(serde_json::from_str::<StuckLane>(&text).unwrap(), original);
}
