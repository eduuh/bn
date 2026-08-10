//! Wall-clock bounds on git's network calls — the difference between a slow fetch and a wedged one.
//!
//! git has no timeout of its own. A `fetch` whose TCP connection dies without a FIN sits in `read`
//! forever: the remote is gone, the socket is not, and nothing above notices. That is not a
//! hypothetical failure mode — it is the one that took the dispatcher down. `bn dispatch` ran
//! `git fetch origin`, its `ssh git@github.com git-upload-pack` hung on a dead connection, and the
//! resident dispatcher — which waits on that child synchronously — stopped heartbeating and read as
//! offline for twenty-five minutes while the process was, technically, perfectly healthy.
//!
//! So every `git` bn spawns gets three bounds, applied here rather than at each of the dozen call
//! sites that would otherwise have to remember:
//!
//!   * **ssh** — `ConnectTimeout` caps the handshake; `ServerAlive*` turn a dead peer into an error
//!     after ~45s instead of a permanent block. This is the one that matters, because the ssh
//!     transport is where the hang actually happened.
//!   * **https** — git's own low-speed abort: a transfer moving under a byte a second for 45s is
//!     not a slow transfer, it is a dead one.
//!   * **prompts** — a credential prompt with no terminal to type into is an infinite wait dressed
//!     up as a question. Suppressed *only* when stdin is not a TTY, so an interactive
//!     `bn wt clone` of a private HTTPS repo can still ask.
//!
//! Every value is a floor, not an override: an operator who exports `GIT_SSH_COMMAND` or sets the
//! low-speed knobs keeps theirs. The point is that the unconfigured default stops being "wait
//! forever".

use std::process::Command;

/// Seconds ssh will spend on a TCP handshake before giving up.
const SSH_CONNECT_TIMEOUT: u32 = 10;
/// Seconds between ssh keepalive probes on an established connection.
const SSH_ALIVE_INTERVAL: u32 = 15;
/// Unanswered keepalives before ssh declares the peer gone (~45s total with the interval above).
const SSH_ALIVE_COUNT: u32 = 3;
/// Bytes/second under which an HTTPS transfer counts as stalled rather than slow.
const HTTP_LOW_SPEED_LIMIT: u32 = 1;
/// Seconds an HTTPS transfer may stay under the limit before git aborts it.
const HTTP_LOW_SPEED_TIME: u32 = 45;

/// Apply the network bounds to a `git` command about to be spawned.
///
/// Call this on every `git` spawn — cheap, idempotent, and a no-op for commands that touch no
/// network (the env vars simply go unread). Pre-existing environment always wins, so this raises
/// the floor without taking the wheel from anyone who has already configured their transport.
pub fn bound(cmd: &mut Command) {
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -o ConnectTimeout={SSH_CONNECT_TIMEOUT} \
                 -o ServerAliveInterval={SSH_ALIVE_INTERVAL} \
                 -o ServerAliveCountMax={SSH_ALIVE_COUNT}"
            ),
        );
    }
    if std::env::var_os("GIT_HTTP_LOW_SPEED_LIMIT").is_none() {
        cmd.env("GIT_HTTP_LOW_SPEED_LIMIT", HTTP_LOW_SPEED_LIMIT.to_string());
        cmd.env("GIT_HTTP_LOW_SPEED_TIME", HTTP_LOW_SPEED_TIME.to_string());
    }
    // A prompt is only answerable when someone is there to answer it. With no terminal — cron,
    // launchd, the dispatcher, a tmux-less agent — git would block on a question nobody will ever
    // read, which is the exact failure this module exists to end.
    if std::env::var_os("GIT_TERMINAL_PROMPT").is_none() && !stdin_is_terminal() {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
}

fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env a command carries, as the pairs `bound` actually set.
    fn applied(cmd: &Command) -> Vec<(String, Option<String>)> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn sets_ssh_and_http_bounds() {
        let _lock = crate::test_support::env_lock();
        let _ssh = crate::test_support::EnvGuard::remove("GIT_SSH_COMMAND");
        let _limit = crate::test_support::EnvGuard::remove("GIT_HTTP_LOW_SPEED_LIMIT");
        let mut cmd = Command::new("git");
        bound(&mut cmd);
        let env = applied(&cmd);
        let ssh = env
            .iter()
            .find(|(k, _)| k == "GIT_SSH_COMMAND")
            .and_then(|(_, v)| v.clone())
            .expect("ssh command set");
        assert!(ssh.contains("ConnectTimeout=10"), "{ssh}");
        assert!(ssh.contains("ServerAliveInterval=15"), "{ssh}");
        assert!(ssh.contains("ServerAliveCountMax=3"), "{ssh}");
        assert!(env
            .iter()
            .any(|(k, v)| k == "GIT_HTTP_LOW_SPEED_TIME" && v.as_deref() == Some("45")));
    }

    /// The floor must never become an override — a configured transport survives untouched.
    #[test]
    fn respects_a_configured_ssh_command() {
        let _lock = crate::test_support::env_lock();
        let _ssh = crate::test_support::EnvGuard::set("GIT_SSH_COMMAND", "ssh -i /custom/key");
        let mut cmd = Command::new("git");
        bound(&mut cmd);
        assert!(
            !applied(&cmd).iter().any(|(k, _)| k == "GIT_SSH_COMMAND"),
            "an operator's GIT_SSH_COMMAND must win"
        );
    }
}
