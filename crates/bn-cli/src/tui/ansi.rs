//! ANSI-aware width and truncation.
//!
//! Rows carry SGR escapes (`agent_dot`, the doctor severity colours, the preview's rule), so the
//! renderer cannot use `str::len()` or `chars().count()` to decide what fits in a column. It also
//! cannot reach for `unicode-width`: that crate is not a dependency, and it would be answering a
//! question this codebase has already settled elsewhere.
//!
//! **Every non-escape `char` is one column.** That is exact for the vocabulary bn emits —
//! ASCII, the single-width Nerd Font PUA glyphs `bn_core::glyph` was built around, and the handful
//! of geometric/warning symbols in the row builders (`● ◍ ○ ◌ ☁ ✓ ✗ ⚠ ━`). It is wrong for CJK and
//! emoji, which bn deliberately does not emit — `glyph.rs` says so, and the reason it gives is this
//! exact failure: they "misalign columns in fzf/tmux". If that ever changes, this is the module
//! that has to grow a width table, and [`display_width`]'s tests are where the change announces
//! itself.

/// How many terminal columns `s` occupies, ignoring escape sequences.
pub(crate) fn display_width(s: &str) -> usize {
    let mut cols = 0;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            skip_escape(&mut it);
        } else {
            cols += 1;
        }
    }
    cols
}

/// `s` with every escape sequence removed — what the fuzzy matcher should see.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            skip_escape(&mut it);
        } else {
            out.push(c);
        }
    }
    out
}

/// `s` cut to at most `cols` columns, never mid-escape. A cut that lands inside a styled run gets a
/// trailing reset, so a truncated row can never bleed its colour into the rest of the frame.
pub(crate) fn truncate_cols(s: &str, cols: usize) -> String {
    if display_width(s) <= cols {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut used = 0;
    let mut styled = false;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            let esc = take_escape(&mut it);
            // A bare `ESC[m` / `ESC[0m` closes the run; anything else opens or changes one.
            styled = !matches!(esc.as_str(), "[m" | "[0m");
            out.push('\x1b');
            out.push_str(&esc);
            continue;
        }
        if used == cols {
            break;
        }
        out.push(c);
        used += 1;
    }
    if styled {
        out.push_str("\x1b[0m");
    }
    out
}

/// `s` truncated to `cols` and then padded with spaces to exactly `cols` columns.
pub(crate) fn fit_cols(s: &str, cols: usize) -> String {
    let mut out = truncate_cols(s, cols);
    let pad = cols.saturating_sub(display_width(&out));
    out.push_str(&" ".repeat(pad));
    out
}

/// Consume the body of an escape sequence, `it` positioned just after the ESC.
fn skip_escape(it: &mut std::str::Chars<'_>) {
    let _ = take_escape(it);
}

/// Consume and return the body of an escape sequence (everything after the ESC byte).
///
/// CSI (`ESC [ … final`) ends at a byte in `@..~`. OSC (`ESC ] …`) ends at BEL or ST (`ESC \`) —
/// bn does not emit OSC today, but a title-setting sequence arriving from a captured pane must not
/// be miscounted as a screenful of columns. Anything else is a two-byte sequence.
fn take_escape(it: &mut std::str::Chars<'_>) -> String {
    let mut body = String::new();
    let Some(first) = it.next() else {
        return body;
    };
    body.push(first);
    match first {
        '[' => {
            for c in it.by_ref() {
                body.push(c);
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
        ']' => {
            let mut prev_esc = false;
            for c in it.by_ref() {
                body.push(c);
                if c == '\x07' || (prev_esc && c == '\\') {
                    break;
                }
                prev_esc = c == '\x1b';
            }
        }
        _ => {}
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_one_column_per_char() {
        assert_eq!(display_width("plain"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn sgr_escapes_cost_nothing() {
        // The doctor picker's severity row (picker.rs) and tat's agent dot (rows.rs).
        assert_eq!(display_width("\x1b[1;31m⚠ cron\x1b[0m"), 6);
        assert_eq!(display_width("\x1b[32m●\x1b[0m "), 2);
    }

    /// The shape every `agent_dot` row marker takes: a coloured single-width glyph plus a space.
    /// The sub-picker's column alignment rests on all of them measuring the same; `tat::rows` pins
    /// that for the real function, this pins the measurement it relies on.
    #[test]
    fn a_coloured_glyph_plus_a_space_is_two_columns() {
        for glyph in ['\u{25cf}', '\u{25cd}', '\u{25cb}', '\u{25cc}'] {
            assert_eq!(display_width(&format!("\x1b[32m{glyph}\x1b[0m ")), 2);
        }
    }

    #[test]
    fn stripping_leaves_only_the_text() {
        assert_eq!(strip_ansi("\x1b[1;31m⚠ cron\x1b[0m"), "⚠ cron");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }

    #[test]
    fn an_osc_sequence_is_not_counted_as_text() {
        assert_eq!(display_width("\x1b]0;a title\x07x"), 1);
        assert_eq!(display_width("\x1b]0;a title\x1b\\x"), 1);
    }

    #[test]
    fn truncation_counts_columns_not_bytes() {
        assert_eq!(truncate_cols("abcdef", 3), "abc");
        assert_eq!(truncate_cols("abc", 10), "abc");
    }

    #[test]
    fn a_cut_inside_a_styled_run_is_closed() {
        let out = truncate_cols("\x1b[32mgreen text\x1b[0m", 5);
        assert!(out.starts_with("\x1b[32m"));
        assert!(out.ends_with("\x1b[0m"), "must not bleed colour: {out:?}");
        assert_eq!(display_width(&out), 5);
    }

    #[test]
    fn a_cut_after_the_reset_adds_no_second_reset() {
        let out = truncate_cols("\x1b[32mab\x1b[0mcdef", 3);
        assert_eq!(out.matches("\x1b[0m").count(), 1);
    }

    #[test]
    fn fitting_pads_to_exactly_the_width() {
        assert_eq!(display_width(&fit_cols("ab", 5)), 5);
        assert_eq!(display_width(&fit_cols("abcdef", 4)), 4);
        assert_eq!(display_width(&fit_cols("\x1b[32m●\x1b[0m x", 6)), 6);
    }
}
