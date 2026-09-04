//! Discovering an editor and deciding what state it is in.
//!
//! Two facts, both self-healing, and deliberately no state file between them:
//!
//! 1. **Is nvim alive?** One `connect()` on the derived socket. A refused connect *is* the answer,
//!    so there is nothing to keep in sync and nothing that can go stale.
//! 2. **Where is its pane?** One `list-panes -a`, matching the pane-scoped `@bn_edit` option
//!    against the worktree path.
//!
//! [`classify`] then folds those into a state. It is pure — no tmux, no filesystem — so the whole
//! machine is testable from fabricated rows, the way `wt::decide_window` is.

use super::target::EditTarget;
use bn_core::editor;
use std::path::{Path, PathBuf};

/// Which shape a worktree window is in, from the window-scoped `@bn_shape` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Shape {
    /// `bn wt open` — pane 0 is an interactive shell. The editor is a guest pane.
    #[default]
    Shell,
    /// `bn dispatch` — pane 0 is the harness. The editor is a guest pane.
    Agent,
    /// nvim *is* the window; the agent lives in a terminal buffer inside it. There is no guest
    /// pane to park, so hiding is an in-editor act rather than a tmux one.
    Editor,
}

impl Shape {
    pub(crate) fn parse(s: &str) -> Shape {
        match s.trim() {
            "agent" => Shape::Agent,
            "editor" => Shape::Editor,
            _ => Shape::Shell,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Shape::Shell => "shell",
            Shape::Agent => "agent",
            Shape::Editor => "editor",
        }
    }
}

/// One tmux pane carrying bn's editor marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditPane {
    pub(crate) pane: String,
    pub(crate) window: String,
    pub(crate) session: String,
    pub(crate) dead: bool,
    pub(crate) worktree: String,
    /// The `@<window_id>` this pane belongs back in, from `@bn_edit_home`.
    pub(crate) home: String,
}

/// What `bn edit` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditState {
    /// No live socket and no marked pane — nothing exists yet.
    Absent,
    /// Marked pane sits in its home window.
    Shown,
    /// Marked pane is parked in the hold session.
    Hidden,
    /// Marked pane is neither home nor parked — someone broke it out by hand.
    Detached,
    /// The pane is dead, or it exists but the socket refuses. Tear down and recreate.
    Dead,
    /// nvim *is* the window. There is no guest pane to move.
    Host,
}

impl EditState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            EditState::Absent => "absent",
            EditState::Shown => "shown",
            EditState::Hidden => "hidden",
            EditState::Detached => "detached",
            EditState::Dead => "dead",
            EditState::Host => "host",
        }
    }
}

/// The whole picture for one worktree.
#[derive(Debug, Clone)]
pub(crate) struct EditSession {
    pub(crate) target: EditTarget,
    pub(crate) socket: PathBuf,
    pub(crate) alive: bool,
    pub(crate) pane: Option<EditPane>,
    pub(crate) shape: Shape,
    pub(crate) state: EditState,
}

/// Fold the two observations into a state. Pure: every input is already gathered.
///
/// `home_ok` says whether the pane's recorded home window still exists — a parked editor whose
/// worktree window was killed has to be re-homed rather than blindly joined back.
pub(crate) fn classify(
    alive: bool,
    pane: Option<&EditPane>,
    home_ok: bool,
    shape: Shape,
) -> EditState {
    if shape == Shape::Editor {
        return EditState::Host;
    }
    let Some(pane) = pane else {
        // No pane. A socket with no pane is a leftover from a killed window: treat it as absent
        // and let create unlink it, rather than reporting a liveness we cannot act on.
        return EditState::Absent;
    };
    if pane.dead || !alive {
        return EditState::Dead;
    }
    if pane.session == editor::HOLD_SESSION {
        return EditState::Hidden;
    }
    if home_ok && pane.window == pane.home {
        return EditState::Shown;
    }
    EditState::Detached
}

/// The `list-panes` format this module parses. Order matters — [`parse_pane_rows`] reads it
/// positionally, and both halves must change together.
pub(crate) const PANE_FORMAT: &str = concat!(
    "#{pane_id}\t#{window_id}\t#{session_name}\t#{pane_dead}\t",
    "#{@bn_edit}\t#{@bn_edit_home}"
);

