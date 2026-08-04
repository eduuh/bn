//! SSH remote-exec transport (#718, epic #707 M1) — the reusable, hardened transport the
//! orchestrator/daemon uses to reach a worker node. Factors the one-shot SSH shape that lived
//! inside `bn connect`'s `dispatch_remote` into a library API the scheduler can call:
//! [`exec`], [`probe`], and [`poll_status`].
//!
//! Design invariants (the hardening from epic #707 v4):
//!
//! - **Structured, not shell-exec.** ssh is invoked through an argv API
//!   ([`std::process::Command`] with separate args), *never* a local `sh -c`. Every remote
//!   argument the caller supplies is individually POSIX-single-quoted ([`shq`]) before it reaches
//!   the remote shell, so a hostile alias/arg (`; rm -rf /`) can never break out. Only a
//!   fixed compile-time [`REMOTE_PATH`] prelude (no user input) is left unquoted.
//! - **Trusted inventory only.** Hosts are resolved *exclusively* through the registered
//!   `~/.ssh/config` + `[hosts]` inventory ([`resolve_host`]); an arbitrary alias — or one bearing
//!   shell/ssh-option metacharacters — is rejected before any process spawns. There is no path
//!   from caller input to an ssh *option*.
//! - **Fail-closed hardening.** Every op carries `BatchMode=yes` (key auth only — no password
//!   prompting), a `ConnectTimeout`, `ServerAliveInterval`/`ServerAliveCountMax` (a TCP blackhole
//!   errors instead of hanging), `IdentitiesOnly=yes` (the daemon key isn't shadowed by an agent
//!   offer), and `StrictHostKeyChecking=yes` (a changed host key fails closed). An *external*
//!   command deadline bounds the whole subprocess — `ConnectTimeout` alone doesn't bound command
//!   execution or a mid-stream mux stall.
//! - **One connection per worker.** Every op carries a daemon-owned `ControlMaster=auto`,
//!   `ControlPath`, and `ControlPersist`, so launch, status polling, and future sync calls to the
//!   same worker share one authenticated SSH connection without relying on the user's ssh config.
//! - **3-way outcome, not binary.** [`ExecOutcome`] is `NotStarted | Completed | Indeterminate`.
//!   A mid-exec disconnect (external timeout) is `Indeterminate` — the remote command may or may
//!   not have run — so a *mutating* op is never blind-retried. Only non-mutating reads
//!   ([`probe`]/[`poll_status`]) retry, and never on an auth/host-key/inventory error.
//!
//! The ssh subprocess goes through the injectable [`SshRunner`] seam so the whole transport is
//! unit-testable with a [`FakeSshRunner`] — no network, no real `ssh`.

use crate::hosts::{self, Host};
use crate::resolver::Config;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// PATH prelude for remote commands: `ssh <host> '<cmd>'` runs a non-login shell whose PATH often
/// misses the user bin dirs (`~/.cargo/bin`, homebrew), so extend it explicitly. This is a fixed
/// compile-time constant with **no** user input, which is why it is the one part of the remote
/// command string left unquoted.
pub const REMOTE_PATH: &str = "export PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:$HOME/.bin:/opt/homebrew/bin:/usr/local/bin:$PATH\";";

/// `~/.ssh`, overridable with `$BN_SSH_DIR` (the test contract — hosts come from
/// `<ssh_dir>/config`). Mirrors `bn-cli`'s `connect::ssh_dir` so the transport and the human
/// picker resolve the same inventory.
fn ssh_dir() -> PathBuf {
    match std::env::var("BN_SSH_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_default().join(".ssh"),
    }
}

/// Private directory for daemon-owned SSH control sockets. `%C` in the socket filename is
/// expanded by OpenSSH to a hash of the local host, remote host, port, and user, giving every
/// worker endpoint one stable socket without placing raw aliases in a filesystem path.
fn control_dir() -> PathBuf {
    ssh_dir().join("bn-control")
}

fn control_path() -> String {
    control_dir().join("%C").to_string_lossy().into_owned()
}

