//! The dialog an agent is blocked on, parsed into something a human — or a policy — can answer.
//!
//! Supervision could always say *that* an agent was waiting ([`crate::orchestrate::classify`]
//! returns `trust-menu` / `command-approval` / `question-menu`). It could never say **what** it was
//! waiting on: the question text and the option list stayed locked in the pane. So every surface
//! that offered to "approve" was guessing a digit — `bn fleet steer` literally offered
//! `Approve (1)` and `Approve (2)` and sent that number, which on a menu whose option 2 is
//! "No, and tell Claude what to do differently" is a rejection, not an approval.
//!
//! This module reads the pane once and returns the whole dialog: the question, the subject
//! (the command or tool under review), every numbered option with its real label and what that
//! label *means*, and a fingerprint of the pane it was read from. Everything downstream — the CLI
//! answer command, the tmux quick-steer submenu, the auto-approve policy, the web approval panel —
//! consumes this one structure, so they cannot disagree about what is on screen.
//!
//! Pure over a captured tail: no tmux, no clock, no I/O.

use crate::orchestrate::{
    self, classify, is_box_rule, is_frame_only, unframe, PaneObservation, PaneState,
    APPROVE_ONCE_MARKERS, REJECT_FEEDBACK_MARKERS, REJECT_MARKERS, SESSION_ALLOW_MARKERS,
};
use serde::{Deserialize, Serialize};

/// How many non-empty pane lines to capture when reading a dialog.
///
/// The fleet board's own glance uses 6 and the orchestrate supervisor 8 — enough to spot a spinner
/// or a bare prompt, and far too few for a permission dialog, which is a bordered box carrying a
/// title, the command, a description, the question and three options. Reading a dialog needs the
/// whole box, so the answering path captures its own, deeper tail.
pub const PROMPT_SCAN_LINES: usize = 40;

/// What an option *does*, independent of how the harness worded it. The vocabulary every surface
/// offers its buttons in, and the vocabulary the auto-approve policy is written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptSemantic {
    /// Allow this once ("Yes").
    Approve,
    /// Allow, and stop asking for this class of action ("Yes, and don't ask again for …").
    ApproveSession,
    /// Decline, ending the request ("No").
    Reject,
    /// Decline **and** open a text box for feedback ("No, and tell Claude what to do differently").
    ///
    /// Kept distinct from [`Self::Reject`] because answering it does not end the turn — it parks
    /// the agent in a composer waiting for prose. A supervisor that means "no" and picks this one
    /// leaves the lane blocked in a *new* way, which is worse than not answering at all.
    RejectWithFeedback,
    /// Anything else the harness offered — a third path, a "tell me more", a custom action.
    Other,
}

impl PromptSemantic {
    /// The kebab-case label used in the CLI/JSON surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::ApproveSession => "approve-session",
            Self::Reject => "reject",
            Self::RejectWithFeedback => "reject-with-feedback",
            Self::Other => "other",
        }
    }

    /// Classify one option's label. Order matters: the session-scoped allow and the feedback
    /// rejection are both *more specific* than the plain yes/no they contain, so they are tested
    /// first. "Yes, and don't ask again" must never read as a bare [`Self::Approve`], and "No, and
    /// tell Claude…" must never read as a bare [`Self::Reject`].
    pub fn classify(label: &str) -> PromptSemantic {
        let approve = orchestrate::label_matches(label, APPROVE_ONCE_MARKERS);
        if orchestrate::label_matches(label, SESSION_ALLOW_MARKERS) {
            return if approve {
                Self::ApproveSession
            } else {
                Self::Other
            };
        }
        if orchestrate::label_matches(label, REJECT_FEEDBACK_MARKERS) {
            return Self::RejectWithFeedback;
        }
        if approve {
            return Self::Approve;
        }
        if orchestrate::label_matches(label, REJECT_MARKERS) {
            return Self::Reject;
        }
        Self::Other
    }
}

/// One numbered choice the harness is offering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptOption {
    /// The digit to send to pick it.
    pub number: u8,
    /// The harness's own wording — what a button must say, so the human reads the same text the
    /// pane shows rather than a label bn invented.
    pub label: String,
    /// What picking it means.
    pub semantic: PromptSemantic,
    /// Whether the harness's cursor is currently on this option.
    pub selected: bool,
}

