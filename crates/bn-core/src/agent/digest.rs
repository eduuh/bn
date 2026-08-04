use super::session::{sessions_for, AgentSession};
use crate::doctor::last_activity;
use crate::model::Note;
use crate::resolver::Config;
use serde::Serialize;

// --- digest: session → proposed note lines --------------------------------------------------
//
// A reconcile pass that mirrors `bn sync`'s propose-never-write contract ([`crate::sync`]): it
// reads the recorded sessions plus the note and returns suggested note lines — work that
// happened in an agent session but never landed in the note. The caller (CLI confirm, `--yes`,
// or `--json`) decides whether to apply; nothing here writes.

/// Which note section a [`DigestProposal`] would be filed under, mirroring the `bn add` sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestKind {
    /// A progress bullet appended to the note body — what the session changed.
    Progress,
    /// A decision line — recorded when the session shipped a non-trivial code change.
    Decision,
    /// A follow-up todo — left when the session's work looks unfinished (changes, no PR-close).
    Todo,
}

impl DigestKind {
    /// The `bn add` section keyword (`progress` is body prose, not a structured section).
    pub fn section(self) -> &'static str {
        match self {
            DigestKind::Progress => "progress",
            DigestKind::Decision => "decision",
            DigestKind::Todo => "todo",
        }
    }
}

/// One proposed note line drafted from an uncovered session. Carries the session context the
/// confirm prompt / agent needs to decide.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DigestProposal {
    /// The session this line summarizes — the dedup key, so an applied line is never re-proposed.
    pub session_id: String,
    /// Where the line would be filed.
    pub kind: DigestKind,
    /// The drafted line text.
    pub text: String,
    /// Session date (`YYYY-MM-DD`) for the prompt.
    pub date: String,
    /// Rough wall-clock minutes the session ran.
    pub duration_mins: i64,
    /// "claude" | "copilot".
    pub harness: String,
}

/// The digest result: every line `bn agent digest` would propose. Never applied here — the
/// caller confirms, `--yes`-applies, or emits this as JSON. `is_empty` drives the silent, clean
/// exit when the note already reflects every recorded session.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Digest {
    /// The marker the digest reconciled from — the note's latest activity date (`YYYY-MM-DD`).
    pub since: String,
    /// Count of sessions considered uncovered (newer than `since`).
    pub sessions: usize,
    /// The proposed note lines, newest session first.
    pub proposals: Vec<DigestProposal>,
}

impl Digest {
    /// True when there is nothing to propose — drives the silent, clean exit.
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

/// Reconcile this branch's recorded agent sessions against the note, returning proposed note
/// lines for the work that never landed. Reads the synced metrics log (via [`sessions_for`]);
/// pure beyond that read — writes nothing.
pub fn digest(cfg: &Config, note: &Note, repo: &str, branch: &str) -> Digest {
    digest_sessions(note, &sessions_for(cfg, repo, branch))
}

/// [`digest`]'s pure core: given the note and its sessions (newest-first), draft the proposals.
/// A session is *covered* when its end date is on or before the note's last-activity marker — the
/// same date dedup `bn sync` uses — so re-running after applying (which bumps that marker) never
/// re-proposes the same work.
pub fn digest_sessions(note: &Note, sessions: &[AgentSession]) -> Digest {
    let since = last_activity(note);
    let uncovered: Vec<&AgentSession> = sessions
        .iter()
        .filter(|s| crate::util::date_of(s.ended_ts) > since)
        .collect();

    let mut proposals = Vec::new();
    for s in &uncovered {
        proposals.extend(draft_lines(s));
    }

    Digest {
        since,
        sessions: uncovered.len(),
        proposals,
    }
}

/// Draft the note lines for one uncovered session: always a progress bullet; a decision and a
/// follow-up todo only when the session shipped a non-trivial code change, since a no-code
/// session (planning, reading) warrants neither.
fn draft_lines(s: &AgentSession) -> Vec<DigestProposal> {
    let mins = (s.duration_secs() + 30) / 60; // round to nearest minute
    let date = crate::util::date_of(s.ended_ts);
    let make = |kind: DigestKind, text: String| DigestProposal {
        session_id: s.session_id.clone(),
        kind,
        text,
        date: date.clone(),
        duration_mins: mins,
        harness: s.harness.clone(),
    };

    let mut out = vec![make(DigestKind::Progress, progress_text(s, mins))];

    // A session with real code changes is worth a decision + a follow-up todo; one without
    // (pure exploration) gets only the progress note.
    if has_code_change(s) {
        out.push(make(DigestKind::Decision, decision_text(s)));
        out.push(make(
            DigestKind::Todo,
            "review the agent session's changes and commit / open a PR".to_string(),
        ));
    }
    out
}

/// True when the session touched code — used to gate the decision/todo proposals. Claude
/// transcripts carry no diff stats, so this is Copilot-only in practice (lines/files are 0 for
/// Claude); the gate degrades to "progress only" rather than guessing.
fn has_code_change(s: &AgentSession) -> bool {
    s.files_modified > 0 || s.lines_added > 0 || s.lines_removed > 0
}

/// The progress bullet: rough time + the diff shape when known, falling back to turn/tool counts
/// for a no-diff (Claude) session so the line still says something concrete.
fn progress_text(s: &AgentSession, mins: i64) -> String {
    if has_code_change(s) {
        format!(
            "{mins}m agent session ({}): +{}/-{} across {} file{}",
            s.harness,
            s.lines_added,
            s.lines_removed,
            s.files_modified,
            if s.files_modified == 1 { "" } else { "s" },
        )
    } else {
        format!(
            "{mins}m agent session ({}): {} turn{}, {} tool call{}",
            s.harness,
            s.turns,
            if s.turns == 1 { "" } else { "s" },
            s.tool_calls,
            if s.tool_calls == 1 { "" } else { "s" },
        )
    }
}

/// The decision line drafted for a code-changing session — a stub the user fills in with the
/// *why*, anchored to the concrete change so it's not a blank prompt.
fn decision_text(s: &AgentSession) -> String {
    format!(
        "agent session changed {} file{} (+{}/-{}) — record the rationale",
        s.files_modified,
        if s.files_modified == 1 { "" } else { "s" },
        s.lines_added,
        s.lines_removed,
    )
}


