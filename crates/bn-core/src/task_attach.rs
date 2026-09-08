//! Files captured with a task — screenshots, logs, specs (#1006, EPIC #993).
//!
//! A task's [context doc](crate::task_doc) holds everything that can be *written*. This holds
//! everything that cannot: the screenshot of the broken layout, the failing CI log, the PDF spec
//! someone sent. They are the difference between "fix the SSE reconnect storm" and a task an agent
//! can actually act on, so they are captured at the same moment the task is.
//!
//! # Beside the doc, always
//!
//! ```text
//! <work_root>/tasks/t42.md                            the doc, in staging
//! <work_root>/tasks/t42.files/screenshot.png          its attachments
//!
//! <note_dir>/task.md                                  the doc, after pickup
//! <note_dir>/task.files/screenshot.png                its attachments
//! ```
//!
//! The directory is derived from the doc's own file name ([`dir_for_doc`]), so the pair moves
//! together and [`adopt_into_note`](crate::task_doc::adopt_into_note) copying one implies copying
//! the other. Nothing outside this module needs to know which of the two locations is live.
//!
//! # Agents resolve paths by asking, never by reading one out of the doc
//!
//! The doc records attachment *names* in its `## Attachments` manifest, not paths. A path is a
//! property of the machine that wrote it — `<work_root>` differs per host, and the doc itself
//! moves on pickup — so a path baked into a synced document is a dangling reference waiting to
//! happen. `bn task attach <id>` resolves the current location on the machine that asks, which is
//! the same discipline the rest of bn uses: point at the record, let the reader resolve it.
//!
//! # Untrusted by construction
//!
//! A file name arrives from a browser upload or a third party's issue, so [`sanitize_name`] is not
//! a nicety: it is what keeps `../../.ssh/authorized_keys` from being a valid attachment name.

use crate::resolver::Config;
use crate::todo::Task;
use std::path::{Path, PathBuf};

/// The largest single attachment bn will store, in bytes.
///
/// Generous enough for a screenshot, a stack trace or a slide, and far short of the video someone
/// will eventually try to drop on it. The notes tree is a git repo that syncs to every machine —
/// what lands here is paid for on every clone, forever.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// The largest a single task's attachment directory may grow to. Ten files at the per-file cap is
/// already more than any task needs; past this, the answer is a link to where the data really
/// lives, not another copy inside the notes repo.
pub const MAX_TOTAL_BYTES: u64 = 40 * 1024 * 1024;

/// The suffix appended to a doc's stem to name its attachment directory.
const DIR_SUFFIX: &str = ".files";

/// One stored attachment, as [`list`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The sanitized file name — the only identifier that survives a move between machines.
    pub name: String,
    /// Where it is *on this machine, right now*. Never persist this into a synced document.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// A guessed content type, for rendering. `application/octet-stream` when unknown.
    pub mime: &'static str,
}

impl Attachment {
    /// Whether a viewer can be expected to render this inline.
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    /// A human size for the doc manifest: whole KB/MB, since a byte count is noise at this scale.
    pub fn human_size(&self) -> String {
        match self.size {
            0..=1023 => format!("{} B", self.size),
            1024..=1_048_575 => format!("{} KB", self.size / 1024),
            _ => format!("{:.1} MB", self.size as f64 / 1_048_576.0),
        }
    }
}

/// Content type by extension — a fixed table rather than a dependency, because the set of things
/// worth *rendering* is small and everything else is a download either way.
pub fn mime_for(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "diff" | "patch" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The attachment directory belonging to a doc at `doc`: its path with `.files` appended to the
/// stem (`t42.md` → `t42.files`, `task.md` → `task.files`).
///
/// Derived rather than configured, so a doc and its files can only ever be found together.
pub fn dir_for_doc(doc: &Path) -> PathBuf {
    let stem = doc
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("task")
        .to_string();
    doc.with_file_name(format!("{stem}{DIR_SUFFIX}"))
}

/// Where this task's attachments live *now* — beside the note-dir doc once it has been picked up,
/// else beside the staged one. `None` only when the task has no doc to hang them off.
pub fn dir(cfg: &Config, task: &Task) -> Option<PathBuf> {
    crate::task_doc::doc_path(cfg, task)
        .as_deref()
        .map(dir_for_doc)
}

/// Reject anything that is not a plain file name.
///
/// Everything that could make a name mean a *location* is removed: directory separators (both
/// kinds — a Windows-authored zip travels), `..`, leading dots, control characters and NULs. What
/// survives is `[A-Za-z0-9._-]`, truncated to a sane length, with a fallback for a name that
/// sanitizes down to nothing (an all-emoji file name is not a security problem, but an empty path
/// is a correctness one).
pub fn sanitize_name(raw: &str) -> String {
    // Take only the last segment first: a browser on some platforms still sends a full path, and
    // the tail is the part the user actually named.
    let tail = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_start_matches('.');
    let mut out: String = tail
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '-',
        })
        .collect();
    // Collapse the runs the replacement above creates, so `my file (1).png` is not `my-file--1-.png`.
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(['-', '.']).to_string();
    if out.is_empty() {
        return "attachment".to_string();
    }
    out.chars().take(120).collect()
}

