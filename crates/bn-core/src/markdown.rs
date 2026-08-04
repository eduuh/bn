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
//! [`FenceScanner::in_code`] before treating a line as structure. Fence syntax follows CommonMark:
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

    /// Whether the scanner is currently inside a fenced code block, i.e. whether the *next*
    /// line to be observed is code. Prefer the return value of [`observe`](Self::observe),
    /// which also accounts for the fence delimiter lines themselves.
    pub fn in_code(&self) -> bool {
        self.open.is_some()
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


