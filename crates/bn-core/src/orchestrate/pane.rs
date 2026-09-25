//! The agent-pane state machine (#340) — the pure classifier half of `bn orchestrate`.
//!
//! Split out of `orchestrate.rs` so both halves stay under the 1000-line ratchet and so the
//! pane vocabulary (states, marker tables, the numbered-menu reader) has one home. The persisted
//! queue model stays in the parent module; everything here is pure over a captured pane tail —
//! no tmux, no git, no clock.
//!
//! Re-exported wholesale by [`crate::orchestrate`], so every existing `orchestrate::classify`,
//! `orchestrate::PaneState`, … path keeps resolving unchanged.

use serde::{Deserialize, Serialize};

// ===========================================================================
// The agent-pane state machine (#340)
// ===========================================================================

/// The classified state of one live agent pane — the #340 state set. Each maps to exactly one
/// deterministic [`SupervisorAction`] via [`action_for`], so a supervision pass never re-derives
/// pane-string heuristics in prose. Serialized kebab-case for the `bn orchestrate status` surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaneState {
    /// A live spinner / interrupt hint — the agent is mid-turn. Leave it alone.
    Working,
    /// A bare idle prompt with **no** uncommitted or unpushed work — finished-and-clean, or a
    /// just-launched agent that hasn't started. Nothing for the orchestrator to recover.
    IdleClean,
    /// A bare idle prompt but the worktree carries WIP (dirty tree or commits ahead of base) — the
    /// agent died / stalled with unpushed work. The recovery case that was hand-driven all session.
    IdleWithWip,
    /// The harness's "do you trust the files in this folder?" launch prompt is up, blocking boot.
    TrustMenu,
    /// The harness is asking whether it may run a command/tool, with an explicit session-scoped
    /// allow option. Trusted ship lanes may clear this without guessing an agent decision.
    CommandApproval,
    /// A blocking choice prompt ("Shall I proceed?" / a numbered menu / a `[y/n]`) awaiting a pick.
    QuestionMenu,
    /// The pane shows a harness/API error (5xx, "API Error", request failed) and is not retrying.
    ApiError,
    /// The pane shows a network/DNS failure (ENOTFOUND, getaddrinfo, connection refused/timeout).
    DnsOutage,
    /// The harness hit its usage/rate limit (its "wait for limit to reset / upgrade your plan"
    /// prompt is up). No relaunch can clear it — the fix is a harness switch (#382), so the
    /// daemon falls this lane over to the other harness in place.
    Limited,
    /// The agent signalled completion (Plane A `@bn_agent=done`) — harvest + land + reap.
    Done,
    /// No live tmux window for this agent — the process is gone. Recover any WIP, else reap.
    Dead,
}

impl PaneState {
    /// The kebab-case label used in status output and event logs.
    pub fn label(self) -> &'static str {
        match self {
            PaneState::Working => "working",
            PaneState::IdleClean => "idle-clean",
            PaneState::IdleWithWip => "idle-with-wip",
            PaneState::TrustMenu => "trust-menu",
            PaneState::CommandApproval => "command-approval",
            PaneState::QuestionMenu => "question-menu",
            PaneState::ApiError => "api-error",
            PaneState::DnsOutage => "dns-outage",
            PaneState::Limited => "limited",
            PaneState::Done => "done",
            PaneState::Dead => "dead",
        }
    }
}

/// The deterministic action a supervision pass takes for a [`PaneState`]. The whole #340 mapping is
/// [`action_for`]; the CLI layer realizes each variant against the live fleet (tmux send-keys, the
/// merge-queue, `bn agent recover`, `bn dispatch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupervisorAction {
    /// Do nothing — the agent is making progress (or idle-clean with nothing to recover).
    Leave,
    /// Harvest the agent's result, gate + land its PR via the merge-queue, then reap the worktree.
    Land,
    /// Clear the trust-folder menu (send Down+Enter to pick "trust"). Rare when dispatch pre-trusts.
    AnswerTrustMenu,
    /// Handle a harness command approval according to the lane's trust policy.
    AnswerCommandApproval,
    /// Answer a blocking question menu (surface for a decision, or auto-answer the default).
    AnswerQuestion,
    /// Relaunch the agent to resume its work (recover unpushed WIP first), with exponential backoff.
    RelaunchToResume,
    /// Fall a usage-limited agent over to the other harness in place (`bn switch --in-place`) so the
    /// work continues instead of waiting for the limit to reset — the #382 bias+fallback.
    SwitchHarness,
}

