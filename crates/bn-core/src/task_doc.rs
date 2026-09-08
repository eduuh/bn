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
//! <work_root>/tasks/<id>.md                          staging — before pickup
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

/// `<work_root>/tasks/<name>` — where a doc lives before its task is picked up.
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

/// The heading holding what the operator wrote when they recorded the task.
///
/// Deliberately **not** `## Why`: [`why`] is the SMART gate's escape hatch for a title that opens
/// with a vague verb, so routing freeform capture text there would let anything with a paste in it
/// past the gate. Context is what the operator knows; Why is the argument that it matters.
pub const CONTEXT_HEADING: &str = "Context";

/// The heading holding URLs captured with the task.
pub const LINKS_HEADING: &str = "Links";

/// The heading holding the manifest of files captured with the task.
pub const ATTACHMENTS_HEADING: &str = "Attachments";

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
    out.push_str(&format!("## {CONTEXT_HEADING}\n\n\n"));
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

/// A URL captured with a task, with the label to show for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    /// The human label. Falls back to the URL itself when empty.
    pub title: String,
}

impl Link {
    /// Render as a markdown list item, with both fields escaped so neither can break out of the
    /// link syntax and become document structure.
    fn to_bullet(&self) -> String {
        let title = escape_inline(if self.title.trim().is_empty() {
            &self.url
        } else {
            &self.title
        });
        // Parentheses in a URL would close the link target early; angle brackets are markdown's
        // own answer to that and need no other escaping.
        format!("- [{title}](<{}>)", self.url.replace(['<', '>'], ""))
    }
}

/// Only the characters that would turn a label into link/emphasis syntax.
fn escape_inline(text: &str) -> String {
    text.replace('\\', r"\\")
        .replace('[', r"\[")
        .replace(']', r"\]")
        .trim()
        .to_string()
}

