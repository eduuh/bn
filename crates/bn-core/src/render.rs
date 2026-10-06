//! Output rendering: the stable JSON envelope (D2) and text helpers.
//!
//! Every command emits one envelope: `{ ok, command, data, error }`. The CLI
//! renders text for humans or this envelope for `--json`; the engine itself
//! only builds the value.

use crate::model::{Note, Section};
use serde::Serialize;
use serde_json::Value;

/// The envelope contract's version, emitted as `v` on every envelope.
///
/// A consumer that checks it gets a loud failure instead of a silent one: before this existed, a
/// script parsing `data` had no way to tell "this field moved" from "this field is absent today",
/// and bn had no way to say so. Bump it only when a change would break a reader that was correct
/// under the previous number — adding a key to some command's `data` is not that.
pub const ENVELOPE_VERSION: u32 = 1;

/// The stable machine envelope shared by every command (architecture.md D2).
#[derive(Debug, Serialize)]
pub struct Envelope {
    /// The contract version ([`ENVELOPE_VERSION`]), first so it is readable without parsing the
    /// rest — `head -c 16` is enough to reject an envelope a script does not understand.
    pub v: u32,
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
            v: ENVELOPE_VERSION,
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
            v: ENVELOPE_VERSION,
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
        serde_json::to_string(self)
            .unwrap_or_else(|_| format!("{{\"v\":{ENVELOPE_VERSION},\"ok\":false}}"))
    }
}