impl SupervisorAction {
    /// The kebab-case label used in status output and event logs.
    pub fn label(self) -> &'static str {
        match self {
            SupervisorAction::Leave => "leave",
            SupervisorAction::Land => "land",
            SupervisorAction::AnswerTrustMenu => "answer-trust-menu",
            SupervisorAction::AnswerCommandApproval => "answer-command-approval",
            SupervisorAction::AnswerQuestion => "answer-question",
            SupervisorAction::RelaunchToResume => "relaunch-to-resume",
            SupervisorAction::SwitchHarness => "switch-harness",
        }
    }
}

/// The **total** #340 pane-state → action mapping. Deterministic and exhaustive — the single source
/// of truth for "given this state, what does the supervisor do?", replacing the per-wave prose.
pub fn action_for(state: PaneState) -> SupervisorAction {
    match state {
        PaneState::Working => SupervisorAction::Leave,
        PaneState::IdleClean => SupervisorAction::Leave,
        PaneState::IdleWithWip => SupervisorAction::RelaunchToResume,
        PaneState::TrustMenu => SupervisorAction::AnswerTrustMenu,
        PaneState::CommandApproval => SupervisorAction::AnswerCommandApproval,
        PaneState::QuestionMenu => SupervisorAction::AnswerQuestion,
        PaneState::ApiError => SupervisorAction::RelaunchToResume,
        PaneState::DnsOutage => SupervisorAction::RelaunchToResume,
        PaneState::Limited => SupervisorAction::SwitchHarness,
        PaneState::Done => SupervisorAction::Land,
        PaneState::Dead => SupervisorAction::RelaunchToResume,
    }
}

/// The harness the orchestrate daemon dispatches under when nothing else is configured — the
/// back-compat default for a run whose state predates `bn orchestrate --agent` (#382).
pub const DEFAULT_HARNESS: &str = "copilot";

/// [`classify`] so the classifier needs no tmux/git — a `capture-pane` tail, the window's Plane-A
/// `@bn_agent` option, whether a live window exists, and whether the worktree has WIP.
#[derive(Debug, Clone)]
pub struct PaneObservation<'a> {
    /// The recent non-empty lines of the agent's pane (already trimmed), oldest→newest.
    pub tail: &'a [String],
    /// Whether a live tmux window exists for this agent. `false` ⇒ [`PaneState::Dead`].
    pub has_window: bool,
    /// The window's `@bn_agent` Plane-A state (`working` | `waiting` | `done` | `""`). Authoritative
    /// for `done`; a hint otherwise.
    pub plane_a: &'a str,
    /// Whether the worktree carries unpushed/uncommitted work (dirty tree or commits ahead of base).
    pub has_wip: bool,
}

/// A menu's keybinding footer — a line that tells you how to *commit a choice*.
///
/// This exists because "esc to cancel" is genuinely ambiguous: it is Copilot's spinner marker, and
/// it is also the right-hand half of Claude's menu footer, `Enter to confirm · Esc to cancel`.
/// Without this guard the single most common blocking dialog there is — Claude's folder-trust
/// prompt at launch — classifies as [`PaneState::Working`], so the supervisor's `AnswerTrustMenu`
/// arm can never fire and the lane sits at the prompt indefinitely looking busy.
///
/// Deliberately narrow: either an explicit "commit your choice" instruction, or a cancel hint
/// listed alongside another key that only a dialog offers. Claude prints both shapes —
/// `Enter to confirm · Esc to cancel` on the trust menu, `Esc to cancel · Tab to amend · ctrl+e to
/// explain` on a permission prompt. A spinner line ("✻ Clauding… (6m · ↓21k tokens · esc to
/// interrupt)") and Claude's permanent mode line ("bypass permissions on (shift+tab to cycle) ·
/// esc to interrupt") carry none of these, so neither is mistaken for a footer.
fn is_menu_footer(lower: &str) -> bool {
    const COMMIT: &[&str] = &[
        "enter to confirm",
        "enter to select",
        "enter to choose",
        "enter to submit",
        "↵ to confirm",
        "to navigate",
    ];
    const DIALOG_KEYS: &[&str] = &[
        "tab to amend",
        "to explain",
        "ctrl+e",
        "to edit",
        "to expand",
    ];
    COMMIT.iter().any(|m| lower.contains(m))
        || (lower.contains("esc to cancel") && DIALOG_KEYS.iter().any(|m| lower.contains(m)))
}

