//! Table-driven tests for the pane classifier ([`classify`] / [`decide`]). These two functions
//! decide whether `bn fleet reap` treats an agent as mid-turn and whether `bn agent dialog` may
//! answer a prompt, so every outcome gets a small captured-pane fixture.

use super::*;

fn lines(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

struct Case {
    name: &'static str,
    tail: &'static [&'static str],
    has_window: bool,
    plane_a: &'static str,
    has_wip: bool,
    want: PaneState,
}

const fn case(name: &'static str, tail: &'static [&'static str], want: PaneState) -> Case {
    Case {
        name,
        tail,
        has_window: true,
        plane_a: "",
        has_wip: false,
        want,
    }
}

const CLAUDE_SPINNER: &[&str] = &[
    "⏺ Reading the module",
    "✻ Clauding… (6m · ↓21k tokens · esc to interrupt)",
];

const COPILOT_SPINNER: &[&str] = &["● Running tests", "◐ Thinking (esc to cancel)"];

const SPINNER_OVER_ERROR: &[&str] = &[
    "API Error: 529 overloaded_error (retrying)",
    "✻ Clauding… (esc to interrupt)",
];

const TRUST_MENU: &[&str] = &[
    "Do you trust the files in this folder?",
    "❯ 1. Yes, proceed",
    "  2. No, exit",
    "Enter to confirm · Esc to cancel",
];

const MCP_TRUST_MENU: &[&str] = &[
    "3 new MCP servers found in .mcp.json",
    "Select any to trust",
    "❯ 1. bn",
    "  2. playwright",
];

const CLAUDE_PERMISSION_BOX: &[&str] = &[
    "╭──────────────────────────────────────────╮",
    "│ Bash command                             │",
    "│   cargo test -p bn-core                  │",
    "│ Do you want to proceed?                  │",
    "│ ❯ 1. Yes                                 │",
    "│   2. Yes, and don't ask again for cargo  │",
    "│   3. No, and tell Claude what to do (esc)│",
    "╰──────────────────────────────────────────╯",
];

const COMMAND_APPROVAL_HEADING: &[&str] = &[
    "Allow command: rm -rf target",
    "1. Yes",
    "2. Yes, and remember this path for this session",
    "3. No",
];

const DNS_OUTAGE: &[&str] = &[
    "Error: getaddrinfo ENOTFOUND api.anthropic.com",
    "Request failed with status 0",
    "❯",
];

const API_ERROR: &[&str] = &["API Error: 500 Internal Server Error", "❯"];

const USAGE_LIMIT: &[&str] = &[
    "Claude usage limit reached.",
    "1. Stop and wait for limit to reset",
    "2. Upgrade your plan",
];

const QUESTION_MARKER: &[&str] = &["I have a plan ready. Shall I proceed?", "❯"];

const NUMBERED_MENU: &[&str] = &[
    "Which approach should I take?",
    "1. Rewrite the parser",
    "2. Patch the existing one",
];

/// "Yes, and don't ask again" alone is not a permission dialog: it lacks a separate plain approval.
const SINGLE_SESSION_OPTION_MENU: &[&str] = &[
    "Which approach?",
    "1. Yes, and don't ask again",
    "2. Something else entirely",
];

const CURSOR_MENU: &[&str] = &[
    "Pick a strategy?",
    "❯ Merge",
    "Rebase",
    "↑/↓ select · enter accept · esc cancel",
];

const CLAUDE_IDLE: &[&str] = &["Pushed the branch and opened the PR.", "❯"];
const COPILOT_IDLE: &[&str] = &["All done.", "┃"];

/// "working" in prose at an idle prompt is not the animated spinner word.
const SAYS_WORKING_AT_IDLE: &[&str] = &["I'm working on the fix next.", "❯"];

const AMBIGUOUS: &[&str] = &["compiling bn-core v0.1.0"];

fn cases() -> Vec<Case> {
    vec![
        Case {
            has_window: false,
            plane_a: "done",
            ..case(
                "no window is dead, even when done",
                CLAUDE_IDLE,
                PaneState::Dead,
            )
        },
        Case {
            plane_a: "done",
            ..case(
                "plane-a done beats a spinner",
                CLAUDE_SPINNER,
                PaneState::Done,
            )
        },
        case("claude spinner", CLAUDE_SPINNER, PaneState::Working),
        case("copilot spinner", COPILOT_SPINNER, PaneState::Working),
        case(
            "spinner beats an error mentioned mid-turn",
            SPINNER_OVER_ERROR,
            PaneState::Working,
        ),
        case(
            "trust menu footer is not a spinner",
            TRUST_MENU,
            PaneState::TrustMenu,
        ),
        case("mcp trust menu", MCP_TRUST_MENU, PaneState::TrustMenu),
        case(
            "boxed claude permission prompt",
            CLAUDE_PERMISSION_BOX,
            PaneState::CommandApproval,
        ),
        case(
            "explicit command approval heading",
            COMMAND_APPROVAL_HEADING,
            PaneState::CommandApproval,
        ),
        case(
            "dns failure is checked before api error",
            DNS_OUTAGE,
            PaneState::DnsOutage,
        ),
        case("api error", API_ERROR, PaneState::ApiError),
        case(
            "usage-limit menu is limited, not a question",
            USAGE_LIMIT,
            PaneState::Limited,
        ),
        case(
            "question marker over a prompt",
            QUESTION_MARKER,
            PaneState::QuestionMenu,
        ),
        case("numbered menu", NUMBERED_MENU, PaneState::QuestionMenu),
        case(
            "a lone session-allow option is a question",
            SINGLE_SESSION_OPTION_MENU,
            PaneState::QuestionMenu,
        ),
        case("copilot cursor menu", CURSOR_MENU, PaneState::QuestionMenu),
        case("claude idle, clean", CLAUDE_IDLE, PaneState::IdleClean),
        Case {
            has_wip: true,
            ..case(
                "copilot idle with wip",
                COPILOT_IDLE,
                PaneState::IdleWithWip,
            )
        },
        case(
            "prose 'working' at idle is not busy",
            SAYS_WORKING_AT_IDLE,
            PaneState::IdleClean,
        ),
        Case {
            plane_a: "working",
            ..case(
                "ambiguous trusts plane-a working",
                AMBIGUOUS,
                PaneState::Working,
            )
        },
        Case {
            has_wip: true,
            ..case("ambiguous with wip", AMBIGUOUS, PaneState::IdleWithWip)
        },
        case("ambiguous and clean", AMBIGUOUS, PaneState::IdleClean),
        case("empty tail", &[], PaneState::IdleClean),
    ]
}

#[test]
fn classify_covers_every_pane_state() {
    for c in cases() {
        let tail = lines(c.tail);
        let obs = PaneObservation {
            tail: &tail,
            has_window: c.has_window,
            plane_a: c.plane_a,
            has_wip: c.has_wip,
        };
        assert_eq!(classify(&obs), c.want, "case: {}", c.name);
    }
}

#[test]
fn decide_pairs_the_state_with_its_action() {
    for c in cases() {
        let tail = lines(c.tail);
        let obs = PaneObservation {
            tail: &tail,
            has_window: c.has_window,
            plane_a: c.plane_a,
            has_wip: c.has_wip,
        };
        let (state, action) = decide(&obs);
        assert_eq!(state, c.want, "case: {}", c.name);
        assert_eq!(action, action_for(c.want), "case: {}", c.name);
    }
}

#[test]
fn decide_actions_for_the_states_callers_branch_on() {
    let table: &[(&[&str], &str, PaneState, SupervisorAction)] = &[
        (
            CLAUDE_SPINNER,
            "",
            PaneState::Working,
            SupervisorAction::Leave,
        ),
        (
            TRUST_MENU,
            "",
            PaneState::TrustMenu,
            SupervisorAction::AnswerTrustMenu,
        ),
        (
            CLAUDE_PERMISSION_BOX,
            "",
            PaneState::CommandApproval,
            SupervisorAction::AnswerCommandApproval,
        ),
        (
            NUMBERED_MENU,
            "",
            PaneState::QuestionMenu,
            SupervisorAction::AnswerQuestion,
        ),
        (
            USAGE_LIMIT,
            "",
            PaneState::Limited,
            SupervisorAction::SwitchHarness,
        ),
        (
            API_ERROR,
            "",
            PaneState::ApiError,
            SupervisorAction::RelaunchToResume,
        ),
        (CLAUDE_IDLE, "done", PaneState::Done, SupervisorAction::Land),
        (
            CLAUDE_IDLE,
            "",
            PaneState::IdleClean,
            SupervisorAction::Leave,
        ),
    ];
    for (raw, plane_a, state, action) in table {
        let tail = lines(raw);
        let obs = PaneObservation {
            tail: &tail,
            has_window: true,
            plane_a,
            has_wip: false,
        };
        assert_eq!(decide(&obs), (*state, *action), "tail: {raw:?}");
    }
}

/// The answering path must target the live dialog's own session-allow option, never a stale one.
#[test]
fn command_approval_option_picks_the_session_allow_number() {
    assert_eq!(
        command_approval_option(&lines(CLAUDE_PERMISSION_BOX)),
        Some(2)
    );
    assert_eq!(
        command_approval_option(&lines(COMMAND_APPROVAL_HEADING)),
        Some(2)
    );
    assert_eq!(command_approval_option(&lines(NUMBERED_MENU)), None);
    assert_eq!(
        command_approval_option(&lines(SINGLE_SESSION_OPTION_MENU)),
        None
    );
}

/// An approval menu that scrolled up under newer output is not a live approval.
#[test]
fn stale_approval_scrollback_is_never_command_approval() {
    let mut tail = lines(COMMAND_APPROVAL_HEADING);
    tail.push("Ran the command; every test passed and the branch is pushed now.".into());
    tail.push("❯".into());
    let obs = PaneObservation {
        tail: &tail,
        has_window: true,
        plane_a: "",
        has_wip: false,
    };
    assert_eq!(command_approval_option(&tail), None);
    assert_ne!(classify(&obs), PaneState::CommandApproval);
}
