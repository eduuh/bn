//! The keyboard vocabulary, and the one place crossterm's event type is translated into it.
//!
//! Keeping [`Key`] independent of crossterm is what lets `model.rs` — the whole interaction model —
//! be unit-tested by feeding it a `Vec<Key>` with no terminal attached.

/// A key the picker understands. Anything else is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Key {
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Char(char),
    Ctrl(char),
}

impl Key {
    /// How the key prints in a hint line (`ctrl-x`, `tab`, `↵`).
    pub(crate) fn label(self) -> String {
        match self {
            Key::Enter => "↵".to_string(),
            Key::Esc => "esc".to_string(),
            Key::Tab => "tab".to_string(),
            Key::Backspace => "bksp".to_string(),
            Key::Up => "↑".to_string(),
            Key::Down => "↓".to_string(),
            Key::PageUp => "pgup".to_string(),
            Key::PageDown => "pgdn".to_string(),
            Key::Home => "home".to_string(),
            Key::End => "end".to_string(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("ctrl-{c}"),
        }
    }
}

/// Translate a crossterm key event, or `None` for one the picker has no use for.
///
/// Raw mode clears `ISIG`, so `^C` arrives here as a key rather than a signal — it is mapped to
/// [`Key::Ctrl`]`('c')` and the model turns it into a cancel, which is how the fzf exit code 130
/// that callers already branch on is reproduced.
pub(crate) fn from_crossterm(ev: crossterm::event::KeyEvent) -> Option<Key> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    // A terminal with kitty-protocol key reporting sends press *and* release; acting on both would
    // double every keystroke.
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    Some(match ev.code {
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn control_chars_normalize_to_lowercase() {
        assert_eq!(
            from_crossterm(ev(KeyCode::Char('X'), KeyModifiers::CONTROL)),
            Some(Key::Ctrl('x'))
        );
        assert_eq!(
            from_crossterm(ev(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Key::Ctrl('x'))
        );
    }

    #[test]
    fn a_shifted_letter_stays_a_char() {
        assert_eq!(
            from_crossterm(ev(KeyCode::Char('X'), KeyModifiers::SHIFT)),
            Some(Key::Char('X'))
        );
    }

    #[test]
    fn key_releases_are_dropped_so_a_keystroke_acts_once() {
        let mut e = ev(KeyCode::Char('a'), KeyModifiers::NONE);
        e.kind = KeyEventKind::Release;
        assert_eq!(from_crossterm(e), None);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        assert_eq!(from_crossterm(ev(KeyCode::F(5), KeyModifiers::NONE)), None);
    }

    #[test]
    fn labels_read_the_way_a_header_should() {
        assert_eq!(Key::Ctrl('x').label(), "ctrl-x");
        assert_eq!(Key::Enter.label(), "↵");
    }
}
