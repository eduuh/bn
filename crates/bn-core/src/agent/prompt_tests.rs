use super::*;
use crate::orchestrate::PaneObservation;

/// Copilot's `ask_user` form as the stuck Forge agent showed it (2026-09-23): a plan whose own
/// body is a numbered list, a tab header, the question, a description, a cursor menu, and a footer.
fn copilot_plan_form() -> Vec<String> {
    [
        "Proposed plan:",
        "1. Identify the exact networking regression from repo history and branch state, then record the chosen fix in bn.",
        "2. Make the smallest manifest/documentation change that addresses the regression without touching unrelated cluster config.",
        "3. Validate locally with repo-native checks that do not require live cluster access first.",
        "4. Update Forge with a Markdown report and close the activity only after validation is complete.",
        "Confirm this plan, or tell me to adjust scope before I implement.",
        "Proceed with this plan?   Optional adjustments",
        "Proceed with this plan?:",
        "Approve the outlined implementation/validation sequence so work can continue.",
        "❯ Yes",
        "No",
        "Other (type your answer)",
        "↑/↓ select · enter accept · tab next · ctrl+d decline · esc cancel",
        "────────────────────────────────────────────────────────",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn observe(tail: &[String]) -> PaneState {
    let obs = PaneObservation {
        tail,
        has_window: true,
        plane_a: "",
        has_wip: false,
    };
    crate::orchestrate::classify(&obs)
}

#[test]
fn copilot_cursor_menu_is_a_question_bn_can_read() {
    let tail = copilot_plan_form();
    assert_eq!(
        observe(&tail),
        PaneState::QuestionMenu,
        "its footer is not a spinner"
    );

    let prompt = parse_pending_prompt(&tail, "", true, false).expect("a readable dialog");
    assert!(prompt.cursor);
    assert_eq!(prompt.kind, PaneState::QuestionMenu);
    assert_eq!(prompt.question, "Proceed with this plan?");
    let labels: Vec<_> = prompt.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Yes", "No", "Other (type your answer)"]);
    assert!(prompt.options[0].selected && !prompt.options[1].selected);
    assert_eq!(prompt.resolve(PromptSemantic::Approve), Some(1));
    assert_eq!(prompt.resolve(PromptSemantic::Reject), Some(2));
    assert!(prompt.subject.unwrap().starts_with("Approve the outlined"));
}

#[test]
fn a_cursor_menu_is_never_auto_answered() {
    let prompt = parse_pending_prompt(&copilot_plan_form(), "", true, false).unwrap();
    let policy: crate::agent::ApprovalPolicy = toml::from_str(
        "[approve]\nenabled = true\nscope = \"all\"\n[[approve.rule]]\ncommands = [\"proceed\"]\n",
    )
    .unwrap();
    let ctx = crate::agent::PolicyCtx {
        lane: crate::agent::LaneKind::Dispatched,
        trusted: true,
    };
    assert!(!crate::agent::decide(&prompt, &ctx, &policy).is_auto());
}

#[test]
fn a_working_agent_printing_a_numbered_plan_is_not_a_menu() {
    let mut tail = copilot_plan_form();
    tail.truncate(6); // the plan and its closing sentence, no form
    tail.push("◉ Working · 17.8 KiB esc interrupt".to_string());
    assert_eq!(observe(&tail), PaneState::Working);
    assert!(parse_pending_prompt(&tail, "", true, false).is_none());
}
