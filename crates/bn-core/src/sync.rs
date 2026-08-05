//! `bn sync` — the git/PR → note reconciler. Derives note state from ground truth (the commit
//! log since the note was last touched, plus the current PR state) and returns a set of
//! **proposals** — never writes. The caller (CLI interactive confirm, `--yes`, or `--json` for
//! an agent) decides whether to apply them.
//!
//! Three proposal kinds, mirroring the acceptance criteria:
//! - a **progress line** drafted from the commit subjects since the last-updated marker;
//! - **`bn done` candidates** — each commit mapped to the open todo it most likely closes
//!   (keyword overlap), so a finished-but-still-open todo gets nudged shut;
//! - on a **MERGED** PR (via `gh pr view`), a **close + `wt cleanup`** proposal.
//!
//! All git/gh reads go through the [`CommandRunner`] seam so the whole reconciliation is
//! deterministic under [`FakeRunner`] + [`FixedClock`]; nothing here touches the filesystem.

use crate::clock::Clock;
use crate::doctor::last_activity;
use crate::model::Note;
use crate::runner::CommandRunner;
use serde::Serialize;
use std::collections::HashSet;

/// One commit since the note's last-updated marker: short hash + subject line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
}

/// A commit matched to the open todo it most likely closes — the basis for a `bn done`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoneCandidate {
    /// The open todo's id (e.g. `t3`).
    pub todo_id: String,
    /// The open todo's text (for the confirm prompt / agent).
    pub todo_text: String,
    /// The commit whose subject best matched it.
    pub commit: Commit,
}

/// The reconciliation result: everything `bn sync` would propose. Never applied here — the
/// caller confirms, `--yes`-applies, or emits this as JSON. `is_empty` drives the silent/clean
/// exit when the note is already in sync.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Proposals {
    /// The marker reconciliation ran from — the note's latest activity date (`YYYY-MM-DD`).
    pub since: String,
    /// Commits on the branch newer than `since`, newest first.
    pub commits: Vec<Commit>,
    /// A single progress bullet drafted from `commits`, or `None` when there were none.
    pub progress_line: Option<String>,
    /// Open todos a commit looks to have closed — propose `bn done` for each.
    pub done_candidates: Vec<DoneCandidate>,
    /// Set when the branch PR is MERGED: propose `bn close` + `wt cleanup`.
    pub close: Option<CloseProposal>,
}

/// The close proposal raised on a MERGED PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseProposal {
    /// The merged PR's URL (for the prompt / `bn pr` record).
    pub pr_url: String,
    /// The `wt cleanup` slug to run after closing (`<repo>/<branch>`).
    pub cleanup_slug: String,
}

impl Proposals {
    /// True when there is nothing to propose — drives `bn sync`'s silent, clean exit.
    pub fn is_empty(&self) -> bool {
        self.progress_line.is_none() && self.done_candidates.is_empty() && self.close.is_none()
    }
}

/// Compute the proposals for `note` on branch `repo/branch`, reading commits and PR state
/// through `runner` and "now" through `clock`. Pure: reads only, writes nothing.
///
/// `since` is the note's last-activity date ([`last_activity`]); commits are read with
/// `git log --since=<since> <ref>` so reconciliation only considers work after the note was
/// last touched. The PR state comes from `gh pr view --json state,url`.
pub fn reconcile(
    note: &Note,
    repo: &str,
    branch: &str,
    runner: &dyn CommandRunner,
    _clock: &dyn Clock,
) -> Proposals {
    let since = last_activity(note);
    let commits = commits_since(&since, runner);

    let progress_line = draft_progress_line(&commits);
    let done_candidates = match_todos(note, &commits);
    let close = merged_close(repo, branch, runner);

    Proposals {
        since,
        commits,
        progress_line,
        done_candidates,
        close,
    }
}

