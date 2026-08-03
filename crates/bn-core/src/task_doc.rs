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
        for line in self.text.lines() {
            if line.trim_end() == want {
                inside = true;
                continue;
            }
            if inside {
                if line.starts_with("## ") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::TodoFile;

    fn cfg_at(root: &Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            work_root: root.join("work"),
            hostname: "testhost".into(),
            ..Default::default()
        }
    }

    fn task(line: &str) -> Task {
        TodoFile::parse(line).tasks().next().unwrap().clone()
    }

    #[test]
    fn a_task_with_no_worktree_gets_a_staging_doc() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn\n");

        let path = ensure(&cfg, &t).unwrap();
        assert_eq!(path, dir.path().join("tasks/t42.md"));
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# fix the thing"));
    }

    #[test]
    fn an_untouched_template_states_no_criteria() {
        // The gate's "measurable" check must not be satisfiable by the template alone.
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn\n");
        ensure(&cfg, &t).unwrap();

        let doc = read(&cfg, &t).unwrap();
        assert!(doc.done_when().is_empty(), "placeholder must not count");
        assert!(!doc.has_done_when());
        assert_eq!(doc.why(), None, "an empty Why section reads as absent");
    }

    #[test]
    fn done_when_parses_through_the_shared_note_parser() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn\n");
        let path = ensure(&cfg, &t).unwrap();
        std::fs::write(
            &path,
            "---\ntask: t42\n---\n\n# fix the thing\n\n## Why\nIt breaks on flaky LAN.\n\n\
             ## Done When\n- [ ] reconnect backs off\n- [x] a test covers it\n\n## Investigation\n",
        )
        .unwrap();

        let doc = read(&cfg, &t).unwrap();
        let items = doc.done_when();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], (false, "reconnect backs off".to_string()));
        assert_eq!(items[1], (true, "a test covers it".to_string()));
        assert!(doc.has_done_when());
        assert_eq!(doc.why().as_deref(), Some("It breaks on flaky LAN."));

        // The identical body parsed as a branch note yields the same criteria — one parser.
        let via_note = crate::store::section_bullets(&doc.text, Section::DoneWhen);
        assert_eq!(via_note, items);
    }

    #[test]
    fn pickup_copies_the_doc_beside_the_note_and_resolution_follows() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn branch:fix-sse\n");
        let staged = ensure(&cfg, &t).unwrap();
        std::fs::write(
            &staged,
            "---\ntask: t42\n---\n\n## Done When\n- [ ] it works\n",
        )
        .unwrap();

        let adopted = adopt_into_note(&cfg, &t).unwrap().unwrap();
        assert_eq!(adopted, note_copy_path(&cfg, "bn", "fix-sse"));
        assert!(adopted.exists());

        // A copy, not a move: the old path still resolves to something real.
        assert!(
            staged.exists(),
            "staging copy survives so no reference dangles"
        );
        // And resolution now prefers the note-dir copy.
        assert_eq!(doc_path(&cfg, &t).unwrap(), adopted);
        assert_eq!(read(&cfg, &t).unwrap().path, adopted);
    }

    #[test]
    fn adoption_never_overwrites_work_already_recorded_beside_the_note() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn branch:fix-sse\n");
        let staged = ensure(&cfg, &t).unwrap();
        std::fs::write(&staged, "staged version\n").unwrap();

        let target = note_copy_path(&cfg, "bn", "fix-sse");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "an investigator already wrote here\n").unwrap();

        adopt_into_note(&cfg, &t).unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "an investigator already wrote here\n"
        );
    }

    #[test]
    fn a_task_with_no_branch_has_nothing_to_adopt() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn\n");
        ensure(&cfg, &t).unwrap();
        assert_eq!(adopt_into_note(&cfg, &t).unwrap(), None);
    }

    #[test]
    fn ensure_never_clobbers_accumulated_context() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn\n");
        let path = ensure(&cfg, &t).unwrap();
        std::fs::write(&path, "hard-won investigation notes\n").unwrap();

        ensure(&cfg, &t).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hard-won investigation notes\n"
        );
    }

    #[test]
    fn an_explicit_doc_meta_wins_over_the_id_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("fix the thing id:t42 repo:bn doc:custom-name.md\n");
        assert_eq!(
            doc_path(&cfg, &t).unwrap(),
            dir.path().join("tasks/custom-name.md")
        );
    }

    #[test]
    fn a_task_without_an_id_has_no_doc() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(dir.path());
        let t = task("a hand written line with no id\n");
        assert_eq!(doc_path(&cfg, &t), None);
        assert!(ensure(&cfg, &t).is_err());
    }

    #[test]
    fn the_template_carries_provenance_when_the_task_has_it() {
        let t = task("cache issues id:t7 repo:atlas src:gh#963\n");
        let text = template(&t);
        assert!(text.contains("task: t7"));
        assert!(text.contains("repo: atlas"));
        assert!(text.contains("src: gh#963"));
        assert!(text.contains("# cache issues"));
        for heading in [
            "## Why",
            "## Done When",
            "## Investigation",
            "## Open questions",
        ] {
            assert!(text.contains(heading), "missing {heading}");
        }
    }
}
