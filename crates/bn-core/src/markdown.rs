//! Fence-aware markdown scanning — the one thing every note-section reader and writer needs.
//!
//! Notes are markdown, and their `## Heading` lines are structural: they delimit the Goal, the
//! Plan, the item lists. Every scanner in this crate used to test `line.starts_with("## ")`
//! directly, which is right until a section *contains a code block* — a plan quoting a note
//! template, a debug log pasting a shell session, a skill showing its own markdown. Then the
//! `## Todos` inside the fence reads as a real heading and the section splits in the wrong place:
//! `bn notes` appends a bullet into the middle of a code block, `set_section` swallows the rest of
//! the plan, `section_content` truncates.
//!
//! [`FenceScanner`] is the shared fix. Drive it one line at a time over a body and ask
//! `FenceScanner::in_code` before treating a line as structure. Fence syntax follows CommonMark:
//! an opening fence is three-or-more backticks or tildes indented at most three spaces, and it
//! closes on a fence of the same character that is at least as long and carries no info string.

/// Tracks whether the current line sits inside a fenced code block.
///
/// Feed it every line of a body in order via [`observe`](Self::observe); it returns whether that
/// line is code (fence delimiters count as code, so they're never read as structure either).
#[derive(Debug, Default, Clone)]
pub struct FenceScanner {
    /// The open fence's delimiter char and length, or `None` outside a fence.
    open: Option<(char, usize)>,
}

impl FenceScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance over one line, returning whether that line is part of a code block (including the
    /// opening and closing fence lines). A line for which this returns `true` must never be
    /// treated as a heading or a bullet.
    pub fn observe(&mut self, line: &str) -> bool {
        match self.open {
            None => match fence_open(line) {
                Some((ch, len)) => {
                    self.open = Some((ch, len));
                    true
                }
                None => false,
            },
            Some((ch, len)) => {
                if closes(line, ch, len) {
                    self.open = None;
                }
                true
            }
        }
    }
}

/// The delimiter char and run length of an opening fence, or `None` if `line` doesn't open one.
/// Up to three leading spaces are allowed (four would make it an indented code block); a backtick
/// fence's info string may not itself contain a backtick.
fn fence_open(line: &str) -> Option<(char, usize)> {
    let rest = strip_fence_indent(line)?;
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = &rest[len..];
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len))
}

/// Whether `line` closes an open fence of `ch` repeated `open_len` times: the same character, at
/// least as long, and nothing but whitespace after the run.
fn closes(line: &str, ch: char, open_len: usize) -> bool {
    let Some(rest) = strip_fence_indent(line) else {
        return false;
    };
    let len = rest.chars().take_while(|c| *c == ch).count();
    len >= open_len && rest[len..].trim().is_empty()
}

/// Drop a fence line's leading indent, rejecting the four-plus spaces that would instead make it
/// an indented code block.
fn strip_fence_indent(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    (indent <= 3).then(|| line.trim_start_matches(' '))
}

/// Whether `line` is a structural heading — an ATX heading at exactly level 2 (`## `), the level
/// the note format uses to delimit sections. Callers must already have excluded fenced lines.
pub fn is_section_heading(line: &str) -> bool {
    line.starts_with("## ")
}

/// Split a body into `(line, is_code)` pairs, the shape most callers want.
pub fn scan(body: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut fences = FenceScanner::new();
    body.lines().map(move |line| {
        let code = fences.observe(line);
        (line, code)
    })
}

/// Split `---\n<yaml>\n---\n<body>` into (frontmatter-block-without-fences, body-after-fence).
/// Returns `None` if there is no leading `---` fence pair. The frontmatter block keeps the
/// trailing newline of its last field (serde-safe); the body has its leading blank lines
/// trimmed. This is the single source of truth for fence parsing — callers across the crate
/// (notes, daily, skills, migration) route through it rather than hand-slicing `---`.
///
/// Both LF (`\n`) and CRLF (`\r\n`) line endings are accepted: a note.md saved on Windows opens
/// with `---\r\n`, so matching only the LF fence would reject it as missing frontmatter and drop
/// the entire structured block. The returned slices keep whatever endings the source used
/// (serde_yaml parses CRLF fine); only the body's leading blank lines are trimmed.
pub(crate) fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    // The closing fence must stand alone on its line: "\n---" followed by a newline (LF or CRLF)
    // or EOF. A naive `find("\n---")` would also match inside a multi-line YAML value or a "----"
    // rule, truncating the frontmatter and corrupting the parse.
    let mut from = 0;
    let end = loop {
        let idx = rest[from..].find("\n---")? + from;
        let after = &rest[idx + 4..];
        if after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n") {
            break idx;
        }
        from = idx + 1;
    };
    // Keep the last field's trailing newline (serde-safe); then skip the "\n---" fence and drop
    // leading blank lines. CRLF-safe: trims both \r and \n.
    let front = &rest[..end + 1];
    let body = rest[end + 4..].trim_start_matches(['\n', '\r']);
    Some((front, body))
}

/// Split the frontmatter fence off `text` and deserialize the YAML block into `T` — the common
/// "read a fenced doc's frontmatter" shape shared by the daily layer and its queues. Errors
/// uniformly: a missing fence or a YAML parse failure both surface as [`crate::fs::StoreError::Parse`].
pub(crate) fn parse_frontmatter<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<T, crate::fs::StoreError> {
    let (block, _) = split_frontmatter(text)
        .ok_or_else(|| crate::fs::StoreError::Parse("missing frontmatter fence".into()))?;
    serde_yaml::from_str(block).map_err(|e| crate::fs::StoreError::Parse(e.to_string()))
}

/// Collect bullet lines under a heading until the next `## ` / `---` / EOF.
/// Returns (done, text) per bullet; `done` is meaningful only for todo checkboxes.
/// Skips the template placeholder lines (`-`, `- [ ]`, `- [ ] test`).
pub fn section_bullets(body: &str, heading: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut in_section = false;
    for (line, code) in crate::markdown::scan(body) {
        if !in_section {
            if !code && line == heading {
                in_section = true;
            }
            continue;
        }
        if !code && (line.starts_with("## ") || line == "---") {
            break;
        }
        // Bullets quoted inside a fence are sample text, not items of this section.
        if code {
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed == "-" || trimmed == "- [ ]" || trimmed == "- [ ] test" {
            continue; // placeholder seed
        }
        if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            out.push((false, rest.to_string()));
        } else if let Some(rest) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
        {
            // Markdown task lists accept both `x` and `X` as the checked marker.
            out.push((true, rest.to_string()));
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            out.push((false, rest.to_string()));
        }
    }
    out
}
