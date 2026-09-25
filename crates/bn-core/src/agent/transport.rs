//! How bn talks to an agent, once [`super::channel`] has worked out *which* agent it is.
//!
//! bn has long conflated three things into one string: which agent, which tmux window, and how to
//! reach it. The first is answered well by [`AgentChannel`](super::channel::AgentChannel) and its
//! tier ladder. This module is the third — and it exists because those three stop being the same
//! object the moment an agent runs inside an editor rather than directly in a pane. The window is
//! still there (it runs the editor) and everything keyed on the window stays correct; only the
//! *delivery* changes.
//!
//! Today there is exactly one implementation, [`TmuxTransport`], and it emits the same tmux
//! commands the call sites emitted inline. That is the point of landing it on its own: the seam is
//! provably behaviour-preserving before anything new hangs off it.
//!
//! Deliberately **not** in this trait:
//!
//! - **presence** — "is a human looking at this?" is a property of the *window*, not the agent, and
//!   already lives in [`crate::tmux`];
//! - **kill and spawn** — container operations. A site that types a shell command to *start* a
//!   harness is not steering an agent, even though it also calls `send-keys`;
//! - **paste-versus-type** — that distinction only exists because tmux's `send-keys` is
//!   character-by-character. It is an implementation detail of one transport, not a concept the
//!   callers should carry.

use crate::agent::channel::AgentEndpoint;

/// A key or chunk of input to deliver to an agent.
///
/// High level on purpose: each transport realizes these its own way. `Text` is one concept with two
/// realizations — tmux stages it through a paste buffer so a TUI cannot mangle a character-by-
/// character type, while an editor-hosted agent takes a single atomic write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A whole message, delivered as one block.
    Text(String),
    /// A raw carriage return (0x0d), not the named `Enter` key — Copilot's compose TUI was observed
    /// to swallow the named key and leave the message unsent.
    Submit,
    Escape,
    /// A single digit, for answering a numbered prompt.
    Digit(u8),
    /// Cursor keys, for a menu chosen by moving a `❯` rather than typing a number.
    Up,
    Down,
}

/// Whether an agent is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    Live,
    /// Reachable, but the agent's process is gone.
    Exited,
    /// The transport itself could not be reached.
    Unreachable,
}

/// One way of talking to a live agent.
pub trait AgentTransport {
    /// Deliver a key plan. `false` means it did not land.
    fn write_keys(&self, keys: &[Key]) -> bool;

    /// The agent's last `lines` rendered lines, newest last.
    fn read_screen(&self, lines: usize) -> Option<Vec<String>>;

    /// Whether the agent is still reachable.
    fn liveness(&self) -> Liveness;
}

/// The transport for a resolved endpoint.
pub fn transport_for(endpoint: &AgentEndpoint) -> Box<dyn AgentTransport> {
    match endpoint {
        AgentEndpoint::TmuxPane(target) => Box::new(TmuxTransport::new(target)),
    }
}

/// Talking to an agent that *is* a tmux pane.
pub struct TmuxTransport {
    /// The `=session:window` form, so a name can never fnmatch its way into a neighbouring pane.
    target: String,
}

impl TmuxTransport {
    pub fn new(target: &str) -> TmuxTransport {
        TmuxTransport {
            target: crate::tmux::exact_target(target),
        }
    }

    /// The buffer name used to stage a `Text` key. Per-target so two concurrent steers cannot
    /// clobber each other's message.
    fn buffer_name(&self) -> String {
        format!(
            "bn-{}",
            self.target
                .trim_start_matches('=')
                .replace(['=', ':', '/', '.'], "-")
        )
    }
}

