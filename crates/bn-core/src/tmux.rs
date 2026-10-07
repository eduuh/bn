//! Reusable tmux / process-integration wrappers (issue #428).
//!
//! tmux integration used to be walled off in bn-cli under the "interactive / side-effecting
//! spawns stay in the CLI" doctrine. That doctrine is dropped: the shared wrappers live here in
//! `bn-core` so the CLI and MCP can reuse them in-process rather than
//! re-implementing the bridge or shelling out to a `bn` subprocess.
//!
//! Every wrapper spawns `tmux` with discrete argv (never a shell), so caller-supplied names are
//! not injection vectors.

use std::ffi::OsStr;
use std::process::Command;

/// Build the `tmux` command with `args` forwarded verbatim — the single sanctioned
/// `Command::new("tmux")` in the workspace. Callers that need bespoke configuration
/// (`current_dir`, environment, conditional args, `.spawn()`, or inspecting the raw
/// [`std::process::Output`] for stderr) build on this; the common cases go through [`tmux`] /
/// [`tmux_raw`] / [`tmux_run`]. Generic over `AsRef<OsStr>` so both `&[&str]` and `&[String]` call
/// sites route through it unchanged.
pub fn tmux_command<S: AsRef<OsStr>>(args: &[S]) -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(args);
    cmd
}