/// Commits authored after `since` (`YYYY-MM-DD`), newest first, read through the runner.
/// Each line is `<short-hash>\x1f<subject>` (a unit-separator avoids clashing with subjects
/// that contain any printable delimiter). Empty when git fails or there's nothing newer.
fn commits_since(since: &str, runner: &dyn CommandRunner) -> Vec<Commit> {
    let fmt = "--pretty=format:%h\x1f%s";
    let since_arg = format!("--since={since}");
    let out = runner.git(&["log", &since_arg, fmt]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('\x1f')?;
            let subject = subject.trim();
            if hash.is_empty() || subject.is_empty() {
                return None;
            }
            Some(Commit {
                hash: hash.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect()
}

/// Draft one progress bullet from the commit subjects (conventional-commit prefixes stripped),
/// or `None` when there were no commits. One commit → its cleaned subject; several → a count
/// plus the most recent, so the line stays short.
fn draft_progress_line(commits: &[Commit]) -> Option<String> {
    let first = commits.first()?;
    let head = clean_subject(&first.subject);
    if commits.len() == 1 {
        Some(head)
    } else {
        Some(format!("{} ({} commits)", head, commits.len()))
    }
}

/// Map each open todo to the commit whose subject shares the most significant words with it,
/// proposing a `bn done` when the overlap is non-trivial. At most one candidate per todo (its
/// best commit) and one per commit (a commit closes a single todo), so an applied batch never
/// double-counts. Ordered by todo id for a stable proposal set.
fn match_todos(note: &Note, commits: &[Commit]) -> Vec<DoneCandidate> {
    let mut candidates = Vec::new();
    let mut used_commits: Vec<usize> = Vec::new();
    for todo in note.front.todos.iter().filter(|t| t.is_open_todo()) {
        let todo_words: HashSet<String> = significant_words(&todo.text).into_iter().collect();
        if todo_words.is_empty() {
            continue;
        }
        let mut best: Option<(usize, usize)> = None; // (commit index, overlap)
        for (i, commit) in commits.iter().enumerate() {
            if used_commits.contains(&i) {
                continue;
            }
            // Count *distinct* shared words: a commit subject that repeats one keyword
            // (`"tracker tracker"`) must not clear MIN_OVERLAP on a single real match.
            let commit_words: HashSet<String> =
                significant_words(&commit.subject).into_iter().collect();
            let overlap = commit_words.intersection(&todo_words).count();
            if overlap >= MIN_OVERLAP && best.is_none_or(|(_, b)| overlap > b) {
                best = Some((i, overlap));
            }
        }
        if let Some((i, _)) = best {
            used_commits.push(i);
            candidates.push(DoneCandidate {
                todo_id: todo.id.clone(),
                todo_text: todo.text.clone(),
                commit: commits[i].clone(),
            });
        }
    }
    candidates
}

/// Minimum shared significant words for a commit→todo match. Two keeps it from firing on a
/// single incidental word ("the", already filtered, aside) while still catching real overlaps.
const MIN_OVERLAP: usize = 2;

/// If the branch PR is MERGED, propose closing the note and cleaning up the worktree. Reads
/// `gh pr view --json state,url`; `None` when gh is absent, there's no PR, or it isn't merged.
fn merged_close(repo: &str, branch: &str, runner: &dyn CommandRunner) -> Option<CloseProposal> {
    let out = runner.gh(&["pr", "view", "--json", "state,url"]);
    let stdout = out.ok_stdout()?;
    let parsed: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    if parsed.get("state").and_then(|s| s.as_str()) != Some("MERGED") {
        return None;
    }
    let pr_url = parsed.get("url").and_then(|u| u.as_str())?.to_string();
    Some(CloseProposal {
        pr_url,
        cleanup_slug: format!("{repo}/{branch}"),
    })
}

/// Strip a leading conventional-commit prefix (`feat:`, `fix(scope):`, …) and any trailing
/// `(#123)` PR-number suffix, leaving the human-readable summary.
fn clean_subject(subject: &str) -> String {
    let mut s = subject.trim();
    // Drop a leading `type` / `type(scope)` followed by `:` — the conventional-commit prefix.
    if let Some((prefix, rest)) = s.split_once(':') {
        let prefix = prefix.trim();
        let kind = prefix.split('(').next().unwrap_or(prefix);
        let is_cc = !kind.is_empty()
            && kind.chars().all(|c| c.is_ascii_lowercase())
            && (prefix == kind || prefix.ends_with(')'));
        if is_cc {
            s = rest.trim();
        }
    }
    // Drop a trailing `(#123)` PR reference.
    if let Some(idx) = s.rfind("(#") {
        if s[idx..].ends_with(')') && s[idx + 2..s.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            s = s[..idx].trim();
        }
    }
    s.to_string()
}

/// Lowercased words of length ≥ 3 that aren't common stop-words — the keyword set used for the
/// commit↔todo overlap. Punctuation is split on, so `feat(nvim):` → `feat`, `nvim`.
fn significant_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.len() >= 3 && !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Filler words that carry no matching signal — dropped before computing overlap.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "add", "added", "fix", "fixed", "use", "using", "via", "from",
    "into", "out", "all", "new", "now", "not", "this", "that", "than", "then",
];