/// A dialog an agent is currently blocked on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPrompt {
    /// Which kind of block this is — the refined [`PaneState`]. See [`parse_pending_prompt`] for
    /// why this can differ from a bare `classify` of the same tail.
    pub kind: PaneState,
    /// The question line ("Do you want to proceed?").
    pub question: String,
    /// The command or tool under review, when the dialog names one. This is what a deny rule is
    /// matched against, and what a fleet card shows in one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Every numbered choice, in the order the harness listed them.
    pub options: Vec<PromptOption>,
    /// A fingerprint of the pane this was read from — [`crate::agent::pane_activity_fingerprint`].
    ///
    /// Carried through every answer path as an `--expect` token. A pane that has moved on produces
    /// a different fingerprint, so an answer computed against *this* dialog is refused rather than
    /// landing on whatever replaced it. Without it, a stale browser tab or a slow menu could
    /// approve a command nobody ever read.
    pub fingerprint: u64,
    /// The unframed dialog text, for display.
    pub excerpt: Vec<String>,
}

impl PendingPrompt {
    /// The option carrying `semantic`, if the harness offered one.
    pub fn option_for(&self, semantic: PromptSemantic) -> Option<&PromptOption> {
        self.options.iter().find(|o| o.semantic == semantic)
    }

    /// The number to send for `semantic`, preferring an exact match and falling back for approvals:
    /// asking to approve when only a session-scoped allow is offered should succeed, and vice
    /// versa, because both grant the action. A rejection never falls back — declining into a
    /// feedback composer is a different outcome from declining outright.
    pub fn resolve(&self, semantic: PromptSemantic) -> Option<u8> {
        if let Some(o) = self.option_for(semantic) {
            return Some(o.number);
        }
        match semantic {
            PromptSemantic::Approve => self.option_for(PromptSemantic::ApproveSession),
            PromptSemantic::ApproveSession => self.option_for(PromptSemantic::Approve),
            _ => None,
        }
        .map(|o| o.number)
    }

    /// A one-line summary for a fleet card or a toast.
    pub fn summary(&self) -> String {
        match &self.subject {
            Some(s) if !s.is_empty() => format!("{} — {s}", self.question),
            _ => self.question.clone(),
        }
    }
}

/// How many lines above the option block may be read as the dialog's heading. Bounded so a long
/// scrollback above a menu never becomes the "question".
const MAX_HEADING_LINES: usize = 8;

/// How long a rendered subject may get before it is elided.
const MAX_SUBJECT_CHARS: usize = 240;

/// Parse the dialog a pane is blocked on, or `None` when it is not blocked on one.
///
/// The kind comes from the existing [`classify`] — there is one pane classifier, and this is not a
/// second opinion — with one refinement: a trailing block whose own options include both an
/// approval and an explicit session-scoped allow **is** a command approval, whatever the heading
/// says. Claude's permission prompt heads with the generic "Do you want to proceed?", which
/// `classify` reads (correctly, on wording alone) as `QuestionMenu`; the option shape is what
/// tells the two apart, and no arbitrary question offers "don't ask again".
pub fn parse_pending_prompt(
    tail: &[String],
    plane_a: &str,
    has_window: bool,
    has_wip: bool,
) -> Option<PendingPrompt> {
    let obs = PaneObservation {
        tail,
        has_window,
        plane_a,
        has_wip,
    };
    let state = classify(&obs);

    // A trust menu blocks boot before any numbered list is drawn, so it is reported with whatever
    // options are on screen — possibly none.
    let block = orchestrate::trailing_menu_block(tail);
    let (heading_start, menu_start, last_option) = match block {
        Some(b) => b,
        None => return trust_menu_without_options(state, tail, plane_a),
    };

    let options: Vec<PromptOption> = tail[menu_start..=last_option]
        .iter()
        .filter_map(|line| {
            let (number, label) = orchestrate::menu_entry(line)?;
            Some(PromptOption {
                number,
                label: label.to_string(),
                semantic: PromptSemantic::classify(label),
                selected: orchestrate::menu_selected(line),
            })
        })
        .collect();
    if options.is_empty() {
        return trust_menu_without_options(state, tail, plane_a);
    }

    let approval_shaped = options
        .iter()
        .any(|o| o.semantic == PromptSemantic::ApproveSession)
        && options
            .iter()
            .any(|o| o.semantic == PromptSemantic::Approve);
    let kind = match state {
        PaneState::QuestionMenu if approval_shaped => PaneState::CommandApproval,
        s => s,
    };
    if !matches!(
        kind,
        PaneState::TrustMenu | PaneState::CommandApproval | PaneState::QuestionMenu
    ) {
        return None;
    }

    let heading = heading_lines(&tail[heading_start..menu_start]);
    let (question, subject) = split_heading(&heading);

    Some(PendingPrompt {
        kind,
        question,
        subject,
        options,
        fingerprint: super::pane_activity_fingerprint(tail, plane_a),
        excerpt: heading
            .iter()
            .cloned()
            .chain(dialog_lines(&tail[menu_start..]))
            .collect(),
    })
}

