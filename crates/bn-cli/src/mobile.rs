//! Mobile transport helper for issue #633 P4.
//!
//! `bn mobile <host>` reuses the SSH host inventory from `bn connect`, then prints a mosh command
//! that starts or reattaches the persistent remote `bn` tmux session.

use bn_core::hosts::{self, Host};
use bn_core::resolver::Config;
use std::path::PathBuf;
use std::process::Command;

const REMOTE_TMUX_SESSION: &str = "bn";
const REMOTE_PATH: &str = "export PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:$HOME/.bin:/opt/homebrew/bin:/usr/local/bin:$PATH\";";

fn ssh_dir() -> PathBuf {
    match std::env::var("BN_SSH_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_default().join(".ssh"),
    }
}

fn load_hosts(cfg: &Config) -> Vec<Host> {
    hosts::load(&ssh_dir(), &cfg.hosts)
}

fn find_host(cfg: &Config, alias: &str) -> Host {
    let all = load_hosts(cfg);
    match all.iter().find(|h| h.alias == alias) {
        Some(h) => h.clone(),
        None => {
            let known: Vec<&str> = all.iter().map(|h| h.alias.as_str()).collect();
            let hint = if known.is_empty() {
                "no Host stanzas found in ~/.ssh/config".to_string()
            } else {
                format!("known hosts: {}", known.join(", "))
            };
            crate::fail(&format!(
                "bn mobile: no ssh-config Host '{alias}' — {hint} (add a Host stanza to ~/.ssh/config)"
            ));
        }
    }
}

fn mobile_command(alias: &str) -> String {
    [
        "mosh".to_string(),
        "--ssh='ssh'".to_string(),
        crate::shq(alias),
        "--".to_string(),
        "tmux".to_string(),
        "new-session".to_string(),
        "-A".to_string(),
        "-s".to_string(),
        REMOTE_TMUX_SESSION.to_string(),
    ]
    .join(" ")
}

fn command_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        crate::cmd::dispatch::is_executable(&candidate)
            || crate::cmd::dispatch::is_executable(&candidate.with_extension("exe"))
    })
}

fn remote_probe_command() -> String {
    format!(
        "{REMOTE_PATH} for b in mosh-server tmux; do command -v \"$b\" >/dev/null 2>&1 || echo \"MISSING:$b\"; done"
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ProbeFailure {
    Ssh(String),
    Missing(Vec<String>),
}

fn classify_probe(success: bool, stdout: &str, stderr: &str) -> Result<(), ProbeFailure> {
    if !success {
        return Err(ProbeFailure::Ssh(stderr.trim().to_string()));
    }
    let missing: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("MISSING:").map(str::to_string))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ProbeFailure::Missing(missing))
    }
}

fn validate_remote(alias: &str) {
    let probe = Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ])
        .arg(alias)
        .arg(remote_probe_command())
        .output()
        .unwrap_or_else(|e| crate::fail(&format!("bn mobile: failed to run ssh: {e}")));

    match classify_probe(
        probe.status.success(),
        &String::from_utf8_lossy(&probe.stdout),
        &String::from_utf8_lossy(&probe.stderr),
    ) {
        Ok(()) => {}
        Err(ProbeFailure::Ssh(detail)) => crate::fail(&format!(
            "bn mobile: ssh to '{alias}' failed — verify the Tailscale host is online and `ssh {alias}` works\n{detail}"
        )),
        Err(ProbeFailure::Missing(missing)) => crate::fail(&format!(
            "bn mobile: '{alias}' is missing on PATH: {} — install mosh and tmux on the remote host",
            missing.join(", ")
        )),
    }
}

pub fn run(cfg: &Config, alias: &str) {
    let host = find_host(cfg, alias);
    if !command_on_path("mosh") {
        crate::fail(
            "bn mobile: mosh is not installed on this client — install it before using the mobile recipe",
        );
    }
    validate_remote(&host.alias);

    println!("mobile target: {} ({})", host.alias, host.target());
    println!("client: mosh found");
    println!("remote: mosh-server + tmux found");
    println!("connect: {}", mobile_command(&host.alias));
}