/// Push captured prose down below the document's own structure.
///
/// The doc's headings are load-bearing — [`crate::store::section_bullets`] finds `## Done When`
/// anywhere in the file, so a paste containing one would hand the SMART gate acceptance criteria
/// nobody wrote. Demoting every ATX heading in captured text to `####` keeps the paste readable
/// while making it structurally inert. Fenced code is left alone: a `#` in a shell snippet is a
/// comment, not a heading.
pub fn demote_headings(text: &str) -> String {
    crate::markdown::scan(text)
        .map(|(line, code)| {
            if code || !line.trim_start().starts_with('#') {
                return line.to_string();
            }
            let trimmed = line.trim_start();
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            // `#hashtag` is not a heading — ATX requires a space after the run.
            if hashes > 6 || !trimmed[hashes..].starts_with(' ') {
                return line.to_string();
            }
            format!("#### {}", trimmed[hashes..].trim_start())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace the body of `## heading` with `body`, creating the section at the end when absent and
/// removing it entirely when `body` is empty.
///
/// The removal case is what keeps a regenerated manifest honest: a task whose last attachment was
/// deleted should have no `## Attachments` section, not an empty one implying files exist.
fn replace_under(text: &str, heading: &str, body: &str) -> String {
    let want = format!("## {heading}");
    let scanned: Vec<(String, bool)> = crate::markdown::scan(text)
        .map(|(l, c)| (l.to_string(), c))
        .collect();
    let body = body.trim_end();
    let Some(start) = scanned.iter().position(|(l, c)| !c && l.trim_end() == want) else {
        if body.is_empty() {
            return text.to_string();
        }
        let mut out = text.trim_end().to_string();
        out.push_str(&format!("\n\n## {heading}\n\n{body}\n"));
        return out;
    };
    let mut end = scanned.len();
    for (j, (line, code)) in scanned.iter().enumerate().skip(start + 1) {
        if !*code && line.starts_with("## ") {
            end = j;
            break;
        }
    }
    let mut out: Vec<String> = scanned[..start].iter().map(|(l, _)| l.clone()).collect();
    if !body.is_empty() {
        out.push(want);
        out.push(String::new());
        out.extend(body.lines().map(str::to_string));
        out.push(String::new());
    }
    out.extend(scanned[end..].iter().map(|(l, _)| l.clone()));
    let mut joined = out.join("\n");
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    if text.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Write the operator's captured prose and links into the task's doc, creating it if needed.
///
/// Called at capture time, so the context a task was recorded with is durable from the moment it
/// exists rather than from whenever someone remembers to open the doc.
pub fn capture(
    cfg: &Config,
    task: &Task,
    body: Option<&str>,
    links: &[Link],
) -> Result<TaskDoc, DocError> {
    let path = doc_path(cfg, task).ok_or(DocError::NoPath)?;
    let _lock = crate::todo::lock(&path);
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| template(task));
    if let Some(body) = body.map(str::trim).filter(|b| !b.is_empty()) {
        text = replace_under(&text, CONTEXT_HEADING, &demote_headings(body));
    }
    if !links.is_empty() {
        let rendered = links
            .iter()
            .map(Link::to_bullet)
            .collect::<Vec<_>>()
            .join("\n");
        text = replace_under(&text, LINKS_HEADING, &rendered);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(DocError::Io)?;
    }
    std::fs::write(&path, &text).map_err(DocError::Io)?;
    Ok(TaskDoc { path, text })
}

/// Regenerate the `## Attachments` manifest from what is actually on disk.
///
/// The manifest lists **names, not paths** — a path is a property of the machine that wrote it,
/// and this document syncs to every other machine and moves on pickup. The line telling the reader
/// how to resolve those names is part of the manifest, because the agent reading this doc is the
/// one that needs it.
pub fn sync_attachments(cfg: &Config, task: &Task) -> Result<TaskDoc, DocError> {
    let id = task.id().unwrap_or("?").to_string();
    let files = crate::task_attach::list(cfg, task);
    let body = if files.is_empty() {
        String::new()
    } else {
        let mut out = format!(
            "Captured with this task. Run `bn task attach {id}` for their paths on this machine.\n\n"
        );
        for file in &files {
            out.push_str(&format!(
                "- `{}` — {}, {}\n",
                file.name,
                file.mime,
                file.human_size()
            ));
        }
        out
    };
    let path = doc_path(cfg, task).ok_or(DocError::NoPath)?;
    let _lock = crate::todo::lock(&path);
    let current = std::fs::read_to_string(&path).unwrap_or_else(|_| template(task));
    let updated = replace_under(&current, ATTACHMENTS_HEADING, &body);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(DocError::Io)?;
    }
    std::fs::write(&path, &updated).map_err(DocError::Io)?;
    Ok(TaskDoc {
        path,
        text: updated,
    })
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
    let Some(name) = doc_name(task) else {
        return Ok(None);
    };
    let staged = staging_path(cfg, &name);
    if target.exists() {
        // The doc is already here, but its files may not be — an earlier pickup predating
        // attachments, or a doc an agent created before anything was attached to the staged one.
        if staged.exists() {
            crate::task_attach::adopt_beside(&staged, &target)?;
        }
        return Ok(Some(target));
    }
    if !staged.exists() {
        return Ok(None);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&staged, &target)?;
    // The doc's manifest names files relative to wherever the doc is, so the files have to make
    // the same move — otherwise a picked-up task documents attachments nothing can resolve.
    crate::task_attach::adopt_beside(&staged, &target)?;
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

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPath => write!(f, "task has no id or doc: to derive a context doc from"),
            Self::Conflict(landed) => {
                write!(f, "the doc changed under this write (now {landed})")
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> Task {
        crate::todo::Task::new_todo(id, "fix the reconnect storm", None, "", "web", "2026-08-09")
    }

    fn cfg(root: &std::path::Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn a_pasted_done_when_cannot_become_the_gates_criteria() {
        let pasted = "The reporter wrote:\n\n## Done When\n- [ ] ship it\n\nEnd of paste.";
        let demoted = demote_headings(pasted);
        // The section parser matches a whole line, so what matters is that no *line* is the
        // heading any more — `#### Done When` still contains it as a substring.
        assert!(!demoted.lines().any(|l| l.trim_end() == "## Done When"));
        assert!(demoted.contains("#### Done When"));

        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        let task = task("t1");
        capture(&cfg, &task, Some(pasted), &[]).expect("capture");
        let doc = read(&cfg, &task).expect("doc");
        assert!(
            doc.done_when().is_empty(),
            "a paste must not smuggle acceptance criteria past the SMART gate"
        );
    }

    #[test]
    fn demotion_leaves_code_fences_and_hashtags_alone() {
        let text = "```sh\n# not a heading\n```\n#hashtag\n### real heading";
        let out = demote_headings(text);
        assert!(out.contains("# not a heading"), "{out}");
        assert!(out.contains("#hashtag"), "{out}");
        assert!(out.contains("#### real heading"), "{out}");
    }

    #[test]
    fn capture_writes_context_and_links_without_touching_why() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        let task = task("t2");
        let links = vec![
            Link {
                url: "https://github.com/o/r/issues/7".into(),
                title: "the issue".into(),
            },
            Link {
                url: "https://example.com/a(b)".into(),
                title: String::new(),
            },
        ];
        capture(&cfg, &task, Some("Repros on flaky LAN only."), &links).expect("capture");

        let doc = read(&cfg, &task).expect("doc");
        assert_eq!(
            doc.section(CONTEXT_HEADING).as_deref(),
            Some("Repros on flaky LAN only.")
        );
        assert!(doc.why().is_none(), "Why stays the gate's own escape hatch");
        let rendered = doc.section(LINKS_HEADING).expect("links");
        assert!(rendered.contains("[the issue](<https://github.com/o/r/issues/7>)"));
        // A URL with parentheses stays inside its angle-bracket target.
        assert!(
            rendered.contains("(<https://example.com/a(b)>)"),
            "{rendered}"
        );
    }

    #[test]
    fn a_link_label_cannot_break_out_of_its_own_syntax() {
        let bullet = Link {
            url: "https://example.com".into(),
            title: "](javascript:alert(1)) [pwned".into(),
        }
        .to_bullet();
        // Both brackets are escaped, so the only `]` markdown can close the label on is the one
        // this function wrote — the target stays `<https://example.com>` and the injected
        // `(javascript:…)` is inert label text.
        assert_eq!(
            bullet,
            r"- [\](javascript:alert(1)) \[pwned](<https://example.com>)"
        );
        assert!(bullet.ends_with("](<https://example.com>)"));
    }

    #[test]
    fn capture_is_re_runnable_and_replaces_rather_than_appends() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        let task = task("t3");
        capture(&cfg, &task, Some("first"), &[]).expect("capture");
        capture(&cfg, &task, Some("second"), &[]).expect("recapture");
        let doc = read(&cfg, &task).expect("doc");
        assert_eq!(doc.section(CONTEXT_HEADING).as_deref(), Some("second"));
        assert_eq!(doc.text.matches("## Context").count(), 1);
    }

    #[test]
    fn the_manifest_tracks_disk_and_disappears_when_the_last_file_goes() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        let task = task("t4");
        ensure(&cfg, &task).expect("ensure");

        crate::task_attach::save(&cfg, &task, "shot.png", &[0u8; 2048]).expect("save");
        let doc = sync_attachments(&cfg, &task).expect("sync");
        assert!(
            doc.text.contains("- `shot.png` — image/png, 2 KB"),
            "{}",
            doc.text
        );
        // Names, never paths: the doc syncs to machines where this path does not exist.
        assert!(!doc.text.contains(root.path().to_str().expect("utf8")));
        assert!(doc.text.contains("bn task attach t4"));

        crate::task_attach::remove(&cfg, &task, "shot.png").expect("remove");
        let doc = sync_attachments(&cfg, &task).expect("resync");
        assert!(
            !doc.text.contains("## Attachments"),
            "an empty manifest implies files that are not there"
        );
    }

    #[test]
    fn pickup_moves_the_files_with_the_doc() {
        let root = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(root.path());
        let mut task = task("t5");
        ensure(&cfg, &task).expect("ensure");
        crate::task_attach::save(&cfg, &task, "trace.log", b"boom").expect("save");

        task.set_meta_raw("repo", "atlas");
        task.set_meta_raw("branch", "fix-sse");
        let landed = adopt_into_note(&cfg, &task).expect("adopt").expect("path");

        assert!(landed.ends_with("task.md"));
        let files = crate::task_attach::list(&cfg, &task);
        assert_eq!(files.len(), 1, "attachments must resolve after pickup");
        assert_eq!(files[0].name, "trace.log");
        assert!(files[0].path.starts_with(landed.parent().expect("parent")));
    }
}