impl AgentTransport for TmuxTransport {
    fn write_keys(&self, keys: &[Key]) -> bool {
        let mut ok = true;
        for key in keys {
            ok &= match key {
                // Staged through a buffer and pasted as one block: a character-by-character
                // `send-keys` lets an autosuggest/highlight TUI mangle the message. `--` guards a
                // leading `-` in the text from being read as a flag; `-d` deletes the buffer after.
                Key::Text(text) => {
                    let buf = self.buffer_name();
                    crate::tmux::tmux_ok(&["set-buffer", "-b", &buf, "--", text])
                        && crate::tmux::tmux_ok(&[
                            "paste-buffer",
                            "-d",
                            "-b",
                            &buf,
                            "-t",
                            &self.target,
                        ])
                }
                // The literal CR byte rather than the named key (#306).
                Key::Submit => crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, "-H", "0d"]),
                Key::Escape => crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, "Escape"]),
                Key::Digit(d) => {
                    crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, &d.to_string()])
                }
                Key::Up => crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, "Up"]),
                Key::Down => crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, "Down"]),
            };
        }
        ok
    }

    fn read_screen(&self, lines: usize) -> Option<Vec<String>> {
        let raw = crate::tmux::tmux_raw(&["capture-pane", "-p", "-t", &self.target])?;
        Some(tail(&raw, lines))
    }

    fn liveness(&self) -> Liveness {
        if crate::tmux::tmux_ok(&["display-message", "-p", "-t", &self.target, "#{window_id}"]) {
            Liveness::Live
        } else {
            Liveness::Unreachable
        }
    }
}

/// The last `lines` non-empty-trailing lines of a captured screen.
///
/// `capture-pane` right-pads to the pane height, so a naive tail is mostly blanks. Trailing blank
/// lines are dropped before counting.
pub fn tail(raw: &str, lines: usize) -> Vec<String> {
    let mut rows: Vec<&str> = raw.lines().map(|l| l.trim_end()).collect();
    while rows.last().is_some_and(|l| l.is_empty()) {
        rows.pop();
    }
    let start = rows.len().saturating_sub(lines);
    rows[start..].iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tail_tests {
    use super::*;

    #[test]
    fn trailing_blank_padding_is_dropped_before_counting() {
        // `capture-pane` pads to the pane height, so counting raw lines would return mostly blanks
        // and push the actual output out of the window a classifier looks at.
        let raw = "one\ntwo\nthree\n\n\n\n";
        assert_eq!(tail(raw, 2), vec!["two", "three"]);
    }

    #[test]
    fn asking_for_more_than_there_is_returns_everything() {
        assert_eq!(tail("a\nb", 99), vec!["a", "b"]);
    }

    #[test]
    fn an_empty_capture_yields_nothing() {
        assert!(tail("\n\n\n", 5).is_empty());
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    #[test]
    fn both_halves_of_the_target_are_anchored() {
        // This used to assert `=bn:feat` — which anchors only the SESSION. tmux still resolves the
        // window half as an fnmatch pattern, so with windows `feat` and `feat-two` open, every
        // write this transport makes (paste-buffer, send-keys, Escape, a menu digit) could land in
        // `feat-two`. Verified against tmux 3.6: `set-window-option -t '=s:feat'` wrote onto
        // `feat-two`. `exact_target` anchors both halves, which is the whole point of it.
        assert_eq!(TmuxTransport::new("bn:feat").target, "=bn:=feat");
    }

    #[test]
    fn a_prefix_of_another_window_name_cannot_match_it() {
        assert_ne!(
            TmuxTransport::new("bn:feat").target,
            TmuxTransport::new("bn:feat-two").target
        );
    }

    #[test]
    fn an_already_anchored_target_is_left_alone() {
        assert_eq!(TmuxTransport::new("=bn:=feat").target, "=bn:=feat");
    }

    #[test]
    fn a_pane_id_passes_through_untouched() {
        assert_eq!(TmuxTransport::new("%12").target, "%12");
    }

    #[test]
    fn each_target_stages_text_in_its_own_buffer() {
        // Two concurrent steers must not clobber each other's pending message.
        let a = TmuxTransport::new("bn:feat").buffer_name();
        let b = TmuxTransport::new("bn:other").buffer_name();
        assert_ne!(a, b);
        assert!(!a.contains(':'), "buffer names must not carry tmux syntax");
        // The anchoring `=` is tmux syntax too, and it is now INSIDE the target, not just leading.
        assert!(!a.contains('='), "buffer names must not carry tmux syntax");
    }
}
