use bn_core::queue::tick::Dispatch;

/// Start heavy work detached, with a wrapper that reports its own completion.
///
/// The tick cannot wait for it — that is the whole reason this shape exists — so the entry stays
/// leased and something has to close it. The same trick the worker path already uses: wrap the
/// command so that whatever its exit status, it calls back to resolve the entry. If the wrapper
/// never runs (machine died mid-build) the lease reaper picks it up on TTL, so the entry is
/// recoverable either way rather than leased forever.
pub(super) fn spawn(d: &Dispatch, cmd: &str) -> Result<(), String> {
    let wrapped = completion_wrapped(cmd, &d.repo, &d.id);
    if spawn_tmux(d, &wrapped) {
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
fn spawn_tmux(d: &Dispatch, wrapped: &str) -> bool {
    let (session, window) = tmux_target(d);
    let target = format!("={session}");
    let has = crate::proc::tmux_run(&["has-session", "-t", &target]);
    if !has && !crate::proc::tmux_run(&["new-session", "-d", "-s", &session]) {
        return false;
    }
    let command = format!(
        "PATH={}; {}; code=$?; echo; echo {}; read _; exit \"$code\"",
        shq(&crate::cmd::cron::job_path()),
        wrapped,
        shq(&format!(
            "-- queue job {}/{} finished; press enter to close --",
            d.repo, d.id
        )),
    );
    crate::proc::tmux_run(&["new-window", "-t", &target, "-n", &window, &command])
}

fn tmux_target(d: &Dispatch) -> (String, String) {
    match &d.exec {
        bn_core::queue::kind::ExecSpec::Job { job, repo, .. } => {
            let session = repo
                .as_deref()
                .unwrap_or(if job == "nightly-release" {
                    "bn"
                } else {
                    &d.repo
                })
                .to_string();
            (session, crate::wt::sanitize_branch(job))
        }
        _ => (
            d.repo.clone(),
            format!("queue-{}", crate::wt::sanitize_branch(&d.id)),
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