/// Run tmux, returning stdout verbatim on success (no trim) — for multi-line `-F` enumerations
/// whose rows carry trailing empty `@user-option` columns a trim would eat.
pub fn tmux_raw<S: AsRef<OsStr>>(args: &[S]) -> Option<String> {
    let out = tmux_command(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run tmux, returning trimmed non-empty stdout — for single-value captures (a window/session/pane
/// id).
pub fn tmux<S: AsRef<OsStr>>(args: &[S]) -> Option<String> {
    tmux_raw(args)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run tmux for exit status only, returning whether it succeeded (false if tmux can't be spawned).
pub fn tmux_run<S: AsRef<OsStr>>(args: &[S]) -> bool {
    tmux_command(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run tmux for exit status only, capturing (and discarding) output; true if it exited 0 (false if
/// tmux can't be spawned). The captured sibling of [`tmux_run`], for control commands
/// (`send-keys`, `paste-buffer`) whose output must not leak to the terminal — the faithful
/// replacement for the old `.output().map(|o| o.status.success())` call sites.
pub fn tmux_ok<S: AsRef<OsStr>>(args: &[S]) -> bool {
    tmux_command(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// How long presence outlives its last positive observation — the decay window (#1138 follow-up).
///
/// Live presence is a knife-edge: switch tmux windows to check something, or detach while you think,
/// and a probe taken a second later says nobody is here. Fifteen minutes is chosen against what it
/// guards — the reap grace is 5 minutes and the stall verdict needs 10 — so a lane you were just
/// reading outlives both clocks, and a lane you have genuinely walked away from is collectable one
/// decay window later.
pub const ATTENDED_DECAY_SECS: i64 = 15 * 60;

/// The decay window is only protective if it outlives the clocks it guards: the reap grace
/// ([`crate::fleet_gc::DEFAULT_MIN_IDLE_SECS`], 5 min) and the idle-clean stall dwell (10 min).
/// Enforced at compile time so tuning the constant can't quietly make the guard a no-op.
const _: () = assert!(ATTENDED_DECAY_SECS > 10 * 60);

/// The tmux **window user-option** holding the epoch secs of the last time a human was positively
/// observed at this window. Written by [`observe_presence`] on a `Live` verdict only, and read back
/// through the same `-F` enumeration — tmux is the storage, so the decay survives across the
/// independent, stateless sweeps (scheduled cleanup, the orchestrate tick) without a ledger of its own,
/// and dies with the window it describes. Same mechanism as the existing `@bn_agent` window options.
const ATTENDED_AT_OPT: &str = "@bn_attended_at";

/// The `-F` format [`observe_presence`] asks tmux for, one row per pane of the target window:
/// `pane_in_mode`, `window_active`, `window_last_flag`, `session_attached`, `session_name`, and the
/// [`ATTENDED_AT_OPT`] stamp. The stamp is last and is frequently empty, so rows are parsed
/// **without** trimming — a trailing tab is a real, meaningful column here.
const PRESENCE_FMT: &str = "#{pane_in_mode}\t#{window_active}\t#{window_last_flag}\t#{session_attached}\t#{session_name}\t#{@bn_attended_at}";

/// The `-F` format for the per-client keystroke clock: which session a client is in, and when it
/// last had input.
const CLIENT_FMT: &str = "#{client_session}\t#{client_activity}";

/// How present a human is at one agent's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Somebody is looking at this window **now** — scrolling its scrollback, or displaying it on an
    /// attached client. The only verdict that refreshes the [`ATTENDED_AT_OPT`] stamp.
    Live,
    /// Nobody is looking at it this instant, but somebody was within the decay window — they were
    /// just here, or they are still typing in the session this window was the last one they viewed.
    /// Protective, but never self-renewing: a `Recent` verdict does not restamp, so presence decays
    /// to `Away` on its own instead of latching a window as protected forever.
    Recent,
    /// Nobody has been here within the decay window.
    Away,
}

impl Presence {
    /// Whether this verdict protects the window from being taken away.
    pub fn is_attended(self) -> bool {
        !matches!(self, Presence::Away)
    }

    /// The kebab-case label for logs and `--json` surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Presence::Live => "live",
            Presence::Recent => "recent",
            Presence::Away => "away",
        }
    }
}

/// One window's raw presence signals, folded from the [`PRESENCE_FMT`] rows of all its panes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowPresence {
    /// Any pane is in copy-mode — somebody is scrolling the scrollback.
    pub in_copy_mode: bool,
    /// This is its session's active window (what a client attached to that session displays).
    pub displayed: bool,
    /// This is its session's *previously* active window — where you were before the window you are
    /// in now. The side-quest case: glance at a build error in another window, come back.
    pub previous: bool,
    /// How many clients are attached to this window's session.
    pub clients: u32,
    /// The session this window belongs to, for matching against the per-client keystroke clock.
    pub session: String,
    /// Epoch secs of the last `Live` observation ([`ATTENDED_AT_OPT`]); `0` when never stamped.
    pub attended_at: i64,
}

/// Parse the [`PRESENCE_FMT`] rows of one window into a [`WindowPresence`]. Pure. `None` when tmux
/// returned nothing usable (no such window). Copy-mode is folded across panes — any pane counts —
/// while the window/session columns are identical on every row, so the first readable row wins.
pub fn parse_presence(out: &str) -> Option<WindowPresence> {
    let mut found = false;
    let mut p = WindowPresence::default();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 6 {
            continue;
        }
        if f[0] == "1" {
            p.in_copy_mode = true;
        }
        if !found {
            p.displayed = f[1] == "1";
            p.previous = f[2] == "1";
            p.clients = f[3].trim().parse().unwrap_or(0);
            p.session = f[4].to_string();
            p.attended_at = f[5].trim().parse().unwrap_or(0);
            found = true;
        }
    }
    found.then_some(p)
}

/// Parse the [`CLIENT_FMT`] rows into "when did a client attached to `session` last receive input",
/// taking the most recent when several clients share a session. Pure. `None` when no attached client
/// is in that session at all.
pub fn parse_client_activity(out: &str, session: &str) -> Option<i64> {
    out.lines()
        .filter_map(|line| {
            let (s, ts) = line.split_once('\t')?;
            (s == session).then(|| ts.trim().parse::<i64>().ok())?
        })
        .max()
}

/// The presence verdict for one window. **Pure** — the single unit-tested source of truth for "is a
/// human here?", so the policy is not re-derived between the reaper and the supervisor.
///
/// `Live` — somebody is looking right now:
///   - a pane is in copy-mode (nothing but a human puts a pane in copy-mode), or
///   - the window is displayed by an attached client.
///
/// `Recent` — the decay window, which is what makes the guard survive a human's ordinary movement:
///   - a `Live` observation was stamped within `decay_secs`, so switching windows or detaching for a
///     minute does not instantly forfeit protection; or
///   - this window is the active or previous window of a session whose client has typed within
///     `decay_secs` — you stepped one window over and are still at the keyboard.
///
/// The keystroke arm deliberately requires `displayed || previous`: input anywhere in a session must
/// not protect every lane that session happens to hold, only the one you were actually at.
pub fn classify_presence(
    p: &WindowPresence,
    client_activity: Option<i64>,
    now: i64,
    decay_secs: i64,
) -> Presence {
    if p.in_copy_mode || (p.displayed && p.clients > 0) {
        return Presence::Live;
    }
    if p.attended_at > 0 && now.saturating_sub(p.attended_at) <= decay_secs {
        return Presence::Recent;
    }
    if (p.displayed || p.previous) && p.clients > 0 {
        if let Some(ts) = client_activity {
            if now.saturating_sub(ts) <= decay_secs {
                return Presence::Recent;
            }
        }
    }
    Presence::Away
}

/// Observe how present a human is at `target` (a tmux window name or id), at `now`.
///
/// One `list-panes` round-trip always; a second `list-clients` call only when the window is a
/// keystroke-arm candidate and live presence has already been ruled out. **Side effect on purpose:**
/// a [`Presence::Live`] verdict refreshes the [`ATTENDED_AT_OPT`] stamp on the window, which is what
/// gives the decay window something to decay from. A `Recent` verdict deliberately does not restamp.
///
/// Fails **closed** in one direction only: if tmux can't be reached or the window is gone, the answer
/// is [`Presence::Away`] — a window nobody can see is a window nobody is reading. Every other
/// ambiguity resolves toward presence, since sparing a dead lane costs one slot for one decay window,
/// and a false negative costs a human their work mid-review.
pub fn observe_presence(target: &str, now: i64) -> Presence {
    // `exact_target`, not `format!("={target}")`: the latter anchors only the session half, so on a
    // server holding `feat` and `feat-two` this read — and the `@bn_attended_at` stamp below —
    // silently addressed the wrong window.
    let t = exact_target(target);
    let Some(p) = tmux_raw(&["list-panes", "-t", &t, "-F", PRESENCE_FMT])
        .and_then(|out| parse_presence(&out))
    else {
        return Presence::Away;
    };
    // The keystroke clock is only ever consulted for a window that could win on it — skip the
    // round-trip otherwise. That reduces to the *previous* window of an attached session: a
    // displayed window with a client is already `Live`, and one without a client can't reach the
    // keystroke arm at all, so `previous` is the only case left.
    let needs_clients = !p.in_copy_mode && p.previous && p.clients > 0;
    let client_activity = needs_clients
        .then(|| tmux_raw(&["list-clients", "-F", CLIENT_FMT]))
        .flatten()
        .and_then(|out| parse_client_activity(&out, &p.session));

    let verdict = classify_presence(&p, client_activity, now, ATTENDED_DECAY_SECS);
    if verdict == Presence::Live {
        // Fire-and-forget: a failed stamp costs this window its decay window, never its protection.
        let _ = tmux_ok(&[
            "set-option",
            "-w",
            "-t",
            &t,
            ATTENDED_AT_OPT,
            &now.to_string(),
        ]);
    }
    verdict
}

/// Whether a **human is watching this tmux window**, now or within the decay window — the presence
/// signal every destructive fleet action (reap, hard restart, stop-after-ship kill) must consult
/// before it takes a pane away. The boolean face of [`observe_presence`], which is what most callers
/// want; use `observe_presence` directly to log *why* a lane was spared.
pub fn window_attended(target: &str) -> bool {
    observe_presence(target, crate::util::now_epoch_secs()).is_attended()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECAY: i64 = ATTENDED_DECAY_SECS;
    const NOW: i64 = 1_786_394_117;

    /// A window in a session with one attached client, not currently displayed and never stamped —
    /// the baseline a case overrides field-by-field.
    fn win() -> WindowPresence {
        WindowPresence {
            clients: 1,
            session: "bn".to_string(),
            ..WindowPresence::default()
        }
    }

    fn verdict(p: &WindowPresence, activity: Option<i64>) -> Presence {
        classify_presence(p, activity, NOW, DECAY)
    }

    // --- live presence ------------------------------------------------------

    #[test]
    fn copy_mode_is_live_even_on_a_background_window() {
        // Scrolling the scrollback is the clearest "I am reading this" there is — it counts even
        // when the window isn't on screen and the session has no client attached.
        let p = WindowPresence {
            in_copy_mode: true,
            clients: 0,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Live);
    }

    #[test]
    fn the_displayed_window_of_an_attached_session_is_live() {
        let p = WindowPresence {
            displayed: true,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Live);
    }

    #[test]
    fn a_displayed_window_of_a_detached_session_is_not_live() {
        let p = WindowPresence {
            displayed: true,
            clients: 0,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Away);
    }

    // --- the decay window ---------------------------------------------------

    #[test]
    fn a_window_you_just_left_stays_protected() {
        // Detached, switched away, laptop shut — the stamp is all that's left, and it holds.
        let p = WindowPresence {
            clients: 0,
            attended_at: NOW - 60,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Recent);
    }

    #[test]
    fn protection_outlives_both_clocks_it_guards() {
        // The reap grace is 5 minutes and the stall verdict needs 10 (the constant's compile-time
        // assertion pins that); a lane quiet for the full stall dwell is still held.
        let p = WindowPresence {
            clients: 0,
            attended_at: NOW - 10 * 60,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Recent);
    }

    #[test]
    fn protection_expires_once_the_decay_window_passes() {
        let p = WindowPresence {
            clients: 0,
            attended_at: NOW - DECAY - 1,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Away);
    }

    #[test]
    fn a_stamp_from_the_future_does_not_wrap_around() {
        // Clock skew between the stamping pass and this one must not underflow into "ancient".
        let p = WindowPresence {
            clients: 0,
            attended_at: NOW + 3_600,
            ..win()
        };
        assert_eq!(verdict(&p, None), Presence::Recent);
    }

    // --- the keystroke arm --------------------------------------------------

    #[test]
    fn the_window_you_stepped_away_from_is_held_while_you_keep_typing() {
        // The side-quest: you jump one window over to check a build, still at the keyboard. The lane
        // you were reading is the session's *previous* window.
        let p = WindowPresence {
            previous: true,
            ..win()
        };
        assert_eq!(verdict(&p, Some(NOW - 30)), Presence::Recent);
    }

    #[test]
    fn stale_keystrokes_do_not_hold_it() {
        let p = WindowPresence {
            previous: true,
            ..win()
        };
        assert_eq!(verdict(&p, Some(NOW - DECAY - 1)), Presence::Away);
    }

    #[test]
    fn typing_in_a_session_does_not_protect_every_lane_it_holds() {
        // Neither displayed nor previous: you have not looked at this one. A busy session must not
        // blanket-immunize the lanes it happens to contain.
        let p = win();
        assert_eq!(verdict(&p, Some(NOW)), Presence::Away);
    }

    #[test]
    fn recent_never_promotes_itself_to_live() {
        // Only `Live` restamps. If `Recent` could, a window glanced at once would renew its own
        // protection on every sweep and never become collectable.
        let p = WindowPresence {
            previous: true,
            attended_at: NOW - 60,
            ..win()
        };
        assert_ne!(verdict(&p, Some(NOW)), Presence::Live);
    }

    // --- parsing ------------------------------------------------------------

    #[test]
    fn presence_rows_parse_with_an_empty_trailing_stamp() {
        // The real shape from an unstamped window: the last column is empty, so the row ends in a
        // tab. Trimming the line would eat the column and shift every field.
        let p = parse_presence("0\t1\t0\t1\tbn\t\n").expect("a row");
        assert_eq!(
            p,
            WindowPresence {
                in_copy_mode: false,
                displayed: true,
                previous: false,
                clients: 1,
                session: "bn".to_string(),
                attended_at: 0,
            }
        );
    }

    #[test]
    fn copy_mode_folds_across_every_pane_of_the_window() {
        let p = parse_presence("0\t0\t0\t1\tbn\t0\n1\t0\t0\t1\tbn\t0\n").expect("rows");
        assert!(p.in_copy_mode);
    }

    #[test]
    fn a_stamped_window_reads_its_stamp_back() {
        let p = parse_presence("0\t0\t0\t0\tbn\t1786394064\n").expect("a row");
        assert_eq!(p.attended_at, 1_786_394_064);
    }

    #[test]
    fn unreadable_output_is_no_window_at_all() {
        assert_eq!(parse_presence(""), None);
        assert_eq!(parse_presence("1\n"), None);
    }

    #[test]
    fn client_activity_takes_the_most_recent_client_of_the_session() {
        let out = "bn\t100\nother\t9999\nbn\t250\n";
        assert_eq!(parse_client_activity(out, "bn"), Some(250));
        assert_eq!(parse_client_activity(out, "nobody"), None);
    }

    #[test]
    fn a_malformed_client_row_is_skipped_not_fatal() {
        assert_eq!(parse_client_activity("bn\nbn\t42\n", "bn"), Some(42));
    }
}

/// A `session:window` target with **both halves anchored** for exact matching: `=session:=window`.
///
/// `=` anchors only the part it precedes, so the obvious `=session:window` pins the session and
/// leaves the *window* matching by prefix. That is not a theoretical widening. Measured against a
/// live server with no window named `main` beside a `main-stale`, `capture-pane -t '=bn:main'`
/// returned `main-stale`'s pane at exit 0 — a confident answer about the wrong agent, and via
/// `send-keys`/`kill-window` a write aimed at one. Anchoring the window makes tmux answer
/// `can't find window: main`, which every caller already handles.
///
/// One command stays lenient regardless: `display-message` falls back to the session's *current*
/// window instead of failing, so it must never be used to test whether a window exists.
///
/// Ids (`%` pane, `@` window, `$` session) and already-anchored targets pass through untouched, and
/// a trailing `session:` keeps tmux's own meaning of "that session's current window".
pub fn exact_target(target: &str) -> String {
    if target.starts_with(['=', '%', '@', '$']) {
        return target.to_string();
    }
    match target.split_once(':') {
        Some((session, window)) if !window.is_empty() => format!("={session}:={window}"),
        _ => format!("={target}"),
    }
}

#[cfg(test)]
mod exact_target_tests {
    use super::exact_target;

    /// The regression this function exists for: a session-only anchor lets `main` resolve to a
    /// same-prefixed neighbour like `main-stale`.
    #[test]
    fn both_halves_are_anchored() {
        assert_eq!(exact_target("bn:main"), "=bn:=main");
        assert_ne!(exact_target("bn:main"), "=bn:main");
    }

    /// Window names may themselves contain colons; only the first splits session from window.
    #[test]
    fn only_the_first_colon_splits_session_from_window() {
        assert_eq!(exact_target("bn:feat-a:b"), "=bn:=feat-a:b");
    }

    /// A trailing colon is tmux's "that session's current window" and must not become `:=`, which
    /// would ask for a window literally named the empty string.
    #[test]
    fn a_trailing_colon_still_means_the_sessions_current_window() {
        assert_eq!(exact_target("bn:"), "=bn:");
    }

    #[test]
    fn a_bare_session_name_is_anchored_once() {
        assert_eq!(exact_target("bn"), "=bn");
    }

    #[test]
    fn ids_and_already_exact_targets_pass_through_untouched() {
        for t in ["%12", "@5", "$1", "=bn:=main", "=bn"] {
            assert_eq!(exact_target(t), t);
        }
    }
}
