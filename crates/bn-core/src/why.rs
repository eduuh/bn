//! `bn why` — commit ↔ branch-note provenance (#248).
//!
//! Write side: a `prepare-commit-msg` git hook (installed by `bn hooks install`, CLI-side)
//! calls into [`append_trailers`] to stamp `BN-Note: <repo>/<branch>` (and the latest
//! `BN-Decision:`) onto commits made on branches that have a note. Read side: [`why_file`] /
//! [`why_commit`] blame or inspect, read the trailers back, and resolve them to note context
//! (goal, decisions, matching done todos) — including closed and archived notes. The read
//! path is offline: notes + git only, all through the [`CommandRunner`] seam.

use crate::model::Note;
use crate::resolver::{Config, Resolved};
use crate::runner::CommandRunner;
use crate::{render, store};
use serde::Serialize;
use std::path::PathBuf;

/// Trailer linking a commit to the branch note that produced it: `BN-Note: <repo>/<branch>`.
/// `<branch>` is the note key (slashes already normalized to dashes by the resolver), so the
/// value splits unambiguously at its first `/`.
pub const NOTE_TRAILER: &str = "BN-Note";
/// Trailer carrying the most recent decision at commit time: `BN-Decision: <text>`.
pub const DECISION_TRAILER: &str = "BN-Decision";

// --- trailer writing (the prepare-commit-msg hook body) --------------------------------------

/// Append `BN-Note:` (and optionally `BN-Decision:`) trailers to a commit-message file's
/// content. Returns the new content, or `None` when nothing should change (a `BN-Note:` line
/// is already present). Trailers land before the `#` comment block a template commit shows in
/// the editor, and never touch the subject/body: an existing trailer block (`Signed-off-by:`,
/// `Co-Authored-By:`) is extended in place, anything else gets a new blank-line-separated block.
pub fn append_trailers(msg: &str, note_ref: &str, decision: Option<&str>) -> Option<String> {
    let (body, comments) = msg.split_at(comment_block_start(msg));
    let already = body
        .lines()
        .any(|l| l.trim_start().starts_with(&format!("{NOTE_TRAILER}:")));
    if already {
        return None;
    }

    let mut block = format!("{NOTE_TRAILER}: {note_ref}\n");
    if let Some(d) = decision {
        let d = d.replace(['\n', '\r'], " ");
        let d = d.trim();
        if !d.is_empty() {
            block.push_str(&format!("{DECISION_TRAILER}: {d}\n"));
        }
    }

    let trimmed = body.trim_end();
    let new_body = if trimmed.is_empty() {
        // Editor template with no message yet: keep the top blank for the subject to come.
        format!("\n\n{block}")
    } else if ends_in_trailer_block(trimmed) {
        format!("{trimmed}\n{block}")
    } else {
        format!("{trimmed}\n\n{block}")
    };
    Some(new_body + comments)
}

/// Byte offset where the trailing `#` comment block starts (the template `git commit` shows in
/// the editor), or the end of the message when there is none. Trailers must go before it.
fn comment_block_start(msg: &str) -> usize {
    let mut off = 0;
    for line in msg.split_inclusive('\n') {
        if line.starts_with('#') {
            return off;
        }
        off += line.len();
    }
    msg.len()
}