/// A trust menu that blocks before drawing a numbered list still needs reporting — it is the most
/// common thing a fresh non-`--yolo` dispatch parks on. Every other option-less state is not a
/// dialog, and returns `None`.
fn trust_menu_without_options(
    state: PaneState,
    tail: &[String],
    plane_a: &str,
) -> Option<PendingPrompt> {
    (state == PaneState::TrustMenu).then(|| {
        let heading = heading_lines(tail);
        let (question, subject) = split_heading(&heading);
        PendingPrompt {
            kind: state,
            question,
            subject,
            options: Vec::new(),
            fingerprint: super::pane_activity_fingerprint(tail, plane_a),
            excerpt: heading,
        }
    })
}

/// The dialog's heading: the content lines above the option block, unframed, bounded.
///
/// A box rule is a hard boundary — everything above the dialog's top border is the agent's own
/// prose, and pulling it in would make the last thing the agent *said* part of the subject a policy
/// rule is matched against.
fn heading_lines(lines: &[String]) -> Vec<String> {
    let after_border = lines
        .iter()
        .rposition(|l| is_box_rule(l))
        .map_or(0, |i| i + 1);
    let content: Vec<String> = lines[after_border..]
        .iter()
        .filter(|l| !is_frame_only(l))
        .map(|l| unframe(l).to_string())
        .collect();
    let start = content.len().saturating_sub(MAX_HEADING_LINES);
    content[start..].to_vec()
}

/// The option block and any trailing chrome, unframed, for display.
fn dialog_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| !is_frame_only(l))
        .map(|l| unframe(l).to_string())
        .collect()
}

/// Split a heading into the question and the subject under review.
///
/// The question is the last heading line that reads as one (ends in `?` or `:`); everything above
/// it is the subject — for a Claude permission dialog, that is the tool title plus the command
/// itself, which is exactly what a deny rule needs to match on. A heading with no question line
/// falls back to its last line, so a dialog bn has never seen still gets a sensible summary rather
/// than an empty one.
fn split_heading(heading: &[String]) -> (String, Option<String>) {
    // A question mark *anywhere* on the line, not only at the end. A harness hard-wraps its prompt
    // to the pane width, so the real question frequently ends mid-sentence — Claude's trust prompt
    // arrives as "…or one you trust? (Like your own code, a" with the parenthetical spilling onto
    // the next line. Matching only on a trailing `?` skipped it entirely and fell back to whatever
    // label happened to sit above.
    let q_idx = heading
        .iter()
        .rposition(|s| s.contains('?'))
        .or_else(|| heading.iter().rposition(|s| s.ends_with(':')));
    let question = match q_idx {
        // Cut at the question mark: everything after it on a wrapped line is the next sentence.
        Some(i) => match heading[i].find('?') {
            Some(pos) => heading[i][..=pos].trim().to_string(),
            None => heading[i].clone(),
        },
        None => heading.last().cloned().unwrap_or_default(),
    };
    let subject_end = q_idx.unwrap_or(heading.len().saturating_sub(1));
    let subject: String = heading[..subject_end].join(" · ").trim().to_string();
    // Bounded so a long preamble can't turn a fleet card into a paragraph, or a policy rule into a
    // match against half the screen.
    let subject = match subject.char_indices().nth(MAX_SUBJECT_CHARS) {
        Some((cut, _)) => format!("{}…", &subject[..cut]),
        None => subject,
    };
    (question, (!subject.is_empty()).then_some(subject))
}