/// Parse `list-panes -a -F PANE_FORMAT` output, keeping only panes carrying bn's marker.
///
/// Pure, so the discovery half is testable from a captured string.
pub(crate) fn parse_pane_rows(out: &str) -> Vec<EditPane> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let pane = f.next()?.trim().to_string();
            let window = f.next()?.trim().to_string();
            let session = f.next()?.trim().to_string();
            let dead = f.next()?.trim() == "1";
            let worktree = f.next()?.trim().to_string();
            let home = f.next().unwrap_or("").trim().to_string();
            // An unmarked pane inherits nothing here: `@bn_edit` is only ever set at pane scope,
            // so an empty value means "not an editor" and never "ask the window".
            (!worktree.is_empty()).then_some(EditPane {
                pane,
                window,
                session,
                dead,
                worktree,
                home,
            })
        })
        .collect()
}

/// Every editor pane on the server, from one tmux call. The picker uses this to badge a whole
/// list of worktrees without paying a tmux round-trip per row.
pub(crate) fn find_panes_all() -> Vec<EditPane> {
    let out = crate::proc::tmux(&["list-panes", "-a", "-F", PANE_FORMAT]).unwrap_or_default();
    parse_pane_rows(&out)
}

/// The editor pane serving `worktree`, if any.
pub(crate) fn find_pane(worktree: &Path) -> Option<EditPane> {
    let want = editor::canonical(worktree);
    find_panes_all()
        .into_iter()
        .find(|p| Path::new(&p.worktree) == want)
}

/// Is an nvim listening on this socket? A plain `connect()` — a syscall, not a subprocess — so
/// `bn edit where` and the picker's per-row badge stay cheap enough to call in a preview.
pub(crate) fn nvim_alive(sock: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(sock).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = sock;
        false
    }
}

/// Remove a socket file that nothing is listening on. `nvim --listen` refuses to start when the
/// path already exists, so a stale file would otherwise wedge every future create.
pub(crate) fn clear_stale(sock: &Path) {
    if sock.exists() && !nvim_alive(sock) {
        let _ = std::fs::remove_file(sock);
    }
}

/// Does this tmux window still exist? Captured (`tmux_ok`) rather than status-only (`tmux_run`),
/// which inherits stdio and would leak the window id to the terminal.
fn window_exists(window: &str) -> bool {
    !window.is_empty()
        && crate::proc::tmux_ok(&["display-message", "-p", "-t", window, "#{window_id}"])
}

/// Read a window's `@bn_shape`.
fn shape_of(window: &str) -> Shape {
    crate::proc::tmux(&[
        "display-message",
        "-p",
        "-t",
        window,
        &format!("#{{{}}}", editor::SHAPE_OPT),
    ])
    .map(|s| Shape::parse(&s))
    .unwrap_or_default()
}

/// Gather both facts for a target and classify them.
pub(crate) fn probe(target: EditTarget) -> EditSession {
    let socket = editor::socket_for(&target.worktree);
    let alive = nvim_alive(&socket);
    let pane = find_pane(&target.worktree);
    let home_ok = pane.as_deref_home().is_some_and(window_exists);
    let shape = pane
        .as_ref()
        .map(|p| shape_of(&p.window))
        .unwrap_or_default();
    let state = classify(alive, pane.as_ref(), home_ok, shape);
    EditSession {
        target,
        socket,
        alive,
        pane,
        shape,
        state,
    }
}

/// Small helper so `probe` can ask "what home does the pane claim?" without unwrapping twice.
trait HomeRef {
    fn as_deref_home(&self) -> Option<&str>;
}

impl HomeRef for Option<EditPane> {
    fn as_deref_home(&self) -> Option<&str> {
        self.as_ref().map(|p| p.home.as_str())
    }
}

#[cfg(test)]
mod pane_parse_tests {
    use super::*;

    fn row(pane: &str, window: &str, session: &str, dead: &str, wt: &str, home: &str) -> String {
        format!("{pane}\t{window}\t{session}\t{dead}\t{wt}\t{home}")
    }