/// A `Key: value` line with a git-trailer-shaped key (alphanumeric + dashes).
fn is_trailer_line(l: &str) -> bool {
    match l.split_once(':') {
        Some((key, _)) => {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        None => false,
    }
}

/// Whether the (trailing-whitespace-trimmed) body already ends in a trailer block — a final
/// paragraph made entirely of trailer lines that isn't the message's only paragraph (a bare
/// `fix: subject` line is a subject, not a trailer).
fn ends_in_trailer_block(body: &str) -> bool {
    let Some(idx) = body.rfind("\n\n") else {
        return false;
    };
    let last = &body[idx + 2..];
    !last.trim().is_empty() && last.lines().all(is_trailer_line)
}

/// Value of trailer `key` in a full commit message: scanned from the message's last paragraph
/// (git's trailer block), last occurrence wins. `None` for single-paragraph messages — a
/// subject line is never a trailer.
pub fn trailer_value(message: &str, key: &str) -> Option<String> {
    let body = message.trim_end();
    let idx = body.rfind("\n\n")?;
    let prefix = format!("{key}:");
    let mut found = None;
    for l in body[idx + 2..].lines() {
        if let Some(rest) = l.strip_prefix(&prefix) {
            found = Some(rest.trim().to_string());
        }
    }
    found
}

/// The decision to stamp on the next commit: the note's most recent decision, unless HEAD's
/// `BN-Decision:` trailer already carries it (i.e. nothing new since the last commit).
pub fn decision_to_stamp(note: &Note, head_message: Option<&str>) -> Option<String> {
    let last = note.front.decisions.last()?;
    let text = last.text.replace(['\n', '\r'], " ").trim().to_string();
    if text.is_empty() {
        return None;
    }
    if let Some(head) = head_message {
        if trailer_value(head, DECISION_TRAILER).as_deref() == Some(text.as_str()) {
            return None;
        }
    }
    Some(text)
}

// --- note resolution (including closed + archived notes) -------------------------------------

/// A note resolved from a `<repo>/<branch>` trailer value, wherever it lives now.
#[derive(Debug)]
pub struct LocatedNote {
    /// The `note.md` path.
    pub path: PathBuf,
    pub note: Note,
    /// True when found under the root-level `archive/` (moved there by `bn archive`).
    pub archived: bool,
}

/// Resolve a `BN-Note: <repo>/<branch>` value to its note: this machine's subtree first, then
/// the other machines' subtrees (a commit may have been made on another host), then the
/// root-level archive. Closed notes still live in their machine subtree until archived, so
/// this covers active, closed, and archived alike.
pub fn locate_note(cfg: &Config, note_ref: &str) -> Option<LocatedNote> {
    let (repo, branch) = note_ref.split_once('/')?;
    if repo.is_empty() || branch.is_empty() {
        return None;
    }
    let root = cfg.notes_dir_for(repo);
    for m in machine_dirs(cfg, repo) {
        let p = root.join(&m).join(repo).join(branch).join("note.md");
        if let Ok(note) = store::read(&p) {
            return Some(LocatedNote {
                path: p,
                note,
                archived: false,
            });
        }
    }
    let p = root.join("archive").join(repo).join(branch).join("note.md");
    match store::read(&p) {
        Ok(note) => Some(LocatedNote {
            path: p,
            note,
            archived: true,
        }),
        Err(_) => None,
    }
}

/// Machine subtree names under `repo`'s notes root, this machine first, the rest sorted.
/// The root-level `archive/` dir is not a machine.
fn machine_dirs(cfg: &Config, repo: &str) -> Vec<String> {
    let mut out = vec![cfg.machine().to_string()];
    if let Ok(entries) = std::fs::read_dir(cfg.notes_dir_for(repo)) {
        let mut rest: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|m| m != "archive" && m != cfg.machine())
            .collect();
        rest.sort();
        out.extend(rest);
    }
    out
}

/// Fallback resolution for pre-trailer commits: find `repo`'s note whose frontmatter `pr`
/// references PR `number` (bare number or a URL ending in `/<number>`). Scans the machine
/// subtrees then the archive; returns the `<repo>/<branch>` ref alongside the note.
pub fn locate_note_by_pr(cfg: &Config, repo: &str, number: u64) -> Option<(String, LocatedNote)> {
    let root = cfg.notes_dir_for(repo);
    let mut tiers: Vec<(PathBuf, bool)> = machine_dirs(cfg, repo)
        .into_iter()
        .map(|m| (root.join(m).join(repo), false))
        .collect();
    tiers.push((root.join("archive").join(repo), true));

    for (repo_dir, archived) in tiers {
        let Ok(entries) = std::fs::read_dir(&repo_dir) else {
            continue;
        };
        let mut branches: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        branches.sort();
        for branch_dir in branches {
            let p = branch_dir.join("note.md");
            let Ok(note) = store::read(&p) else {
                continue;
            };
            let matches = note.front.pr.as_ref().is_some_and(|s| {
                let d = s.display();
                d == number.to_string() || d.ends_with(&format!("/{number}"))
            });
            if matches {
                let branch = branch_dir
                    .file_name()
                    .map(|b| b.to_string_lossy().to_string())
                    .unwrap_or_default();
                return Some((
                    format!("{repo}/{branch}"),
                    LocatedNote {
                        path: p,
                        note,
                        archived,
                    },
                ));
            }
        }
    }
    None
}

