//! A task's context of record — the long form a one-line task can't hold (#997, EPIC #993).
//!
//! A task line is one line. Everything that makes it *SMART* — why it matters, the done-when
//! checklist, what an investigator found, what is still unknown — needs somewhere durable to live.
//! That somewhere is the notes tree, where it already syncs with the notes git repo, is indexed by
//! [`crate::recall`], and is readable offline.
//!
//! # Two locations, one resolver
//!
//! ```text
//! <personal_root>/tasks/<id>.md                      staging — before pickup
//! <personal_root>/<machine>/<repo>/<branch>/task.md  after pickup, beside the branch note
//! ```
//!
//! [`Config::note_dir`] needs `(repo, branch)`, and a freshly created task has no branch until it
//! is picked up into a worktree — so "in the note folder" is unsatisfiable at creation time, and
//! the doc starts in staging.
//!
//! On pickup the doc is **copied**, not moved, and [`doc_path`] prefers the note-dir copy. A move
//! would leave every previously-read path dangling; a copy plus a resolver means a stale reference
//! still resolves to something real. For the same reason the task's `doc:` meta holds a *file
//! name*, never a path: a bare name cannot point at the wrong directory.
//!
//! # The document is a note body
//!
//! `## Done When` is the shipped [`Section::DoneWhen`], read through
//! [`crate::store::section_bullets`] — the same parser branch notes use. So a task doc renders with
//! the machinery that already exists, and the SMART gate's "measurable" check (#998) is a call, not
//! a new parser.

use crate::model::Section;
use crate::resolver::{Config, Resolved};
use crate::todo::Task;
use std::path::{Path, PathBuf};

/// The meta key holding a task's context-doc file name.
pub const DOC_KEY: &str = "doc";

/// A task's context doc, parsed only as far as callers need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDoc {
    /// Where it was read from — the note-dir copy when one exists, else staging.
    pub path: PathBuf,
    /// The whole file, frontmatter included.
    pub text: String,
}

impl TaskDoc {
    /// The `## Done When` checklist, as `(done, text)`.
    ///
    /// Empty for an untouched template — the placeholder bullet is skipped by the shared parser —
    /// which is exactly what makes a task fail the SMART gate until someone fills it in.
    pub fn done_when(&self) -> Vec<(bool, String)> {
        crate::store::section_bullets(&self.text, Section::DoneWhen)
    }

    /// Whether the doc states at least one acceptance criterion.
    pub fn has_done_when(&self) -> bool {
        !self.done_when().is_empty()
    }

    /// The prose under a `## <heading>`, trimmed. `None` when the section is absent or blank.
    pub fn section(&self, heading: &str) -> Option<String> {
        let want = format!("## {heading}");
        let mut out = Vec::new();
        let mut inside = false;
        for (line, code) in crate::markdown::scan(&self.text) {
            if !code && line.trim_end() == want {
                inside = true;
                continue;
            }
            if inside {
                if !code && line.starts_with("## ") {
                    break;
                }
                out.push(line);
            }
        }
        let joined = out.join("\n");
        let trimmed = joined.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// The `## Why` prose — the SMART gate's escape hatch for a title that opens with a vague verb.
    pub fn why(&self) -> Option<String> {
        self.section("Why")
    }

    /// A content fingerprint for optimistic concurrency (`If-Match`) — stable for identical bytes,
    /// dependency-free. Enough to answer "did this change under me" between a read and a write.
    pub fn version(&self) -> String {
        version_of(&self.text)
    }
}

/// The staging file name for a task id (`t42` → `t42.md`).
pub fn staging_name(id: &str) -> String {
    format!("{id}.md")
}

/// `<personal_root>/tasks/<name>` — where a doc lives before its task is picked up.
pub fn staging_path(cfg: &Config, name: &str) -> PathBuf {
    cfg.tasks_dir().join(name)
}

/// `<note_dir>/task.md` — where a doc lives once its task has a worktree.
pub fn note_copy_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    cfg.note_dir(&Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    })
    .join("task.md")
}

