//! Output rendering: the stable JSON envelope (D2) and text helpers.
//!
//! Every command emits one envelope: `{ ok, command, data, error }`. The CLI
//! renders text for humans or this envelope for `--json`; the engine itself
//! only builds the value.

use crate::model::{Note, Section};
use serde::Serialize;
use serde_json::Value;

/// The stable machine envelope shared by every command (architecture.md D2).
#[derive(Debug, Serialize)]
pub struct Envelope {
    pub ok: bool,
    pub command: String,
    pub data: Value,
    pub error: Option<EnvelopeError>,
}

#[derive(Debug, Serialize)]
pub struct EnvelopeError {
    pub code: String,
    pub message: String,
}

impl Envelope {
    pub fn success(command: impl Into<String>, data: Value) -> Self {
        Envelope {
            ok: true,
            command: command.into(),
            data,
            error: None,
        }
    }

    pub fn failure(
        command: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Envelope {
            ok: false,
            command: command.into(),
            data: Value::Null,
            error: Some(EnvelopeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"ok\":false}".to_string())
    }
}

/// Lean context snapshot for `bn brief` (SessionStart): goal + open blockers + top-N open
/// todos + recent progress + recent decisions. Mirrors the oracle's cmd_brief shape.
pub fn brief(note: &Note, repo: &str, branch: &str) -> String {
    let mut out = String::new();
    let goal = first_nonblank_in_section(&note.body, "## Goal");
    match goal {
        Some(g) => out.push_str(&format!("# {repo}/{branch} — {g}\n")),
        None => out.push_str(&format!("# {repo}/{branch}\n")),
    }

    let blockers = &note.front.blockers;
    if !blockers.is_empty() {
        out.push_str("\nBlockers:\n");
        for b in blockers {
            out.push_str(&format!("- [!] {}\n", b.text));
        }
    }

    let open: Vec<_> = note
        .front
        .todos
        .iter()
        .filter(|t| t.is_open_todo())
        .take(10)
        .collect();
    if !open.is_empty() {
        out.push_str("\nOpen todos:\n");
        for t in open {
            match t.pct {
                Some(p) => out.push_str(&format!("- [{}] {} ({p}%)\n", t.id, t.text)),
                None => out.push_str(&format!("- [{}] {}\n", t.id, t.text)),
            }
        }
    }

    let progress = bullets_in_section(&note.body, "## Progress");
    let recent: Vec<_> = progress.iter().rev().take(3).rev().collect();
    if !recent.is_empty() {
        out.push_str("\nRecent progress:\n");
        for p in recent {
            out.push_str(&format!("- {p}\n"));
        }
    }

    let decisions = &note.front.decisions;
    if !decisions.is_empty() {
        out.push_str("\nRecent decisions:\n");
        for d in decisions.iter().rev().take(3).rev() {
            out.push_str(&format!("- {}\n", d.text));
        }
    }
    out
}

/// First non-blank line in a `## Heading` body section (e.g. the goal line).
/// The note's Goal — the first non-blank line under `## Goal`, if any.
pub fn goal_text(body: &str) -> Option<String> {
    first_nonblank_in_section(body, "## Goal")
}

/// Return `body` with the `## Goal` section's content replaced by `text` (a single line,
/// the inverse of `goal_text`). Preserves the rest of the body verbatim. If there's no
/// `## Goal` heading, appends one. Keeps the template shape: heading, blank, goal line.
pub fn set_goal_in_body(body: &str, text: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut replaced = false;
    while i < lines.len() {
        if lines[i] == "## Goal" {
            out.push("## Goal".to_string());
            out.push(String::new());
            out.push(text.to_string());
            // Drop the old section content up to (not including) the next heading.
            i += 1;
            while i < lines.len() && !lines[i].starts_with("## ") {
                i += 1;
            }
            out.push(String::new()); // blank line before whatever follows
            replaced = true;
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    if !replaced {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.push("## Goal".to_string());
        out.push(String::new());
        out.push(text.to_string());
    }
    format!("{}\n", out.join("\n").trim_end_matches('\n'))
}

/// Append `line` to the end of the `## Heading` body section (just before the next heading,
/// or at EOF), creating the section at the end of the body if it's absent. Used by `bn notes`,
/// `bn tc`, and `bn log-progress` to grow the prose sections. Preserves everything else.
pub fn append_to_section(body: &str, heading: &str, line: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut inserted = false;
    for &l in &lines {
        // Leaving our section (next heading): drop trailing blanks, append, then a blank.
        if in_section && l.starts_with("## ") {
            while out.last().is_some_and(|s| s.is_empty()) {
                out.pop();
            }
            out.push(line.to_string());
            out.push(String::new());
            in_section = false;
            inserted = true;
        }
        if l == heading {
            in_section = true;
        }
        out.push(l.to_string());
    }
    // Section ran to EOF without a following heading.
    if in_section && !inserted {
        while out.last().is_some_and(|s| s.is_empty()) {
            out.pop();
        }
        out.push(line.to_string());
        inserted = true;
    }
    // No such heading anywhere: create it at the end.
    if !inserted {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.push(heading.to_string());
        out.push(String::new());
        out.push(line.to_string());
    }
    format!("{}\n", out.join("\n").trim_end_matches('\n'))
}

/// Replace the entire content of a `## Heading` body section with `content` verbatim, creating the
/// section at the end if absent. Unlike [`append_to_section`], this is idempotent — re-running with
/// the same content leaves exactly one copy (used by `bn review approve` to record the approved
/// plan without stacking duplicates on re-approve). `content` keeps its own line structure; leading
/// / trailing blank lines are trimmed and the section is fenced from neighbours by a blank line.
///
/// Callers must keep `content` free of level-2 (`## `) headings — those are the note's own section
/// delimiters, so an embedded one would leak into a sibling section (see `commands::record_plan`,
/// which demotes a plan's headings first).
pub fn set_section(body: &str, heading: &str, content: &str) -> String {
    let content = content.trim_matches('\n');
    let mut out: Vec<String> = Vec::new();
    let mut skipping = false;
    let mut replaced = false;
    for line in body.lines() {
        // While swallowing the old section body, drop everything until the next heading.
        if skipping {
            if line.starts_with("## ") {
                skipping = false;
            } else {
                continue;
            }
        }
        if line == heading && !replaced {
            out.push(line.to_string());
            out.push(String::new());
            for c in content.lines() {
                out.push(c.to_string());
            }
            out.push(String::new());
            skipping = true;
            replaced = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.push(heading.to_string());
        out.push(String::new());
        for c in content.lines() {
            out.push(c.to_string());
        }
    }
    format!("{}\n", out.join("\n").trim_end_matches('\n'))
}

/// The content lines under a `## Heading` body section (excluding the heading), with leading
/// and trailing blank lines trimmed. Empty when the heading is absent. Used to list `bn notes`
/// / `bn tc`.
pub fn section_content(body: &str, heading: &str) -> Vec<String> {
    let mut in_section = false;
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            in_section = line == heading;
            continue;
        }
        if in_section {
            out.push(line.to_string());
        }
    }
    while out.first().is_some_and(|s| s.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|s| s.trim().is_empty()) {
        out.pop();
    }
    out
}

fn first_nonblank_in_section(body: &str, heading: &str) -> Option<String> {
    let mut in_section = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            in_section = line == heading;
            continue;
        }
        if in_section && !line.trim().is_empty() {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// All `- ` bullet texts under a `## Heading` body section.
fn bullets_in_section(body: &str, heading: &str) -> Vec<String> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            in_section = line == heading;
            continue;
        }
        if in_section {
            if let Some(rest) = line.trim_start().strip_prefix("- ") {
                if !rest.trim().is_empty() {
                    out.push(rest.trim().to_string());
                }
            }
        }
    }
    out
}

