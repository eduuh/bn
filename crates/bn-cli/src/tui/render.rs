//! The renderer: one pure function from a view to the exact bytes of a frame.
//!
//! Pure so every layout question — does a row fit at 40 columns, does the preview pane collapse,
//! does the cursor highlight land on exactly one line — is a unit test rather than something you
//! discover by squinting at a popup.
//!
//! Frames are painted with `ESC[H` (home) plus a per-line `ESC[K` (clear to end of line) and a
//! closing `ESC[J`, not with `ESC[2J`. Clearing the whole screen first is what makes a full-screen
//! redraw flicker, and a picker redraws on every keystroke. Lines are joined with `\r\n`: raw mode
//! turns off `ONLCR`, so a bare `\n` drops a line without returning the carriage.

use super::ansi::{display_width, fit_cols, truncate_cols};

/// The narrowest terminal that still gets a preview pane. Below this the list is the only thing
/// worth showing, and squeezing two columns into 40 makes both unreadable.
const MIN_COLS_FOR_PREVIEW: u16 = 50;

/// One list row, already decorated by the caller.
pub(crate) struct Line<'a> {
    pub(crate) display: &'a str,
    pub(crate) marked: bool,
    pub(crate) focused: bool,
}

/// Everything the renderer needs. Borrowed, so building one allocates nothing.
pub(crate) struct View<'a> {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) header: &'a str,
    pub(crate) query: &'a str,
    pub(crate) matched: usize,
    pub(crate) total: usize,
    /// Exactly the rows that should be visible — the caller has already applied the scroll offset.
    pub(crate) list: &'a [Line<'a>],
    pub(crate) preview: Option<&'a str>,
    /// Percentage of the width given to the preview pane; 0 disables it.
    pub(crate) preview_pct: u16,
    pub(crate) notice: Option<&'a str>,
    pub(crate) hints: &'a str,
}

impl View<'_> {
    /// Width of the preview pane, or 0 when there isn't one.
    pub(crate) fn preview_cols(&self) -> usize {
        if self.preview.is_none() || self.preview_pct == 0 || self.cols < MIN_COLS_FOR_PREVIEW {
            return 0;
        }
        let pct = self.preview_pct.min(80) as usize;
        (self.cols as usize * pct / 100).max(10)
    }

    /// Width of the list pane, always at least 1.
    pub(crate) fn list_cols(&self) -> usize {
        let preview = self.preview_cols();
        if preview == 0 {
            return self.cols as usize;
        }
        // One column for the divider.
        (self.cols as usize).saturating_sub(preview + 1).max(1)
    }

    /// How many rows the list pane can show: everything but header, query line and footer.
    pub(crate) fn list_rows(&self) -> usize {
        (self.rows as usize).saturating_sub(3).max(1)
    }
}

/// Render the whole screen.
pub(crate) fn frame(v: &View<'_>) -> String {
    let cols = v.cols as usize;
    let list_cols = v.list_cols();
    let preview_cols = v.preview_cols();
    let preview_lines: Vec<&str> = v.preview.map(|p| p.lines().collect()).unwrap_or_default();

    let mut lines: Vec<String> = Vec::with_capacity(v.rows as usize);
    lines.push(truncate_cols(&format!("\x1b[1m{}\x1b[0m", v.header), cols));
    lines.push(query_line(v, cols));

    for row in 0..v.list_rows() {
        let left = match v.list.get(row) {
            Some(line) => list_row(line, list_cols),
            None => " ".repeat(list_cols),
        };
        if preview_cols == 0 {
            lines.push(truncate_cols(&left, cols));
            continue;
        }
        let right = preview_lines.get(row).copied().unwrap_or("");
        lines.push(format!(
            "{left}\u{2502}{}",
            truncate_cols(right, preview_cols)
        ));
    }

    lines.push(truncate_cols(
        &match v.notice {
            Some(n) => format!("\x1b[33m{n}\x1b[0m"),
            None => format!("\x1b[2m{}\x1b[0m", v.hints),
        },
        cols,
    ));

    let mut out = String::from("\x1b[H");
    for (i, line) in lines.iter().take(v.rows as usize).enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(line);
        out.push_str("\x1b[K");
    }
    out.push_str("\x1b[J");
    out
}

/// `> query` on the left, `matched/total` right-aligned.
fn query_line(v: &View<'_>, cols: usize) -> String {
    let count = format!("{}/{}", v.matched, v.total);
    let prompt = format!("> {}", v.query);
    let room = cols.saturating_sub(display_width(&count) + 1);
    let left = fit_cols(&prompt, room);
    format!("{left} \x1b[2m{count}\x1b[0m")
}

