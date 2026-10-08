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
    /// Queue a message typed while the agent is mid-turn. Copilot leaves it in the box on Enter
    /// and queues it on Ctrl+Q (its `ctrl+q enqueue` hint).
    Enqueue,
}

impl Key {
    /// The tmux key name for a key sent with plain `send-keys` (not `Text`, not the raw `Submit`).
    pub fn tmux_key(&self) -> Option<String> {
        match self {
            Key::Escape => Some("Escape".into()),
            Key::Digit(d) => Some(d.to_string()),
            Key::Up => Some("Up".into()),
            Key::Down => Some("Down".into()),
            Key::Enqueue => Some("C-q".into()),
            Key::Text(_) | Key::Submit => None,
        }
    }
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

/// The programs an agent runs as. A pane whose processes include none of these is a shell, an
/// editor or a finished agent, even when its window is named and stamped for one, and a message
/// typed into it is lost (or run as a shell command).
pub const HARNESS_BINS: &[&str] = &["claude", "copilot", "codex", "gemini", "opencode"];

/// Whether any process at or under one of `roots` runs an agent harness. `ps` is the output of
/// `ps -eo pid=,ppid=,args=`; the program is the basename of each process's first argument, since
/// Copilot shows up as `node`'s child `MainThread` by name but `copilot ...` by arguments.
pub fn tree_runs_agent(ps: &str, roots: &[u32]) -> bool {
    let rows: Vec<(u32, u32, &str)> = ps
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let prog = it.next().unwrap_or("");
            Some((pid, ppid, prog.rsplit('/').next().unwrap_or(prog)))
        })
        .collect();
    let mut seen: Vec<u32> = roots.to_vec();
    let mut i = 0;
    while i < seen.len() {
        let pid = seen[i];
        if rows
            .iter()
            .any(|(p, _, prog)| *p == pid && HARNESS_BINS.contains(prog))
        {
            return true;
        }
        for (child, parent, _) in &rows {
            if *parent == pid && !seen.contains(child) {
                seen.push(*child);
            }
        }
        i += 1;
    }
    false
}

/// [`Liveness`] from a pane's process ids and a `ps` listing: no pane is `Unreachable`, a pane
/// with no harness under it is `Exited`. Without a listing (no `ps`) a pane counts as live.
pub fn pane_liveness(pane_pids: &[u32], ps: Option<&str>) -> Liveness {
    if pane_pids.is_empty() {
        return Liveness::Unreachable;
    }
    match ps {
        Some(ps) if !tree_runs_agent(ps, pane_pids) => Liveness::Exited,
        _ => Liveness::Live,
    }
}

/// `ps -eo pid=,ppid=,args=` on this machine, when it runs.
pub fn local_ps() -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,args="])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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

/// What a pane runs when it is not the agent: an editor split beside it, or a bare shell.
const NOT_AGENT: &[&str] = &[
    "nvim", "vim", "vi", "hx", "emacs", "nano", "less", "man", "zsh", "bash", "sh", "fish",
];

/// Of a window's panes (`list-panes -F '#{pane_id}\t#{pane_current_command}\t#{pane_active}'`), the
/// one the agent runs in: the active pane when it is not an editor or a shell, else the first that
/// is not. `None` when nothing qualifies, and the caller keeps the window target.
pub fn pick_agent_pane(rows: &str) -> Option<String> {
    let panes: Vec<(&str, &str, bool)> = rows
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?, f.next()?, f.next() == Some("1")))
        })
        .collect();
    let agent = |cmd: &str| !NOT_AGENT.contains(&cmd);
    panes
        .iter()
        .find(|(_, cmd, active)| *active && agent(cmd))
        .or_else(|| panes.iter().find(|(_, cmd, _)| agent(cmd)))
        .map(|(id, _, _)| id.to_string())
}

