//! Running a picker verb now, in a visible tmux window in the session of the repo it serves.
//!
//! A verb pressed in `bn tat` wants to be running before the person looks up, and findable while it
//! runs. So it opens a detached window named after the job, runs `bn run <job> --repo R` there, and
//! lets the window close when the run exits 0. A failed run keeps its dead pane to be read. `bn run`
//! records the outcome itself (the running marker and the `cron_run` event), so `bn cron` sees a
//! picker run like any other, and the picker reads the row's state back from the window.
use crate::*;
use bn_core::wt_actions::WtAction;
use std::collections::BTreeMap;

/// How a picker verb started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Started {
    /// Running in its own window in the repo's session.
    Window,
    /// tmux could not host it, so it runs detached with no window to watch.
    Background,
    /// Its window already holds a live run; nothing new was started.
    AlreadyRunning,
}

impl Started {
    /// The tail of the picker's `✓ <verb> — …` line.
    pub(crate) fn describe(self, repo: &str) -> String {
        match self {
            Started::Window => format!("started in a tmux window in the {repo} session"),
            Started::Background => "started in the background (no tmux window)".to_string(),
            Started::AlreadyRunning => "already running".to_string(),
        }
    }
}

/// Start one picker verb for `repo`, optionally against `branch`, right now.
///
/// A verb whose job wants a branch and was given none runs against `main`, because `bn run --repo`
/// refuses a repo without a branch and a repo script resolves through the pair.
pub(crate) fn start_action(
    cfg: &Config,
    action: &WtAction,
    repo: &str,
    branch: Option<&str>,
) -> Result<Started, String> {
    let branch = branch.or(action.needs_branch.then_some("main"));
    let window = window_name(action, branch);
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve the bn executable: {e}"))?
        .to_string_lossy()
        .into_owned();
    let mut command = format!("{} run {} --repo {}", shq(&exe), shq(action.job), shq(repo));
    if let Some(branch) = branch {
        command.push_str(&format!(" --branch {}", shq(branch)));
    }
    match open_window(cfg, repo, &window, &command) {
        WindowOpen::Opened => return Ok(Started::Window),
        WindowOpen::Live => return Ok(Started::AlreadyRunning),
        WindowOpen::Unavailable => {}
    }
    let mut args = vec!["run", action.job, "--repo", repo];
    if let Some(branch) = branch {
        args.extend(["--branch", branch]);
    }
    bn_core::bn_subprocess::spawn_detached(&args)
        .map(|_| Started::Background)
        .map_err(|e| format!("could not start {}: {e}", action.label))
}

/// The window a verb runs in: the job's name, or `<job>-<branch>` for a verb aimed at one branch,
/// so two branches' runs of the same job do not read as one.
pub(crate) fn window_name(action: &WtAction, branch: Option<&str>) -> String {
    match branch.filter(|_| action.scope != bn_core::wt_actions::Scope::Repo) {
        Some(branch) => crate::wt::sanitize_branch(&format!("{}-{branch}", action.job)),
        None => crate::wt::sanitize_branch(action.job),
    }
}

/// What one job window in a session says right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowState {
    /// A pane is still running.
    Live,
    /// Every pane has exited: the last run failed and its window was kept. Carries the exit status
    /// when tmux recorded one.
    Dead(Option<i32>),
}

/// Every window in `session` by name, with whether its run is live or dead. One `tmux` call, so the
/// picker can afford it on each repaint. Empty when the session does not exist.
pub(crate) fn window_states(session: &str) -> BTreeMap<String, WindowState> {
    crate::proc::tmux_raw(&[
        "list-panes",
        "-s",
        "-t",
        &format!("={session}"),
        "-F",
        "#{window_id}\t#{window_name}\t#{pane_dead}\t#{pane_dead_status}",
    ])
    .map(|panes| classify_windows(&panes))
    .unwrap_or_default()
}

/// The pure half of [`window_states`]: a window is live while any of its panes is.
fn classify_windows(panes: &str) -> BTreeMap<String, WindowState> {
    let mut out: BTreeMap<String, WindowState> = BTreeMap::new();
    for line in panes.lines() {
        let mut f = line.split('\t');
        let (Some(_id), Some(name), Some(dead)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let state = if dead.trim() == "1" {
            WindowState::Dead(f.next().and_then(|s| s.trim().parse().ok()))
        } else {
            WindowState::Live
        };
        match out.get(name) {
            Some(WindowState::Live) => {}
            _ => {
                out.insert(name.to_string(), state);
            }
        }
    }
    out
}

/// What [`open_window`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowOpen {
    /// A new window is running the command.
    Opened,
    /// A same-named window still has a live pane; nothing was opened.
    Live,
    /// tmux could not host the window at all.
    Unavailable,
}