/// Spinner / interrupt markers a harness prints only while a turn is in flight: Claude's
/// `esc to interrupt`, Copilot's `esc to cancel`. A `Working…` spinner word is matched only in its
/// animated form (trailing ellipsis) so an agent merely *saying* "working on the fix" at an idle
/// prompt never reads as busy — the sharper cousin of `bn agent live`'s spinner heuristic.
///
/// A [`is_menu_footer`] line is never a spinner, whatever else it says.
fn is_spinner_line(lower: &str) -> bool {
    if is_menu_footer(lower) {
        return false;
    }
    lower.contains("esc to interrupt")
        || lower.contains("esc to cancel")
        || lower.contains("esc cancel")
        || lower.contains("esc interrupt")
        || lower.contains("working…")
        || lower.contains("working...")
        || lower.contains("● working")
        || lower.contains("· working")
}

/// Network/DNS-outage markers: name-resolution and connection failures. A pane wedged on any of
/// these can't reach the API — relaunching once the network returns is the fix (with backoff).
const DNS_OUTAGE_MARKERS: &[&str] = &[
    "enotfound",
    "getaddrinfo",
    "eai_again",
    "could not resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "no address associated with hostname",
    "network is unreachable",
    "no route to host",
    "connection refused",
    "econnrefused",
    "connection reset",
    "econnreset",
    "connection timed out",
    "etimedout",
];

/// API/server-error markers: the harness reached the network but the API returned an error (5xx,
/// overloaded, request failed). Distinct from a DNS outage so status/observability can tell a
/// network drop from a server-side wobble; both relaunch-to-resume with backoff.
const API_ERROR_MARKERS: &[&str] = &[
    "api error",
    "api request failed",
    "request failed with status",
    "internal server error",
    "500 internal",
    "502 bad gateway",
    "bad gateway",
    "503 service unavailable",
    "service unavailable",
    "504 gateway timeout",
    "gateway timeout",
    "overloaded_error",
    "overloaded",
    "upstream error",
];

/// Usage/rate-limit markers: the harness reached its plan/session cap and put up its blocking limit
/// prompt. Distinct from an outage because no relaunch can clear it — the fix is a harness switch
/// (#382). Matched case-insensitively; checked *after* the spinner so an agent merely *mentioning* a
/// limit mid-turn (spinner still up) never reads as limited. Mirrors `bn agent live`'s detection.
const USAGE_LIMIT_MARKERS: &[&str] = &[
    "wait for limit to reset",
    "upgrade your plan",
    "usage limit reached",
    "reached your usage limit",
    "hit your session limit",
    "session limit reached",
];

/// Trust-menu launch markers — both the harness's "trust the files in this folder?" boot prompt
/// and Claude's "N new MCP servers found · Select any to trust" approval menu (#545). Dispatch can
/// pre-trust the folder (`bn trust-folder` under `--yolo`) so the folder prompt rarely fires, but a
/// non-yolo web dispatch parks on one of these until answered; the supervisor clears it
/// deterministically instead of a human sending keystrokes, and the fleet surfaces it as needs-you.
const TRUST_MENU_MARKERS: &[&str] = &[
    "do you trust the files",
    "do you trust the authors",
    "trust the files in this folder",
    "do you want to trust",
    "trust this folder",
    "new mcp server",
    "new mcp servers",
    "select any to trust",
];

/// Harness-owned command/tool approval markers. These are deliberately narrower than generic
/// numbered menus: only prompts that explicitly identify a command/tool permission and offer a
/// session-scoped allow choice classify as [`PaneState::CommandApproval`].
const COMMAND_APPROVAL_MARKERS: &[&str] = &[
    "allow command:",
    "do you want to run this command",
    "would you like to run this command",
    "do you want to use this tool",
    "would you like to use this tool",
];

/// Option wording that grants a **session-scoped** allow. Claude phrases it per-scope ("don't ask
/// again for cargo commands in /path"), so the bare "don't ask again" stem has to match too — the
/// older session-suffixed variants alone never fired against a real Claude permission dialog.
pub(crate) const SESSION_ALLOW_MARKERS: &[&str] = &[
    "don't ask again",
    "don’t ask again",
    "do not ask again",
    "always allow",
    // Copilot: "Yes, and remember this path / these paths for this session". An option only
    // counts as a session allow when it also approves, so a "No …" option cannot match.
    "for this session",
];

