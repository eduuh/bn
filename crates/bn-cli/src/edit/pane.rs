//! The tmux argv for creating, parking and restoring an editor pane.
//!
//! Every builder here is pure and returns argv, following `wt::window_tmux_cmds` — the sequences
//! are the fiddly part of this feature, so they are unit-tested rather than only observed.

use bn_core::editor;
use std::path::Path;

/// Which side of the window the editor sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub(crate) fn parse(s: &str) -> Side {
        if s.eq_ignore_ascii_case("right") {
            Side::Right
        } else {
            Side::Left
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    /// `split-window`/`join-pane` place the new pane *after* the target by default; `-b` puts it
    /// before. Left means before.
    fn before(self) -> bool {
        self == Side::Left
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Is this editor an nvim we can drive over a socket? Only nvim understands `--listen`, so a
/// `$EDITOR` of anything else gets a plain pane and loses the RPC half rather than failing to start.
pub(crate) fn is_nvim(editor_bin: &str) -> bool {
    Path::new(editor_bin)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "nvim" || n == "nvim.appimage")
}

/// `$EDITOR`, defaulting to nvim — matching every other editor spawn in bn.
pub(crate) fn editor_bin() -> String {
    std::env::var("EDITOR").unwrap_or_else(|_| "nvim".to_string())
}

/// The `split-window` that creates the editor pane, printing its new pane id.
///
/// `anchor_pane` is a real `%id`, never a `<window>.0` composite: the `.N` pane-index suffix is
/// only valid after a window *name or index*, so appending it to a `@window_id` fails outright
/// ("can't find pane: 0"). Ids are what the rest of this module uses, so the caller resolves the
/// window's first pane and passes it here.
///
/// `-d` so a scripted call never yanks focus (the discipline `wt::window` already applies when
/// opening a worktree window); `--` so a landing argument can never be parsed as a tmux flag; and
/// the editor runs as the pane's **direct** process rather than under `exec $SHELL -c`, so the pane
/// dies exactly when the editor does and there is no shell left holding it open.
pub(crate) fn create_split_cmd(
    anchor_pane: &str,
    worktree: &Path,
    sock: &Path,
    side: Side,
    size: &str,
    editor_bin: &str,
    land_spec: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![s("split-window"), s("-h")];
    if side.before() {
        argv.push(s("-b"));
    }
    argv.extend([
        s("-d"),
        s("-l"),
        s(size),
        s("-t"),
        s(anchor_pane),
        s("-c"),
        worktree.display().to_string(),
    ]);
    if is_nvim(editor_bin) {
        // Exported so anything started inside the pane — including an agent — can find its editor
        // without re-deriving anything.
        argv.extend([s("-e"), format!("BN_EDIT_SOCK={}", sock.display())]);
        argv.extend([s("-e"), format!("BN_EDIT_WT={}", worktree.display())]);
    }
    argv.extend([s("-P"), s("-F"), s("#{pane_id}"), s("--")]);
    argv.push(s(editor_bin));
    if is_nvim(editor_bin) {
        argv.extend([s("--listen"), sock.display().to_string()]);
        if let Some(spec) = land_spec {
            argv.extend([
                s("-c"),
                format!(
                    "lua require('bn').land_file('{}')",
                    spec.display().to_string().replace('\'', "\\'")
                ),
            ]);
        }
    } else {
        argv.push(s("."));
    }
    argv
}

/// Stamp a freshly created (or freshly moved) editor pane.
///
/// `@bn_edit`/`@bn_edit_home` are set with `-p` **without exception**: pane options inherit from
/// window options, so setting these at window scope would make every pane in the window claim to
/// be the editor.
pub(crate) fn stamp_cmds(pane: &str, worktree: &Path, home: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            s("set-option"),
            s("-p"),
            s("-t"),
            s(pane),
            s(editor::PANE_OPT),
            worktree.display().to_string(),
        ],
        vec![
            s("set-option"),
            s("-p"),
            s("-t"),
            s(pane),
            s(editor::PANE_HOME_OPT),
            s(home),
        ],
        vec![
            s("set-option"),
            s("-w"),
            s("-t"),
            s(home),
            s(editor::WINDOW_OPT),
            s(pane),
        ],
    ]
}

