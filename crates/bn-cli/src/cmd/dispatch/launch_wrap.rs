//! Shell wrappers around a dispatched agent's tmux launch command: a memory ceiling, a worker
//! completion report, and the one-shot window close. Each takes the assembled command and returns
//! a new one that runs it, so they compose in the order `run_dispatch` applies them.
//!
//! Split from `dispatch.rs` under the file-size ratchet; the seam is real: these are pure string
//! builders (bar the `systemd-run` probe), testable without tmux or a harness.

use crate::wt::shq;

pub(super) fn worker_memory_wrapped_command(cmd: &str, memory_ceiling_mb: u64, systemd_run: bool) -> String {
    let inner = vec!["sh".to_string(), "-lc".to_string(), cmd.to_string()];
    bn_core::worker::spawn::wrap(
        bn_core::worker::spawn::MemoryCap::from_estimate_mb(memory_ceiling_mb),
        systemd_run,
        &inner,
    )
    .iter()
    .map(|arg| shq(arg))
    .collect::<Vec<_>>()
    .join(" ")
}

pub(super) fn worker_completion_wrapped_command(
    cmd: &str,
    request_id: &str,
    executable: &std::path::Path,
) -> String {
    let script = r#"sh -lc "$1"
code=$?
if [ "$code" -eq 0 ]; then
  "$3" worker finish "$2" --exit-code "$code"
else
  "$3" worker finish "$2" --fail --exit-code "$code"
fi
finish_code=$?
if [ "$finish_code" -ne 0 ]; then
  exit "$finish_code"
fi
exit "$code""#;
    ["sh", "-c", script, "bn-worker-complete", cmd, request_id]
        .into_iter()
        .map(shq)
        .chain(std::iter::once(shq(&executable.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Close a one-shot agent's window when its run exits cleanly.
///
/// Agent panes keep `remain-on-exit on` so a crashed harness leaves its output to read, which also
/// keeps the window, and the plan split's editor in it, alive after a clean finish. A one-shot lane
/// has nothing left to show on success (its results are in the note and the PR), so a zero exit
/// kills its own window; a failure keeps the dead pane for inspection, as before.
pub(super) fn one_shot_wrapped_command(cmd: &str) -> String {
    let script = r#"sh -lc "$1"
code=$?
if [ "$code" -eq 0 ] && [ -n "${TMUX_PANE:-}" ]; then
  tmux kill-window -t "$TMUX_PANE"
fi
exit "$code""#;
    ["sh", "-c", script, "bn-one-shot", cmd]
        .into_iter()
        .map(shq)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn systemd_run_available() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a wrapped command in `sh` with `TMUX_PANE` unset, so the kill branch is a no-op, and
    /// return its exit code. The wrapper must hand back the agent's own status either way.
    fn exit_code(wrapped: &str) -> i32 {
        std::process::Command::new("sh")
            .args(["-c", wrapped])
            .env_remove("TMUX_PANE")
            .status()
            .expect("sh runs")
            .code()
            .unwrap_or(-1)
    }

    #[test]
    fn one_shot_wrapper_keeps_the_agents_exit_status() {
        assert_eq!(exit_code(&one_shot_wrapped_command("true")), 0);
        assert_eq!(exit_code(&one_shot_wrapped_command("exit 3")), 3);
    }

    /// Only a clean exit closes the window; a failed run keeps its dead pane to read.
    #[test]
    fn one_shot_wrapper_closes_the_window_only_on_success() {
        let wrapped = one_shot_wrapped_command("x");
        assert!(wrapped.contains(r#"[ "$code" -eq 0 ]"#), "{wrapped}");
        assert!(wrapped.contains(r#"tmux kill-window -t "$TMUX_PANE""#), "{wrapped}");
    }

    /// A launch command carrying quotes and env assignments survives the extra shell layer.
    #[test]
    fn one_shot_wrapper_preserves_a_quoted_command() {
        let inner = "GH_CONFIG_DIR='/tmp/a b' sh -c 'test \"$GH_CONFIG_DIR\" = \"/tmp/a b\"'";
        assert_eq!(exit_code(&one_shot_wrapped_command(inner)), 0);
    }
}