/// Prepare the private mux directory before any real transport is constructed. If this fails the
/// caller treats the SSH node as unavailable, leaving the daemon's local execution path intact.
fn prepare_control_dir() -> Result<(), TransportError> {
    let dir = control_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        TransportError::MuxSetup(format!("could not create {}: {e}", dir.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            TransportError::MuxSetup(format!("could not secure {}: {e}", dir.display()))
        })?;
    }
    Ok(())
}

/// The hardened SSH option profile every transport op runs under. Defaults to the fail-closed
/// daemon profile; the fields exist so a caller (or a test) can widen a timeout without reaching
/// into the argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshOptions {
    /// `ConnectTimeout` — seconds to establish the TCP/auth handshake.
    pub connect_timeout_secs: u32,
    /// External wall-clock deadline on the *whole* ssh subprocess. Bounds command execution and
    /// mux stalls that `ConnectTimeout` does not.
    pub command_timeout: Duration,
    /// `ServerAliveInterval` — seconds between keepalive probes on an established channel.
    pub server_alive_interval: u32,
    /// `ServerAliveCountMax` — missed keepalives before ssh tears the connection down.
    pub server_alive_count_max: u32,
    /// `BatchMode=yes` — never prompt for a password/passphrase (key auth only). The daemon path
    /// requires this so a wedged auth prompt fails fast instead of hanging.
    pub batch_mode: bool,
    /// `IdentitiesOnly=yes` — offer only the configured key, so an ssh-agent identity can't be
    /// tried first and hit a forced-command.
    pub identities_only: bool,
    /// `StrictHostKeyChecking=yes` — fail closed on an unknown/changed host key.
    pub strict_host_key_checking: bool,
    /// How many *additional* attempts a non-mutating read op ([`probe`]/[`poll_status`]) makes
    /// after a transient failure. Mutating [`exec`] ignores this (never auto-retries).
    pub read_retries: u32,
}

impl Default for SshOptions {
    /// The fail-closed daemon profile: key-auth only, strict host keys, bounded everywhere.
    fn default() -> Self {
        SshOptions {
            connect_timeout_secs: 5,
            command_timeout: Duration::from_secs(30),
            server_alive_interval: 15,
            server_alive_count_max: 3,
            batch_mode: true,
            identities_only: true,
            strict_host_key_checking: true,
            read_retries: 1,
        }
    }
}

impl SshOptions {
    /// The `-o KEY=VALUE` argv fragment for this profile. Each option flag and its value is a
    /// *separate* argv element — they are passed to `ssh` directly, never through a shell, so no
    /// value here is ever re-interpreted. Order is stable for deterministic tests.
    pub fn option_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let mut push = |k: &str, v: String| {
            args.push("-o".to_string());
            args.push(format!("{k}={v}"));
        };
        if self.batch_mode {
            push("BatchMode", "yes".to_string());
        }
        push("ConnectTimeout", self.connect_timeout_secs.to_string());
        push(
            "ServerAliveInterval",
            self.server_alive_interval.to_string(),
        );
        push(
            "ServerAliveCountMax",
            self.server_alive_count_max.to_string(),
        );
        if self.identities_only {
            push("IdentitiesOnly", "yes".to_string());
        }
        push(
            "StrictHostKeyChecking",
            if self.strict_host_key_checking {
                "yes".to_string()
            } else {
                "accept-new".to_string()
            },
        );
        push("ControlMaster", "auto".to_string());
        push("ControlPath", control_path());
        push("ControlPersist", "600".to_string());
        args
    }
}

/// A typed transport failure. Every failure is one of these — nothing fails silently. The
/// classification drives retry policy: [`Self::is_retryable`] is true only for the transient
/// connection-level failures a non-mutating read may safely re-attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The alias is not in the trusted inventory.
    HostNotFound { alias: String, known: Vec<String> },
    /// The alias is known but not registered under `[hosts]` — the per-host opt-in the
    /// unattended transport requires.
    NotRegistered { alias: String },
    /// The alias contains characters that must never reach an ssh argv (shell/option
    /// metacharacters, whitespace, a leading `-`). Rejected before any spawn.
    InvalidAlias { alias: String },
    /// The `ssh` binary could not be spawned at all (not on PATH, etc.).
    Spawn(String),
    /// The daemon-owned ControlMaster socket directory could not be created or secured.
    MuxSetup(String),
    /// Key-auth failed or the host key changed. **Never** auto-retried — a retry can't fix
    /// credentials and a changed key is a fail-closed security event.
    AuthOrHostKey(String),
    /// The connection failed at the transport level (ssh exited 255) for a transient reason —
    /// unreachable, connection refused, mux stall. Retryable for reads.
    Connection(String),
    /// The external command deadline fired. For a mutating op this surfaces as
    /// [`ExecOutcome::Indeterminate`]; the reads treat it as a retryable transient.
    Timeout,
    /// A read op ran but its output could not be parsed as expected (e.g. non-JSON status).
    Parse(String),
}

