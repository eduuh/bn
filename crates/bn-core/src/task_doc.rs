//! A task's context of record — the long form a one-line task can't hold (#997, EPIC #993).
//!
//! A task line is one line. Everything that makes it *SMART* — why it matters, the done-when
//! checklist, what an investigator found, what is still unknown — needs somewhere durable to live.
//! Task context lives independently of its current branch binding:
//!
//! ```text
//! <tasks_dir>/context/<doc-name>.md
//! ```
//!
//! Reads support historical task documents; [`ensure`] imports their content and attachments
//! before a mutation or branch rebinding, without changing historical files. The `doc:` meta
//! stores a file name, not a branch-specific path. Markdown sections use the shared renderer.

use crate::resolver::{Config, Resolved};
use crate::todo::Task;
use std::path::PathBuf;

/// The meta key holding a task's context-doc file name.
pub const DOC_KEY: &str = "doc";

/// Append task context without creating or reading any session-history artifact.
pub fn append_context(
    cfg: &Config,
    ctx: &Resolved,
    heading: &str,
    content: &str,
) -> std::io::Result<()> {
    let task = crate::pipeline::gate::governing_task(cfg, &ctx.repo, &ctx.branch)
        .ok_or_else(|| std::io::Error::other("no governing task; capture or claim a task first"))?;
    let path = ensure(cfg, &task)?;
    let _lock = crate::todo::lock_required(&path)?;
    let text = std::fs::read_to_string(&path)?;
    let heading = format!("## {heading}");
    if crate::render::section_content(&text, &heading)
        .iter()
        .any(|line| line.trim() == content.trim())
    {
        return Ok(());
    }
    crate::fs::write_atomic(
        &path,
        crate::render::append_to_section(&text, &heading, content).as_bytes(),
    )
}

/// A task's context doc, parsed only as far as callers need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDoc {
    /// Canonical context path, or a historical task-document path awaiting import.
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
        crate::markdown::section_bullets(&self.text, "## Done When")
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

/// `<artifact_dir>/task.md` — where a doc lives once its task has a worktree.
fn legacy_doc_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    cfg.artifact_dir(&Resolved {
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

/// Resolve canonical context first, then historical branch documents and staging.
///
/// `None` only when the task has neither a `doc:` nor an `id:` to derive one from. The returned
/// path may not exist yet — [`ensure`] is what creates it.
pub fn doc_path(cfg: &Config, task: &Task) -> Option<PathBuf> {
    let name = doc_name(task)?;
    let stable = cfg.tasks_dir().join("context").join(&name);
    if stable.is_file() {
        return Some(stable);
    }
    if let (Some(repo), Some(branch)) = (task.meta("repo"), task.meta("branch")) {
        if !repo.is_empty() && !branch.is_empty() {
            let in_note = legacy_doc_path(cfg, repo, branch);
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
    let source = doc_path(cfg, task).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "task has no id or doc: to derive a context doc from",
        )
    })?;
    let name = doc_name(task).ok_or_else(|| std::io::Error::other("task has no document name"))?;
    let path = cfg.tasks_dir().join("context").join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = crate::todo::lock_required(&path)?;
    if path.is_file() {
        return Ok(path);
    }
    if source.is_file() {
        crate::task_attach::adopt_beside(&source, &path)?;
        crate::fs::write_atomic(&path, &std::fs::read(&source)?)?;
    } else {
        crate::fs::write_atomic(&path, template(task).as_bytes())?;
    }
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
/// The doc's headings are load-bearing — [`crate::render::section_bullets`] finds `## Done When`
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
    let path = ensure(cfg, task).map_err(DocError::Io)?;
    let _lock = crate::todo::lock_required(&path).map_err(DocError::Io)?;
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
    let path = ensure(cfg, task).map_err(DocError::Io)?;
    let _lock = crate::todo::lock_required(&path).map_err(DocError::Io)?;
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
    let path = ensure(cfg, task).map_err(DocError::Io)?;
    let _lock = crate::todo::lock_required(&path).map_err(DocError::Io)?;
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
    let path = ensure(cfg, task).map_err(DocError::Io)?;
    let _lock = crate::todo::lock_required(&path).map_err(DocError::Io)?;
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

    #[test]
    fn review_regression_document_mutations_share_the_rmw_lock() {
        crate::test_support::with_state_dir_path(|root| {
            let cfg = crate::test_support::config(root);
            let ctx = Resolved {
                repo: "repo".into(),
                branch: "work".into(),
            };
            let task = crate::task_store::capture(
                &cfg,
                "Preserve concurrent edits",
                Some(ctx.rb()),
                "test",
            )
            .unwrap();
            let path = ensure(&cfg, &task).unwrap();
            let lock = crate::todo::lock(&path);
            let original = std::fs::read_to_string(&path).unwrap();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                let result = append_context(&cfg, &ctx, "Progress", "Concurrent progress");
                done_tx.send(result).unwrap();
            });
            started_rx.recv().unwrap();
            let early = done_rx.recv_timeout(std::time::Duration::from_millis(250));
            std::fs::write(
                &path,
                format!("{original}\n## Context\n\nConcurrent capture\n"),
            )
            .unwrap();
            drop(lock);
            let completed_early = early.is_ok();
            early.unwrap_or_else(|_| done_rx.recv().unwrap()).unwrap();
            worker.join().unwrap();
            let landed = std::fs::read_to_string(path).unwrap();
            assert!(
                !completed_early,
                "append bypassed the document writer's lock"
            );
            assert!(
                landed.contains("Concurrent capture") && landed.contains("Concurrent progress")
            );
        });
    }

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
        let landed = ensure(&cfg, &task).expect("stable path");

        assert!(landed.ends_with("context/t5.md"));
        let files = crate::task_attach::list(&cfg, &task);
        assert_eq!(files.len(), 1, "attachments must resolve after pickup");
        assert_eq!(files[0].name, "trace.log");
        assert!(files[0].path.starts_with(landed.parent().expect("parent")));
    }

    #[test]
    fn legacy_task_context_and_attachments_survive_branch_rebinding() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        let mut task = task("t6");
        task.set_meta_raw("repo", "atlas");
        task.set_meta_raw("branch", "old");
        let legacy = legacy_doc_path(&cfg, "atlas", "old");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "## Investigation\n\nLatest findings\n").unwrap();
        crate::task_attach::save(&cfg, &task, "trace.log", b"evidence").unwrap();
        let store = crate::task_store::task_store(&cfg);
        store.append(&task).unwrap();
        assert!(crate::task_store::claim(&cfg, "t6", "atlas", "new").unwrap());
        let file = store.load().unwrap();
        let task = file.tasks().find(|task| task.id() == Some("t6")).unwrap();
        let stable = cfg.tasks_dir().join("context/t6.md");
        assert_eq!(doc_path(&cfg, task), Some(stable));
        assert_eq!(
            read(&cfg, task)
                .unwrap()
                .section("Investigation")
                .as_deref(),
            Some("Latest findings")
        );
        assert_eq!(crate::task_attach::list(&cfg, task)[0].name, "trace.log");
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "## Investigation\n\nLatest findings\n"
        );
    }
}
