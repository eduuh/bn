//! Raw-mode lifecycle: enter, paint, read, and — the part that actually matters — always leave.
//!
//! # Why there is both a `Drop` guard and a panic hook
//!
//! `[profile.release]` sets `panic = "abort"` (see the workspace `Cargo.toml`), so a panic does not
//! unwind and **`Drop` never runs**. A picker that relies on RAII alone would, on any panic, hand
//! the user back a terminal still in raw mode with a hidden cursor — inside a `display-popup` that
//! then tears down, leaving the *outer* shell wedged. A panic hook still runs under abort, so both
//! are installed and both funnel into the same idempotent [`restore`].

use std::io::{IsTerminal, Write};
use std::sync::Once;

/// Is there a terminal to drive? Input is the binding constraint: without a tty on stdin there are
/// no keystrokes to read, and the caller should fall back to printing a plain list.
pub(crate) fn available() -> bool {
    std::io::stdin().is_terminal() && (std::io::stdout().is_terminal() || tty_writer().is_some())
}

/// A writable handle on the controlling terminal, for when stdout has been redirected —
/// `bn tat | cat` must not paint frames into the pipe.
fn tty_writer() -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()
}

/// Put the terminal back the way we found it. Idempotent, and safe to call from a panic hook.
pub(crate) fn restore() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut out: Box<dyn Write> = match tty_writer() {
        Some(f) => Box::new(f),
        None => Box::new(std::io::stdout()),
    };
    let _ = crossterm::execute!(
        out,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = out.flush();
}

static HOOK: Once = Once::new();

/// Chain a terminal restore in front of the existing panic hook, once per process.
fn install_panic_hook() {
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));
    });
}

/// The alternate screen, held for as long as the picker runs.
pub(crate) struct Screen {
    out: Box<dyn Write>,
}

impl Screen {
    /// Enter raw mode and the alternate screen. Returns `None` when the terminal refuses, so the
    /// caller degrades to a plain list instead of half-painting.
    pub(crate) fn enter() -> Option<Self> {
        install_panic_hook();
        let mut out: Box<dyn Write> = if std::io::stdout().is_terminal() {
            Box::new(std::io::stdout())
        } else {
            Box::new(tty_writer()?)
        };
        if crossterm::terminal::enable_raw_mode().is_err() {
            return None;
        }
        if crossterm::execute!(
            out,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )
        .is_err()
        {
            restore();
            return None;
        }
        Some(Self { out })
    }

    /// The terminal's current size. Read directly rather than waiting for a resize event: tmux
    /// often sizes a popup before the first key is read, and a frame drawn at the wrong size is the
    /// first thing the user sees.
    pub(crate) fn size(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or((80, 24))
    }

    pub(crate) fn paint(&mut self, frame: &str) {
        let _ = self.out.write_all(frame.as_bytes());
        let _ = self.out.flush();
    }

    /// Leave the alternate screen, run `f` on the real terminal, then come back.
    ///
    /// This is fzf's `execute(...)` binding: an action that prints for the user to read, or wants
    /// the terminal to itself.
    pub(crate) fn suspended<R>(&mut self, f: impl FnOnce() -> R) -> R {
        restore();
        let out = f();
        if crossterm::terminal::enable_raw_mode().is_ok() {
            let _ = crossterm::execute!(
                self.out,
                crossterm::terminal::EnterAlternateScreen,
                crossterm::cursor::Hide
            );
        }
        out
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore();
    }
}

/// Block for the next terminal event, up to `timeout`.
pub(crate) fn next_event(timeout: std::time::Duration) -> Option<crossterm::event::Event> {
    match crossterm::event::poll(timeout) {
        Ok(true) => crossterm::event::read().ok(),
        _ => None,
    }
}