impl TransportError {
    /// Whether a *non-mutating* read op may safely re-attempt after this error. Only the
    /// transient connection-level failures qualify; inventory, auth, and host-key errors never do.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransportError::Connection(_) | TransportError::Timeout
        )
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::HostNotFound { alias, known } => {
                let hint = if known.is_empty() {
                    "no Host stanzas found in ~/.ssh/config".to_string()
                } else {
                    format!("known hosts: {}", known.join(", "))
                };
                write!(f, "no ssh-config Host '{alias}' — {hint}")
            }
            TransportError::NotRegistered { alias } => write!(
                f,
                "host '{alias}' is not registered under [hosts] — the unattended transport \
                 requires the per-host opt-in"
            ),
            TransportError::InvalidAlias { alias } => {
                write!(f, "invalid host alias '{alias}' (illegal characters)")
            }
            TransportError::Spawn(e) => write!(f, "failed to spawn ssh: {e}"),
            TransportError::MuxSetup(e) => write!(f, "ssh multiplexing unavailable: {e}"),
            TransportError::AuthOrHostKey(e) => write!(f, "ssh auth/host-key failure: {e}"),
            TransportError::Connection(e) => write!(f, "ssh connection failed: {e}"),
            TransportError::Timeout => write!(f, "ssh command timed out (external deadline)"),
            TransportError::Parse(e) => write!(f, "could not parse remote output: {e}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The 3-way outcome of a remote exec — never a bare success/fail boolean. The distinction is
/// load-bearing for retry safety: only [`Completed`](ExecOutcome::Completed) is a definite result;
/// [`Indeterminate`](ExecOutcome::Indeterminate) means the remote command *may* have run, so a
/// mutating op must reconcile (not blind-retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOutcome {
    /// The remote command definitely never ran — the connection never established. Safe to retry.
    NotStarted,
    /// The remote command ran to completion. `exit_status` is the remote process's status.
    Completed {
        exit_status: i32,
        stdout: String,
        stderr: String,
    },
    /// The subprocess was killed by the external deadline (or the channel dropped mid-exec). The
    /// remote command's fate is unknown — never blind-retry a mutating op on this.
    Indeterminate {
        partial_stdout: String,
        partial_stderr: String,
    },
}

/// POSIX single-quote a remote argument so no metacharacter can break out of the remote shell.
/// Wraps in `'…'` and rewrites embedded `'` as the classic `'\''`. This is the boundary that
/// makes caller-supplied remote args injection-safe.
pub fn shq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Build the single remote-command string from a structured argv: the fixed [`REMOTE_PATH`]
/// prelude (unquoted, no user input) followed by every caller arg individually [`shq`]-quoted.
/// This is the only place caller input meets the remote shell, and it is fully quoted.
pub fn build_remote_command(argv: &[&str]) -> String {
    let quoted: Vec<String> = argv.iter().map(|a| shq(a)).collect();
    format!("{REMOTE_PATH} {}", quoted.join(" "))
}

/// Build the full local `ssh` argv for one op: the hardened options, the trusted destination
/// alias, then the assembled remote command as a *single* final argument. The alias is assumed
/// already validated by [`validate_alias`]; the remote command is assembled by
/// [`build_remote_command`]. Every element is a discrete argv entry — there is no shell.
pub fn build_ssh_argv(opts: &SshOptions, alias: &str, remote_argv: &[&str]) -> Vec<String> {
    let mut argv = opts.option_args();
    // `--` terminates option parsing so an (already validated) alias can never be read as a flag.
    argv.push("--".to_string());
    argv.push(alias.to_string());
    argv.push(build_remote_command(remote_argv));
    argv
}