/// Open a detached window named `window` in `session` running `command`, creating the session
/// rooted at the repo's main checkout when it does not exist yet.
///
/// The window lands in the session of the repository the work serves, and nowhere else — a job may
/// open a window in a repo's session, never a session of its own.
pub(crate) fn open_window(cfg: &Config, session: &str, window: &str, command: &str) -> WindowOpen {
    let (_, worktree_dir) = crate::wt::layout(cfg);
    let home = crate::wt::respawn_fallback(&worktree_dir, session);
    let home = home.to_string_lossy().to_string();
    let target = format!("={session}");
    let has = crate::proc::tmux_run(&["has-session", "-t", &target]);
    if !has && !crate::proc::tmux_run(&["new-session", "-d", "-s", session, "-c", &home]) {
        return WindowOpen::Unavailable;
    }
    // `exec` so the command *is* the pane's process: its exit status becomes the pane's, which is
    // what `remain-on-exit failed` reads to decide whether to keep the pane.
    let command = format!("PATH={}; exec {}", shq(&crate::cmd::cron::job_path()), command);
    let argvs = match existing_window(session, window) {
        WindowSlot::Live => return WindowOpen::Live,
        WindowSlot::Vacant => window_argv(session, window, &home, &command),
        WindowSlot::Dead(id) => respawn_argv(&id, &home, &command),
    };
    for argv in argvs {
        if !crate::proc::tmux_run(&argv) {
            // The first command opens the window; a failure there means no window. The trailing
            // one only tunes it, so a tmux too old for `remain-on-exit failed` degrades to a
            // window that also closes on failure — worse observability, never a leak.
            if matches!(argv.first().map(String::as_str), Some("new-window" | "respawn-window")) {
                return WindowOpen::Unavailable;
            }
        }
    }
    WindowOpen::Opened
}

/// What `session` already holds under this job's window name.
///
/// A failed run leaves its pane behind to be read, and nothing sweeps those up on a schedule. The
/// next run reuses that window in place rather than killing it: a job window can be its session's
/// last, and killing the last window destroys the session. So debris stays bounded at one window
/// per job, and no session is ever closed.
fn existing_window(session: &str, window: &str) -> WindowSlot {
    let target = format!("={session}");
    crate::proc::tmux_raw(&[
        "list-panes",
        "-s",
        "-t",
        &target,
        "-F",
        "#{window_id}\t#{window_name}\t#{pane_dead}",
    ])
    .map(|panes| window_slot(&panes, window))
    .unwrap_or(WindowSlot::Vacant)
}

/// What a session already holds under a given window name.
#[derive(Debug, PartialEq, Eq)]
enum WindowSlot {
    /// No window by that name.
    Vacant,
    /// A window by that name still has a running pane.
    Live,
    /// A window by that name has only dead panes: the last failed run, reused for the next one.
    Dead(String),
}

