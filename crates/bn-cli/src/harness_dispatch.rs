//! Launching one agent harness as a child process and capturing what it printed.
//!
//! [`harness_command`] builds the invocation for a named harness (`claude` / `copilot`) with a
//! prompt, an optional model, and an optional session to resume. [`dispatch_capture`] runs it,
//! merges stdout and stderr, and enforces an optional wall-clock bound — a wedged harness must not
//! wedge the caller.
//!
//! Both lived in `cmd/staff.rs` while the staff roster was their only caller. The roster is gone;
//! the plan-review gate, the task workflow and `bn agent run` still launch harnesses, so this is
//! not staff-specific machinery.

pub(crate) fn harness_command(
    harness: &str,
    prompt: &str,
    model: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::process::Command> {
    let mut c = match harness {
        "copilot" => {
            let mut c = std::process::Command::new("copilot");
            c.args(["-p", prompt]);
            c.args(bn_core::mcp::Harness::Copilot.autonomy_args());
            c
        }
        "claude" => {
            let mut c = std::process::Command::new("claude");
            c.args(["-p", prompt]);
            c.args(bn_core::mcp::Harness::Claude.autonomy_args());
            c
        }
        _ => return None,
    };
    // Only pass `--model` when the configured id is one this harness accepts; an id it rejects
    // (e.g. bn's dotted claude-opus-4.8 handed to Claude Code) would park the agent on "model does
    // not exist", so omit it and let the harness use its own default (bn#963).
    if let Some(m) =
        model.and_then(|m| bn_core::mcp::Harness::parse(harness).and_then(|h| h.model_arg(m)))
    {
        c.args(["--model", m]);
    }
    if harness_supports_session_id(harness) {
        if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
            c.args(["--session-id", sid]);
        }
    }
    Some(c)
}

pub(crate) fn dispatch_capture(
    mut cmd: std::process::Command,
    wall: Option<std::time::Duration>,
) -> (bool, String, String) {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", crate::cmd::cron::job_path());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, String::new(), format!("spawn failed: {e}")),
    };
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_h = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = wall.map(|w| Instant::now() + w);
    let (success, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(st)) => break (st.success(), false),
            Ok(None) => {
                if deadline.is_some_and(|dl| Instant::now() >= dl) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => break (false, false),
        }
    };
    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    let combined = format!("{stderr}{stdout}");
    if timed_out {
        return (
            false,
            stdout,
            format!("dispatch exceeded wall budget\n{combined}"),
        );
    }
    (success, stdout, combined)
}

/// Only Claude accepts `--session-id`; Copilot rejects the flag outright, so passing it there
/// turns a working dispatch into a usage error.
fn harness_supports_session_id(harness: &str) -> bool {
    harness == "claude"
}

/// Where a visible agent run lands: the repo's tmux session, and the window's name in it.
pub(crate) struct AgentWindow {
    pub session: String,
    pub window: String,
}

impl From<(&str, String)> for AgentWindow {
    fn from((session, window): (&str, String)) -> Self {
        AgentWindow {
            session: session.to_string(),
            window,
        }
    }
}

impl AgentWindow {
    pub(crate) fn new(session: &str, window: &str) -> Self {
        AgentWindow {
            session: session.to_string(),
            window: window.to_string(),
        }
    }
}