/// Reject any alias that must never reach an ssh argv: empty, leading `-` (would look like a
/// flag), or containing whitespace / shell / ssh-option metacharacters. The trusted inventory is
/// the real gate — this is defense in depth so even a corrupt config entry can't inject.
pub fn validate_alias(alias: &str) -> Result<(), TransportError> {
    let bad = alias.is_empty()
        || alias.starts_with('-')
        || alias.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    ';' | '&'
                        | '|'
                        | '`'
                        | '$'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '\''
                        | '"'
                        | '\\'
                        | '='
                        | '\n'
                        | '\r'
                )
        });
    if bad {
        Err(TransportError::InvalidAlias {
            alias: alias.to_string(),
        })
    } else {
        Ok(())
    }
}

/// Resolve `alias` through the trusted inventory ONLY. Returns the [`Host`] iff the alias is a
/// real ssh-config Host, is registered under `[hosts]` (the unattended opt-in), and passes
/// [`validate_alias`]. There is no path from an arbitrary caller string to a spawned ssh.
pub fn resolve_host(cfg: &Config, alias: &str) -> Result<Host, TransportError> {
    validate_alias(alias)?;
    let all = hosts::load(&ssh_dir(), &cfg.hosts);
    let Some(host) = all.iter().find(|h| h.alias == alias).cloned() else {
        return Err(TransportError::HostNotFound {
            alias: alias.to_string(),
            known: all.iter().map(|h| h.alias.clone()).collect(),
        });
    };
    if !host.registered {
        return Err(TransportError::NotRegistered {
            alias: alias.to_string(),
        });
    }
    Ok(host)
}

/// The captured result of one ssh subprocess run through the [`SshRunner`] seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshRun {
    /// ssh spawned and exited. `status` is `None` when the external deadline killed it.
    Spawned {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// ssh could not be spawned at all (binary missing, etc.).
    SpawnFailed(String),
}

/// Injectable ssh runner. The real impl ([`RealSshRunner`]) spawns `ssh` with an external
/// deadline; tests use a [`FakeSshRunner`] to drive the transport with canned results and to
/// assert on the exact argv that would be spawned.
pub trait SshRunner {
    /// Run `ssh <argv>`, optionally feeding `stdin`, bounded by `timeout`. Never panics — a spawn
    /// failure is [`SshRun::SpawnFailed`], a deadline kill is [`SshRun::Spawned`] with
    /// `status: None`.
    fn run(&self, argv: &[String], stdin: Option<&[u8]>, timeout: Duration) -> SshRun;
}

/// Production runner: spawns `ssh` with piped stdio, drains stdout/stderr on background threads
/// (so a chatty remote can't deadlock on a full pipe), and enforces `timeout` as a hard external
/// deadline by killing the child. std-only — no `wait-timeout` dependency.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSshRunner;

impl SshRunner for RealSshRunner {
    fn run(&self, argv: &[String], stdin: Option<&[u8]>, timeout: Duration) -> SshRun {
        let timer = crate::log::enabled().then(crate::log::Timer::start);
        let mut child = match Command::new("ssh")
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let run = SshRun::SpawnFailed(e.to_string());
                log_ssh(argv, &run, timer);
                return run;
            }
        };

        if let Some(bytes) = stdin {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(bytes);
                // drop closes stdin so the remote read terminates
            }
        } else {
            drop(child.stdin.take());
        }

        // Drain both pipes on threads so a full pipe can't wedge the child before we time out.
        let out_pipe = child.stdout.take();
        let err_pipe = child.stderr.take();
        let out_handle = std::thread::spawn(move || drain(out_pipe));
        let err_handle = std::thread::spawn(move || drain(err_pipe));

        let deadline = Instant::now() + timeout;
        let mut killed = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.code(),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        killed = true;
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break None,
            }
        };

        let stdout = out_handle.join().unwrap_or_default();
        let stderr = err_handle.join().unwrap_or_default();
        let run = SshRun::Spawned {
            status: if killed { None } else { status },
            stdout,
            stderr,
        };
        log_ssh(argv, &run, timer);
        run
    }
}

