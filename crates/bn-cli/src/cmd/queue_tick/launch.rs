//! Waiting for a launch without betting the dispatcher on it returning.
//!
//! `realize` starts an agent by re-entering `bn dispatch` and waiting for it. That wait used to be
//! `Command::status()` — unbounded, on a child that shells out to git, which shells out to ssh.
//! When one of those connections died without a FIN, the whole resident dispatcher stopped for
//! twenty-five minutes behind a `read` that would never return.
//!
//! So the wait has a deadline, and expiry kills the process *group*.

/// How a bounded launch ended.
pub(super) enum Launch {
    Exited(std::process::ExitStatus),
    /// The deadline passed and the child's process group was killed.
    TimedOut(u64),
    Failed(String),
}

/// Seconds a single `bn dispatch` may take before the dispatcher gives up on it.
///
/// Five minutes is far beyond an honest launch — fetch, worktree add, tmux — and far below the
/// point at which one wedged entry has cost the machine a meaningful amount of work. Override with
/// `BN_QUEUE_LAUNCH_TIMEOUT_SECS` on a box that legitimately clones something enormous; `0`
/// restores the old unbounded wait.
const LAUNCH_TIMEOUT_SECS: u64 = 300;

pub(super) fn launch_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("BN_QUEUE_LAUNCH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(LAUNCH_TIMEOUT_SECS);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Maximum wall-clock time for work executed inline by one dispatcher pass.
///
/// Express work holds the machine tick lock until it returns, so unlike agent launch this bound
/// cannot be disabled: an unbounded value restores the exact failure mode the lock exists to
/// prevent. Jobs that honestly need longer belong on the detached `heavy` path.
const EXPRESS_TIMEOUT_SECS: u64 = 300;

pub(super) fn express_timeout() -> std::time::Duration {
    let secs = std::env::var("BN_QUEUE_EXPRESS_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(EXPRESS_TIMEOUT_SECS)
        .clamp(1, 3600);
    std::time::Duration::from_secs(secs)
}

/// Spawn `cmd` with inherited stdio and wait for it, killing its whole process group if it outlives
/// `timeout`.
///
/// The group, not the child, is what gets killed — and that is the entire point. The hang that
/// motivated this was two levels down (`bn dispatch` → `git fetch` → `ssh`); reaping only the
/// direct child would return control to the dispatcher while leaving the blocked `ssh` holding the
/// bare repo's lock, so the next pass would inherit the same wedge with none of the evidence.
///
/// stdio stays inherited so the child's diagnostics keep landing in the dispatcher log where
/// operators already look for them.
pub(super) fn wait_bounded(
    mut cmd: std::process::Command,
    timeout: Option<std::time::Duration>,
) -> Launch {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Launch::Failed(e.to_string()),
    };
    let Some(timeout) = timeout else {
        return match child.wait() {
            Ok(st) => Launch::Exited(st),
            Err(e) => Launch::Failed(e.to_string()),
        };
    };
    let group = child.id();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Launch::Exited(st),
            Ok(None) => {}
            Err(e) => return Launch::Failed(e.to_string()),
        }
        if std::time::Instant::now() >= deadline {
            let _ = bn_core::runner::kill_process_tree(group, &mut child);
            let _ = child.wait();
            return Launch::TimedOut(timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::queue_tick::pid_alive;

    /// The bound that would have capped the incident at five minutes instead of twenty-five — and
    /// specifically that it reaches the *grandchild*, which is where the hang actually lived
    /// (`bn dispatch` → `git fetch` → `ssh`). Killing only the direct child would pass a weaker
    /// version of this test while leaving the blocked process holding the repo.
    #[test]
    fn a_launch_that_never_returns_is_killed_along_with_its_children() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "sleep 60 & echo $! > {}; wait",
            pidfile.to_string_lossy()
        ));

        let started = std::time::Instant::now();
        let outcome = wait_bounded(cmd, Some(std::time::Duration::from_secs(1)));

        assert!(matches!(outcome, Launch::TimedOut(_)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the wait must end at its deadline, not at the child's convenience"
        );

        let grandchild: u32 = std::fs::read_to_string(&pidfile)
            .expect("the shell should have recorded its background pid")
            .trim()
            .parse()
            .expect("a pid");
        // The group signal lands immediately; reparenting and reaping take a moment longer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pid_alive(grandchild) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !pid_alive(grandchild),
            "the whole process group must die, not just the process we spawned"
        );
    }

    /// …and a launch that returns normally still reports its own exit status.
    #[test]
    fn a_launch_that_returns_keeps_its_status() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 3");
        match wait_bounded(cmd, Some(std::time::Duration::from_secs(30))) {
            Launch::Exited(st) => assert_eq!(st.code(), Some(3)),
            _ => panic!("a prompt child should report its exit status"),
        }
    }
}
