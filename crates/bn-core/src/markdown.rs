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

#[cfg(test)]
mod tests {
    use super::*;

    fn code_lines(body: &str) -> Vec<&str> {
        scan(body).filter(|(_, c)| *c).map(|(l, _)| l).collect()
    }

    #[test]
    fn plain_prose_is_never_code() {
        let body = "## Goal\n\nship it\n\n## Notes\n- a note\n";
        assert!(scan(body).all(|(_, code)| !code));
    }

    #[test]
    fn backtick_fence_covers_its_contents_and_delimiters() {
        let body = "before\n```\n## Todos\n```\nafter";
        assert_eq!(code_lines(body), vec!["```", "## Todos", "```"]);
    }

    #[test]
    fn info_string_opens_a_fence() {
        let body = "```rust\nfn main() {}\n```\n## Real";
        assert_eq!(code_lines(body), vec!["```rust", "fn main() {}", "```"]);
    }

    #[test]
    fn tilde_fences_work_and_dont_close_backtick_fences() {
        assert_eq!(
            code_lines("~~~\n## in\n~~~\nout"),
            vec!["~~~", "## in", "~~~"]
        );
        // A tilde line inside a backtick fence is just content, so the block stays open.
        let body = "```\n~~~\n## still in\n```\nout";
        assert_eq!(code_lines(body), vec!["```", "~~~", "## still in", "```"]);
    }

    #[test]
    fn a_longer_closing_fence_closes_but_a_shorter_one_does_not() {
        assert_eq!(
            code_lines("````\ncode\n`````\nout"),
            vec!["````", "code", "`````"]
        );
        // Three backticks can't close a four-backtick fence, so everything stays code.
        assert_eq!(
            code_lines("````\ncode\n```\nstill"),
            vec!["````", "code", "```", "still"]
        );
    }

    #[test]
    fn a_closing_fence_may_not_carry_an_info_string() {
        // The nested ```rust is content, not a close; the block runs to the bare fence.
        let body = "````\n```rust\nx\n```\n````\nout";
        assert_eq!(
            code_lines(body),
            vec!["````", "```rust", "x", "```", "````"]
        );
    }

    #[test]
    fn up_to_three_spaces_of_indent_still_fences_but_four_does_not() {
        assert_eq!(
            code_lines("   ```\n## in\n   ```"),
            vec!["   ```", "## in", "   ```"]
        );
        assert!(scan("    ```\n## out").all(|(_, code)| !code));
    }

    #[test]
    fn an_unclosed_fence_swallows_the_rest_of_the_body() {
        let body = "## Plan\n\n```sh\nmake\n## not a heading";
        assert_eq!(code_lines(body), vec!["```sh", "make", "## not a heading"]);
    }

    #[test]
    fn backtick_info_strings_may_not_contain_backticks() {
        // ```` `x` ```` style inline spans must not be mistaken for an opening fence.
        assert!(scan("``` not `a` fence").all(|(_, code)| !code));
    }
}
