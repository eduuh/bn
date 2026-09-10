use bn_core::queue::kind::ExecSpec;
use bn_core::queue::tick::Dispatch;
use bn_core::resolver::Config;

/// The repository whose session hosts work that names no repository of its own.
///
/// bn owns the cron manifest (`config/crons.toml` declares `repo = "bn"` for every entry), so its
/// own bookkeeping is bn's work and belongs in bn's session. This is the floor of the invariant,
/// not an escape hatch: it is a real repo with a real checkout, unlike the queue's `machine`
/// sentinel, which named nothing and could only ever produce a session named after nothing.
const BN_REPO: &str = "bn";

/// Start heavy work detached, with a wrapper that reports its own completion.
///
/// The tick cannot wait for it — that is the whole reason this shape exists — so the entry stays
/// leased and something has to close it. The same trick the worker path already uses: wrap the
/// command so that whatever its exit status, it calls back to resolve the entry. If the wrapper
/// never runs (machine died mid-build) the lease reaper picks it up on TTL, so the entry is
/// recoverable either way rather than leased forever.
pub(super) fn spawn(cfg: &Config, d: &Dispatch, cmd: &str) -> Result<(), String> {
    let wrapped = completion_wrapped(cmd, &d.repo, &d.id);
    if spawn_tmux(cfg, d, &wrapped) {
        return Ok(());
    }
    std::process::Command::new("/bin/sh")
        .arg("-lc")
        .arg(&wrapped)
        .env("PATH", crate::cmd::cron::job_path())
        .stdin(std::process::Stdio::null())
        // Detached output: heavy work is minutes of build noise, and the dispatcher's own log is
        // not where it belongs. `bn queue why` and the event log carry the outcome.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Start heavy work in a visible tmux window when tmux is available.
///
/// Heavy queue jobs are the background work that makes the box look busy for minutes: pool
/// warming, cache warming, release builds. They still run as native jobs/shells and still resolve
/// themselves with `queue finish`; the tmux window is the live view the operator can attach to.
///
/// The window lands in the session of the repository the job serves, and nowhere else — a cron may
/// open a window in a repo's session, never a session of its own. Returns false when tmux cannot
/// host it, so the caller falls back to the headless path.
fn spawn_tmux(cfg: &Config, d: &Dispatch, wrapped: &str) -> bool {
    let (session, window) = tmux_target(&d.exec, &d.repo, &d.id);
    let home = crate::wt::respawn_fallback(&cfg.worktree_dir, &session);
    let home = home.to_string_lossy().to_string();
    let target = format!("={session}");
    let has = crate::proc::tmux_run(&["has-session", "-t", &target]);
    if !has && !crate::proc::tmux_run(&["new-session", "-d", "-s", &session, "-c", &home]) {
        return false;
    }
    if !claim_window(&session, &window) {
        return false;
    }
    // `exec` so the wrapper *is* the pane's process: its exit status becomes the pane's, which is
    // what `remain-on-exit failed` reads to decide whether to keep the pane.
    let command = format!("PATH={}; exec {}", shq(&crate::cmd::cron::job_path()), wrapped);
    for argv in window_argv(&session, &window, &home, &command) {
        if !crate::proc::tmux_run(&argv) {
            // The first command opens the window; a failure there means no window. The trailing
            // one only tunes it, so a tmux too old for `remain-on-exit failed` degrades to a
            // window that also closes on failure — worse observability, never a leak.
            if argv.first().map(String::as_str) == Some("new-window") {
                return false;
            }
        }
    }
    true
}

/// Make room for this job's window, or decline the tmux path entirely.
///
/// A failed run leaves its pane behind to be read (`remain-on-exit failed`), and nothing sweeps
/// those up on a schedule — `config/crons.toml` keeps window/worktree reclamation deliberately
/// manual. So each run clears its *own* corpse and only its own: a same-named window whose panes
/// have all exited is this job's last failure and goes; a same-named window with a live pane is
/// this job still running, and we decline rather than open a second one. That bounds the debris at
/// one stale window per job, which is what stops a repo's session filling up the way `machine`
/// did.
///
/// This kills no session, no worktree and no branch — only a window this same function opened and
/// that has already exited.
fn claim_window(session: &str, window: &str) -> bool {
    let target = format!("={session}");
    let Some(panes) = crate::proc::tmux_raw(&[
        "list-panes",
        "-s",
        "-t",
        &target,
        "-F",
        "#{window_id}\t#{window_name}\t#{pane_dead}",
    ]) else {
        return true; // nothing listed — no corpse to clear
    };
    match window_slot(&panes, window) {
        WindowSlot::Vacant => true,
        WindowSlot::Live => false,
        WindowSlot::Dead(id) => crate::proc::tmux_run(&["kill-window", "-t", &id]),
    }
}

/// What a session already holds under a given window name.
#[derive(Debug, PartialEq, Eq)]
enum WindowSlot {
    /// No window by that name.
    Vacant,
    /// A window by that name still has a running pane.
    Live,
    /// A window by that name has only dead panes — the last run's corpse, safe to clear.
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

/// The tmux commands that open a heavy job's window. Pure → unit-testable.
///
/// `-d` because a cron must never steal focus: these windows now land in sessions a human is
/// attached to, so selecting one would yank them mid-keystroke. The `=<session>:` target needs its
/// trailing colon — the bare `=<session>` form parses as a pane target and `new-window` rejects it
/// (the same footgun documented in `wt::window::window_tmux_cmds`).
///
/// `remain-on-exit failed` is what makes the window self-clearing: a successful run's window closes
/// with it, and only a failure leaves a dead pane holding its final output. `on` would park every
/// success, which is precisely the bug that piled 14 windows into the `machine` session.
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

/// Which session hosts a heavy job's window, and what the window is called. Pure.
///
/// The rule is one sentence: the session is the repository the work serves.
///
/// Legacy backlog rows may still name the retired `machine` sentinel until compaction rewrites
/// them; those are bn's own bookkeeping too, so the session still falls back to `bn`.
fn tmux_target(exec: &ExecSpec, entry_repo: &str, id: &str) -> (String, String) {
    let session_for = |repo: &str| {
        if repo.is_empty() || repo.eq_ignore_ascii_case("machine") {
            BN_REPO.to_string()
        } else {
            repo.to_string()
        }
    };
    match exec {
        ExecSpec::Job { job, repo, .. } => (
            session_for(repo.as_deref().unwrap_or_default()),
            crate::wt::sanitize_branch(job),
        ),
        // A shell or agent entry has no job repo of its own, so the entry's own repo answers —
        // unless that is the sentinel, which answers for nothing.
        _ => (
            session_for(entry_repo),
            format!("queue-{}", crate::wt::sanitize_branch(id)),
        ),
    }
}

/// Wrap `cmd` so it resolves its own queue entry on exit, whatever that exit is.
fn completion_wrapped(cmd: &str, repo: &str, id: &str) -> String {
    let script = r#"sh -lc "$1"
code=$?
"$4" queue finish "$2" "$3" --exit-code "$code"
exit "$code""#;
    ["sh", "-c", script, "bn-queue-heavy", cmd, repo, id]
        .into_iter()
        .map(shq)
        .chain(std::iter::once(shq(&exe_str())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The absolute path of the running bn, for a wrapper that will outlive this process.
fn exe_str() -> String {
    std::env::current_exe()
        .unwrap_or_else(|_| "bn".into())
        .to_string_lossy()
        .into_owned()
}

pub(super) fn job_command(job: &str, repo: Option<&str>, branch: Option<&str>) -> String {
    let mut command = format!("{} run {} --now", shq(&exe_str()), shq(job));
    if let Some(repo) = repo {
        command.push_str(&format!(" --repo {}", shq(repo)));
    }
    if let Some(branch) = branch {
        command.push_str(&format!(" --branch {}", shq(branch)));
    }
    command
}

/// Single-quote for `sh`.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str, repo: Option<&str>) -> ExecSpec {
        ExecSpec::Job {
            job: name.to_string(),
            repo: repo.map(str::to_string),
            branch: None,
        }
    }

    fn session_of(exec: &ExecSpec, entry_repo: &str) -> String {
        tmux_target(exec, entry_repo, "abc123").0
    }

    #[test]
    fn a_job_lands_in_the_session_of_the_repo_it_serves() {
        assert_eq!(
            tmux_target(&job("pool-warm", Some("arena")), "arena", "id"),
            ("arena".to_string(), "pool-warm".to_string())
        );
        assert_eq!(
            tmux_target(&job("keep-warm", Some("BigMono")), "BigMono", "id"),
            ("BigMono".to_string(), "keep-warm".to_string())
        );
    }

    #[test]
    fn a_job_that_names_no_repo_is_bns_own_bookkeeping() {
        for name in ["self-update", "pool-warm", "refresh-all"] {
            assert_eq!(tmux_target(&job(name, None), "", "id").0, BN_REPO);
        }
    }

    /// The hole the `Job` arm alone did not close: `bn queue add --shell … --repo machine` used the
    /// entry's repo verbatim, so the sentinel became a session name through the other arm.
    #[test]
    fn the_sentinel_never_becomes_a_session_whatever_the_exec() {
        let shell = ExecSpec::Shell {
            cmd: "true".to_string(),
        };
        assert_eq!(session_of(&shell, "machine"), BN_REPO);
        assert_eq!(session_of(&ExecSpec::Agent, "machine"), BN_REPO);
        // A real repo still answers for itself.
        assert_eq!(session_of(&shell, "arena"), "arena");
    }

    /// The invariant, asserted rather than assumed: no input produces a session named after the
    /// queue's sentinel, and none produces an empty one.
    #[test]
    fn no_input_yields_a_session_that_is_not_a_repo() {
        let execs = [
            job("pool-warm", None),
            job("pool-warm", Some("arena")),
            job("self-update", None),
            ExecSpec::Shell {
                cmd: "true".to_string(),
            },
            ExecSpec::Agent,
        ];
        for exec in &execs {
            for entry_repo in ["machine", "arena", ""] {
                let (session, window) = tmux_target(exec, entry_repo, "id");
                assert_ne!(session, "machine", "{exec:?} / {entry_repo}");
                assert!(!session.is_empty(), "{exec:?} / {entry_repo}");
                assert!(!window.is_empty(), "{exec:?} / {entry_repo}");
            }
        }
    }

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
        // old expectation left the WINDOW half as an fnmatch pattern — with a `pool-warm-2` open,
        // this `remain-on-exit` write would have landed on it. Verified against tmux 3.6:
        // `set-window-option -t '=s:feat'` wrote onto a window named `feat-two`.
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

    /// The regression this whole change exists for: the command used to end in `read _`, so every
    /// run parked its window forever. Fourteen of them accumulated in one session.
    #[test]
    fn the_command_never_waits_for_a_keypress() {
        let command = format!("PATH='/bin'; exec {}", completion_wrapped("true", "bn", "id"));
        let cmds = window_argv("bn", "pool-warm", "/tmp", &command);
        let sent = cmds[0].last().expect("command argument");
        assert!(!sent.contains("read"), "{sent}");
    }

    #[test]
    fn a_session_holding_no_such_window_is_vacant() {
        let panes = "@1\tmain\t0\n@2\tother-job\t0\n";
        assert_eq!(window_slot(panes, "pool-warm"), WindowSlot::Vacant);
    }

    #[test]
    fn a_running_job_is_never_displaced_by_a_second_one() {
        // One dead pane beside a live one still means the job is running: split panes, or a pane
        // that died while its sibling builds on.
        let panes = "@7\tpool-warm\t1\n@7\tpool-warm\t0\n";
        assert_eq!(window_slot(panes, "pool-warm"), WindowSlot::Live);
    }

    #[test]
    fn the_last_runs_corpse_is_the_one_thing_we_clear() {
        let panes = "@3\tmain\t0\n@9\tpool-warm\t1\n";
        assert_eq!(
            window_slot(panes, "pool-warm"),
            WindowSlot::Dead("@9".to_string())
        );
    }
}