/// Option wording that grants a **one-shot** approval. Deliberately anchored: a bare "yes" prefix,
/// or an explicit run/allow-once phrase. Matched against the option label, never the whole pane.
pub(crate) const APPROVE_ONCE_MARKERS: &[&str] = &[
    "yes",
    "allow once",
    "approve",
    "run it",
    "proceed",
    "confirm",
    "ok",
];

/// Option wording that **declines**. `RejectWithFeedback` (Claude's "No, and tell Claude what to do
/// differently") is separated from a plain no because answering it opens a text box rather than
/// ending the turn — a supervisor that means "no" must not land in a composer it never fills.
pub(crate) const REJECT_FEEDBACK_MARKERS: &[&str] = &[
    "and tell",
    "tell claude",
    "tell copilot",
    "what to do differently",
    "give feedback",
];

pub(crate) const REJECT_MARKERS: &[&str] = &["no", "cancel", "deny", "reject", "skip", "abort"];

/// Explicit "blocking question" phrases — a pane sitting on one of these is waiting on a decision.
/// A numbered-choice menu is detected separately by [`is_numbered_menu`].
const QUESTION_MENU_MARKERS: &[&str] = &[
    "shall i proceed",
    "do you want me to proceed",
    "would you like me to proceed",
    "do you want to proceed",
    "do you want to continue",
    "proceed? (y/n)",
    "continue? (y/n)",
    "(y/n)",
    "[y/n]",
    "select an option",
    "choose an option",
];

/// Bare idle-prompt glyphs: Copilot's empty box (`┃`), Claude's `❯`, and generic `>` / `│`. A line
/// that trims to exactly one of these is an empty input box awaiting you — nothing typed.
const BARE_PROMPT_GLYPHS: &[&str] = &["┃", "❯", ">", "│"];

fn any_marker(tail: &[String], markers: &[&str]) -> bool {
    tail.iter().any(|l| {
        let lower = l.to_ascii_lowercase();
        markers.iter().any(|m| lower.contains(m))
    })
}

/// Does the tail show a numbered selection menu — at least two `1.` / `2)` / `❯ 1.` option lines?
/// The generic shape of a harness's "pick one" prompt, so a menu with non-canonical wording still
/// classifies as [`PaneState::QuestionMenu`] rather than falling through to idle.
fn is_numbered_menu(tail: &[String]) -> bool {
    tail.iter().filter(|l| menu_option(l).is_some()).count() >= 2
}

/// Vertical frame glyphs a TUI draws its dialog box with. Claude Code renders its permission
/// prompt inside a `╭─╮ │ ╰─╯` box, so every option line arrives as `│ ❯ 1. Yes … │` — which the
/// pre-#-frame reader could not parse at all, because `│` is not a bullet it stripped. The dialog
/// therefore never reached [`command_approval_option`], never classified as
/// [`PaneState::CommandApproval`], and fell through to `QuestionMenu` — surfaced, never answered.
const FRAME_GLYPHS: &[char] = &['│', '┃', '┆', '┊', '║', '▌', '▏', '|'];

/// One captured pane line with its TUI frame removed: leading/trailing box glyphs and the padding
/// inside them. Idempotent on an unframed line, so it is safe to apply everywhere a line is read.
///
/// Trailing-only stripping is deliberate on the right edge: a box's right border may be flush
/// against the text or separated by padding, and either way the content is what sits between.
pub fn unframe(line: &str) -> &str {
    line.trim()
        .trim_start_matches(FRAME_GLYPHS)
        .trim_end_matches(FRAME_GLYPHS)
        .trim()
}

/// Whether a line carries no content once unframed — an empty box row, a horizontal rule, or a
/// blank. These are dropped before the trailing-option-block scan so a dialog's bottom border
/// cannot push its own options out of "trailing" position.
pub fn is_frame_only(line: &str) -> bool {
    let t = unframe(line);
    t.is_empty() || t.chars().all(|c| c.is_whitespace() || is_box_glyph(c))
}

