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