/// Classify the named window from `list-panes -s` output. Pure.
fn window_slot(panes: &str, window: &str) -> WindowSlot {
    let mut id: Option<String> = None;
    for line in panes.lines() {
        let mut f = line.split('\t');
        let (Some(wid), Some(name), Some(dead)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if name != window {
            continue;
        }
        if dead.trim() != "1" {
            return WindowSlot::Live;
        }
        id.get_or_insert_with(|| wid.to_string());
    }
    id.map(WindowSlot::Dead).unwrap_or(WindowSlot::Vacant)
}

/// The tmux commands that open a job's window. Pure → unit-testable.
///
/// `-d` so a run never steals focus: these windows land in sessions a human is attached to. The
/// `=<session>:` target needs its trailing colon — the bare `=<session>` form parses as a pane
/// target and `new-window` rejects it (see `wt::window::window_tmux_cmds`).
///
/// `remain-on-exit failed` is what makes the window self-clearing: a successful run's window closes
/// with it, and only a failure leaves a dead pane holding its final output.
fn window_argv(session: &str, window: &str, home: &str, command: &str) -> Vec<Vec<String>> {
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    vec![
        argv(&[
            "new-window",
            "-d",
            "-t",
            &format!("={session}:"),
            "-c",
            home,
            "-n",
            window,
            command,
        ]),
        argv(&[
            "set-window-option",
            "-t",
            &crate::proc::exact_target(&format!("{session}:{window}")),
            "remain-on-exit",
            "failed",
        ]),
    ]
}

/// The tmux commands that rerun a job in its last failed run's window `id`. Pure.
///
/// `respawn-window` restarts the window's dead pane with the new command, so the window, and with
/// it the session, never closes. `-k` is not needed: every pane in it has already exited.
fn respawn_argv(id: &str, home: &str, command: &str) -> Vec<Vec<String>> {
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    vec![
        argv(&["respawn-window", "-t", id, "-c", home, command]),
        argv(&["set-window-option", "-t", id, "remain-on-exit", "failed"]),
    ]
}

/// Single-quote for `sh`.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_detached_rooted_and_self_clearing() {
        let cmds = window_argv("bn", "pool-warm", "/home/me/projects/worktree/bn/main", "true");
        let new_window = &cmds[0];
        assert_eq!(new_window[0], "new-window");
        // Never steal focus: these land in sessions a human is attached to.
        assert!(new_window.contains(&"-d".to_string()));
        // The trailing colon makes it a *session* target; the bare form is rejected as a pane.
        assert!(new_window.contains(&"=bn:".to_string()));
        assert!(new_window.contains(&"/home/me/projects/worktree/bn/main".to_string()));
        // `=bn:=pool-warm`, not `=bn:pool-warm`. The `=` anchors only the part it precedes, so the
        // WINDOW half would otherwise stay an fnmatch pattern and could land on `pool-warm-2`.
        assert_eq!(
            cmds[1],
            vec![
                "set-window-option",
                "-t",
                "=bn:=pool-warm",
                "remain-on-exit",
                "failed"
            ]
        );
    }

    #[test]
    fn a_session_holding_no_such_window_is_vacant() {
        let panes = "@1\tmain\t0\n@2\tother-job\t0\n";
        assert_eq!(window_slot(panes, "pool-warm"), WindowSlot::Vacant);
    }

    #[test]
    fn a_running_job_is_never_displaced_by_a_second_one() {
        // One dead pane beside a live one still means the job is running.
        let panes = "@7\tpool-warm\t1\n@7\tpool-warm\t0\n";
        assert_eq!(window_slot(panes, "pool-warm"), WindowSlot::Live);
    }

    #[test]
    fn a_failed_runs_window_is_respawned_in_place_never_killed() {
        let cmds = respawn_argv("@9", "/home/me/projects/worktree/bn/main", "true");
        assert_eq!(cmds[0][..3], ["respawn-window", "-t", "@9"]);
        assert!(cmds.iter().all(|argv| argv[0] != "kill-window"));
    }

    #[test]
    fn the_last_failed_run_is_found_by_its_window_id() {
        let panes = "@3\tmain\t0\n@9\tpool-warm\t1\n";
        assert_eq!(
            window_slot(panes, "pool-warm"),
            WindowSlot::Dead("@9".to_string())
        );
    }

    #[test]
    fn window_states_read_live_and_dead_runs_with_their_exit_status() {
        let panes = "@1\tmain\t0\t\n@2\tpool-warm\t0\t\n@3\tbuild-main\t1\t2\n@4\tfetch-prs\t1\t\n";
        let states = classify_windows(panes);
        assert_eq!(states.get("pool-warm"), Some(&WindowState::Live));
        assert_eq!(states.get("build-main"), Some(&WindowState::Dead(Some(2))));
        assert_eq!(states.get("fetch-prs"), Some(&WindowState::Dead(None)));
        assert_eq!(states.get("wt-cleanup"), None);
    }

    #[test]
    fn a_window_with_any_live_pane_reads_live_whatever_the_order() {
        for panes in ["@7\tpool-warm\t1\t1\n@7\tpool-warm\t0\t\n", "@7\tpool-warm\t0\t\n@7\tpool-warm\t1\t1\n"] {
            assert_eq!(
                classify_windows(panes).get("pool-warm"),
                Some(&WindowState::Live),
                "{panes:?}"
            );
        }
    }

    #[test]
    fn a_repo_verb_runs_in_a_window_named_after_its_job() {
        let warm = bn_core::wt_actions::find("pool-warm").expect("pool-warm");
        assert_eq!(window_name(warm, None), "pool-warm");
        // A repo verb given `main` (a script row) still names its window after the job alone.
        assert_eq!(window_name(warm, Some("main")), "pool-warm");
        let verify = bn_core::wt_actions::find("wt-verify").expect("wt-verify");
        assert_eq!(window_name(verify, Some("feat/x")), "verify-feat-x");
    }
}