/// The doc file name a task refers to: its `doc:` meta, else the default for its id.
fn doc_name(task: &Task) -> Option<String> {
    task.meta(DOC_KEY)
        .map(str::to_string)
        .or_else(|| task.id().map(staging_name))
}

/// Where this task's context doc actually is: **the note-dir copy if it exists, else staging.**
///
/// `None` only when the task has neither a `doc:` nor an `id:` to derive one from. The returned
/// path may not exist yet — [`ensure`] is what creates it.
pub fn doc_path(cfg: &Config, task: &Task) -> Option<PathBuf> {
    let name = doc_name(task)?;
    if let (Some(repo), Some(branch)) = (task.meta("repo"), task.meta("branch")) {
        if !repo.is_empty() && !branch.is_empty() {
            let in_note = note_copy_path(cfg, repo, branch);
            if in_note.exists() {
                return Some(in_note);
            }
        }
    }
    Some(staging_path(cfg, &name))
}

/// Read a task's context doc, if it has one on disk.
pub fn read(cfg: &Config, task: &Task) -> Option<TaskDoc> {
    let path = doc_path(cfg, task)?;
    let text = std::fs::read_to_string(&path).ok()?;
    Some(TaskDoc { path, text })
}

/// The starting document for a task: frontmatter linking it back, then the four sections the
/// pipeline fills in.
///
/// `## Done When` carries a bare placeholder rather than an example criterion — the shared parser
/// skips placeholders, so a template nobody edited correctly reads as "no criteria yet" instead of
/// smuggling a fake one past the gate.
pub fn template(task: &Task) -> String {
    let id = task.id().unwrap_or("?");
    let mut out = String::from("---\n");
    out.push_str(&format!("task: {id}\n"));
    if let Some(repo) = task.meta("repo") {
        out.push_str(&format!("repo: {repo}\n"));
    }
    if let Some(src) = task.meta("src") {
        out.push_str(&format!("src: {src}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", task.summary()));
    out.push_str("## Why\n\n\n");
    out.push_str("## Done When\n- [ ]\n\n");
    out.push_str("## Investigation\n\n\n");
    out.push_str("## Open questions\n\n");
    out
}

/// Create the task's context doc from [`template`] if it doesn't exist, and return its path.
///
/// Never overwrites: an existing doc is the task's accumulated context, and this is called on
/// paths that may run more than once.
pub fn ensure(cfg: &Config, task: &Task) -> std::io::Result<PathBuf> {
    let path = doc_path(cfg, task).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "task has no id or doc: to derive a context doc from",
        )
    })?;
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, template(task))?;
    Ok(path)
}

/// Copy a staged context doc next to its branch note — the pickup step.
///
/// A **copy**, deliberately: [`doc_path`] then prefers the note-dir version, while any reference
/// to the staging path still resolves. Returns the note-dir path when a copy happened, `None` when
/// there was nothing to copy or the task has no worktree yet. Never overwrites an existing
/// note-dir doc, which would discard work an agent already recorded there.
pub fn adopt_into_note(cfg: &Config, task: &Task) -> std::io::Result<Option<PathBuf>> {
    let (Some(repo), Some(branch)) = (task.meta("repo"), task.meta("branch")) else {
        return Ok(None);
    };
    if repo.is_empty() || branch.is_empty() {
        return Ok(None);
    }
    let target = note_copy_path(cfg, repo, branch);
    if target.exists() {
        return Ok(Some(target));
    }
    let Some(name) = doc_name(task) else {
        return Ok(None);
    };
    let staged = staging_path(cfg, &name);
    if !staged.exists() {
        return Ok(None);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&staged, &target)?;
    Ok(Some(target))
}

/// Whether `path` is a task doc living beside a branch note (as opposed to in staging).
pub fn is_note_copy(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "task.md")
}