/// A horizontal box rule — a dialog's top or bottom border (`╭────╮`, `└────┘`, `────`).
///
/// Distinct from a merely blank row, because a rule is a *boundary*: everything above a dialog's
/// top border is the agent's prose, not part of the dialog. Reading a heading without this
/// distinction pulls the last sentence the agent said into the question's subject.
pub fn is_box_rule(line: &str) -> bool {
    let t = unframe(line);
    !t.is_empty() && t.chars().all(is_box_glyph)
}

fn is_box_glyph(c: char) -> bool {
    ('\u{2500}'..='\u{257f}').contains(&c) || c == '─' || c == '━'
}

/// The option number on a numbered-menu line (`1.` / `2)` / `❯ 1.` / `│ ❯ 1. … │`), or `None`.
/// Frame-aware: the line is unframed first, so a boxed dialog parses the same as a bare one.
fn menu_option(line: &str) -> Option<u8> {
    menu_entry(line).map(|(n, _)| n)
}

/// The option number **and** its label text. The label is what a human picks by, so a surface that
/// offers "Approve" must be able to show the words the harness actually wrote.
pub(crate) fn menu_entry(line: &str) -> Option<(u8, &str)> {
    let t = unframe(line)
        .trim_start_matches(['❯', '>', '●', '○', '◉', '◯', ' '])
        .trim_start();
    let mut chars = t.chars();
    let digit = chars.next()?.to_digit(10)? as u8;
    if !matches!(chars.next(), Some('.') | Some(')')) || t.len() <= 2 || t[2..].trim().is_empty() {
        return None;
    }
    Some((digit, t[2..].trim()))
}

/// Whether this line is the *highlighted* option — the harness's cursor (`❯`, `●`, `>`) sits on it.
pub(crate) fn menu_selected(line: &str) -> bool {
    unframe(line).starts_with(['❯', '●', '◉', '>'])
}

/// Whether a label matches any of `markers`, case-insensitively. Word-anchored for the short stems
/// ("no", "ok", "yes") so "Notify the team" is never read as a rejection.
pub(crate) fn label_matches(label: &str, markers: &[&str]) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    markers.iter().any(|m| {
        if m.len() <= 3 {
            lower == *m
                || lower.starts_with(&format!("{m} "))
                || lower.starts_with(&format!("{m},"))
                || lower.starts_with(&format!("{m}."))
        } else {
            lower.contains(m)
        }
    })
}

/// The bounds of the pane's **trailing** numbered-option block, as indices into `tail`, plus the
/// index the block's heading starts at. `None` when the tail does not end on a menu.
///
/// "Trailing" is the safety property the whole answering path rests on: only a block that is still
/// the last thing on screen may be answered, so stale approval scrollback can never be mistaken for
/// a live question. Frame-only rows (a box's bottom border) and short footer hints ("esc to
/// cancel") are allowed to follow it — they are chrome the harness draws *around* a live dialog,
/// not new output that would mean the dialog is gone.
pub(crate) fn trailing_menu_block(tail: &[String]) -> Option<(usize, usize, usize)> {
    let last_option = tail.iter().rposition(|line| menu_option(line).is_some())?;
    if !tail[last_option + 1..]
        .iter()
        .all(|line| is_frame_only(line) || is_footer_hint(line))
    {
        return None;
    }
    // Walk back over the contiguous option lines. A single frame-only row (a blank row inside the
    // box) may be stepped over, but only when another option sits above it — so the walk stops at
    // the dialog's top border instead of swallowing the prose above it.
    let mut menu_start = last_option;
    while menu_start > 0 {
        let prev = menu_start - 1;
        if menu_option(&tail[prev]).is_some() {
            menu_start = prev;
        } else if is_frame_only(&tail[prev]) && prev >= 1 && menu_option(&tail[prev - 1]).is_some()
        {
            menu_start = prev - 1;
        } else {
            break;
        }
    }
    let heading_start = tail[..menu_start]
        .iter()
        .rposition(|line| menu_option(line).is_some())
        .map_or(0, |idx| idx + 1);
    Some((heading_start, menu_start, last_option))
}

