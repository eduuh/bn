//! Native SSH: `bn connect` (the cross-machine `tat`) and the remote half of
//! `bn dispatch --host`. Hosts come from `~/.ssh/config` enriched by config.toml `[hosts]`
//! (see `bn_core::hosts`); this module owns the interactive picker, the tmux-backed attach,
//! and the precondition-checked remote dispatch.
//!
//! Mirrors the self-referencing `tat` shape: the same binary renders the fzf row list and the
//! preview panel via a hidden `--pick-stage`, and degrades to a numbered list when fzf is absent.

use bn_core::hosts::{self, Host};
use bn_core::resolver::Config;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The hidden callback stages a `connect` fzf session re-invokes (`bn connect --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ConnectStage {
    /// Emit the host row list (the fzf `start` reload feed).
    List,
    /// Render the preview panel for the focused host: config facts + a live remote probe.
    Preview,
}

/// PATH prelude for remote one-shot commands: `ssh <host> '<cmd>'` runs a non-login shell whose
/// PATH often misses the user bin dirs (`~/.cargo/bin`, homebrew), so extend it explicitly.
const REMOTE_PATH: &str = "export PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:$HOME/.bin:/opt/homebrew/bin:/usr/local/bin:$PATH\";";

/// One non-interactive SSH policy for remote dispatch probing and execution.
const DISPATCH_SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=3",
];

/// Short non-interactive policy for the live picker preview.
const PREVIEW_SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=3",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=3",
];

/// Interactive connect policy. BatchMode is deliberately omitted so passphrase and 2FA
/// prompts keep working.
const INTERACTIVE_SSH_OPTIONS: &[&str] = &[
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=3",
];

const PREVIEW_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const DISPATCH_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_DISPATCH_TIMEOUT: Duration = Duration::from_secs(120);

/// The remote session every `bn connect` attaches to. `new-session -A` re-attaches an existing
/// `bn` session, so a dropped pipe resumes work instead of killing it.
const REMOTE_TMUX_SESSION: &str = "bn";

fn ssh_command(options: &[&str]) -> Command {
    let mut command = Command::new("ssh");
    command.args(options);
    command
}

fn dispatch_ssh(alias: &str) -> Command {
    let mut command = ssh_command(DISPATCH_SSH_OPTIONS);
    command.arg(alias);
    command
}

/// Fetch the typed worker-status data object over the same hardened SSH transport used by remote
/// dispatch. `None` means the node is unreachable or returned an invalid status payload.
pub(crate) fn worker_status(cfg: &Config, alias: &str) -> Option<serde_json::Value> {
    let runner = bn_core::worker::transport::RealSshRunner;
    let transport = bn_core::worker::transport::Transport::connect(
        cfg,
        alias,
        bn_core::worker::transport::SshOptions::default(),
        &runner,
    )
    .ok()?;
    transport.poll_status().ok()
}

struct SshRun {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

enum DispatchSshError {
    Spawn(std::io::Error),
    Run(SshRun),
}

impl std::fmt::Display for DispatchSshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to run ssh: {error}"),
            Self::Run(run) => write!(f, "{}", ssh_failure("ssh failed", run)),
        }
    }
}

fn wait_for_ssh(
    child: &mut Child,
    timeout: Option<Duration>,
) -> std::io::Result<(ExitStatus, bool)> {
    let Some(timeout) = timeout else {
        return child.wait().map(|status| (status, false));
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if Instant::now() >= deadline {
            match kill_ssh_process_group(child) {
                Ok(()) => return child.wait().map(|status| (status, true)),
                Err(error) => {
                    if let Some(status) = child.try_wait()? {
                        return Ok((status, false));
                    }
                    return Err(error);
                }
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::kill is required to terminate the SSH process group on timeout"
)]
fn kill_ssh_process_group(child: &mut Child) -> std::io::Result<()> {
    // SAFETY: the child starts in its own process group, and kill does not dereference pointers.
    let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn kill_ssh_process_group(child: &mut Child) -> std::io::Result<()> {
    child.kill()
}

/// Preserve the interactive terminal while retaining stderr for an actionable final error.
fn run_ssh_with_live_stderr(mut command: Command) -> std::io::Result<SshRun> {
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("ssh stderr pipe was not created"))?;
    let stderr_reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            let read = stderr.read(&mut buf)?;
            if read == 0 {
                break;
            }
            std::io::stderr().write_all(&buf[..read])?;
            captured.extend_from_slice(&buf[..read]);
        }
        Ok(captured)
    });
    let (status, timed_out) = wait_for_ssh(&mut child, None)?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("ssh stderr reader thread panicked"))??;
    Ok(SshRun {
        status,
        stdout: Vec::new(),
        stderr,
        timed_out,
    })
}

fn run_ssh_with_timeout(
    mut command: Command,
    timeout: Duration,
    stream_output: bool,
) -> std::io::Result<SshRun> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("ssh stdout pipe was not created"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("ssh stderr pipe was not created"))?;
    let stdout_reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            let read = stdout.read(&mut buf)?;
            if read == 0 {
                break;
            }
            if stream_output {
                std::io::stdout().write_all(&buf[..read])?;
            }
            captured.extend_from_slice(&buf[..read]);
        }
        Ok(captured)
    });
    let stderr_reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            let read = stderr.read(&mut buf)?;
            if read == 0 {
                break;
            }
            if stream_output {
                std::io::stderr().write_all(&buf[..read])?;
            }
            captured.extend_from_slice(&buf[..read]);
        }
        Ok(captured)
    });
    let (status, timed_out) = wait_for_ssh(&mut child, Some(timeout))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("ssh stdout reader thread panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("ssh stderr reader thread panicked"))??;
    Ok(SshRun {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn ssh_connect_failure_is_transient(run: &SshRun) -> bool {
    if run.timed_out || run.status.code() != Some(255) {
        return false;
    }
    let stderr = String::from_utf8_lossy(&run.stderr).to_lowercase();
    [
        "connection refused",
        "connection timed out",
        "operation timed out",
        "no route to host",
        "network is unreachable",
        "could not resolve hostname",
        "temporary failure in name resolution",
    ]
    .iter()
    .any(|reason| stderr.contains(reason))
}

fn run_dispatch_ssh_with_retry(
    label: &str,
    mut command: impl FnMut() -> Command,
    timeout: Duration,
    stream_output: bool,
) -> Result<SshRun, DispatchSshError> {
    crate::retry::run(
        label,
        || match run_ssh_with_timeout(command(), timeout, stream_output) {
            Ok(run) if run.status.success() => Ok(run),
            Ok(run) => Err(DispatchSshError::Run(run)),
            Err(error) => Err(DispatchSshError::Spawn(error)),
        },
        |error| match error {
            DispatchSshError::Spawn(error) => crate::retry::io_error_is_transient(error),
            DispatchSshError::Run(run) => ssh_connect_failure_is_transient(run),
        },
        std::thread::sleep,
        |message| eprintln!("{message}"),
    )
}

fn ssh_failure(context: &str, run: &SshRun) -> String {
    let status = if run.timed_out {
        "command timed out".to_string()
    } else {
        run.status.to_string()
    };
    let detail = String::from_utf8_lossy(&run.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("{context} ({status})")
    } else {
        format!("{context} ({status})\n{detail}")
    }
}

fn ssh_next_steps(alias: &str) -> String {
    format!(
        "hint: try `ssh {alias}`; check key auth, the Host stanza in ~/.ssh/config, and network; run `bn doctor` for diagnostics"
    )
}

/// `~/.ssh`, overridable with `$BN_SSH_DIR` (the test contract — hosts come from
/// `<ssh_dir>/config`).
fn ssh_dir() -> PathBuf {
    match std::env::var("BN_SSH_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_default().join(".ssh"),
    }
}

fn load_hosts(cfg: &Config) -> Vec<Host> {
    hosts::load(&ssh_dir(), &cfg.hosts)
}

fn host_by_alias<'a>(all: &'a [Host], alias: &str) -> Option<&'a Host> {
    all.iter().find(|h| h.alias == alias)
}

fn known_hosts_hint(all: &[Host]) -> String {
    let known: Vec<&str> = all.iter().map(|h| h.alias.as_str()).collect();
    if known.is_empty() {
        "no Host stanzas found in ~/.ssh/config".to_string()
    } else {
        format!("known hosts: {}", known.join(", "))
    }
}

/// Find `alias` among the known hosts, or die listing what IS known — the "host absent from
/// ssh-config" degrade path.
fn find_host(cfg: &Config, alias: &str) -> Host {
    let all = load_hosts(cfg);
    match host_by_alias(&all, alias) {
        Some(h) => h.clone(),
        None => crate::fail(&format!(
            "bn connect: no ssh-config Host '{alias}' — {} (add a Host stanza to ~/.ssh/config)\n{}",
            known_hosts_hint(&all),
            ssh_next_steps(alias)
        )),
    }
}

/// `bn connect [host]`: attach directly with an alias, else run the picker.
pub fn run(cfg: &Config, host: Option<&str>, window: bool, read_only: bool) {
    match host {
        Some(alias) if !alias.is_empty() => {
            let h = find_host(cfg, alias);
            attach(&h.alias, window, read_only);
        }
        _ => pick(cfg, window, read_only),
    }
}

/// The hidden `--pick-stage` callbacks an fzf session re-invokes.
pub fn stage(cfg: &Config, stage: ConnectStage, host: Option<&str>) {
    match stage {
        ConnectStage::List => build_list(cfg),
        ConnectStage::Preview => preview(cfg, host.unwrap_or("")),
    }
}

/// `alias \t user@hostname[:port] \t enrichment` — the fzf row. Field 1 is the alias every
/// callback keys on.
fn row(h: &Host) -> String {
    let mut notes = Vec::new();
    if let Some(hn) = &h.extra.harness {
        notes.push(format!("harness:{hn}"));
    }
    if h.extra.bn == Some(false) {
        notes.push("bn:absent".to_string());
    } else if h.registered {
        notes.push("bn:yes".to_string());
    }
    let note = if h.registered {
        format!("\x1b[32m●\x1b[0m {}", notes.join(" "))
    } else {
        "\x1b[2m○ unregistered\x1b[0m".to_string()
    };
    format!("{}\t\x1b[2m{}\x1b[0m\t{}", h.alias, h.target(), note)
}

fn build_list(cfg: &Config) {
    let mut out = std::io::stdout().lock();
    for h in load_hosts(cfg) {
        let _ = writeln!(out, "{}", row(&h));
    }
}

/// The preview panel: connection facts from config, then a live probe of the remote's
/// `bn summary` (its status/agents dashboard), degrading to `uptime` when bn is absent
/// and to "unreachable" when ssh fails.
fn preview(cfg: &Config, alias: &str) {
    if alias.is_empty() {
        return;
    }
    let Some(h) = load_hosts(cfg).into_iter().find(|h| h.alias == alias) else {
        println!("unknown host: {alias}");
        return;
    };
    println!("\x1b[1m{}\x1b[0m  {}", h.alias, h.target());
    if let Some(root) = &h.extra.repo_root {
        println!("repo_root: {root}");
    }
    if let Some(harness) = &h.extra.harness {
        println!("harness:   {harness}");
    }
    if !h.registered {
        println!(
            "\x1b[2mnot in [hosts] — connect works; dispatch --host needs registration\x1b[0m"
        );
    }
    println!();
    // Live probe. BatchMode keeps a password prompt from wedging the preview pane; a short
    // ConnectTimeout keeps an unreachable box from freezing the picker.
    let remote_cmd = if h.extra.bn == Some(false) {
        format!("{REMOTE_PATH} uptime")
    } else {
        format!("{REMOTE_PATH} bn summary 2>/dev/null || uptime")
    };
    let mut command = ssh_command(PREVIEW_SSH_OPTIONS);
    command.arg(&h.alias).arg(remote_cmd);
    let out = run_ssh_with_timeout(command, PREVIEW_COMMAND_TIMEOUT, false);
    match out {
        Ok(run) if run.status.success() => print!("{}", String::from_utf8_lossy(&run.stdout)),
        Ok(o) => println!(
            "\x1b[2m{}\n{}\x1b[0m",
            ssh_failure("unreachable (ssh batch probe failed)", &o),
            ssh_next_steps(&h.alias)
        ),
        Err(e) => println!(
            "\x1b[2munreachable (failed to run ssh: {e})\n{}\x1b[0m",
            ssh_next_steps(&h.alias)
        ),
    }
}

/// Interactive host picker: fzf over the row list with a live preview; a plain numbered list
/// read from stdin when fzf is absent.
fn pick(cfg: &Config, window: bool, read_only: bool) {
    let all = load_hosts(cfg);
    if all.is_empty() {
        crate::fail(
            "bn connect: no Host stanzas in ~/.ssh/config — add one (wildcards are skipped)",
        );
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--delimiter=\t")
        .arg("--header=Enter: connect (ssh + tmux)")
        .arg(format!("--preview={bn} connect --pick-stage preview {{1}}"))
        .arg("--preview-window=right:55%:wrap")
        .arg(format!(
            "--bind=start:reload({bn} connect --pick-stage list)"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to a numbered list.
            pick_numbered(&all, window, read_only);
            return;
        }
    };
    let Ok(out) = child.wait_with_output() else {
        return;
    };
    if !out.status.success() {
        return; // Esc / ctrl-c / no match
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let alias = text
        .lines()
        .next()
        .and_then(|l| l.split('\t').next())
        .unwrap_or("")
        .trim()
        .to_string();
    if !alias.is_empty() {
        attach(&alias, window, read_only);
    }
}

/// fzf-less degrade: print `n) alias  target`, read a number or alias from stdin.
fn pick_numbered(all: &[Host], window: bool, read_only: bool) {
    for (i, h) in all.iter().enumerate() {
        println!("{}) {}  {}", i + 1, h.alias, h.target());
    }
    print!("connect to: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return;
    }
    match selected_host(all, &line) {
        Ok(Some(h)) => attach(&h.alias, window, read_only),
        Ok(None) => {}
        Err(error) => eprintln!("bn connect: {error}"),
    }
}

fn selected_host<'a>(all: &'a [Host], selection: &str) -> Result<Option<&'a Host>, String> {
    let selection = selection.trim();
    if selection.is_empty() {
        return Ok(None);
    }
    let alias = match selection.parse::<usize>() {
        Ok(n) if n >= 1 && n <= all.len() => all[n - 1].alias.as_str(),
        _ => selection,
    };
    host_by_alias(all, alias)
        .map(Some)
        .ok_or_else(|| format!("unknown host '{alias}'\n{}", ssh_next_steps(alias)))
}

fn remote_attach_command(read_only: bool) -> String {
    if read_only {
        format!(
            "{REMOTE_PATH} command -v tmux >/dev/null 2>&1 && tmux has-session -t {REMOTE_TMUX_SESSION} 2>/dev/null && exec tmux attach-session -r -t {REMOTE_TMUX_SESSION}; exec \"${{SHELL:-/bin/sh}}\" -l"
        )
    } else {
        format!(
            "{REMOTE_PATH} command -v tmux >/dev/null 2>&1 && exec tmux new-session -A -s {REMOTE_TMUX_SESSION}; exec \"${{SHELL:-/bin/sh}}\" -l"
        )
    }
}

fn ssh_args(alias: &str, read_only: bool) -> Vec<String> {
    INTERACTIVE_SSH_OPTIONS
        .iter()
        .copied()
        .chain(["-t", alias])
        .map(str::to_string)
        .chain(std::iter::once(remote_attach_command(read_only)))
        .collect()
}

fn interactive_ssh(alias: &str, read_only: bool) -> Command {
    let mut command = ssh_command(INTERACTIVE_SSH_OPTIONS);
    command
        .arg("-t")
        .arg(alias)
        .arg(remote_attach_command(read_only));
    command
}

fn ssh_shell_command(alias: &str, read_only: bool) -> String {
    let ssh = std::iter::once("ssh".to_string())
        .chain(
            ssh_args(alias, read_only)
                .into_iter()
                .map(|arg| crate::shq(&arg)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{ssh}; status=$?; if [ \"$status\" -ne 0 ]; then \
         printf '\\nbn connect: ssh to %s failed (exit status: %s)\\n' {} \"$status\" >&2; \
         printf 'SSH stderr is shown above; exit this local shell to close the pane.\\n' >&2; \
         printf '%s\\n' {} >&2; \
         exec \"${{SHELL:-/bin/sh}}\" -l; fi",
        crate::shq(alias),
        crate::shq(&ssh_next_steps(alias))
    )
}

/// `ssh -t <alias>` into `tmux new-session -A -s bn` on the target. One remote command handles
/// both worlds: tmux present → attach/create the persistent session (a dropped pipe re-attaches
/// on the next connect); tmux absent → degrade to a plain login shell. No probe round-trip.
///
/// Bounded so a dead/blackholed host errors instead of hanging forever (gap #714): a
/// `ConnectTimeout` caps the initial handshake and `ServerAliveInterval`/`ServerAliveCountMax`
/// tear down a mid-session TCP blackhole. `BatchMode` is deliberately NOT set here — this is the
/// interactive human path, so passphrase/2FA prompting must still work (the non-interactive
/// key-auth-only path is `bn_core::worker::transport`).
fn attach(alias: &str, window: bool, read_only: bool) {
    if window {
        attach_window(alias, read_only);
    } else {
        attach_direct(alias, read_only);
    }
}

fn attach_direct(alias: &str, read_only: bool) {
    let result = run_ssh_with_live_stderr(interactive_ssh(alias, read_only));
    match result {
        Ok(run) if run.status.success() => {}
        Ok(run) => crate::fail(&format!(
            "{}\n{}",
            ssh_failure(&format!("bn connect: ssh to '{alias}' failed"), &run),
            ssh_next_steps(alias)
        )),
        Err(e) => crate::fail(&format!(
            "bn connect: failed to run ssh: {e}\n{}",
            ssh_next_steps(alias)
        )),
    }
}

fn local_session_id(session: &str) -> Option<String> {
    let listing =
        crate::proc::tmux_raw(&["list-sessions", "-F", "#{session_name}\t#{session_id}"])?;
    session_id_from_listing(&listing, session)
}

fn session_id_from_listing(listing: &str, session: &str) -> Option<String> {
    listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(name, id)| (name == session).then(|| id.to_string()))
}

fn attach_window(alias: &str, read_only: bool) {
    let session = format!("host:{alias}");
    let command = ssh_shell_command(alias, read_only);

    if std::env::var_os("TMUX").is_none() {
        let existing = local_session_id(&session);
        let status = match existing {
            Some(id) => crate::proc::tmux_command(&["attach-session", "-t", &id]).status(),
            None => crate::proc::tmux_command(&["new-session", "-s", &session])
                .arg(command)
                .status(),
        }
        .unwrap_or_else(|e| crate::fail(&format!("bn connect: failed to run tmux: {e}")));
        if !status.success() {
            crate::fail(&format!(
                "bn connect: failed to attach local tmux session '{session}'"
            ));
        }
        return;
    }

    let id = match local_session_id(&session) {
        Some(id) => id,
        None => {
            let out = crate::proc::tmux_command(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{session_id}",
                "-s",
                &session,
            ])
            .arg(command)
            .output()
            .unwrap_or_else(|e| crate::fail(&format!("bn connect: failed to run tmux: {e}")));
            if !out.status.success() {
                crate::fail(&format!(
                    "bn connect: failed to create local tmux session '{session}'"
                ));
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    if id.is_empty() {
        crate::fail(&format!(
            "bn connect: tmux returned no id for local session '{session}'"
        ));
    }

    if !crate::proc::tmux_run(&["switch-client", "-t", &id]) {
        crate::fail(&format!(
            "bn connect: failed to switch to local tmux session '{session}'"
        ));
    }
}

/// The remote half of `bn dispatch --host <alias>`: precondition-check the target over ssh
/// (bn, tmux, and the harness present — clear failures), then run
/// `bn dispatch <task> --repo <repo> --agent <harness> --yolo` **on the target**. The remote
/// dispatch creates the worktree, seeds the note (machine-scoped to the target — it flows home
/// via `bn sync`), and launches the agent in a detached tmux window there.
///
/// Security: the seed is the user's own command-line text, so `--yolo` carries the same trust
/// as an interactive launch — and registration under `[hosts]` is the per-host opt-in this
/// requires. An item that resolves to an external URL on the target is defused by the remote
/// dispatch itself (its yolo-blocked-external rule runs where the seed is built).
#[allow(clippy::too_many_arguments)]
pub fn dispatch_remote(
    cfg: &Config,
    alias: &str,
    repo: Option<&str>,
    item: &str,
    base: Option<&str>,
    branch: Option<&str>,
    name: Option<&str>,
    agent: Option<&str>,
    scout: bool,
    skill: Option<&str>,
    verify: bool,
    json: bool,
) {
    if item.trim().is_empty() {
        crate::fail("bn dispatch: empty item");
    }
    let all = load_hosts(cfg);
    let Some(host) = all.iter().find(|h| h.alias == alias) else {
        let known: Vec<&str> = all.iter().map(|h| h.alias.as_str()).collect();
        crate::fail(&format!(
            "bn dispatch: no ssh-config Host '{alias}' — known hosts: {}",
            if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            }
        ));
    };
    // Trust gate: remote dispatch runs the agent unattended (--yolo on the target), so it needs
    // the explicit per-host opt-in that `[hosts]` registration is.
    if !host.registered {
        crate::fail(&format!(
            "bn dispatch: '{alias}' is not registered under [hosts] in config.toml — remote \
             dispatch runs the agent unattended, so opt in with a [hosts.{alias}] entry"
        ));
    }
    if host.extra.bn == Some(false) {
        crate::fail(&format!(
            "bn dispatch: [hosts.{alias}] says bn is not installed there (bn = false) — install \
             bn on the target, then flip the flag"
        ));
    }
    let Some(repo) = repo else {
        crate::fail(
            "bn dispatch: --host needs a target repo — pass --repo <name> (or run from inside one)",
        );
    };
    // Harness: explicit --agent wins, else the host's configured default, else copilot.
    let harness = match agent {
        Some(a) => a.to_string(),
        None => match host.extra.harness.as_deref() {
            Some(h) => crate::normalize_agent(h)
                .unwrap_or_else(|| {
                    crate::fail(&format!(
                        "bn dispatch: [hosts.{alias}] has unknown harness '{h}' (use: copilot | claude)"
                    ))
                })
                .to_string(),
            None => "copilot".to_string(),
        },
    };

    // Preconditions in one round-trip: bn, tmux (the detached window lives in it), and the
    // harness CLI must exist on the target. BatchMode so a password prompt fails fast instead
    // of wedging a non-interactive dispatch.
    let probe_cmd = format!(
        "{REMOTE_PATH} for b in bn tmux {harness}; do command -v \"$b\" >/dev/null 2>&1 || echo \"MISSING:$b\"; done"
    );
    let probe = run_dispatch_ssh_with_retry(
        &format!("bn dispatch: ssh probe to '{alias}'"),
        || {
            let mut command = dispatch_ssh(alias);
            command.arg(&probe_cmd);
            command
        },
        DISPATCH_PROBE_TIMEOUT,
        false,
    )
    .unwrap_or_else(|error| match error {
        DispatchSshError::Spawn(error) => {
            crate::fail(&format!("bn dispatch: failed to run ssh: {error}"))
        }
        DispatchSshError::Run(run) => crate::fail(&format!(
            "{}\n{}",
            ssh_failure(
                &format!("bn dispatch: ssh to '{alias}' failed — is it reachable with key auth?"),
                &run,
            ),
            ssh_next_steps(alias)
        )),
    });
    let missing: Vec<String> = String::from_utf8_lossy(&probe.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("MISSING:").map(str::to_string))
        .collect();
    if !missing.is_empty() {
        crate::fail(&format!(
            "bn dispatch: '{alias}' is missing on PATH: {} — install before dispatching there",
            missing.join(", ")
        ));
    }

    // Run the dispatch on the target. --repo makes it cwd-independent; the remote's own
    // dispatch does the rest (worktree, note seed, detached tmux window, agent launch).
    let args = crate::cmd::dispatch::DispatchInvocation::new(item, repo, &harness)
        .base(base)
        .branch(branch)
        .name(name)
        .scout(scout)
        .skill(skill)
        .verify(verify)
        .yolo(true)
        .json(json)
        .args();
    let remote = format!(
        "{REMOTE_PATH} bn {}",
        args.iter()
            .map(|arg| crate::shq(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    run_dispatch_ssh_with_retry(
        &format!("bn dispatch: remote launch on '{alias}'"),
        || {
            let mut command = dispatch_ssh(alias);
            command.arg(&remote);
            command
        },
        REMOTE_DISPATCH_TIMEOUT,
        true,
    )
    .unwrap_or_else(|error| match error {
        DispatchSshError::Spawn(error) => {
            crate::fail(&format!("bn dispatch: failed to run ssh: {error}"))
        }
        DispatchSshError::Run(run) => crate::fail(&format!(
            "{}\n{}",
            ssh_failure(
                &format!("bn dispatch: remote dispatch on '{alias}' failed"),
                &run,
            ),
            ssh_next_steps(alias)
        )),
    });
    if !json {
        println!("  watch it: bn connect {alias}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(alias: &str) -> Host {
        Host {
            alias: alias.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn host_lookup_reports_known_aliases_without_ssh() {
        let all = [host("fedora"), host("builder")];
        assert_eq!(
            host_by_alias(&all, "builder").map(|h| h.alias.as_str()),
            Some("builder")
        );
        assert_eq!(host_by_alias(&all, "missing"), None);
        assert_eq!(known_hosts_hint(&all), "known hosts: fedora, builder");
        assert_eq!(
            known_hosts_hint(&[]),
            "no Host stanzas found in ~/.ssh/config"
        );
    }

    #[test]
    fn numbered_picker_selection_accepts_index_or_alias_without_ssh() {
        let all = [host("fedora"), host("builder")];
        assert_eq!(
            selected_host(&all, " 2 ")
                .unwrap()
                .map(|h| h.alias.as_str()),
            Some("builder")
        );
        assert_eq!(
            selected_host(&all, "fedora")
                .unwrap()
                .map(|h| h.alias.as_str()),
            Some("fedora")
        );
        assert_eq!(selected_host(&all, " \n").unwrap(), None);
        let numeric = selected_host(&all, "9").unwrap_err();
        assert!(numeric.starts_with("unknown host '9'"));
        assert!(numeric.contains("try `ssh 9`"));
        assert!(numeric.contains("run `bn doctor`"));
        let missing = selected_host(&all, "missing").unwrap_err();
        assert!(missing.starts_with("unknown host 'missing'"));
        assert!(missing.contains("Host stanza in ~/.ssh/config"));
        assert!(missing.contains("try `ssh missing`"));
    }

    #[test]
    fn remote_dispatch_ssh_policy_is_single_and_bounded() {
        assert_eq!(
            DISPATCH_SSH_OPTIONS,
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ]
        );
    }

    #[test]
    fn preview_and_interactive_ssh_policies_are_bounded() {
        assert_eq!(
            PREVIEW_SSH_OPTIONS,
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=3",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ]
        );
        assert_eq!(
            INTERACTIVE_SSH_OPTIONS,
            [
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ]
        );
        assert!(!INTERACTIVE_SSH_OPTIONS.contains(&"BatchMode=yes"));
    }

    #[test]
    fn ssh_failure_reports_exit_status_and_stderr() {
        let status = Command::new("sh")
            .args(["-c", "exit 23"])
            .status()
            .expect("sh should run");
        let run = SshRun {
            status,
            stdout: Vec::new(),
            stderr: b"Permission denied (publickey).\n".to_vec(),
            timed_out: false,
        };
        assert_eq!(
            ssh_failure("ssh failed", &run),
            "ssh failed (exit status: 23)\nPermission denied (publickey)."
        );
    }

    #[test]
    fn ssh_failure_still_reports_status_without_stderr() {
        let status = Command::new("sh")
            .args(["-c", "exit 255"])
            .status()
            .expect("sh should run");
        let run = SshRun {
            status,
            stdout: Vec::new(),
            stderr: b" \n".to_vec(),
            timed_out: false,
        };
        assert_eq!(
            ssh_failure("ssh failed", &run),
            "ssh failed (exit status: 255)"
        );
    }

    #[test]
    fn only_pre_connection_ssh_failures_are_retryable() {
        let status = Command::new("sh")
            .args(["-c", "exit 255"])
            .status()
            .expect("sh should run");
        let transient = SshRun {
            status,
            stdout: Vec::new(),
            stderr: b"ssh: connect to host fedora port 22: Connection refused\n".to_vec(),
            timed_out: false,
        };
        assert!(ssh_connect_failure_is_transient(&transient));

        let auth = SshRun {
            status,
            stdout: Vec::new(),
            stderr: b"Permission denied (publickey).\n".to_vec(),
            timed_out: false,
        };
        assert!(!ssh_connect_failure_is_transient(&auth));

        let indeterminate = SshRun {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
        };
        assert!(!ssh_connect_failure_is_transient(&indeterminate));
    }

    #[test]
    fn ssh_next_steps_are_actionable_for_the_selected_alias() {
        let hint = ssh_next_steps("fedora");
        assert!(hint.contains("try `ssh fedora`"));
        assert!(hint.contains("key auth"));
        assert!(hint.contains("Host stanza in ~/.ssh/config"));
        assert!(hint.contains("network"));
        assert!(hint.contains("run `bn doctor`"));
    }

    #[test]
    fn bounded_ssh_run_terminates_a_stalled_command() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5 & wait"]);
        let started = Instant::now();
        let run = run_ssh_with_timeout(command, Duration::from_millis(25), false)
            .expect("stalled command should be terminated");
        assert!(run.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            ssh_failure("ssh failed", &run),
            "ssh failed (command timed out)"
        );
    }

    #[test]
    fn remote_attach_command_preserves_create_and_shell_fallback() {
        let command = remote_attach_command(false);
        assert!(command.contains("exec tmux new-session -A -s bn"));
        assert!(command.contains("exec \"${SHELL:-/bin/sh}\" -l"));
        assert!(!command.contains("attach-session -r"));
    }

    #[test]
    fn read_only_remote_attach_never_creates_the_session() {
        let command = remote_attach_command(true);
        assert!(command.contains("tmux has-session -t bn"));
        assert!(command.contains("exec tmux attach-session -r -t bn"));
        assert!(command.contains("exec \"${SHELL:-/bin/sh}\" -l"));
        assert!(!command.contains("new-session"));
    }

    #[test]
    fn nested_session_command_reuses_the_interactive_ssh_contract() {
        let command = ssh_shell_command("fedora", true);
        assert!(command.starts_with("ssh "));
        assert!(command.contains("'ConnectTimeout=10'"));
        assert!(command.contains("'ServerAliveInterval=15'"));
        assert!(command.contains("'ServerAliveCountMax=3'"));
        assert!(command.contains("'fedora'"));
        assert!(command.contains("attach-session -r -t bn"));
        assert!(command.contains("status=$?"));
        assert!(command.contains("failed (exit status: %s)"));
        assert!(command.contains("SSH stderr is shown above"));
        assert!(command.contains("try `ssh fedora`"));
        assert!(command.contains("run `bn doctor`"));
        assert!(command.contains("exec \"${SHELL:-/bin/sh}\" -l"));
        assert!(Command::new("sh")
            .args(["-n", "-c", &command])
            .status()
            .expect("sh should parse the nested command")
            .success());
    }

    #[test]
    fn direct_and_nested_attach_use_identical_ssh_arguments() {
        let command = interactive_ssh("fedora", true);
        let direct_args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(direct_args, ssh_args("fedora", true));
    }

    #[test]
    fn colon_named_host_session_is_resolved_by_id() {
        let listing = "planning\t$0\nhost:fedora\t$1\nhost:other\t$2\n";
        assert_eq!(
            session_id_from_listing(listing, "host:fedora").as_deref(),
            Some("$1")
        );
        assert_eq!(session_id_from_listing(listing, "host:missing"), None);
    }

    #[test]
    fn managed_tmux_config_contains_host_nesting_contract() {
        let config = include_str!("../../../config/workflow/tmux.conf");
        assert!(config.contains("set-hook -g client-session-changed"));
        assert!(config.contains("#{m:host:*,#{session_name}}"));
        assert!(config.contains("set -g status off ; set key-table off"));
        assert!(config.contains("set -g status 2 ; set -u key-table"));
        assert!(config.contains("bind -T off F12 switch-client -l"));
    }
}