    #[test]
    fn only_panes_carrying_the_marker_are_editors() {
        let out = [
            row("%1", "@1", "bn", "0", "/wt/bn/feat", "@1"),
            // A plain shell pane in the same window: no `@bn_edit`, so it must not be picked up.
            row("%2", "@1", "bn", "0", "", ""),
        ]
        .join("\n");
        let panes = parse_pane_rows(&out);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane, "%1");
    }

    #[test]
    fn a_parked_pane_keeps_the_home_it_came_from() {
        let out = row("%7", "@42", "_bn_edit", "0", "/wt/bn/feat", "@12");
        let panes = parse_pane_rows(&out);
        assert_eq!(panes[0].session, "_bn_edit");
        assert_eq!(
            panes[0].home, "@12",
            "restore has to know where to put it back"
        );
    }

    #[test]
    fn a_truncated_row_is_skipped_rather_than_panicking() {
        assert!(parse_pane_rows("%1\t@1").is_empty());
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    fn pane(session: &str, window: &str, home: &str, dead: bool) -> EditPane {
        EditPane {
            pane: "%1".into(),
            window: window.into(),
            session: session.into(),
            dead,
            worktree: "/wt/bn/feat".into(),
            home: home.into(),
        }
    }

    #[test]
    fn nothing_found_is_absent() {
        assert_eq!(
            classify(false, None, false, Shape::Shell),
            EditState::Absent
        );
    }

    #[test]
    fn a_socket_with_no_pane_is_absent_not_alive() {
        // A leftover socket from a killed window. Reporting it as live would strand the caller:
        // there is no pane to show, and create must be free to unlink and start over.
        assert_eq!(classify(true, None, false, Shape::Shell), EditState::Absent);
    }

    #[test]
    fn a_pane_at_home_with_a_live_socket_is_shown() {
        let p = pane("bn", "@12", "@12", false);
        assert_eq!(
            classify(true, Some(&p), true, Shape::Shell),
            EditState::Shown
        );
    }

    #[test]
    fn a_pane_in_the_hold_session_is_hidden() {
        let p = pane(editor::HOLD_SESSION, "@42", "@12", false);
        assert_eq!(
            classify(true, Some(&p), true, Shape::Shell),
            EditState::Hidden
        );
    }

    #[test]
    fn a_pane_somewhere_else_entirely_is_detached() {
        // `prefix !` breaks a pane into its own window. It is still our editor, just not where we
        // left it, so `show` joins it home rather than splitting in a second nvim.
        let p = pane("bn", "@99", "@12", false);
        assert_eq!(
            classify(true, Some(&p), true, Shape::Shell),
            EditState::Detached
        );
    }

    #[test]
    fn a_parked_pane_whose_home_window_died_is_still_hidden() {
        // Being parked is decided by the session it sits in, not by whether home survived — a
        // worktree window closed while the editor was away still has to be re-homed, not recreated.
        let p = pane(editor::HOLD_SESSION, "@42", "@12", false);
        assert_eq!(
            classify(true, Some(&p), false, Shape::Shell),
            EditState::Hidden
        );
    }

    #[test]
    fn a_live_pane_with_a_refusing_socket_is_dead() {
        // nvim quit but `remain-on-exit on` kept the pane. Reusing it would attach to nothing.
        let p = pane("bn", "@12", "@12", false);
        assert_eq!(
            classify(false, Some(&p), true, Shape::Shell),
            EditState::Dead
        );
    }

    #[test]
    fn a_dead_pane_is_dead_even_when_something_answers_the_socket() {
        let p = pane("bn", "@12", "@12", true);
        assert_eq!(
            classify(true, Some(&p), true, Shape::Shell),
            EditState::Dead
        );
    }

    #[test]
    fn an_editor_shaped_window_is_always_host() {
        // nvim *is* the window. There is no guest pane to park, and `break-pane` on the only pane
        // would strip the window of its identity and trip the respawn-last path.
        assert_eq!(classify(true, None, false, Shape::Editor), EditState::Host);
        let p = pane("bn", "@12", "@12", false);
        assert_eq!(
            classify(true, Some(&p), true, Shape::Editor),
            EditState::Host
        );
    }
}