/// Flatten `text` to a single line of at most `max` characters, marking with `…` when anything was
/// dropped.
///
/// Branch-note items are free text, and some of them are whole agent prompts — `bn summary` was
/// printing a 30-line dispatch brief inline, so one row buried every other row on the screen. A
/// list view shows the first line; the full text is what `bn cat` is for.
///
/// Cuts on `char_indices`, never a byte offset, so a multi-byte title is not split mid-codepoint.
pub fn oneline(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim_end();
    let has_more_lines = text.lines().nth(1).is_some();
    match first.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", first[..cut].trim_end()),
        None if has_more_lines => format!("{first} …"),
        None => first.to_string(),
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

/// Whether a scanned `(line, is_code)` pair is a *structural* section heading — a `## ` line that
/// isn't sitting inside a fenced code block. Section readers and writers below all go through this
/// rather than testing `starts_with("## ")` directly, so a plan or note that quotes markdown in a
/// fence doesn't split into phantom sections (see [`crate::markdown`]).
fn is_heading((line, code): (&str, bool)) -> bool {
    !code && crate::markdown::is_section_heading(line)
}

/// First non-blank line in a `## Heading` body section (e.g. the goal line).
/// The note's Goal — the first non-blank line under `## Goal`, if any.
pub fn goal_text(body: &str) -> Option<String> {
    first_nonblank_in_section(body, "## Goal")
}

/// Flatten `text` into the one line a goal actually is.
///
/// [`goal_text`] reads the goal back as *the first non-blank line* of the section, so a single line
/// is the whole data model — anything past the first newline was never readable as the goal. Two
/// things follow, and the second is why this function exists rather than being a caller's problem.
///
/// A goal carrying `\n## Goal` inside it made [`set_goal_in_body`] **double the body on every
/// call**. The replace drops old content only up to the next heading, so an embedded `## Goal`
/// terminated the delete early and survived; the scan then reached that survivor, matched it, and
/// inserted the whole text again. Each existing occurrence became two. On a real branch note an
/// agent's seed prompt — which has its own `## Goal` and `## Rules` headings — went through this
/// about twenty-one times and reached **4.2 GB** (2^21 copies), which took the dashboard's rollups
/// out of memory and got the server OOM-killed repeatedly.
///
/// Collapsing whitespace keeps every word while making a heading impossible: a heading has to
/// *start a line*, and after this there is only one line. The leading-`#` escape covers the
/// degenerate case where the goal itself begins with `## `.
fn one_line_goal(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.starts_with('#') {
        // `\#` renders as `#` and cannot be a heading.
        format!("\\{flat}")
    } else {
        flat
    }
}

/// Return `body` with the `## Goal` section's content replaced by `text`. Preserves the rest of the
/// body verbatim. If there's no `## Goal` heading, appends one. Keeps the template shape: heading,
/// blank, goal line.
///
/// `text` is flattened to one line first — see [`one_line_goal`] for the data model that requires
/// it and the runaway it prevents.
pub fn set_goal_in_body(body: &str, text: &str) -> String {
    let text = &one_line_goal(text);
    let lines: Vec<(&str, bool)> = crate::markdown::scan(body).collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut replaced = false;
    while i < lines.len() {
        if !lines[i].1 && lines[i].0 == "## Goal" {
            out.push("## Goal".to_string());
            out.push(String::new());
            out.push(text.to_string());
            // Drop the old section content up to (not including) the next heading.
            i += 1;
            while i < lines.len() && !is_heading(lines[i]) {
                i += 1;
            }
            out.push(String::new()); // blank line before whatever follows
            replaced = true;
            continue;
        }
        out.push(lines[i].0.to_string());
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
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut inserted = false;
    for entry in crate::markdown::scan(body) {
        let (l, code) = entry;
        // Leaving our section (next heading): drop trailing blanks, append, then a blank.
        if in_section && is_heading(entry) {
            while out.last().is_some_and(|s| s.is_empty()) {
                out.pop();
            }
            out.push(line.to_string());
            out.push(String::new());
            in_section = false;
            inserted = true;
        }
        if !code && l == heading {
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
/// Callers must keep `content`'s *prose* free of level-2 (`## `) headings — those are the note's own
/// section delimiters, so an embedded one would leak into a sibling section (see
/// `commands::record_plan`, which demotes a plan's headings first). A `## ` inside a fenced code
/// block is safe: every reader here is fence-aware, so quoted markdown stays quoted.
pub fn set_section(body: &str, heading: &str, content: &str) -> String {
    let content = content.trim_matches('\n');
    let mut out: Vec<String> = Vec::new();
    let mut skipping = false;
    let mut replaced = false;
    for entry in crate::markdown::scan(body) {
        let (line, code) = entry;
        // While swallowing the old section body, drop everything until the next heading.
        if skipping {
            if is_heading(entry) {
                skipping = false;
            } else {
                continue;
            }
        }
        if !code && line == heading && !replaced {
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
    for entry in crate::markdown::scan(body) {
        if is_heading(entry) {
            in_section = entry.0 == heading;
            continue;
        }
        if in_section {
            out.push(entry.0.to_string());
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
    for entry in crate::markdown::scan(body) {
        if is_heading(entry) {
            in_section = entry.0 == heading;
            continue;
        }
        if in_section && !entry.0.trim().is_empty() {
            return Some(entry.0.trim().to_string());
        }
    }
    None
}

/// All `- ` bullet texts under a `## Heading` body section.
fn bullets_in_section(body: &str, heading: &str) -> Vec<String> {
    let mut in_section = false;
    let mut out = Vec::new();
    for entry in crate::markdown::scan(body) {
        if is_heading(entry) {
            in_section = entry.0 == heading;
            continue;
        }
        // A `- ` line inside a fence is sample text, not a bullet of this section.
        if in_section && !entry.1 {
            if let Some(rest) = entry.0.trim_start().strip_prefix("- ") {
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

/// The `## Links` footer of `bn cat`: this repo's saved links, most-used first, so a fresh
/// session sees the PR, board or dashboard it would otherwise go looking for. Empty when the
/// repo has none.
pub fn links_footer(links: &[crate::links::Link]) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Links (`bn links` to open)\n");
    for l in links {
        let tags = if l.tags.is_empty() {
            String::new()
        } else {
            format!(" · {}", l.tags.join(" "))
        };
        let new = if l.new { " · •new" } else { "" };
        out.push_str(&format!("- [{}]({}){tags}{new}\n", l.name, l.url));
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
mod goal_tests {
    use super::*;

    const TEMPLATE: &str = "# repo / branch\n\n## Goal\n\n\n## Progress\n";

    /// Headings are *lines*, so count lines. A flattened goal may legitimately contain the text
    /// `## Goal` mid-line — that is precisely what makes it harmless, and a substring count would
    /// report it as a second section.
    fn goal_headings(body: &str) -> usize {
        body.lines().filter(|l| l.trim_end() == "## Goal").count()
    }

    /// The regression, stated as the property that was violated: setting a goal is **idempotent in
    /// size**. Set the same goal twenty times and the note is the same length it was after one.
    ///
    /// The seed text here is the shape that caused the outage — an agent prompt carrying its own
    /// `## Goal` and `## Rules` headings. Before the fix each call doubled the body: twenty-one
    /// passes reached 4.2 GB and OOM-killed every reader on the machine.
    #[test]
    fn setting_a_goal_repeatedly_does_not_grow_the_note() {
        let seed =
            "Set up the repo.\n\n## Goal\n\nCreate the surface.\n\n## Rules\n\n- be idempotent.\n";

        let once = set_goal_in_body(TEMPLATE, seed);
        let mut body = once.clone();
        for _ in 0..20 {
            body = set_goal_in_body(&body, seed);
        }

        assert_eq!(
            body, once,
            "setting the same goal again must not change the note"
        );
        assert_eq!(
            goal_headings(&body),
            1,
            "the note must keep exactly one Goal heading"
        );
    }

    /// And the goal survives the flattening: every word is still there, readable back out.
    #[test]
    fn a_multiline_goal_keeps_its_words_on_one_line() {
        let body = set_goal_in_body(TEMPLATE, "ship the\n\n## Rules\n\nlogin fix");
        assert_eq!(
            goal_text(&body).as_deref(),
            Some("ship the ## Rules login fix")
        );
    }

    /// A goal that itself begins with `## ` cannot be allowed to become a heading — that is the
    /// same wound in its smallest form.
    #[test]
    fn a_goal_that_starts_like_a_heading_is_escaped() {
        let body = set_goal_in_body(TEMPLATE, "## not a section");
        assert_eq!(
            goal_headings(&body),
            1,
            "only the real Goal heading may be a heading"
        );
        let re_set = set_goal_in_body(&body, "## not a section");
        assert_eq!(re_set, body, "and it stays stable across repeats");
    }

    /// The ordinary case is untouched: a plain one-line goal round-trips exactly.
    #[test]
    fn a_plain_goal_round_trips() {
        let body = set_goal_in_body(TEMPLATE, "make the build green");
        assert_eq!(goal_text(&body).as_deref(), Some("make the build green"));
        assert!(
            body.contains("## Progress"),
            "the rest of the body survives"
        );
    }
}

#[cfg(test)]
mod oneline_tests {
    use super::oneline;

    #[test]
    fn keeps_a_short_single_line_verbatim() {
        assert_eq!(oneline("fix the cron", 40), "fix the cron");
    }

    #[test]
    fn marks_a_dropped_body() {
        // The `bn summary` case: a whole dispatch prompt stored as one todo.
        assert_eq!(
            // Deliberately not phrased as a role prompt. check-prompt-sources.sh greps the
            // second-person role opener across all of bn-core/src, and its exemptions cover
            // `tests.rs` and `tests/` but not an inline `#[cfg(test)] mod tests` like this one.
            oneline("Repo setup agent brief.\n\n## Goal\n\nDo the thing.", 60),
            "Repo setup agent brief. …"
        );
    }

    #[test]
    fn truncates_past_the_cap() {
        assert_eq!(oneline("abcdefghij", 4), "abcd…");
    }

    #[test]
    fn never_splits_a_codepoint() {
        // Every prefix must stay valid UTF-8 — a byte-offset cut would panic here.
        let text = "héllo wörld — ✓ däne";
        for max in 1..text.chars().count() + 2 {
            let cut = oneline(text, max);
            assert!(cut.chars().count() <= max + 2, "{max}: {cut:?}");
        }
    }

    #[test]
    fn handles_empty_and_blank_input() {
        assert_eq!(oneline("", 10), "");
        assert_eq!(oneline("\n\nbody", 10), " …");
    }
}

#[cfg(test)]
mod links_footer_tests {
    use super::links_footer;
    use crate::links::Link;

    #[test]
    fn no_links_adds_nothing_and_links_render_as_markdown() {
        assert_eq!(links_footer(&[]), "");
        let l = Link {
            id: "k1".into(),
            url: "https://example.com/board".into(),
            name: "Team board".into(),
            tags: vec!["standup".into()],
            repo: Some("bn".into()),
            branch: None,
            created: "2026-10-01".into(),
            uses: 0,
            last: None,
            new: false,
            expires: None,
        };
        let out = links_footer(&[l]);
        assert!(out.contains("## Links"));
        assert!(out.contains("- [Team board](https://example.com/board) · standup"));
    }
}