/// Ensure the hold session exists.
///
/// It carries a permanent `hold` window on purpose. `wt`'s window teardown matches worktree windows
/// by pane cwd, and a parked editor's cwd is still the worktree — so closing a worktree finds and
/// kills the parked window too, which is right. But a session down to its last window is
/// *respawned* rather than killed, which would leave a zombie shell behind for every worktree ever
/// closed. The placeholder guarantees there is always a second window.
pub(crate) fn ensure_hold_cmd() -> Vec<String> {
    vec![
        s("new-session"),
        s("-d"),
        s("-s"),
        s(editor::HOLD_SESSION),
        s("-n"),
        s("hold"),
        s("sleep 2147483647"),
    ]
}

/// Park the editor: re-stamp its home (the window may have moved since it was shown), then break
/// it out into the hold session without following it.
pub(crate) fn hide_cmds(pane: &str, home: &str, name: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            s("set-option"),
            s("-p"),
            s("-t"),
            s(pane),
            s(editor::PANE_HOME_OPT),
            s(home),
        ],
        vec![
            s("break-pane"),
            s("-d"),
            s("-n"),
            s(name),
            s("-s"),
            s(pane),
            s("-t"),
            format!("{}:", editor::HOLD_SESSION),
        ],
        vec![
            s("set-option"),
            s("-w"),
            s("-t"),
            s(home),
            s("-u"),
            s(editor::WINDOW_OPT),
        ],
    ]
}

/// Bring a parked editor back.
///
/// The explicit `resize-pane -x` is not redundant: `join-pane -l` sizes the joined pane relative to
/// the *target pane*, not the window, so it lands wrong the moment the window holds more than one
/// pane. Sizing again against the window is what makes 70% mean 70%.
pub(crate) fn restore_cmds(
    pane: &str,
    home: &str,
    anchor_pane: &str,
    side: Side,
    size: &str,
) -> Vec<Vec<String>> {
    let mut join = vec![s("join-pane")];
    if side.before() {
        join.push(s("-b"));
    }
    join.extend([
        s("-h"),
        s("-l"),
        s(size),
        s("-s"),
        s(pane),
        s("-t"),
        s(anchor_pane),
    ]);
    vec![
        join,
        vec![
            s("resize-pane"),
            s("-t"),
            s(pane),
            s("-x"),
            format!("{}%", size.trim_end_matches('%')),
        ],
        // Re-record home and re-point the window at the pane. `@bn_edit` itself still holds the
        // worktree path — pane options travel with the pane through break/join — so only the two
        // location facts need refreshing.
        vec![
            s("set-option"),
            s("-p"),
            s("-t"),
            s(pane),
            s(editor::PANE_HOME_OPT),
            s(home),
        ],
        vec![
            s("set-option"),
            s("-w"),
            s("-t"),
            s(home),
            s(editor::WINDOW_OPT),
            s(pane),
        ],
    ]
}

/// Focus an editor pane and the window holding it.
pub(crate) fn focus_cmds(pane: &str, home: &str) -> Vec<Vec<String>> {
    vec![
        vec![s("select-window"), s("-t"), s(home)],
        vec![s("select-pane"), s("-t"), s(pane)],
    ]
}

#[cfg(test)]
mod create_tests {
    use super::*;

    fn argv(v: &[String]) -> String {
        v.join(" ")
    }

    #[test]
    fn nvim_gets_a_socket_and_runs_as_the_panes_own_process() {
        let cmd = create_split_cmd(
            "%9",
            Path::new("/wt/bn/feat"),
            Path::new("/run/user/1000/bn/edit/abc.sock"),
            Side::Left,
            "70%",
            "nvim",
            None,
        );
        let line = argv(&cmd);
        assert!(line.contains("--listen /run/user/1000/bn/edit/abc.sock"));
        assert!(
            line.contains("-l 70%"),
            "the pane must be sized on creation"
        );
        assert!(line.contains("-d"), "creating must not steal focus");
        assert!(
            line.contains("-P -F #{pane_id}"),
            "the caller needs the new pane id to stamp it"
        );
        assert!(
            !line.contains("$SHELL"),
            "no shell wrapper: the pane must die exactly when nvim does"
        );
        assert!(cmd.contains(&"--".to_string()), "argv must be terminated");
    }

    #[test]
    fn a_non_nvim_editor_gets_a_plain_pane_and_no_socket() {
        // Losing the RPC half is fine; failing to start because `--listen` is unknown is not.
        let cmd = create_split_cmd(
            "%9",
            Path::new("/wt/bn/feat"),
            Path::new("/tmp/x.sock"),
            Side::Left,
            "70%",
            "vim",
            None,
        );
        let line = argv(&cmd);
        assert!(!line.contains("--listen"));
        assert!(!line.contains("BN_EDIT_SOCK"));
        assert!(line.ends_with("vim ."));
    }

