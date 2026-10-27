//! `bn agent launch --host <machine>`: an agent that outlives the connection that started it.
//!
//! The agent used to run *inside* the ssh session (`ssh -tt host "cd <wt> && … launch"`), so a
//! tunnel drop, the laptop sleeping or the terminal closing killed it mid-turn. Now one short ssh
//! call starts it in a detached tmux window on that machine (session `<repo>`, window named after
//! the branch) and returns. This terminal then attaches to the window over a connection of its own
//! and attaches again whenever that connection drops. Detaching, or the agent exiting, ends the
//! view; nothing here ends the agent. Launching the same branch again attaches to the agent that
//! is already running instead of starting a second one.

use std::process::{Command, Stdio};

/// What the remote side did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Started {
    Started,
    Running,
}

/// Exit code of the remote script when the branch has no checkout there and none could be made.
const MISSING: i32 = 3;

/// Shell lines, for a script run on the remote machine, that set `$wt` to the branch's checkout
/// there, making one with `bn wt add` when the branch is pushed but not checked out yet; without
/// one they print MISSING and exit [`MISSING`]. They expect `$root`, `$repo` and `$branch`, and bn
/// on PATH.
pub(crate) fn checkout_snippet() -> String {
    format!(
        r#"find_wt() {{ git -C "$root/$repo/main" worktree list --porcelain 2>/dev/null | awk -v b="refs/heads/$branch" '/^worktree /{{w = substr($0, 10)}} $0 == "branch " b {{print w; exit}}'; }}
wt=$(find_wt)
[ -n "$wt" ] || {{ [ -d "$root/$repo/$branch" ] && wt="$root/$repo/$branch"; }}
[ -n "$wt" ] || {{ (cd "$root/$repo/main" && bn wt add "$branch" </dev/null >/dev/null 2>&1); wt=$(find_wt); }}
[ -n "$wt" ] || {{ echo MISSING; exit {MISSING}; }}"#
    )
}

/// The script run on the remote machine. It finds the branch's checkout, making one with
/// `bn wt add` when the branch is pushed but not checked out there, then:
/// - no window for the branch: starts `launch` in a new detached one;
/// - the branch's window is an idle shell (`bn wt add` opens one): types `launch` into it, so the
///   shell is back when the agent exits, as with a local window;
/// - the window is already running something (a launched agent, or a program in that shell):
///   leaves it alone.
///
/// Prints STARTED or RUNNING, or exits [`MISSING`]. A launch that fails in a new window keeps it
/// for ten minutes, so the error can be read.
pub(crate) fn remote_start_script(
    root: &str,
    repo: &str,
    branch: &str,
    window: &str,
    launch: &[String],
) -> String {
    let q = crate::wt::shq;
    let launch = launch.iter().map(|a| q(a)).collect::<Vec<_>>().join(" ");
    let cmd = format!(
        "{launch}; rc=$?; [ $rc -eq 0 ] || {{ echo \"bn agent launch exited with $rc\"; sleep 600; }}"
    );
    format!(
        r#"{path}
root={root}; repo={repo}; branch={branch}; win={win}
{checkout}
t="=$repo:=$win"
if tmux list-windows -t "=$repo" -F '#{{window_name}}' 2>/dev/null | grep -qxF "$win"; then
  pid=$(tmux display-message -p -t "$t" '#{{pane_pid}}')
  if [ -n "$(tmux display-message -p -t "$t" '#{{pane_start_command}}')" ] || pgrep -P "$pid" >/dev/null 2>&1; then
    echo RUNNING; exit 0
  fi
  tmux send-keys -t "$t" -l "cd '$wt' && "{launch_q} && tmux send-keys -t "$t" Enter && echo STARTED
  exit
fi
cmd={cmd}
if tmux has-session -t "=$repo" 2>/dev/null; then
  tmux new-window -d -t "=$repo:" -n "$win" -c "$wt" "$cmd"
else
  tmux new-session -d -s "$repo" -n "$win" -c "$wt" "$cmd"
fi && echo STARTED"#,
        root = q(root),
        repo = q(repo),
        branch = q(branch),
        win = q(window),
        path = super::mesh::REMOTE_PATH,
        checkout = checkout_snippet(),
        launch_q = q(&launch),
        cmd = q(&cmd),
    )
}