/// The `❯` option block of a **cursor menu** — Copilot's `ask_user` form, chosen by moving a
/// cursor and pressing enter rather than by typing a number — as `(menu_start, last_option)`.
///
/// Only when it is the trailing thing on screen: a footer that says how to move and accept
/// ("↑/↓ select · enter accept"), followed by nothing but frame rows. Options are the short lines
/// directly above that footer, exactly one of them carrying the cursor. A plan the agent printed
/// above the form is prose, not options: the walk stops at the first line that reads as a sentence
/// or a question, so a numbered plan body never becomes the menu.
pub fn cursor_menu_block(tail: &[String]) -> Option<(usize, usize)> {
    let footer = tail.iter().rposition(|l| !is_frame_only(l))?;
    let lower = unframe(&tail[footer]).to_lowercase();
    if !(lower.contains('↑') && lower.contains("enter")) {
        return None;
    }
    let is_option = |l: &str| {
        let t = unframe(l).trim();
        !t.is_empty()
            && t.chars().count() <= 60
            && !t.ends_with(['?', ':', '.'])
            && !is_frame_only(l)
    };
    let mut start = footer;
    while start > 0 && is_option(&tail[start - 1]) {
        start -= 1;
    }
    let options = &tail[start..footer];
    let cursors = options
        .iter()
        .filter(|l| unframe(l).trim_start().starts_with('❯'))
        .count();
    (options.len() >= 2 && cursors == 1).then_some((start, footer - 1))
}