/// A content fingerprint used as the `If-Match` version tag on doc writes. SipHash via
/// `DefaultHasher` — deterministic for the same bytes and with no extra dependency.
pub fn version_of(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// What can go wrong writing a task doc.
#[derive(Debug)]
pub enum DocError {
    /// The task has no id or `doc:` to derive a path from.
    NoPath,
    /// A full edit lost a race: the version now on disk (carried here) is not the one the caller
    /// read, so replacing would discard whatever landed in between.
    Conflict(String),
    /// The write itself failed.
    Io(std::io::Error),
}

/// Insert `addition` at the end of the `## heading` section, creating the section at the end when
/// it is absent. Additive and surgical — nothing outside the target section is touched, which is
/// what lets an agent's append interleave safely with a human's full edit.
fn append_under(text: &str, heading: &str, addition: &str) -> String {
    let want = format!("## {heading}");
    let scanned: Vec<(String, bool)> = crate::markdown::scan(text)
        .map(|(l, c)| (l.to_string(), c))
        .collect();
    let add = addition.trim_end().to_string();
    let hpos = scanned.iter().position(|(l, c)| !c && l.trim_end() == want);
    match hpos {
        Some(h) => {
            // The section runs to the next non-code `## ` heading, or the end of the file.
            let mut end = scanned.len();
            for (j, (line, code)) in scanned.iter().enumerate().skip(h + 1) {
                if !*code && line.starts_with("## ") {
                    end = j;
                    break;
                }
            }
            // Sit the addition right after the section's prose, not after its trailing blank lines.
            let mut ins = end;
            while ins > h + 1 && scanned[ins - 1].0.trim().is_empty() {
                ins -= 1;
            }
            let mut out: Vec<String> = scanned[..ins].iter().map(|(l, _)| l.clone()).collect();
            out.push(String::new());
            out.push(add);
            out.extend(scanned[ins..].iter().map(|(l, _)| l.clone()));
            let mut joined = out.join("\n");
            if text.ends_with('\n') && !joined.ends_with('\n') {
                joined.push('\n');
            }
            joined
        }
        None => {
            let mut out = text.trim_end().to_string();
            out.push_str(&format!("\n\n## {heading}\n\n{add}\n"));
            out
        }
    }
}

/// Append `text` under `## heading` (callers pass `Investigation`), creating the doc from
/// [`template`] first when it does not exist yet. Additive: never guarded, never in conflict — two
/// concurrent appends both land.
pub fn append_section(
    cfg: &Config,
    task: &Task,
    heading: &str,
    text: &str,
) -> Result<TaskDoc, DocError> {
    let path = doc_path(cfg, task).ok_or(DocError::NoPath)?;
    let _lock = crate::todo::lock(&path);
    let current = std::fs::read_to_string(&path).unwrap_or_else(|_| template(task));
    let updated = append_under(&current, heading, text);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(DocError::Io)?;
    }
    std::fs::write(&path, &updated).map_err(DocError::Io)?;
    Ok(TaskDoc {
        path,
        text: updated,
    })
}

/// Replace the whole doc, guarded by the version the caller read (`If-Match`). Returns
/// [`DocError::Conflict`] carrying the version that actually landed when the guard fails, so the
/// caller can keep the operator's text and show them what changed underneath it. A `None`
/// `if_match` is an unconditional write (first save of a doc that did not exist).
pub fn write(
    cfg: &Config,
    task: &Task,
    new_text: String,
    if_match: Option<&str>,
) -> Result<TaskDoc, DocError> {
    let path = doc_path(cfg, task).ok_or(DocError::NoPath)?;
    let _lock = crate::todo::lock(&path);
    let landed = std::fs::read_to_string(&path).ok().map(|t| version_of(&t));
    if let (Some(want), Some(have)) = (if_match, landed.as_deref()) {
        if want != have {
            return Err(DocError::Conflict(have.to_string()));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(DocError::Io)?;
    }
    std::fs::write(&path, &new_text).map_err(DocError::Io)?;
    Ok(TaskDoc {
        path,
        text: new_text,
    })
}