/// The local view: attach to the remote window, and attach again whenever that ends in anything
/// but a normal detach, as long as the window is still there. A drop is not always ssh's 255: a
/// killed or hung connection ends with other codes, so the window itself is the test. A second,
/// short ssh asks for it; when that cannot connect either, the loop keeps waiting.
pub(crate) fn attach_loop(host: &str, target: &str) -> String {
    let q = crate::wt::shq;
    let attach = q(&format!("tmux attach -t {}", q(target)));
    let exists = q(&format!("tmux has-session -t {}", q(target)));
    format!(
        "while :; do ssh -tt -o ControlPath=none -o ServerAliveInterval=15 -o ServerAliveCountMax=4 {h} {attach} && exit 0; \
         ssh -o BatchMode=yes -o ConnectTimeout=10 {h} {exists} 2>/dev/null; \
         [ $? -eq 1 ] && {{ echo \"bn: the agent's window on {hs} is gone.\"; exit 0; }}; \
         echo \"bn: lost the connection to {hs}; the agent is still running there. Attaching again in 5s (Ctrl-C to stop).\"; \
         sleep 5; done",
        h = q(host),
        hs = host.replace('"', ""),
    )
}

/// Read what the start script printed.
pub(crate) fn parse_start(code: Option<i32>, stdout: &str) -> Result<Started, String> {
    match (code, stdout.lines().last().unwrap_or("").trim()) {
        (Some(0), "STARTED") => Ok(Started::Started),
        (Some(0), "RUNNING") => Ok(Started::Running),
        (Some(MISSING), _) => Err("the branch has no checkout there, and none could be made".to_string()),
        (Some(255), _) => Err("could not reach it over ssh".to_string()),
        (c, out) => Err(format!("the remote start failed (exit {c:?}{})", if out.is_empty() { String::new() } else { format!(": {out}") })),
    }
}