/// Chrome a harness prints beneath a live dialog: keybinding hints and status footers. Bounded in
/// length so a genuine new paragraph of agent output can never pass as a hint.
fn is_footer_hint(line: &str) -> bool {
    let t = unframe(line);
    if t.chars().count() > 60 {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    t.starts_with('(')
        || [
            "esc",
            "press ",
            "shift+tab",
            "↑",
            "↓",
            "enter to",
            "tab to",
            "ctrl+",
        ]
        .iter()
        .any(|m| lower.contains(m))
}

/// Does this trailing block have the *shape* of a permission dialog — an approval choice plus an
/// explicit session-scoped allow? No arbitrary question offers "don't ask again", so the shape is
/// a sharper signal than the heading wording, and it is the one that actually fires against
/// Claude's boxed prompt (whose heading reads "Do you want to proceed?" — generic question
/// phrasing that [`COMMAND_APPROVAL_MARKERS`] deliberately does not, and should not, match).
fn is_approval_shaped(block: &[String]) -> bool {
    let labels: Vec<&str> = block
        .iter()
        .filter_map(|l| menu_entry(l))
        .map(|(_, s)| s)
        .collect();
    // The two must be *different* options. "Yes, and don't ask again" matches both marker sets on
    // its own, so testing them independently made any menu carrying a single such option — a
    // genuine "which approach?" question among them — read as a permission dialog and become
    // eligible for auto-approval. A real permission dialog always offers the plain approval and the
    // session-scoped one separately; the precedence here mirrors `PromptSemantic::classify`.
    let session = labels
        .iter()
        .filter(|l| label_matches(l, SESSION_ALLOW_MARKERS))
        .count();
    let approve_once = labels
        .iter()
        .filter(|l| {
            label_matches(l, APPROVE_ONCE_MARKERS) && !label_matches(l, SESSION_ALLOW_MARKERS)
        })
        .count();
    session >= 1 && approve_once >= 1
}

/// The numbered session-scoped allow choice in the active trailing command-approval menu.
/// Restricting the match to the final contiguous option block prevents stale approval scrollback
/// from turning a newer arbitrary question into an auto-answer target.
///
/// A block qualifies either by its heading (the narrow [`COMMAND_APPROVAL_MARKERS`] wording) or by
/// its own shape ([`is_approval_shaped`]) — the latter is what lets a boxed Claude permission
/// prompt be recognized and answered rather than merely surfaced.
pub fn command_approval_option(tail: &[String]) -> Option<u8> {
    let (heading_start, menu_start, last_option) = trailing_menu_block(tail)?;
    let block = &tail[menu_start..=last_option];
    if !any_marker(&tail[heading_start..menu_start], COMMAND_APPROVAL_MARKERS)
        && !is_approval_shaped(block)
    {
        return None;
    }

    block.iter().find_map(|line| {
        let (number, label) = menu_entry(line)?;
        label_matches(label, SESSION_ALLOW_MARKERS).then_some(number)
    })
}

pub(crate) fn has_bare_prompt(tail: &[String]) -> bool {
    tail.iter().any(|l| BARE_PROMPT_GLYPHS.contains(&l.trim()))
}

/// Classify an agent pane into a [`PaneState`] — the pure #340 classifier. Priority is deliberate
/// and deterministic:
///   1. no window ⇒ `Dead` (structural — nothing else can be true).
///   2. Plane A `done` ⇒ `Done` (the harness's own lifecycle signal is authoritative).
///   3. a live spinner ⇒ `Working` (busy beats everything below — an error or menu merely *mentioned*
///      mid-turn, with the spinner still up, must not trigger a relaunch/keystroke).
///   4. trust menu ⇒ `TrustMenu`.
///   5. command approval ⇒ `CommandApproval`.
///   6. network/DNS failure ⇒ `DnsOutage` (checked before API so a resolve failure isn't mislabeled).
///   7. API/server error ⇒ `ApiError`.
///   8. usage/rate limit ⇒ `Limited` (before the menu check — its dialog is a numbered menu that
///      would otherwise read as `QuestionMenu`; the daemon switches it rather than surface it).
///   9. a question / numbered menu ⇒ `QuestionMenu`.
///   10. a bare idle prompt ⇒ `IdleWithWip` if the worktree has WIP, else `IdleClean`.
///   11. fallback: Plane A `working` ⇒ `Working`; otherwise WIP decides idle-with-wip vs idle-clean.
///
/// Note: a `Limited` pane maps to a harness switch (#382), not a relaunch — relaunching can't clear
/// a usage limit. Before #382 this state was folded into `QuestionMenu` and merely surfaced; now the
/// daemon falls it over to the other harness automatically.
pub fn classify(obs: &PaneObservation) -> PaneState {
    if !obs.has_window {
        return PaneState::Dead;
    }
    if obs.plane_a == "done" {
        return PaneState::Done;
    }
    // A dialog's keybinding footer sits *below* its options and routinely says "esc to cancel" —
    // which is also Copilot's spinner marker. So spinner evidence at or below a **trailing** option
    // block is that block's own chrome, not a live turn, and is skipped.
    //
    // Structural rather than textual, because the wording moves: Claude's trust menu and its
    // permission prompt print different footers, and matching either one by string is a guess about
    // the next release. This is safe against a working agent whose output merely *ends* in a
    // numbered list, because such a pane still shows the harness's input box below it — and an
    // input box is neither frame nor footer, so no trailing block is found at all. A real dialog
    // replaces that input box, which is exactly when the block does match.
    let menu_start = trailing_menu_block(obs.tail)
        .map(|(_, start, _)| start)
        .or_else(|| cursor_menu_block(obs.tail).map(|(start, _)| start));
    let spinner_scan = match menu_start {
        Some(start) => &obs.tail[..start],
        None => obs.tail,
    };
    let has_spinner = spinner_scan
        .iter()
        .any(|l| is_spinner_line(&l.to_ascii_lowercase()));
    if has_spinner {
        return PaneState::Working;
    }
    if any_marker(obs.tail, TRUST_MENU_MARKERS) {
        return PaneState::TrustMenu;
    }
    if command_approval_option(obs.tail).is_some() {
        return PaneState::CommandApproval;
    }
    if any_marker(obs.tail, DNS_OUTAGE_MARKERS) {
        return PaneState::DnsOutage;
    }
    if any_marker(obs.tail, API_ERROR_MARKERS) {
        return PaneState::ApiError;
    }
    if any_marker(obs.tail, USAGE_LIMIT_MARKERS) {
        return PaneState::Limited;
    }
    if any_marker(obs.tail, QUESTION_MENU_MARKERS)
        || is_numbered_menu(obs.tail)
        || cursor_menu_block(obs.tail).is_some()
    {
        return PaneState::QuestionMenu;
    }
    if has_bare_prompt(obs.tail) {
        return if obs.has_wip {
            PaneState::IdleWithWip
        } else {
            PaneState::IdleClean
        };
    }
    // Ambiguous pane (spinner scrolled off, no prompt captured): trust Plane A when it says working,
    // else let WIP decide whether there's anything to recover.
    if obs.plane_a == "working" {
        PaneState::Working
    } else if obs.has_wip {
        PaneState::IdleWithWip
    } else {
        PaneState::IdleClean
    }
}

/// Classify a pane and resolve its action in one step — the supervisor's per-agent decision.
pub fn decide(obs: &PaneObservation) -> (PaneState, SupervisorAction) {
    let state = classify(obs);
    (state, action_for(state))
}
