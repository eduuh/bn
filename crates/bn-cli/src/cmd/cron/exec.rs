//! How a job's process is actually launched: the shell, the PATH it inherits, and the output tail
//! the `cron_run` ledger keeps.
//!
//! Split out of `cron.rs` so the resolver (which job does this name mean?) and the executor (how
//! does a job become a process?) are separately readable — they fail in very different ways, and
//! [`job_path`] in particular is the whole reason a headless cron can call `bn` at all.
use crate::*;

/// How many trailing output lines the `cron_run` ledger keeps.
const TAIL_LINES: usize = 15;

/// Run `command` to completion, echoing its streams line by line as they arrive and keeping the
/// 15-line tail the event log records. A launch failure reports 127 — the shell's own "could not
/// run it" code — so a job that never started is not mistaken for one that ran and exited 0.
///
/// Streamed, not `output()`: that held the whole run in memory and echoed it only at exit, so a
/// job under systemd (`bn run pool-build-remote`, a BigMono warm of an hour or more) put nothing in the
/// journal past "Starting" until it finished.
fn capture_command(mut command: std::process::Command, launch_target: &str) -> (i32, String) {
    use std::process::Stdio;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("bn run: failed to launch {launch_target}: {e}");
            return (127, e.to_string());
        }
    };
    let tail = std::sync::Mutex::new(std::collections::VecDeque::with_capacity(TAIL_LINES));
    let (out, err) = (child.stdout.take(), child.stderr.take());
    std::thread::scope(|s| {
        let tail = &tail;
        if let Some(out) = out {
            s.spawn(move || relay_lines(out, std::io::stdout(), tail));
        }
        if let Some(err) = err {
            s.spawn(move || relay_lines(err, std::io::stderr(), tail));
        }
    });
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let tail = tail.into_inner().unwrap_or_else(|e| e.into_inner());
    (code, Vec::from(tail).join("\n"))
}

/// Copy `src` to `dst` a line at a time, flushing each one, and remember the last [`TAIL_LINES`]
/// in `tail` (shared by both streams, so it holds the lines in the order they arrived).
fn relay_lines(
    src: impl std::io::Read,
    mut dst: impl std::io::Write,
    tail: &std::sync::Mutex<std::collections::VecDeque<String>>,
) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(src);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let _ = dst.write_all(&buf);
                let _ = dst.flush();
                let line = String::from_utf8_lossy(&buf);
                let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                if tail.len() == TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line.trim_end_matches(['\n', '\r']).to_string());
            }
        }
    }
}

/// The PATH every job process gets; one definition, shared with `bn cmd` and the MCP server.
pub(crate) use bn_core::cmds::job_path;

/// A `<job>.sh` script from one of the job tiers.
fn run_script(script: &std::path::Path) -> (i32, String) {
    if cfg!(windows) {
        let error = "job scripts are unix-only (sh); not supported on native Windows";
        eprintln!("bn run: {error}");
        return (2, error.to_string());
    }
    let mut command = std::process::Command::new("/bin/sh");
    command.arg(script).env("PATH", job_path());
    capture_command(command, &script.display().to_string())
}

/// A repo's own `<job>.sh`, with what its header asked for done first.
///
/// The one directive today is `# bn: latest-main` ([`resolver::ScriptHeader`]): fetch and
/// fast-forward the repo's main checkout and main slot before the script starts. Honoured here,
/// in the runner, rather than by whichever surface started the script — a cron firing, `bn run`
/// and the picker all reach this line, so the script gets the same precondition however it was
/// asked for. The sync is [`crate::cmd::main_sync::sync_repo`], the same piece `build-main` uses.
///
/// A failed sync is a failed job: running a script that said "on latest main" against a stale
/// main would report success for work that did not happen as declared.
pub(crate) fn run_repo_script(
    cfg: &Config,
    ctx: Option<&Resolved>,
    script: &std::path::Path,
) -> (i32, String) {
    if resolver::script_header(script).latest_main {
        let Some(ctx) = ctx else {
            let msg = format!(
                "{} asks for latest main, but no repo is in scope to sync",
                script.display()
            );
            eprintln!("bn run: {msg}");
            return (2, msg);
        };
        let Some(bare) = crate::wt::bare_for(cfg, &ctx.repo) else {
            let msg = format!("{} asks for latest main, but {} has no bare repo here", script.display(), ctx.repo);
            eprintln!("bn run: {msg}");
            return (1, msg);
        };
        let (_, worktree_dir) = crate::wt::layout(cfg);
        println!("bn run: syncing {} to latest main first (# bn: latest-main)", ctx.repo);
        if !crate::cmd::main_sync::sync_repo(&bare, &worktree_dir, &ctx.repo) {
            let msg = format!("could not bring {} up to date", ctx.repo);
            eprintln!("bn run: {msg}");
            return (1, msg);
        }
    }
    run_script(script)
}

/// An agent manifest's `run:` line, verbatim.
pub(crate) fn run_custom_command(command_text: &str) -> (i32, String) {
    if cfg!(windows) {
        let error = "custom agent commands are unix-only (sh); not supported on native Windows";
        eprintln!("bn run: {error}");
        return (2, error.to_string());
    }
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(command_text).env("PATH", job_path());
    capture_command(command, "custom agent command")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A writer that hands every write to a channel, so a test can see *when* output arrives.
    struct Chan(mpsc::Sender<Vec<u8>>);
    impl std::io::Write for Chan {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The journal must see a line while the job is still running, not when it exits: the child
    /// here prints one line and then blocks on stdin until the test lets it go.
    #[test]
    fn relay_forwards_each_line_before_the_child_exits() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "echo first; read _; echo second"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let out = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        let tail = std::sync::Mutex::new(std::collections::VecDeque::new());
        std::thread::scope(|s| {
            let tail = &tail;
            s.spawn(move || relay_lines(out, Chan(tx), tail));
            let got = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("a line while the child is still blocked");
            assert_eq!(got, b"first\n");
            assert!(child.try_wait().expect("try_wait").is_none(), "child still running");
            drop(child.stdin.take()); // EOF on `read` lets it finish
        });
        child.wait().expect("wait");
        let tail: Vec<String> = tail.into_inner().unwrap().into();
        assert_eq!(tail, ["first", "second"]);
    }

    #[test]
    fn relay_keeps_only_the_last_tail_lines() {
        let input: String = (1..=40).map(|n| format!("line {n}\r\n")).collect();
        let tail = std::sync::Mutex::new(std::collections::VecDeque::new());
        relay_lines(input.as_bytes(), std::io::sink(), &tail);
        let tail: Vec<String> = tail.into_inner().unwrap().into();
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(tail.first().map(String::as_str), Some("line 26"));
        assert_eq!(tail.last().map(String::as_str), Some("line 40"));
    }
}
