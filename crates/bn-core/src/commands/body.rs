//! File / body helpers: the note's `note.md` path, its sibling investigation files, and
//! the prose body sections (`bn notes` / `bn tc` / `bn log-progress`).

use super::error::CmdError;
use super::note::ensure_note;
use crate::model::Note;
use crate::resolver::{Config, Resolved};
use crate::store;

/// The current branch note's `note.md` path, creating the note if missing so an editor
/// can always open it. Mirrors bare `bn` (which ensures the note then prints its dir).
pub fn note_path(
    cfg: &Config,
    ctx: &Resolved,
    today: &str,
) -> Result<std::path::PathBuf, CmdError> {
    ensure_note(cfg, ctx, today)?;
    Ok(cfg.note_file(ctx))
}

/// A non-note file living alongside `note.md` (investigation docs, debug logs, …).
#[derive(Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
}

/// List the investigation files in the current note dir (everything but `note.md`),
/// sorted by name. Returns the dir plus entries; an absent dir yields an empty list.
pub fn list_files(cfg: &Config, ctx: &Resolved) -> (std::path::PathBuf, Vec<FileEntry>) {
    let dir = cfg.note_dir(ctx);
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "note.md" {
                continue;
            }
            files.push(FileEntry {
                name,
                path: path.display().to_string(),
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    (dir, files)
}

/// Map a free-text body-section keyword to its markdown heading. These prose sections live in
/// the note body (not the frontmatter item lists): `bn notes`, `bn tc`, `bn log-progress`.
fn body_heading(kw: &str) -> Option<&'static str> {
    match kw {
        "notes" => Some("## Notes"),
        "tc" | "testcase" | "testcases" => Some("## Test Cases"),
        "progress" => Some("## Progress"),
        _ => None,
    }
}

/// Result of `append_body`: the section's human label (heading without `## `).
#[derive(Debug)]
pub struct AppendOutcome {
    pub label: String,
}

/// Append a `- <bullet>` line to a prose body section (`notes` → `## Notes`, `tc` →
/// `## Test Cases`, `progress` → `## Progress`), creating the note and/or the section if
/// absent. Ports the oracle's `_cmd_free_section` / `cmd_log_progress` write path. The caller
/// pre-formats the bullet text (e.g. log-progress prepends its timestamp).
pub fn append_body(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
    bullet: &str,
    today: &str,
) -> Result<AppendOutcome, CmdError> {
    let heading =
        body_heading(section_kw).ok_or_else(|| CmdError::UnknownSection(section_kw.into()))?;
    let bullet = bullet.trim();
    if bullet.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let note_md = cfg.note_file(ctx);
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    // append_body runs on fleet hot paths (merge/verify/agent) concurrently with `bn add`/`bn done`.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    note.body = crate::render::append_to_section(&note.body, heading, &format!("- {bullet}"));
    note.front.record_session();
    store::save(&note, &note_md)?;
    Ok(AppendOutcome {
        label: heading.trim_start_matches("## ").to_string(),
    })
}

/// Record an approved plan verbatim under the note's `## Plan` section (idempotent — replaces any
/// prior plan rather than stacking). Creates the note if absent. Used by `bn review approve` to
/// persist the plan the human signed off on into the branch note (#648: plans weren't landing in
/// notes). Returns [`CmdError::EmptyText`] for an empty plan.
pub fn record_plan(cfg: &Config, ctx: &Resolved, plan: &str, today: &str) -> Result<(), CmdError> {
    let plan = plan.trim();
    if plan.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let note_md = cfg.note_file(ctx);
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    note.body = crate::render::set_section(&note.body, "## Plan", &demote_headings(plan));
    note.front.record_session();
    store::save(&note, &note_md)?;
    Ok(())
}

/// Demote a plan's markdown ATX headings by two levels (`# ` → `### `, `## ` → `#### `, …) so none
/// sits at level 2 — the note's section-delimiter level. This lets the whole plan nest safely
/// inside the note's `## Plan` section without an inner heading leaking into a sibling section.
///
/// Lines inside fenced code blocks are left alone: a `# build the thing` shell comment or a plan
/// quoting markdown is content, and demoting it would silently rewrite the code the plan is
/// telling you to run.
fn demote_headings(plan: &str) -> String {
    crate::markdown::scan(plan)
        .map(|(line, code)| {
            if !code && line.starts_with('#') {
                let hashes = line.chars().take_while(|&c| c == '#').count();
                let rest = &line[hashes..];
                // Only a real ATX heading if a space (or EOL) follows the run of `#`.
                if rest.is_empty() || rest.starts_with(' ') {
                    return format!("##{line}");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// List the content lines under a prose body section, or `NoNote` if the note doesn't exist.
/// Used by bare `bn notes` / `bn tc` (no text argument).
pub fn read_body_section(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
) -> Result<Vec<String>, CmdError> {
    let heading =
        body_heading(section_kw).ok_or_else(|| CmdError::UnknownSection(section_kw.into()))?;
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    let note = store::read(&note_md)?;
    Ok(crate::render::section_content(&note.body, heading))
}


