//! What a finished pane should look like on the tab bar.
//!
//! Completion used to reach the badge one way only: `monitor-silence` fired `alert-silence` after
//! 12s of quiet, and the window got a green ✓. Silence is a *proxy* — it cannot tell a job that
//! finished from an agent that is thinking, and it gives a failed build the same tick as a clean
//! one. tmux knows better: a pane that exits carries its status in `#{pane_dead_status}`.
//!
//! Measured on tmux 3.7b, the two hooks that report this are mutually exclusive, so both can be
//! bound without double-badging a window:
//!
//! | `remain-on-exit` | hook that fires | `#{pane_dead_status}` |
//! |---|---|---|
//! | `on`  | `pane-died`   | available |
//! | `off` | `pane-exited` | empty (the pane is already gone) |
//!
//! The decisions ([`done_mark`], [`should_badge`]) are pure, so they are testable without a tmux
//! server; [`emit`] is the thin tmux I/O around them.

/// How a finished window should be marked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DoneMark {
    /// Stored in `@bn_done`; `window-status-format` renders it and colours by value.
    pub glyph: &'static str,
    /// The line for the desktop toast.
    pub message: String,
    /// Whether this was a failure, for the toast's urgency.
    pub failed: bool,
}

/// The glyph a clean finish gets — unchanged from the silence-watch behaviour.
pub(crate) const OK_GLYPH: &str = "\u{2713}"; // ✓
/// The glyph a non-zero exit gets. `window-status-format` matches on this exact value to pick red.
pub(crate) const FAIL_GLYPH: &str = "\u{2717}"; // ✗

/// Decide the badge from whatever tmux reported as the pane's exit status.
///
/// `None`, empty, or unparseable all mean the same thing: the pane finished but its status is not
/// knowable — `remain-on-exit` was off so the pane was gone before we looked, or the silence watch
/// fired rather than an exit. That is not a failure, and guessing one would cry wolf on every
/// ordinary shell exit, so it keeps the neutral ✓.
pub(crate) fn done_mark(name: &str, status: Option<&str>) -> DoneMark {
    match status.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match s.parse::<i32>() {
            Ok(0) => ok(name),
            Ok(code) => DoneMark {
                glyph: FAIL_GLYPH,
                message: format!("{name} failed (exit {code})"),
                failed: true,
            },
            // tmux can report a signal death as text rather than a number; say what happened
            // instead of pretending it was clean.
            Err(_) => DoneMark {
                glyph: FAIL_GLYPH,
                message: format!("{name} failed ({s})"),
                failed: true,
            },
        },
        None => ok(name),
    }
}

/// What asked for the badge. Each arrives from a different tmux hook, and they do not know the same
/// things: only `Died` still has the pane, so only `Died` has an exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// `alert-silence`: a watched window went quiet. The original, proxy signal.
    Silence,
    /// `pane-died`: the pane exited and `remain-on-exit` kept it, so its status is known.
    Died,
    /// `pane-exited`: the pane exited and is already gone, so its status is not.
    Exited,
}

impl Trigger {
    /// Which hook this invocation came from, read off the flags the hook passed.
    pub(crate) fn from_flags(status: Option<&str>, exited: bool) -> Trigger {
        match (status, exited) {
            (Some(_), _) => Trigger::Died,
            (None, true) => Trigger::Exited,
            (None, false) => Trigger::Silence,
        }
    }
}

/// Whether a trigger should badge the window at all.
///
/// `pane-died` only happens where `remain-on-exit` kept the pane, which bn sets on the windows it
/// runs work in (agent windows, failed queue jobs), so a window that is still there always gets
/// its outcome. `pane-exited` fires for every pane that closes, including a shell someone typed
/// `exit` into, so it badges only a window that is still there and is being watched. The silence
/// watch badges as it always has.
pub(crate) fn should_badge(trigger: Trigger, window_exists: bool, monitor_silence: &str) -> bool {
    match trigger {
        Trigger::Silence => true,
        Trigger::Died => window_exists,
        Trigger::Exited => window_exists && !matches!(monitor_silence.trim(), "" | "0"),
    }
}