// --- the read side: bn why <file> / <commit> --------------------------------------------------

/// One introducing commit, as displayed: abbreviated sha + subject.
#[derive(Debug, Serialize)]
pub struct WhyCommit {
    pub sha: String,
    pub subject: String,
}

/// The note context a group of commits resolved to.
#[derive(Debug, Serialize)]
pub struct WhyContext {
    pub goal: Option<String>,
    pub decisions: Vec<String>,
    /// Done todos whose text references the target file or the group's commits/PR.
    pub done_todos: Vec<String>,
    /// `active` | `closed` | `archived`.
    pub status: String,
    pub note_path: String,
}

/// Commits grouped by provenance: one group per `BN-Note:` value, plus at most one
/// trailer-less group (whose note may still resolve via the squash-subject `(#N)` fallback).
#[derive(Debug, Serialize)]
pub struct WhyGroup {
    /// The resolved `<repo>/<branch>` — from the trailer, or recovered via the PR fallback.
    pub note_ref: Option<String>,
    pub commits: Vec<WhyCommit>,
    pub context: Option<WhyContext>,
    /// PR number parsed from a squash-merge subject, for trailer-less commits.
    pub fallback_pr: Option<u64>,
}

/// The full `bn why` answer for one target.
#[derive(Debug, Serialize)]
pub struct WhyReport {
    pub target: String,
    pub groups: Vec<WhyGroup>,
}

/// `bn why <commit>`: resolve `rev` to a commit and report its branch-note provenance.
pub fn why_commit(
    cfg: &Config,
    ctx: &Resolved,
    runner: &dyn CommandRunner,
    rev: &str,
) -> Result<WhyReport, String> {
    let spec = format!("{rev}^{{commit}}");
    let sha = runner
        .git(&["rev-parse", "--verify", &spec])
        .ok_stdout()
        .ok_or_else(|| format!("'{rev}' is not a commit"))?;
    Ok(build_report(
        cfg,
        ctx,
        runner,
        rev.to_string(),
        None,
        &[sha],
    ))
}

/// `bn why <file> [-L range]`: blame the file, collect the introducing commits, and report
/// their branch-note provenance grouped by note. `lines` is `START` or `START,END`.
pub fn why_file(
    cfg: &Config,
    ctx: &Resolved,
    runner: &dyn CommandRunner,
    file: &str,
    lines: Option<&str>,
) -> Result<WhyReport, String> {
    let mut args: Vec<String> = vec!["blame".into(), "--line-porcelain".into()];
    if let Some(r) = lines {
        let range = if r.contains(',') {
            r.to_string()
        } else {
            format!("{r},{r}")
        };
        args.push("-L".into());
        args.push(range);
    }
    args.push("--".into());
    args.push(file.into());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = runner.git(&arg_refs);
    if !out.success {
        let err = out.stderr.trim();
        return Err(if err.is_empty() {
            format!("git blame failed for {file}")
        } else {
            format!("git blame failed for {file}: {err}")
        });
    }
    let shas = blame_shas(&out.stdout);
    Ok(build_report(
        cfg,
        ctx,
        runner,
        file.to_string(),
        Some(file),
        &shas,
    ))
}

/// Unique commit shas from `git blame --line-porcelain` output, first-seen order. Header lines
/// start with the 40-hex sha; the all-zero sha (uncommitted lines) is skipped.
fn blame_shas(out: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for l in out.lines() {
        let tok = l.split(' ').next().unwrap_or("");
        if tok.len() == 40
            && tok.bytes().all(|b| b.is_ascii_hexdigit())
            && !tok.bytes().all(|b| b == b'0')
            && !seen.iter().any(|s| s == tok)
        {
            seen.push(tok.to_string());
        }
    }
    seen
}