/// Start the agent on `host`, or find it already running there; exits with the reason when neither.
pub(crate) fn start(cfg: &crate::Config, host: &str, repo: &str, branch: &str, launch: &[String]) -> Started {
    if !super::safe_ssh_host(host) {
        eprintln!("bn agent launch: '{host}' is not an ssh host alias");
        std::process::exit(2);
    }
    // Every machine keeps its worktrees in the same place unless `[hosts.<host>] repo_root` says so.
    let root = cfg
        .hosts
        .get(host)
        .and_then(|e| e.repo_root.clone())
        .unwrap_or_else(|| cfg.worktree_dir.to_string_lossy().into_owned());
    let window = bn_core::orchestrate::tmux_window_name(branch);
    let script = remote_start_script(&root, repo, branch, &window, launch);
    // Under sh whatever the login shell is: zsh reads `=word` as a path lookup, among other things.
    let remote = format!("sh -c {}", crate::wt::shq(&script));
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
        .args(super::SSH_SHARED_CONNECTION)
        .args([host, remote.as_str()])
        .stdin(Stdio::null())
        .output();
    let (code, stdout, stderr) = match out {
        Ok(o) => (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (None, String::new(), e.to_string()),
    };
    match parse_start(code, &stdout) {
        Ok(Started::Started) => {
            eprintln!("bn agent launch: started on {host} in tmux {repo}:{window}; it keeps running if this connection drops");
            Started::Started
        }
        Ok(Started::Running) => {
            eprintln!("bn agent launch: already running on {host} in tmux {repo}:{window}; attaching");
            Started::Running
        }
        Err(why) => {
            eprintln!("bn agent launch: {host}: {why}");
            if code == Some(MISSING) {
                eprintln!("  bn wt add {branch} failed there; push the branch first: git push -u origin {branch}");
            } else if !stderr.trim().is_empty() {
                eprintln!("  {}", stderr.trim());
            }
            std::process::exit(1);
        }
    }
}

/// Replace this process with the attach view of the agent's window on `host`.
pub(crate) fn attach(host: &str, repo: &str, branch: &str) -> ! {
    let target = format!("={repo}:={}", bn_core::orchestrate::tmux_window_name(branch));
    super::exec_harness("sh", &["-c".to_string(), attach_loop(host, &target)])
}

/// The pane on `host` that a window target should type into: its agent pane when the window also
/// holds an editor or a shell (see `bn_core::agent::pick_agent_pane`), else the window itself.
pub(crate) fn agent_pane(host: &str, window: &str) -> String {
    let fmt = "#{pane_id}\t#{pane_current_command}\t#{pane_active}";
    std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .args(super::SSH_SHARED_CONNECTION)
        .arg(host)
        .arg(format!(
            "{} tmux list-panes -t {} -F {}",
            super::mesh::REMOTE_PATH,
            crate::wt::shq(window),
            crate::wt::shq(fmt)
        ))
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .filter(|rows| rows.lines().count() > 1)
        .and_then(|rows| bn_core::agent::pick_agent_pane(&rows))
        .unwrap_or_else(|| window.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_script_quotes_the_launch_and_finds_the_checkout() {
        let s = remote_start_script(
            "/home/u/projects/worktree",
            "bn",
            "feat/x",
            "feat-x",
            &["bn".into(), "agent".into(), "launch".into(), "claude".into(), "--seed".into(), "it's $HOME".into()],
        );
        assert!(s.contains("root='/home/u/projects/worktree'; repo='bn'; branch='feat/x'; win='feat-x'"));
        assert!(s.contains("worktree list --porcelain"));
        assert!(s.contains("echo RUNNING; exit 0"));
        // The launch is quoted twice: as its own words, then as the whole tmux command. Evaluating
        // the `cmd=` line in a real shell must give back the words intact, quote and `$` included.
        let line = s.lines().find(|l| l.starts_with("cmd=")).expect("a cmd= line");
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("{line}; printf '%s' \"$cmd\"")])
            .output()
            .expect("sh runs");
        let cmd = String::from_utf8_lossy(&out.stdout);
        assert!(cmd.starts_with("'bn' 'agent' 'launch' 'claude' '--seed' 'it'\\''s $HOME';"), "{cmd}");
    }

    #[test]
    fn the_attach_loop_ends_on_detach_and_retries_while_the_window_lives() {
        let l = attach_loop("CPC-a", "=bn:=feat-x");
        assert!(l.contains("ssh -tt -o ControlPath=none"));
        assert!(l.contains("'CPC-a'"));
        assert!(l.contains("tmux attach -t"));
        assert!(l.contains("&& exit 0;"), "a normal detach ends the view");
        assert!(l.contains("tmux has-session -t"), "anything else checks the window before giving up");
    }

    #[test]
    fn only_plain_aliases_reach_ssh() {
        assert!(crate::cmd::agent::safe_ssh_host("cloud-pc-0R6MD"));
        assert!(crate::cmd::agent::safe_ssh_host("edd@box.lan"));
        assert!(!crate::cmd::agent::safe_ssh_host("-oProxyCommand=touch /tmp/x"));
        assert!(!crate::cmd::agent::safe_ssh_host("host;rm"));
        assert!(!crate::cmd::agent::safe_ssh_host(""));
    }

    #[test]
    fn the_start_outcome_is_read_from_the_last_line() {
        assert_eq!(parse_start(Some(0), "motd\nSTARTED\n"), Ok(Started::Started));
        assert_eq!(parse_start(Some(0), "RUNNING"), Ok(Started::Running));
        assert!(parse_start(Some(3), "MISSING").unwrap_err().contains("no checkout"));
        assert!(parse_start(Some(255), "").unwrap_err().contains("ssh"));
        assert!(parse_start(Some(1), "boom").unwrap_err().contains("boom"));
    }
}