/// Human render for `bn --cat`: the prose body followed by the action sections
/// (Blockers → open Todos → Decisions → To Research → Collaboration → To Ask),
/// mirroring the oracle's `_yaml_render_sections` order and `-`/`- [ ]` placeholders.
/// Items are sourced from the typed model, so all three on-disk layouts render alike.
pub fn cat(note: &Note) -> String {
    // Order matches _yaml_render_sections (bn:814-830), not Section::all(). Done-when leads
    // (acceptance criteria first — "what does finished look like"), and only shows when set.
    const ORDER: [Section; 7] = [
        Section::DoneWhen,
        Section::Blocker,
        Section::Todo,
        Section::Decision,
        Section::Research,
        Section::Collab,
        Section::Ask,
    ];

    let mut out = String::new();
    let body = note.body.trim_end();
    if !body.is_empty() {
        out.push_str(body);
        out.push('\n');
    }

    for section in ORDER {
        // Acceptance criteria are opt-in: skip the section entirely when none are set, rather
        // than printing an empty placeholder on every note.
        if section == Section::DoneWhen && note.front.done_when.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(section.heading());
        out.push('\n');
        if section == Section::DoneWhen {
            // Show every criterion with its state, so met/unmet progress is visible at a glance.
            for item in &note.front.done_when {
                out.push_str(if item.done == Some(true) {
                    "- [x] "
                } else {
                    "- [ ] "
                });
                out.push_str(&item.text);
                out.push('\n');
            }
        } else if section.is_todo() {
            let open: Vec<_> = note
                .front
                .todos
                .iter()
                .filter(|i| i.is_open_todo())
                .collect();
            if open.is_empty() {
                out.push_str("- [ ]\n");
            } else {
                for item in open {
                    out.push_str("- [ ] ");
                    out.push_str(&item.text);
                    if let Some(p) = item.pct {
                        out.push_str(&format!(" ({p}%)"));
                    }
                    out.push('\n');
                }
            }
        } else {
            let items = note.front.items(section);
            if items.is_empty() {
                out.push_str("-\n");
            } else {
                for item in items {
                    out.push_str("- ");
                    out.push_str(&item.text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// The "Related branches" footer appended to `bn --cat`: past branches that touched the same
/// files, ranked by overlap. Empty string when there are none, so the note detail stays clean
/// for branches with no prior art. Each line names the neighbour, the shared-file count, and the
/// recall that makes it worth jumping to (same work-item area, decisions recorded there).
pub fn related_footer(related: &[crate::related::RelatedBranch]) -> String {
    if related.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Related branches (touched the same files)\n");
    for r in related {
        let area = if r.work_item_match {
            " · same area"
        } else {
            ""
        };
        let dec = match r.decisions {
            0 => String::new(),
            1 => " · 1 decision".to_string(),
            n => format!(" · {n} decisions"),
        };
        let archived = if r.status == "closed" {
            " (archived)"
        } else {
            ""
        };
        out.push_str(&format!(
            "- {}/{}{} — {} shared{}{}\n",
            r.repo, r.branch, archived, r.shared_count, area, dec
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_goal_seeds_empty_template_section() {
        let body = "# repo / branch\n\n## Goal\n\n\n## Progress\n";
        let out = set_goal_in_body(body, "Ship CSV export");
        assert_eq!(
            out,
            "# repo / branch\n\n## Goal\n\nShip CSV export\n\n## Progress\n"
        );
        assert_eq!(goal_text(&out).as_deref(), Some("Ship CSV export"));
    }

    #[test]
    fn set_goal_replaces_existing_goal_and_keeps_progress() {
        let body = "# r / b\n\n## Goal\n\nold goal\n\n## Progress\n- did a thing\n";
        let out = set_goal_in_body(body, "new goal");
        assert_eq!(
            out,
            "# r / b\n\n## Goal\n\nnew goal\n\n## Progress\n- did a thing\n"
        );
    }

    #[test]
    fn set_goal_appends_when_no_goal_heading() {
        let body = "# r / b\n\n## Progress\n";
        let out = set_goal_in_body(body, "the goal");
        assert_eq!(goal_text(&out).as_deref(), Some("the goal"));
        assert!(out.contains("## Progress"));
    }

    #[test]
    fn append_to_section_inserts_into_middle_section_keeping_following() {
        let body = "# r / b\n\n## Progress\n- old\n\n## Notes\n- keep\n";
        let out = append_to_section(body, "## Progress", "- new");
        // appended after existing Progress content, before ## Notes; Notes untouched
        assert_eq!(
            out,
            "# r / b\n\n## Progress\n- old\n- new\n\n## Notes\n- keep\n"
        );
        assert_eq!(section_content(&out, "## Progress"), vec!["- old", "- new"]);
        assert_eq!(section_content(&out, "## Notes"), vec!["- keep"]);
    }

    #[test]
    fn append_to_section_creates_missing_section_at_end() {
        let body = "# r / b\n\n## Goal\n\ng\n";
        let out = append_to_section(body, "## Test Cases", "- a case");
        assert!(out.ends_with("## Test Cases\n\n- a case\n"));
        assert_eq!(section_content(&out, "## Test Cases"), vec!["- a case"]);
        // absent section lists empty
        assert!(section_content(body, "## Test Cases").is_empty());
    }

    #[test]
    fn set_section_creates_replaces_and_is_idempotent() {
        let body = "# r / b\n\n## Goal\n\ng\n";
        // Creates a missing section verbatim (multi-line content kept intact). Callers keep the
        // content free of level-2 (`## `) headings — those are the note's own section delimiters.
        let out = set_section(body, "## Plan", "### Approach\nDo X.\n### Files\nsrc/a.rs");
        assert_eq!(
            section_content(&out, "## Plan"),
            vec!["### Approach", "Do X.", "### Files", "src/a.rs"]
        );
        // Prior sections are preserved.
        assert_eq!(section_content(&out, "## Goal"), vec!["g"]);
        // Replacing swaps the content and does not stack duplicates.
        let out2 = set_section(&out, "## Plan", "new plan");
        assert_eq!(section_content(&out2, "## Plan"), vec!["new plan"]);
        assert_eq!(out2.matches("## Plan").count(), 1);
        // Idempotent: same content in → same document out.
        assert_eq!(set_section(&out2, "## Plan", "new plan"), out2);
    }

    #[test]
    fn set_section_replaces_a_middle_section_keeping_neighbours() {
        let body = "# r / b\n\n## Goal\n\ng\n\n## Plan\n\nold plan\n\n## Progress\n\n- p\n";
        let out = set_section(body, "## Plan", "fresh");
        assert_eq!(section_content(&out, "## Plan"), vec!["fresh"]);
        assert_eq!(section_content(&out, "## Goal"), vec!["g"]);
        assert_eq!(section_content(&out, "## Progress"), vec!["- p"]);
    }

    #[test]
    fn related_footer_empty_is_blank() {
        assert_eq!(related_footer(&[]), "");
    }

    #[test]
    fn related_footer_lists_neighbours_with_recall() {
        use crate::related::RelatedBranch;
        let related = vec![
            RelatedBranch {
                repo: "bn".into(),
                branch: "wsl-paths".into(),
                status: "closed".into(),
                shared_files: vec!["resolver.rs".into(), "runner.rs".into()],
                shared_count: 2,
                work_item_match: true,
                decisions: 2,
                path: None,
            },
            RelatedBranch {
                repo: "bn".into(),
                branch: "live".into(),
                status: "active".into(),
                shared_files: vec!["resolver.rs".into()],
                shared_count: 1,
                work_item_match: false,
                decisions: 0,
                path: None,
            },
        ];
        let out = related_footer(&related);
        assert!(out.starts_with("\n## Related branches (touched the same files)\n"));
        assert!(out.contains("- bn/wsl-paths (archived) — 2 shared · same area · 2 decisions\n"));
        // No work-item / decisions / archived noise on the live, no-decision neighbour.
        assert!(out.contains("- bn/live — 1 shared\n"));
    }
}