/// Group `shas` by their `BN-Note:` trailer and resolve each group to note context. `file` is
/// the blamed path (for todo matching), `None` in commit mode.
fn build_report(
    cfg: &Config,
    ctx: &Resolved,
    runner: &dyn CommandRunner,
    target: String,
    file: Option<&str>,
    shas: &[String],
) -> WhyReport {
    // (note_ref from trailer, displayed commits, full shas for todo matching)
    let mut raw: Vec<(Option<String>, Vec<WhyCommit>, Vec<String>)> = Vec::new();
    for sha in shas {
        let msg_out = runner.git(&["show", "-s", "--format=%B", sha]);
        let note_ref = if msg_out.success {
            trailer_value(&msg_out.stdout, NOTE_TRAILER)
        } else {
            None
        };
        let meta = runner
            .git(&["show", "-s", "--format=%h\t%s", sha])
            .ok_stdout()
            .unwrap_or_else(|| sha.clone());
        let (short, subject) = match meta.split_once('\t') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (meta, String::new()),
        };
        let commit = WhyCommit {
            sha: short,
            subject,
        };
        match raw.iter_mut().find(|g| g.0 == note_ref) {
            Some(g) => {
                g.1.push(commit);
                g.2.push(sha.clone());
            }
            None => raw.push((note_ref, vec![commit], vec![sha.clone()])),
        }
    }

    let groups = raw
        .into_iter()
        .map(|(trailer_ref, commits, full_shas)| {
            let mut fallback_pr = None;
            let (note_ref, located) = match &trailer_ref {
                Some(r) => (Some(r.clone()), locate_note(cfg, r)),
                None => {
                    fallback_pr = commits.iter().find_map(|c| pr_from_subject(&c.subject));
                    match fallback_pr.and_then(|n| locate_note_by_pr(cfg, &ctx.repo, n)) {
                        Some((r, ln)) => (Some(r), Some(ln)),
                        None => (None, None),
                    }
                }
            };
            let context = located.map(|ln| {
                let status = if ln.archived {
                    "archived"
                } else if ln.note.front.status == "closed" {
                    "closed"
                } else {
                    "active"
                };
                let done_todos = ln
                    .note
                    .front
                    .todos
                    .iter()
                    .filter(|t| t.done == Some(true))
                    .filter(|t| todo_matches(&t.text, file, &full_shas, fallback_pr))
                    .map(|t| t.text.clone())
                    .collect();
                WhyContext {
                    goal: render::goal_text(&ln.note.body),
                    decisions: ln
                        .note
                        .front
                        .decisions
                        .iter()
                        .map(|d| d.text.clone())
                        .collect(),
                    done_todos,
                    status: status.to_string(),
                    note_path: ln.path.display().to_string(),
                }
            });
            WhyGroup {
                note_ref,
                commits,
                context,
                fallback_pr,
            }
        })
        .collect();
    WhyReport { target, groups }
}

/// PR number from a squash-merge subject's trailing `(#N)` marker (last one wins) — the
/// offline fallback for commits that predate the trailer hook.
pub fn pr_from_subject(subject: &str) -> Option<u64> {
    let mut found = None;
    let mut rest = subject;
    while let Some(i) = rest.find("(#") {
        let tail = &rest[i + 2..];
        let digits: &str = &tail[..tail.bytes().take_while(u8::is_ascii_digit).count()];
        if !digits.is_empty() && tail[digits.len()..].starts_with(')') {
            found = digits.parse().ok();
        }
        rest = tail;
    }
    found
}

/// Whether a done todo's text references the blamed file (full path or basename), one of the
/// group's commits (7+ chars of a sha), or the fallback PR (`#N` with a digit boundary, the
/// `references_pr` precedent from #229 — `#94` matches, `#940` doesn't).
fn todo_matches(text: &str, file: Option<&str>, shas: &[String], pr: Option<u64>) -> bool {
    if let Some(f) = file {
        let base = std::path::Path::new(f)
            .file_name()
            .map(|b| b.to_string_lossy().to_string())
            .unwrap_or_else(|| f.to_string());
        if text.contains(f) || text.contains(base.as_str()) {
            return true;
        }
    }
    if shas.iter().any(|s| s.len() >= 7 && text.contains(&s[..7])) {
        return true;
    }
    if let Some(n) = pr {
        let hash = format!("#{n}");
        for (idx, _) in text.match_indices(&hash) {
            let next = text[idx + hash.len()..].chars().next();
            if !matches!(next, Some(c) if c.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}