/// [`dispatch_capture`], run in a detached window of the repo's tmux session so the agent can be
/// watched and jumped to while it works, instead of as a hidden child of a cron.
///
/// The window closes when the run succeeds and keeps its dead pane when it fails, like a picker
/// run (`cmd/wt_action_run.rs`). Output is teed to files so callers still get stdout and the
/// combined text back. With no reachable tmux server (a bare box, CI) it runs headless as before.
pub(crate) fn dispatch_visible(
    cmd: std::process::Command,
    wall: Option<std::time::Duration>,
    at: AgentWindow,
) -> (bool, String, String) {
    use crate::proc::{exact_target, tmux_raw, tmux_run};
    use std::time::{Duration, Instant};
    if tmux_raw(&["list-sessions", "-F", "#{session_name}"]).is_none() {
        return dispatch_capture(cmd, wall);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("bn-agent-{}-{stamp}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return dispatch_capture(cmd, wall);
    }
    let (out, err, rc) = (dir.join("out"), dir.join("err"), dir.join("rc"));
    let cwd = cmd
        .get_current_dir()
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(Into::into))
        .unwrap_or_else(|| "/".into());
    let script = visible_script(&cmd, &out, &err, &rc);
    let cwd_s = cwd.to_string_lossy();
    if !tmux_run(&["has-session", "-t", &format!("={}", at.session)]) {
        let _ = tmux_run(&["new-session", "-d", "-s", &at.session, "-c", &cwd_s]);
    }
    let shell = format!("bash -c {}", crate::wt::shell_single_quote(&script));
    let target = format!("={}:", at.session);
    if !tmux_run(&[
        "new-window",
        "-d",
        "-t",
        &target,
        "-c",
        &cwd_s,
        "-n",
        &at.window,
        &shell,
    ]) {
        let _ = std::fs::remove_dir_all(&dir);
        return dispatch_capture(cmd, wall);
    }
    let win = exact_target(&format!("{}:{}", at.session, at.window));
    let _ = tmux_run(&["set-window-option", "-t", &win, "remain-on-exit", "failed"]);
    let _ = tmux_run(&["set-window-option", "-t", &win, "@bn_agent", "working"]);

    let deadline = wall.map(|w| Instant::now() + w);
    let finished = loop {
        if rc.is_file() {
            break true;
        }
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            let _ = tmux_run(&["kill-window", "-t", &win]);
            break false;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let (stdout, stderr) = (read(&out), read(&err));
    let code = read(&rc).trim().parse::<i32>().unwrap_or(1);
    let _ = std::fs::remove_dir_all(&dir);
    let combined = format!("{stderr}{stdout}");
    if !finished {
        return (
            false,
            stdout,
            format!("dispatch exceeded wall budget\n{combined}"),
        );
    }
    (code == 0, stdout, combined)
}

/// The bash run inside the window: the harness with its env and argv, stdin closed as in
/// [`dispatch_capture`], both streams shown in the pane and teed to files, then the exit code
/// written last (renamed into place) so the waiter never reads a half-written result.
fn visible_script(
    cmd: &std::process::Command,
    out: &std::path::Path,
    err: &std::path::Path,
    rc: &std::path::Path,
) -> String {
    let q = |s: &str| crate::wt::shell_single_quote(s);
    let mut line = String::from("env");
    line.push_str(&format!(" PATH={}", q(&crate::cmd::cron::job_path())));
    for (k, v) in cmd.get_envs() {
        if let Some(v) = v {
            line.push_str(&format!(
                " {}={}",
                k.to_string_lossy(),
                q(&v.to_string_lossy())
            ));
        }
    }
    line.push(' ');
    line.push_str(&q(&cmd.get_program().to_string_lossy()));
    for a in cmd.get_args() {
        line.push(' ');
        line.push_str(&q(&a.to_string_lossy()));
    }
    let (out, err, rc) = (
        q(&out.to_string_lossy()),
        q(&err.to_string_lossy()),
        rc.to_string_lossy().to_string(),
    );
    format!(
        "{line} </dev/null 2> >(tee {err} >&2) | tee {out}\n\
         code=${{PIPESTATUS[0]}}\n\
         sleep 0.3\n\
         echo \"$code\" > {tmp} && mv {tmp} {rc}\n\
         exit \"$code\"\n",
        tmp = q(&format!("{rc}.tmp")),
        rc = q(&rc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::test_support::EnvGuard;

    /// A private tmux server for the test: its own short socket dir (macOS caps socket paths),
    /// `TMUX` cleared so nothing can reach the developer's server, stopped by socket path on drop.
    struct PrivateTmux {
        dir: std::path::PathBuf,
        _env: [EnvGuard; 3],
    }

    impl PrivateTmux {
        fn start() -> Option<Self> {
            let dir = std::path::PathBuf::from(format!("/tmp/bn-hd-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok()?;
            let env = [
                EnvGuard::remove("TMUX"),
                EnvGuard::remove("TMUX_PANE"),
                EnvGuard::set("TMUX_TMPDIR", &dir),
            ];
            let guard = PrivateTmux { dir, _env: env };
            crate::proc::tmux_run(&["new-session", "-d", "-s", "host"]).then_some(guard)
        }
    }

    impl Drop for PrivateTmux {
        fn drop(&mut self) {
            // Named by socket path, so this can only ever stop the test's own server.
            let uid = std::process::Command::new("id").arg("-u").output();
            let uid = uid.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let socket = self.dir.join(format!("tmux-{}", uid.unwrap_or_default()));
            let socket = socket.join("default").to_string_lossy().to_string();
            let _ = crate::proc::tmux_run(&["-S", &socket, "kill-server"]);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", script]);
        c
    }

    fn pane_dead(target: &str) -> Option<String> {
        crate::proc::tmux_raw(&["list-panes", "-t", target, "-F", "#{pane_dead}"])
    }

    #[test]
    fn a_visible_run_returns_its_output_and_leaves_only_a_failed_window() {
        let _env = bn_core::test_support::env_lock();
        let Some(_tmux) = PrivateTmux::start() else {
            return; // no tmux on this box
        };
        let wall = Some(std::time::Duration::from_secs(20));

        let failing = AgentWindow::new("repo", "failing");
        let (ok, stdout, combined) =
            dispatch_visible(sh("echo to-out; echo to-err >&2; exit 3"), wall, failing);
        assert!(!ok);
        assert!(stdout.contains("to-out"), "{stdout:?}");
        assert!(combined.contains("to-err"), "{combined:?}");
        assert_eq!(
            pane_dead("=repo:failing").as_deref().map(str::trim),
            Some("1")
        );

        let (ok, stdout, _) =
            dispatch_visible(sh("echo fine"), wall, AgentWindow::new("repo", "passing"));
        assert!(ok);
        assert!(stdout.contains("fine"));
        assert!(
            pane_dead("=repo:passing").is_none(),
            "a successful run's window closes"
        );
    }
}