/// One list row: a mark column, then the caller's display, highlighted when focused.
fn list_row(line: &Line<'_>, cols: usize) -> String {
    let mark = if line.marked { "\u{25b8} " } else { "  " };
    let body = fit_cols(&format!("{mark}{}", line.display), cols);
    if line.focused {
        format!("\x1b[7m{body}\x1b[0m")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines<'a>(rows: &'a [(&'a str, bool, bool)]) -> Vec<Line<'a>> {
        rows.iter()
            .map(|(d, marked, focused)| Line {
                display: d,
                marked: *marked,
                focused: *focused,
            })
            .collect()
    }

    /// A view with only the fields a case cares about; everything else is a sane default.
    struct V<'a> {
        cols: u16,
        rows: u16,
        list: &'a [Line<'a>],
        preview: Option<&'a str>,
        notice: Option<&'a str>,
        query: &'a str,
    }

    impl<'a> V<'a> {
        fn view(&'a self) -> View<'a> {
            View {
                cols: self.cols,
                rows: self.rows,
                header: "Enter: open | ctrl-x: kill",
                query: self.query,
                matched: self.list.len(),
                total: self.list.len(),
                list: self.list,
                preview: self.preview,
                preview_pct: 50,
                notice: self.notice,
                hints: "ctrl-r: refresh",
            }
        }
        fn new(cols: u16, rows: u16, list: &'a [Line<'a>]) -> Self {
            V {
                cols,
                rows,
                list,
                preview: None,
                notice: None,
                query: "",
            }
        }
    }

    /// Every painted line, escapes and control bytes accounted for.
    fn painted(frame: &str) -> Vec<String> {
        frame
            .trim_start_matches("\x1b[H")
            .trim_end_matches("\x1b[J")
            .split("\r\n")
            .map(|l| l.trim_end_matches("\x1b[K").to_string())
            .collect()
    }

    #[test]
    fn no_line_ever_exceeds_the_terminal_width() {
        let rows = lines(&[
            (
                "a-very-long-repository-name-that-will-not-fit-anywhere",
                false,
                true,
            ),
            ("short", true, false),
        ]);
        for (cols, height) in [(40u16, 10u16), (80, 24), (200, 60)] {
            let v = V {
                preview: Some("preview text that is also quite long indeed"),
                ..V::new(cols, height, &rows)
            };
            let f = frame(&v.view());
            for line in painted(&f) {
                assert!(
                    display_width(&line) <= cols as usize,
                    "{cols}x{height}: line {line:?} is {} cols",
                    display_width(&line)
                );
            }
        }
    }

    #[test]
    fn the_frame_fills_exactly_the_terminal_height() {
        let rows = lines(&[("only", false, true)]);
        let v = V::new(80, 24, &rows);
        assert_eq!(painted(&frame(&v.view())).len(), 24);
    }

    #[test]
    fn exactly_one_row_carries_the_cursor_highlight() {
        let rows = lines(&[("a", false, false), ("b", false, true), ("c", false, false)]);
        let v = V::new(80, 24, &rows);
        assert_eq!(frame(&v.view()).matches("\x1b[7m").count(), 1);
    }

    #[test]
    fn an_empty_list_renders_without_panicking() {
        let rows: Vec<Line<'_>> = Vec::new();
        let v = V::new(80, 24, &rows);
        let f = frame(&v.view());
        assert!(f.contains("0/0"));
    }

    #[test]
    fn the_preview_pane_collapses_on_a_narrow_terminal() {
        let rows = lines(&[("row", false, true)]);
        let wide = V {
            preview: Some("preview"),
            ..V::new(80, 24, &rows)
        };
        assert!(wide.view().preview_cols() > 0);
        let narrow = V {
            preview: Some("preview"),
            ..V::new(44, 12, &rows)
        };
        assert_eq!(narrow.view().preview_cols(), 0);
        assert_eq!(narrow.view().list_cols(), 44);
    }

    #[test]
    fn a_long_preview_line_is_clipped_not_wrapped_into_the_list() {
        let rows = lines(&[("row", false, true)]);
        let v = V {
            preview: Some(&"x".repeat(500)),
            ..V::new(80, 24, &rows)
        };
        let f = frame(&v.view());
        for line in painted(&f) {
            assert!(display_width(&line) <= 80);
        }
        // The divider survives on the first list row.
        assert!(painted(&f)[2].contains('\u{2502}'));
    }

    #[test]
    fn a_notice_replaces_the_hint_line() {
        let rows = lines(&[("row", false, true)]);
        let v = V {
            notice: Some("refused: worktree is dirty"),
            ..V::new(80, 24, &rows)
        };
        let f = frame(&v.view());
        assert!(f.contains("refused: worktree is dirty"));
        assert!(!f.contains("ctrl-r: refresh"));
    }

    #[test]
    fn the_query_and_the_match_count_share_the_prompt_line() {
        let rows = lines(&[("row", false, true)]);
        let v = V {
            query: "feat",
            ..V::new(80, 24, &rows)
        };
        let line = painted(&frame(&v.view()))[1].clone();
        assert!(line.contains("> feat"));
        assert!(line.contains("1/1"));
        assert!(display_width(&line) <= 80);
    }
}