impl TmuxTransport {
    /// A window target resolves to tmux's *active* pane, and in a dispatched window that is often
    /// the editor split beside the agent: a message typed there landed in nvim and the agent never
    /// saw it (2026-10-08). So a window with several panes is pinned to its agent pane here.
    pub fn new(target: &str) -> TmuxTransport {
        let exact = crate::tmux::exact_target(target);
        let pane = (!target.starts_with('%'))
            .then(|| {
                crate::tmux::tmux_raw(&[
                    "list-panes",
                    "-t",
                    &exact,
                    "-F",
                    "#{pane_id}\t#{pane_current_command}\t#{pane_active}",
                ])
            })
            .flatten()
            .filter(|rows| rows.lines().count() > 1)
            .and_then(|rows| pick_agent_pane(&rows));
        TmuxTransport {
            target: pane.unwrap_or(exact),
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
                Key::Enqueue => crate::tmux::tmux_ok(&["send-keys", "-t", &self.target, "C-q"]),
            };
        }
        ok
    }

    fn read_screen(&self, lines: usize) -> Option<Vec<String>> {
        let raw = crate::tmux::tmux_raw(&["capture-pane", "-p", "-t", &self.target])?;
        Some(tail(&raw, lines))
    }

    fn liveness(&self) -> Liveness {
        // `list-panes`, not `display-message`: for `=session:=window` naming a window that doesn't
        // exist, tmux 3.7's `display-message` falls back to the session's *current* window and
        // exits 0, so every name in a live session read as Live. `list-panes` fails as it should.
        // A window is not enough either: one whose agent exited back to its shell took the next
        // message as a shell command, so a harness has to be running in it.
        let pids: Vec<u32> =
            crate::tmux::tmux_raw(&["list-panes", "-t", &self.target, "-F", "#{pane_pid}"])
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
        pane_liveness(&pids, local_ps().as_deref())
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

#[cfg(test)]
mod liveness_tests {
    use super::*;

    const PS: &str = "  100     1 -zsh\n  200   100 claude --settings {}\n  210   200 /usr/bin/bn-mcp\n\
                      300     1 zsh\n  310   300 nvim notes.md\n\
                      400     1 zsh -c bn agent launch copilot\n  410   400 /home/u/.local/bin/copilot --allow-all\n";

    #[test]
    fn a_harness_anywhere_under_the_pane_makes_it_live() {
        assert!(tree_runs_agent(PS, &[100]));
        assert!(
            tree_runs_agent(PS, &[400]),
            "copilot found by its path's basename"
        );
    }

    #[test]
    fn a_shell_or_an_editor_is_not_an_agent() {
        assert!(!tree_runs_agent(PS, &[300]));
        assert_eq!(pane_liveness(&[300], Some(PS)), Liveness::Exited);
    }

    #[test]
    fn no_pane_is_unreachable_and_no_ps_trusts_the_window() {
        assert_eq!(pane_liveness(&[], Some(PS)), Liveness::Unreachable);
        assert_eq!(pane_liveness(&[300], None), Liveness::Live);
    }
}

#[cfg(test)]
mod pick_agent_pane_tests {
    use super::pick_agent_pane;

    #[test]
    fn the_agent_wins_over_an_active_editor_split() {
        assert_eq!(
            pick_agent_pane("%12\tnode\t0\n%13\tnvim\t1\n").as_deref(),
            Some("%12")
        );
    }

    #[test]
    fn an_active_agent_pane_is_kept_and_shells_are_skipped() {
        assert_eq!(
            pick_agent_pane("%1\tzsh\t0\n%2\tclaude\t1\n").as_deref(),
            Some("%2")
        );
        assert_eq!(
            pick_agent_pane("%1\tzsh\t1\n%2\tcopilot\t0\n").as_deref(),
            Some("%2")
        );
    }

    #[test]
    fn nothing_but_editors_and_shells_keeps_the_window() {
        assert_eq!(pick_agent_pane("%1\tzsh\t1\n%2\tnvim\t0\n"), None);
    }
}
