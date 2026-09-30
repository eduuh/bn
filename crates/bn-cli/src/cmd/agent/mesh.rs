//! Reaching an agent on another machine of the tunnel mesh.
//!
//! `bn agent msg repo:branch` used to reach only a pane on this machine, or on the machine an
//! agent was launched to from here (`agent-host:` in the note). An agent on machine B could
//! therefore never answer the coordinator on machine A. When the target has no live window here,
//! this asks every tunnel host (the `# >>> bn tunnel <machine> >>>` blocks tunnel-connect writes
//! into `~/.ssh/config`) for its live agents, and hands the message to the one machine that has the
//! target, where that machine's own `bn agent msg` finds the pane and confirms the submit.
//!
//! The relayed call carries `BN_MSG_NO_MESH=1`, so a target that has gone by then is refused there
//! instead of being bounced on to a third machine or back here.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Set on a relayed `bn agent msg`, so the receiving machine never relays it again.
pub(crate) const NO_MESH_ENV: &str = "BN_MSG_NO_MESH";

/// How long one machine gets to list its agents. A host whose tunnel is down usually refuses at
/// once; one whose relay hangs would otherwise hold up the message.
const LIVE_DEADLINE: Duration = Duration::from_secs(12);

/// A remote non-login shell does not read the profile, so bn's install dir goes on PATH by hand.
const REMOTE_PATH: &str = "export PATH=\"$HOME/.local/bin:$PATH\";";

/// Where the message went, or why it could not.
pub(crate) enum Relay {
    /// Handed to `host`; its `bn agent msg` exited with this code (its output already printed).
    Sent { code: i32 },
    /// No tunnel host has the target live. `searched` names the machines asked.
    NotFound { searched: Vec<String> },
    /// More than one machine has it live; guessing could message the wrong agent.
    Ambiguous { hosts: Vec<String> },
}

/// Machines this one reaches through tunnel-connect's managed blocks in an ssh config.
pub(crate) fn tunnel_hosts_in(ssh_config: &str) -> Vec<String> {
    ssh_config
        .lines()
        .filter_map(|line| line.strip_prefix("# >>> bn tunnel "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

fn tunnel_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let path = std::path::Path::new(&home).join(".ssh").join("config");
    tunnel_hosts_in(&std::fs::read_to_string(path).unwrap_or_default())
}

/// Whether a `bn agent live --json` envelope lists `target` with a live window.
pub(crate) fn live_in(envelope: &str, target: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(envelope) else {
        return false;
    };
    v["data"]["agents"].as_array().is_some_and(|agents| {
        agents.iter().any(|a| {
            a["target"].as_str() == Some(target) && a["has_window"].as_bool().unwrap_or(false)
        })
    })
}

fn ssh(host: &str, remote: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8"])
        .args(super::SSH_SHARED_CONNECTION)
        .args([host, remote]);
    cmd
}

/// Wait for `child` until `deadline`, then kill it. Its stdout, when it finished in time.
fn finish_by(mut child: Child, deadline: Instant) -> Option<String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The tunnel hosts that have `target` live, asking them all at once.
fn hosts_with(hosts: &[String], target: &str) -> Vec<String> {
    let deadline = Instant::now() + LIVE_DEADLINE;
    // Collected on purpose: every ssh must be spawned before the first is waited on, or the hosts
    // would be asked one after another instead of at once.
    #[allow(clippy::needless_collect)]
    let children: Vec<(String, Option<Child>)> = hosts
        .iter()
        .map(|h| {
            let child = ssh(h, &format!("{REMOTE_PATH} bn agent live --json"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok();
            (h.clone(), child)
        })
        .collect();
    children
        .into_iter()
        .filter_map(|(h, child)| {
            let out = finish_by(child?, deadline)?;
            live_in(&out, target).then_some(h)
        })
        .collect()
}

/// The remote command that delivers the message there, every argument quoted.
pub(crate) fn remote_msg_command(target: &str, text: &str, sender: &str, as_json: bool) -> String {
    let q = crate::wt::shq;
    format!(
        "{REMOTE_PATH} {NO_MESH_ENV}=1 bn agent msg {}{} --from {} {}",
        q(target),
        if as_json { " --json" } else { "" },
        q(sender),
        q(text)
    )
}

/// Find the one tunnel host with `target` live and deliver the message through its own bn.
/// `None` when relaying is off here (this call was itself relayed) or there are no tunnel hosts.
pub(crate) fn relay(target: &str, text: &str, sender: &str, as_json: bool) -> Option<Relay> {
    if std::env::var_os(NO_MESH_ENV).is_some() {
        return None;
    }
    let hosts = tunnel_hosts();
    if hosts.is_empty() {
        return None;
    }
    let found = hosts_with(&hosts, target);
    match found.as_slice() {
        [] => Some(Relay::NotFound { searched: hosts }),
        [host] => {
            let code = ssh(host, &remote_msg_command(target, text, sender, as_json))
                .stdin(Stdio::null())
                .status()
                .ok()
                .and_then(|s| s.code())
                .unwrap_or(1);
            if !as_json {
                eprintln!("bn agent msg: relayed to {host}");
            }
            Some(Relay::Sent { code })
        }
        _ => Some(Relay::Ambiguous { hosts: found }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_hosts_are_read_from_the_managed_blocks_only() {
        let cfg = "Host github.com\n  User git\n# >>> bn tunnel CPC-a >>>\nHost CPC-a\n  Port 2222\n\
                   # <<< bn tunnel CPC-a <<<\n# >>> bn tunnel CPC-b >>>\nMatch originalhost CPC-b exec \"nc\"\n\
                   Host CPC-b\n# <<< bn tunnel CPC-b <<<\n";
        assert_eq!(tunnel_hosts_in(cfg), vec!["CPC-a", "CPC-b"]);
        assert!(tunnel_hosts_in("Host x\n").is_empty());
    }

    #[test]
    fn only_a_live_window_for_the_exact_target_counts() {
        let env = r#"{"v":1,"ok":true,"data":{"agents":[
            {"target":"bn:main","has_window":true},
            {"target":"bn:done","has_window":false}]}}"#;
        assert!(live_in(env, "bn:main"));
        assert!(!live_in(env, "bn:done"));
        assert!(!live_in(env, "bn:mai"));
        assert!(!live_in("not json", "bn:main"));
    }

    #[test]
    fn the_relayed_command_quotes_everything_and_stops_further_relays() {
        let cmd = remote_msg_command("bn:feat-a", "it's done; see $PR", "bn:coord", true);
        assert!(cmd.contains("BN_MSG_NO_MESH=1 bn agent msg 'bn:feat-a' --json --from 'bn:coord' "));
        assert!(cmd.ends_with(&crate::wt::shq("it's done; see $PR")));
    }
}