/// `bn tmux-done-badge <window> [--status=<s> | --exited]`: mark a finished window.
///
/// Every step is best-effort, because this runs inside a tmux hook, and a hook that fails is worse
/// than a badge that is missing.
pub(crate) fn emit(win: &str, status: Option<&str>, exited: bool) {
    if win.is_empty() {
        return;
    }
    let trigger = Trigger::from_flags(status, exited);
    // One read for whether the window is still there, its name, and whether it is watched. A
    // window that is gone answers nothing, which is how `pane-exited` on a last pane stays quiet.
    let facts = crate::proc::tmux_raw(&[
        "display-message",
        "-p",
        "-t",
        win,
        "#{window_id}\t#{window_name}\t#{monitor-silence}",
    ])
    .unwrap_or_default();
    let fields: Vec<&str> = facts.trim_end().split('\t').collect();
    let id = fields.first().copied().unwrap_or("");
    let exists = !id.is_empty() && (!win.starts_with('@') || id == win);
    if !should_badge(trigger, exists, fields.get(2).copied().unwrap_or("")) {
        return;
    }
    let name = fields.get(1).copied().filter(|n| !n.is_empty()).unwrap_or("window");
    let mark = done_mark(name, status);
    let _ = crate::proc::tmux_run(&["set-window-option", "-t", win, "@bn_done", mark.glyph]);
    let _ = crate::proc::tmux_run(&["set-window-option", "-t", win, "monitor-silence", "0"]);
    // A clean finish is a completion, which the default bar shows as the badge alone. A failure
    // means work stopped, which is worth an interruption.
    let (class, key) = match (mark.failed, trigger) {
        (true, _) => (crate::toast::Class::Blocking, format!("tmux:{win}:failed")),
        (false, Trigger::Silence) => (crate::toast::Class::Completion, format!("tmux:{win}:silent")),
        (false, _) => (crate::toast::Class::Completion, format!("tmux:{win}:exit")),
    };
    let _ = crate::toast::toast(class, &key, "tmux", "tmux", &mark.message);
}

fn ok(name: &str) -> DoneMark {
    DoneMark {
        glyph: OK_GLYPH,
        message: format!("{name} finished"),
        failed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_exit_keeps_the_neutral_tick() {
        let m = done_mark("build", Some("0"));
        assert_eq!(m.glyph, OK_GLYPH);
        assert_eq!(m.message, "build finished");
        assert!(!m.failed);
    }

    #[test]
    fn a_non_zero_exit_is_marked_failed_and_names_the_code() {
        let m = done_mark("build", Some("3"));
        assert_eq!(m.glyph, FAIL_GLYPH);
        assert_eq!(m.message, "build failed (exit 3)");
        assert!(m.failed);
    }

    #[test]
    fn an_absent_status_is_a_finish_not_a_failure() {
        // `pane-exited` (remain-on-exit off) and the silence watch both land here. Treating an
        // unknown status as failure would flag every ordinary shell exit.
        for s in [None, Some(""), Some("   ")] {
            let m = done_mark("agent", s);
            assert_eq!(m.glyph, OK_GLYPH, "status {s:?} should stay neutral");
            assert!(!m.failed);
        }
    }

    #[test]
    fn a_non_numeric_status_is_reported_rather_than_assumed_clean() {
        let m = done_mark("build", Some("killed"));
        assert_eq!(m.glyph, FAIL_GLYPH);
        assert_eq!(m.message, "build failed (killed)");
    }

    #[test]
    fn each_hook_is_recognised_from_its_flags() {
        assert_eq!(Trigger::from_flags(None, false), Trigger::Silence);
        assert_eq!(Trigger::from_flags(Some("1"), false), Trigger::Died);
        assert_eq!(Trigger::from_flags(Some(""), false), Trigger::Died);
        assert_eq!(Trigger::from_flags(None, true), Trigger::Exited);
    }

    #[test]
    fn a_died_pane_badges_any_window_that_is_still_there() {
        assert!(should_badge(Trigger::Died, true, "0"));
        assert!(should_badge(Trigger::Died, true, ""));
        assert!(!should_badge(Trigger::Died, false, "12"));
    }

    #[test]
    fn an_exited_pane_badges_only_a_watched_window() {
        assert!(should_badge(Trigger::Exited, true, "12"));
        assert!(!should_badge(Trigger::Exited, true, "0"), "a plain shell someone closed");
        assert!(!should_badge(Trigger::Exited, true, ""));
        assert!(!should_badge(Trigger::Exited, false, "12"), "the last pane took the window with it");
    }

    #[test]
    fn the_silence_watch_badges_as_before() {
        assert!(should_badge(Trigger::Silence, true, "0"));
    }

    #[test]
    fn the_two_glyphs_differ_so_the_format_can_tell_them_apart() {
        // window-status-format matches FAIL_GLYPH by value to pick red; if these ever collide the
        // bar silently loses the distinction.
        assert_ne!(OK_GLYPH, FAIL_GLYPH);
    }
}