    #[test]
    fn side_selects_whether_the_pane_lands_before_the_target() {
        assert!(create_split_cmd(
            "%9",
            Path::new("/w"),
            Path::new("/s"),
            Side::Left,
            "70%",
            "nvim",
            None
        )
        .contains(&"-b".to_string()));
        assert!(!create_split_cmd(
            "%9",
            Path::new("/w"),
            Path::new("/s"),
            Side::Right,
            "70%",
            "nvim",
            None
        )
        .contains(&"-b".to_string()));
    }
}

#[cfg(test)]
mod park_tests {
    use super::*;

    /// Which scope flag an argv uses, by exact option name — never a substring test, because
    /// `@bn_editor` contains `@bn_edit` and a `contains` check silently conflates the two.
    fn scope_for(cmds: &[Vec<String>], option: &str) -> Option<String> {
        cmds.iter()
            .find(|c| c.iter().any(|a| a == option))
            .map(|c| {
                if c.contains(&"-p".to_string()) {
                    "-p".to_string()
                } else if c.contains(&"-w".to_string()) {
                    "-w".to_string()
                } else {
                    "none".to_string()
                }
            })
    }

    #[test]
    fn the_pane_markers_are_only_ever_set_at_pane_scope() {
        // The trap this pins: pane options inherit from window options, so `@bn_edit` set with -w
        // would make every pane in that window answer as the editor.
        let cmds = stamp_cmds("%1", Path::new("/wt/bn/feat"), "@12");
        assert_eq!(
            scope_for(&cmds, bn_core::editor::PANE_OPT).as_deref(),
            Some("-p")
        );
        assert_eq!(
            scope_for(&cmds, bn_core::editor::PANE_HOME_OPT).as_deref(),
            Some("-p")
        );
    }

    #[test]
    fn the_windows_editor_pointer_is_window_scoped() {
        // The counterpart: `@bn_editor` is a property of the window, and is deliberately named
        // differently from `@bn_edit` so inheritance can never leak it into a pane lookup.
        let cmds = stamp_cmds("%1", Path::new("/wt/bn/feat"), "@12");
        assert_eq!(
            scope_for(&cmds, bn_core::editor::WINDOW_OPT).as_deref(),
            Some("-w")
        );
    }

    #[test]
    fn hiding_re_stamps_home_before_breaking_out() {
        // Order matters: once the pane is in the hold session its old window is no longer
        // discoverable from it, so home has to be recorded first.
        let cmds = hide_cmds("%1", "@12", "feat");
        assert!(cmds[0].join(" ").contains(bn_core::editor::PANE_HOME_OPT));
        assert!(cmds[1].join(" ").starts_with("break-pane -d"));
    }

    #[test]
    fn hiding_parks_into_the_hold_session_without_following() {
        let line = hide_cmds("%1", "@12", "feat")[1].join(" ");
        assert!(line.contains("-d"), "parking must not switch the client");
        assert!(line.contains(&format!("{}:", bn_core::editor::HOLD_SESSION)));
    }

    #[test]
    fn hiding_clears_the_windows_editor_pointer() {
        let last = hide_cmds("%1", "@12", "feat").pop().unwrap().join(" ");
        assert!(
            last.contains("-u"),
            "the window must stop claiming an editor"
        );
        assert!(last.contains(bn_core::editor::WINDOW_OPT));
    }

    #[test]
    fn restoring_resizes_against_the_window_after_joining() {
        // `join-pane -l` is relative to the target *pane*, so a window with an extra pane would
        // otherwise get an editor that is 70% of a half.
        let cmds = restore_cmds("%1", "@12", "%9", Side::Left, "70%");
        assert!(cmds[0].join(" ").starts_with("join-pane -b -h -l 70%"));
        assert!(
            cmds[0].join(" ").ends_with("-t %9"),
            "the join target must be a pane id, not a <window>.0 composite"
        );
        assert_eq!(cmds[1].join(" "), "resize-pane -t %1 -x 70%");
    }

    #[test]
    fn the_hold_session_is_created_with_a_placeholder_window() {
        // Without a second window, closing a worktree would respawn the hold session's last
        // window as a bare shell instead of killing it — a zombie per worktree ever closed.
        let line = ensure_hold_cmd().join(" ");
        assert!(line.contains("-d"), "the hold session is never attached");
        assert!(line.contains("-n hold"));
    }
}