/// A name not already taken in `dir`: `shot.png`, then `shot-2.png`, `shot-3.png`, …
///
/// Two screenshots pasted in a row are both called `image.png`, and silently overwriting the first
/// would lose context the operator believes they captured.
fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|e| e.to_str());
    for n in 2..1000 {
        let candidate = match ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    // A thousand collisions on one name is not a case worth a better answer than "overwrite".
    name.to_string()
}

/// What can go wrong storing an attachment.
#[derive(Debug)]
pub enum AttachError {
    /// The task has no id or `doc:` to hang an attachment directory off.
    NoPath,
    /// This file alone is over [`MAX_BYTES`].
    TooLarge { size: u64, limit: u64 },
    /// This file would push the task's directory over [`MAX_TOTAL_BYTES`].
    QuotaExceeded { used: u64, limit: u64 },
    /// The write itself failed.
    Io(std::io::Error),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPath => write!(f, "task has no context doc to attach files to"),
            Self::TooLarge { size, limit } => {
                write!(f, "attachment is {size} bytes, over the {limit}-byte limit")
            }
            Self::QuotaExceeded { used, limit } => write!(
                f,
                "task attachments would reach {used} bytes, over the {limit}-byte limit"
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// The attachments a task currently has, name-sorted so two calls agree and a rendered manifest is
/// stable. Empty for a task with no attachment directory — the common case, and not an error.
pub fn list(cfg: &Config, task: &Task) -> Vec<Attachment> {
    let Some(dir) = dir(cfg, task) else {
        return Vec::new();
    };
    list_dir(&dir)
}

/// [`list`] against an already-resolved directory.
pub fn list_dir(dir: &Path) -> Vec<Attachment> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Attachment> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            // Only files: a nested directory here is not something bn put there.
            if !meta.is_file() {
                return None;
            }
            let name = entry.file_name().to_str()?.to_string();
            Some(Attachment {
                mime: mime_for(&name),
                path: entry.path(),
                size: meta.len(),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Total bytes already stored for this task.
fn used_bytes(dir: &Path) -> u64 {
    list_dir(dir).iter().map(|a| a.size).sum()
}

/// Store `bytes` as an attachment of `task`, under a sanitized, non-colliding name.
///
/// Returns the stored attachment, whose `name` may differ from the one asked for — callers that
/// record the name (a doc manifest, an API response) must use the returned one.
pub fn save(
    cfg: &Config,
    task: &Task,
    raw_name: &str,
    bytes: &[u8],
) -> Result<Attachment, AttachError> {
    let size = bytes.len() as u64;
    if size > MAX_BYTES {
        return Err(AttachError::TooLarge {
            size,
            limit: MAX_BYTES,
        });
    }
    let dir = dir(cfg, task).ok_or(AttachError::NoPath)?;
    std::fs::create_dir_all(&dir).map_err(AttachError::Io)?;
    let used = used_bytes(&dir) + size;
    if used > MAX_TOTAL_BYTES {
        return Err(AttachError::QuotaExceeded {
            used,
            limit: MAX_TOTAL_BYTES,
        });
    }
    let name = unique_name(&dir, &sanitize_name(raw_name));
    let path = dir.join(&name);
    std::fs::write(&path, bytes).map_err(AttachError::Io)?;
    Ok(Attachment {
        mime: mime_for(&name),
        name,
        path,
        size,
    })
}

/// Resolve one attachment by name, for reading or serving it.
///
/// The name is sanitized before it is joined, so a traversal attempt resolves to a file that does
/// not exist rather than to a file outside the directory.
pub fn find(cfg: &Config, task: &Task, raw_name: &str) -> Option<Attachment> {
    let dir = dir(cfg, task)?;
    let name = sanitize_name(raw_name);
    let path = dir.join(&name);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(Attachment {
        mime: mime_for(&name),
        name,
        path,
        size: meta.len(),
    })
}

/// Delete one attachment. `Ok(false)` when there was nothing by that name — removing an absent
/// file is the state the caller wanted, not a failure.
pub fn remove(cfg: &Config, task: &Task, raw_name: &str) -> Result<bool, AttachError> {
    let Some(found) = find(cfg, task, raw_name) else {
        return Ok(false);
    };
    std::fs::remove_file(&found.path).map_err(AttachError::Io)?;
    Ok(true)
}

/// Copy a staged attachment directory in beside the branch note, as the doc's pickup does.
///
/// Called from [`crate::task_doc::adopt_into_note`] so the pair never separates. Individual files
/// are never overwritten: a file already at the destination is one an agent may have put there.
pub fn adopt_beside(staged_doc: &Path, note_doc: &Path) -> std::io::Result<usize> {
    let from = dir_for_doc(staged_doc);
    let to = dir_for_doc(note_doc);
    if !from.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(&to)?;
    let mut copied = 0;
    for attachment in list_dir(&from) {
        let target = to.join(&attachment.name);
        if target.exists() {
            continue;
        }
        std::fs::copy(&attachment.path, &target)?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_dir_is_derived_from_the_docs_own_name() {
        assert_eq!(
            dir_for_doc(Path::new("/notes/tasks/t42.md")),
            PathBuf::from("/notes/tasks/t42.files")
        );
        assert_eq!(
            dir_for_doc(Path::new("/notes/mac/atlas/main/task.md")),
            PathBuf::from("/notes/mac/atlas/main/task.files")
        );
    }

    #[test]
    fn sanitize_strips_everything_that_could_mean_a_location() {
        assert_eq!(
            sanitize_name("../../.ssh/authorized_keys"),
            "authorized_keys"
        );
        assert_eq!(sanitize_name(r"C:\Users\me\shot.png"), "shot.png");
        assert_eq!(
            sanitize_name("..%2f..%2fetc%2fpasswd"),
            "2f..-2fetc-2fpasswd"
        );
        assert_eq!(sanitize_name(".hidden"), "hidden");
        assert_eq!(sanitize_name("my file (1).png"), "my-file-1-.png");
        assert_eq!(sanitize_name("   "), "attachment");
        assert_eq!(sanitize_name("😀😀"), "attachment");
    }

    #[test]
    fn sanitized_name_never_escapes_the_attachment_dir() {
        let base = Path::new("/notes/tasks/t42.files");
        for hostile in [
            "../secret",
            "../../../../etc/passwd",
            r"..\..\windows\system32",
            "/etc/shadow",
        ] {
            let joined = base.join(sanitize_name(hostile));
            assert_eq!(
                joined.parent(),
                Some(base),
                "{hostile} escaped to {}",
                joined.display()
            );
        }
    }

    #[test]
    fn two_pasted_screenshots_both_survive() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("image.png"), b"first").expect("write");
        assert_eq!(unique_name(dir.path(), "image.png"), "image-2.png");
        std::fs::write(dir.path().join("image-2.png"), b"second").expect("write");
        assert_eq!(unique_name(dir.path(), "image.png"), "image-3.png");
        assert_eq!(unique_name(dir.path(), "fresh.png"), "fresh.png");
    }

    #[test]
    fn extensionless_collisions_still_get_a_suffix() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("dump"), b"x").expect("write");
        assert_eq!(unique_name(dir.path(), "dump"), "dump-2");
    }

    #[test]
    fn mime_is_guessed_for_the_things_worth_rendering() {
        assert_eq!(mime_for("shot.PNG"), "image/png");
        assert_eq!(mime_for("trace.log"), "text/plain");
        assert_eq!(mime_for("spec.pdf"), "application/pdf");
        assert_eq!(mime_for("archive.tar.zst"), "application/octet-stream");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }

    #[test]
    fn human_size_rounds_to_something_readable() {
        let at = |size| Attachment {
            name: "x".into(),
            path: PathBuf::new(),
            size,
            mime: "text/plain",
        };
        assert_eq!(at(512).human_size(), "512 B");
        assert_eq!(at(2048).human_size(), "2 KB");
        assert_eq!(at(3_145_728).human_size(), "3.0 MB");
    }

    #[test]
    fn adopt_copies_staged_files_without_clobbering_what_landed_first() {
        let root = tempfile::tempdir().expect("tempdir");
        let staged_doc = root.path().join("tasks/t42.md");
        let note_doc = root.path().join("note/task.md");
        std::fs::create_dir_all(dir_for_doc(&staged_doc)).expect("mkdir");
        std::fs::create_dir_all(dir_for_doc(&note_doc)).expect("mkdir");
        std::fs::write(dir_for_doc(&staged_doc).join("shot.png"), b"staged").expect("write");
        std::fs::write(dir_for_doc(&staged_doc).join("log.txt"), b"staged").expect("write");
        // An agent already wrote its own copy of one of them beside the note.
        std::fs::write(dir_for_doc(&note_doc).join("log.txt"), b"agent").expect("write");

        assert_eq!(adopt_beside(&staged_doc, &note_doc).expect("adopt"), 1);
        let landed = dir_for_doc(&note_doc);
        assert_eq!(
            std::fs::read(landed.join("shot.png")).expect("read"),
            b"staged"
        );
        assert_eq!(
            std::fs::read(landed.join("log.txt")).expect("read"),
            b"agent",
            "adopt must not overwrite what the agent recorded"
        );
    }

    #[test]
    fn adopt_is_a_no_op_when_nothing_was_attached() {
        let root = tempfile::tempdir().expect("tempdir");
        let staged_doc = root.path().join("tasks/t9.md");
        let note_doc = root.path().join("note/task.md");
        assert_eq!(adopt_beside(&staged_doc, &note_doc).expect("adopt"), 0);
        assert!(!dir_for_doc(&note_doc).exists());
    }
}