/// Emit a redacted action-log record for an ssh transport run. No-op when the action log is off
/// (`timer` is `None`). Only argv, spawn/exit outcome, and duration are logged — never the remote
/// stdout/stderr (they can be large and carry secrets).
fn log_ssh(argv: &[String], run: &SshRun, timer: Option<crate::log::Timer>) {
    let Some(timer) = timer else {
        return;
    };
    let result = match run {
        SshRun::Spawned { status, .. } => serde_json::json!({
            "spawned": true,
            "status": status,
            "timed_out": status.is_none(),
        }),
        SshRun::SpawnFailed(e) => serde_json::json!({ "spawned": false, "error": e }),
    };
    crate::log::record(
        crate::log::LogLevel::Debug,
        "ssh",
        "exec",
        serde_json::json!({ "argv": crate::log::redact_args(argv) }),
        result,
        Some(timer.elapsed_ms()),
    );
}

/// Read a child pipe to end-of-stream into a lossy-UTF-8 string. `None` (pipe not captured) or a
/// read error yields an empty string — best-effort, matching the fail-open capture callers.
fn drain<R: Read>(pipe: Option<R>) -> String {
    let mut s = String::new();
    if let Some(mut p) = pipe {
        let mut buf = Vec::new();
        if p.read_to_end(&mut buf).is_ok() {
            s = String::from_utf8_lossy(&buf).into_owned();
        }
    }
    s
}

/// Classify a completed [`SshRun`] into a typed error, or `None` when it is a clean remote result.
/// ssh exit 255 is a *transport* failure (the remote command never ran); its stderr tells us
/// whether it was auth/host-key (never retry) or a transient connection error (retryable).
fn classify(run: &SshRun) -> Option<TransportError> {
    match run {
        SshRun::SpawnFailed(e) => Some(TransportError::Spawn(e.clone())),
        SshRun::Spawned { status: None, .. } => Some(TransportError::Timeout),
        SshRun::Spawned {
            status: Some(255),
            stderr,
            ..
        } => {
            let low = stderr.to_lowercase();
            if low.contains("permission denied")
                || low.contains("host key verification failed")
                || low.contains("remote host identification has changed")
                || low.contains("no matching host key")
            {
                Some(TransportError::AuthOrHostKey(first_line(stderr)))
            } else {
                Some(TransportError::Connection(first_line(stderr)))
            }
        }
        SshRun::Spawned { .. } => None,
    }
}

/// First non-empty line of an ssh stderr blob, trimmed — the human-facing reason without the noise.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn master_check_argv(opts: &SshOptions, alias: &str) -> Vec<String> {
    let mut argv = opts.option_args();
    argv.extend([
        "-O".to_string(),
        "check".to_string(),
        "--".to_string(),
        alias.to_string(),
    ]);
    argv
}

fn master_start_argv(opts: &SshOptions, alias: &str) -> Vec<String> {
    let mut argv = opts.option_args();
    argv.extend([
        "-M".to_string(),
        "-N".to_string(),
        "-f".to_string(),
        "--".to_string(),
        alias.to_string(),
    ]);
    argv
}

/// Reuse an existing daemon-owned master or establish it once before a transport operation can
/// run. The explicit `-M` start makes a control-socket bind failure fatal instead of allowing ssh
/// to continue as an un-multiplexed one-shot connection.
fn ensure_control_master<R: SshRunner>(
    opts: &SshOptions,
    alias: &str,
    runner: &R,
) -> Result<(), TransportError> {
    let check = runner.run(&master_check_argv(opts, alias), None, opts.command_timeout);
    if matches!(
        check,
        SshRun::Spawned {
            status: Some(0),
            ..
        }
    ) {
        return Ok(());
    }

    let start = runner.run(&master_start_argv(opts, alias), None, opts.command_timeout);
    match classify(&start) {
        Some(e) => Err(e),
        None => match start {
            SshRun::Spawned {
                status: Some(0), ..
            } => {
                let verify =
                    runner.run(&master_check_argv(opts, alias), None, opts.command_timeout);
                if matches!(
                    verify,
                    SshRun::Spawned {
                        status: Some(0),
                        ..
                    }
                ) {
                    Ok(())
                } else {
                    let reason = match &verify {
                        SshRun::Spawned { status, stderr, .. } => {
                            let detail = first_line(stderr);
                            if detail.is_empty() {
                                format!("control check exited {status:?}")
                            } else {
                                detail
                            }
                        }
                        SshRun::SpawnFailed(e) => e.clone(),
                    };
                    Err(TransportError::MuxSetup(format!(
                        "master for '{alias}' was not available after startup: {reason}"
                    )))
                }
            }
            SshRun::Spawned { status, stderr, .. } => Err(TransportError::MuxSetup(format!(
                "master for '{alias}' exited {}: {}",
                status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "without a status".to_string()),
                first_line(&stderr)
            ))),
            SshRun::SpawnFailed(error) => Err(TransportError::Spawn(error)),
        },
    }
}

/// The transport bound to a resolved host and an ssh runner. Constructed via [`Transport::connect`]
/// (real ssh) or [`Transport::with_runner`] (a test runner). All ops resolve through the trusted
/// inventory; there is no constructor that takes a raw hostname.
pub struct Transport<'a, R: SshRunner> {
    host: Host,
    opts: SshOptions,
    runner: &'a R,
}

impl<'a> Transport<'a, RealSshRunner> {
    /// Resolve `alias` through the trusted inventory and bind a real-ssh transport to it. This is
    /// the daemon entry point.
    pub fn connect(
        cfg: &Config,
        alias: &str,
        opts: SshOptions,
        runner: &'a RealSshRunner,
    ) -> Result<Self, TransportError> {
        let host = resolve_host(cfg, alias)?;
        prepare_control_dir()?;
        ensure_control_master(&opts, alias, runner)?;
        Ok(Transport { host, opts, runner })
    }
}

impl<'a, R: SshRunner> Transport<'a, R> {
    /// Bind a transport to an already-resolved host with an arbitrary runner. The host MUST have
    /// come from [`resolve_host`]; used by tests and by callers that resolved earlier.
    pub fn with_runner(host: Host, opts: SshOptions, runner: &'a R) -> Self {
        Transport { host, opts, runner }
    }

    /// The bound host's alias.
    pub fn alias(&self) -> &str {
        &self.host.alias
    }

    /// One ssh attempt of `remote_argv`, mapping the run to a typed error or a raw [`SshRun`].
    fn attempt(
        &self,
        remote_argv: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<SshRun, TransportError> {
        let argv = build_ssh_argv(&self.opts, &self.host.alias, remote_argv);
        let run = self.runner.run(&argv, stdin, self.opts.command_timeout);
        match classify(&run) {
            Some(e) => Err(e),
            None => Ok(run),
        }
    }

    /// Run a **mutating** remote command. Never auto-retries: a definite result is
    /// [`ExecOutcome::Completed`]; a deadline/disconnect is [`ExecOutcome::Indeterminate`] (the
    /// caller must reconcile, not blind-retry); a definite connection failure is
    /// [`ExecOutcome::NotStarted`]; auth/host-key/spawn errors surface as [`TransportError`].
    pub fn exec(
        &self,
        remote_argv: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<ExecOutcome, TransportError> {
        match self.attempt(remote_argv, stdin) {
            Ok(SshRun::Spawned {
                status: Some(code),
                stdout,
                stderr,
            }) => Ok(ExecOutcome::Completed {
                exit_status: code,
                stdout,
                stderr,
            }),
            // Unreachable: classify() maps status:None and SpawnFailed to errors below.
            Ok(_) => Ok(ExecOutcome::NotStarted),
            Err(TransportError::Timeout) => Ok(ExecOutcome::Indeterminate {
                partial_stdout: String::new(),
                partial_stderr: String::new(),
            }),
            Err(TransportError::Connection(_)) => Ok(ExecOutcome::NotStarted),
            Err(e) => Err(e),
        }
    }

    /// Run a **non-mutating** read command, retrying on a transient failure up to
    /// [`SshOptions::read_retries`] extra attempts. Returns the [`ExecOutcome::Completed`] result;
    /// a timeout after exhausting retries is [`ExecOutcome::Indeterminate`]. Auth/host-key/spawn
    /// and inventory errors are never retried.
    fn read(&self, remote_argv: &[&str]) -> Result<ExecOutcome, TransportError> {
        let mut last: Option<TransportError> = None;
        for _ in 0..=self.opts.read_retries {
            match self.attempt(remote_argv, None) {
                Ok(SshRun::Spawned {
                    status: Some(code),
                    stdout,
                    stderr,
                }) => {
                    return Ok(ExecOutcome::Completed {
                        exit_status: code,
                        stdout,
                        stderr,
                    });
                }
                Ok(_) => return Ok(ExecOutcome::NotStarted),
                Err(e) if e.is_retryable() => {
                    last = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        match last {
            Some(TransportError::Timeout) => Ok(ExecOutcome::Indeterminate {
                partial_stdout: String::new(),
                partial_stderr: String::new(),
            }),
            Some(e) => Err(e),
            None => Ok(ExecOutcome::NotStarted),
        }
    }

    /// Cheap reachability probe: is the worker reachable with key auth and is `bn` present? Runs
    /// `command -v bn` remotely. `Ok(true)` = reachable + bn present, `Ok(false)` = reachable but
    /// bn missing; a transport error means unreachable/auth. Retries as a non-mutating read.
    pub fn probe(&self) -> Result<bool, TransportError> {
        match self.read(&["command", "-v", "bn"])? {
            ExecOutcome::Completed { exit_status, .. } => Ok(exit_status == 0),
            ExecOutcome::NotStarted => Err(TransportError::Connection(
                "probe did not establish a connection".to_string(),
            )),
            ExecOutcome::Indeterminate { .. } => Err(TransportError::Timeout),
        }
    }

    /// Poll the worker's `bn worker status --json` payload, returning the parsed JSON value (the
    /// success envelope's `data`, or the raw document if it isn't enveloped). Retries as a
    /// non-mutating read; a non-zero remote exit or non-JSON output is a typed [`TransportError`].
    pub fn poll_status(&self) -> Result<serde_json::Value, TransportError> {
        match self.read(&["bn", "worker", "status", "--json"])? {
            ExecOutcome::Completed {
                exit_status: 0,
                stdout,
                ..
            } => {
                let v: serde_json::Value = serde_json::from_str(stdout.trim())
                    .map_err(|e| TransportError::Parse(e.to_string()))?;
                Ok(v.get("data").cloned().unwrap_or(v))
            }
            ExecOutcome::Completed {
                exit_status,
                stderr,
                ..
            } => Err(TransportError::Parse(format!(
                "remote `bn worker status` exited {exit_status}: {}",
                first_line(&stderr)
            ))),
            ExecOutcome::NotStarted => Err(TransportError::Connection(
                "status poll did not establish a connection".to_string(),
            )),
            ExecOutcome::Indeterminate { .. } => Err(TransportError::Timeout),
        }
    }
}

/// A test runner that records every argv it is handed and replays scripted [`SshRun`] results in
/// order (the last is repeated once exhausted). Lets tests assert on the exact ssh invocation and
/// drive the retry/outcome logic without a real ssh.
#[derive(Debug, Default)]
pub struct FakeSshRunner {
    scripted: std::cell::RefCell<std::collections::VecDeque<SshRun>>,
    pub calls: std::cell::RefCell<Vec<Vec<String>>>,
    pub stdins: std::cell::RefCell<Vec<Option<Vec<u8>>>>,
}

impl FakeSshRunner {
    /// A runner that replays `runs` in order.
    pub fn new(runs: Vec<SshRun>) -> Self {
        FakeSshRunner {
            scripted: std::cell::RefCell::new(runs.into_iter().collect()),
            calls: std::cell::RefCell::new(Vec::new()),
            stdins: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl SshRunner for FakeSshRunner {
    fn run(&self, argv: &[String], stdin: Option<&[u8]>, _timeout: Duration) -> SshRun {
        self.calls.borrow_mut().push(argv.to_vec());
        self.stdins.borrow_mut().push(stdin.map(|b| b.to_vec()));
        let mut q = self.scripted.borrow_mut();
        if q.len() > 1 {
            q.pop_front()
                .unwrap_or(SshRun::SpawnFailed("no scripted run".to_string()))
        } else {
            q.front()
                .cloned()
                .unwrap_or(SshRun::SpawnFailed("no scripted run".to_string()))
        }
    }
}

