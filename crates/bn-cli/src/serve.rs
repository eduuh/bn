//! `bn serve` — the thin launcher for the `bn-web` dashboard server (#388), plus its managed-service
//! lifecycle (#499).
//!
//! bn-web (axum + tokio) is a separate binary so its async stack stays out of bn-cli's dependency
//! tree — exactly how `bn-mcp` sits beside `bn`. This command resolves the `bn-web` binary next to
//! the running `bn` (falling back to `PATH`) and execs it. `--daemon` registers a managed,
//! auto-restarting service so the dashboard stays up across crashes and reboots: a launchd agent on
//! macOS, a systemd `--user` service on Linux, and a detached child as the last-resort fallback.
//! `bn serve status` reports an honest running/down/not-installed verdict; `bn serve stop` unloads
//! the service.

use crate::*;
use bn_core::serve::{service_state, ServeState};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// The `bn-web` binary name for the current platform.
fn bn_web_name() -> &'static str {
    if cfg!(windows) {
        "bn-web.exe"
    } else {
        "bn-web"
    }
}

/// Resolve the `bn-web` binary: prefer the sibling next to the running `bn` (same install dir, so a
/// from-source or release install runs its matching build), else fall back to the bare name on
/// `PATH`. Mirrors how `bn-mcp` is resolved beside `bn` (doctor.rs `bindir.join("bn-mcp")`).
///
/// Canonicalize first, for the same reason as `resolve_bn_mcp`: bn is commonly launched through
/// the `<config_dir>/bn -> <bin_dir>/bn` anchor symlink (every cron unit spawns bn that way), and
/// `current_exe()` reports the symlink, whose directory holds no `bn-web`. Without this, a
/// `bn serve` reached through the anchor falls through to the bare name on `PATH` — which is
/// empty for a launchd/systemd job — and the dashboard silently never starts.
fn resolve_bn_web() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            let sibling = real.with_file_name(bn_web_name());
            if sibling.is_file() {
                return sibling;
            }
        }
        let sibling = exe.with_file_name(bn_web_name());
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(bn_web_name())
}

/// The argv passed through to `bn-web` from the `bn serve` flags.
fn web_args(port: Option<u16>, lan: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(p) = port {
        args.push("--port".to_string());
        args.push(p.to_string());
    }
    if lan {
        args.push("--lan".to_string());
    }
    args
}

/// `bn serve [status|stop]` dispatch. Bare `bn serve` (`action: None`) runs/daemonizes the server;
/// the `status` / `stop` subcommands manage the daemon's lifecycle.
pub(crate) fn run_cmd(
    action: Option<ServeCmd>,
    port: Option<u16>,
    lan: bool,
    daemon: bool,
    json: bool,
) {
    match action {
        None => run(port, lan, daemon, json),
        Some(ServeCmd::Status { port }) => status(port, json),
        Some(ServeCmd::Stop) => stop(json),
    }
}

/// `bn serve [--port] [--lan] [--daemon]`.
pub(crate) fn run(port: Option<u16>, lan: bool, daemon: bool, json: bool) {
    let bin = resolve_bn_web();
    let args = web_args(port, lan);

    if daemon {
        serve_daemon(&bin, &args, json);
        return;
    }

    // Foreground: replace this process with bn-web (unix exec), so `bn serve` *is* the server and
    // Ctrl-C / signals flow straight through. Non-unix: spawn + wait and forward the exit code.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() only returns on failure.
        let err = std::process::Command::new(&bin).args(&args).exec();
        fail(&format!(
            "bn serve: could not exec {} ({err}) — is bn-web installed beside bn? Try install.sh.",
            bin.display()
        ));
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&bin).args(&args).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(0)),
            Err(e) => fail(&format!(
                "bn serve: could not run {} ({e}) — is bn-web installed beside bn?",
                bin.display()
            )),
        }
    }
}

/// `--daemon`: register a managed, auto-restarting service — a launchd agent (macOS), else a
/// systemd `--user` service (Linux), else a detached child so the server still outlives this shell.
/// Emits an envelope in `--json` mode.
fn serve_daemon(bin: &std::path::Path, args: &[String], json: bool) {
    let bin_str = bin.to_string_lossy().into_owned();
    let (mode, ok) = if crate::launchd::web_install(&bin_str, args).is_ok() {
        ("launchd", true)
    } else if crate::systemd::web_install(&bin_str, args).is_ok() {
        ("systemd", true)
    } else {
        let spawned = spawn_detached(bin, args);
        ("detached", spawned)
    };

    if !ok {
        let msg = format!("bn serve --daemon: could not launch {}", bin.display());
        if json {
            println!(
                "{}",
                Envelope::failure("serve", "spawn_failed", &msg).to_json()
            );
            std::process::exit(1);
        }
        fail(&msg);
    }

    if json {
        let data = serde_json::json!({ "daemon": true, "mode": mode, "bin": bin_str });
        println!("{}", Envelope::success("serve", data).to_json());
        return;
    }
    match mode {
        "launchd" => {
            println!("bn serve: launchd daemon loaded (com.bn.web) — the dashboard runs in the background.");
            println!("  logs: /tmp/com.bn.web.log");
            println!("  status: bn serve status   stop: bn serve stop");
        }
        "systemd" => {
            println!("bn serve: systemd --user service enabled (bn-web.service) — the dashboard runs in the background.");
            println!("  logs: journalctl --user -u bn-web -f");
            println!("  status: bn serve status   stop: bn serve stop");
        }
        _ => println!(
            "bn serve: no service manager available — started a detached server (survives this shell, not a reboot)."
        ),
    }
}

/// `bn serve status` — an honest verdict on the managed dashboard: is a service *installed* (a
/// launchd plist / systemd unit on disk) and is *something listening* on the port. This is the
/// operator-side twin of the dashboard's honest connection banner (#499): "down" means installed
/// but not serving, not an eternal spinner.
fn status(port: Option<u16>, json: bool) {
    let port = resolved_port(port);
    let installed = crate::launchd::web_installed() || crate::systemd::web_installed();
    let listening = port_listening(port);
    let state = service_state(installed, listening);

    if json {
        let data = serde_json::json!({
            "state": state.as_str(),
            "installed": installed,
            "listening": listening,
            "port": port,
        });
        println!("{}", Envelope::success("serve", data).to_json());
        return;
    }
    println!("bn serve: {}", state.summary());
    println!(
        "  port {port}: {}   managed service: {}",
        if listening {
            "listening"
        } else {
            "not listening"
        },
        if installed { "installed" } else { "none" },
    );
    if matches!(state, ServeState::Down) {
        println!("  the service is registered but not answering — check logs, or `bn serve stop` then `bn serve --daemon`.");
    } else if matches!(state, ServeState::NotInstalled) {
        println!("  start it with `bn serve --daemon` (managed) or `bn serve` (foreground).");
    }
}

/// How long to wait between liveness polls while a signalled listener shuts down.
const STOP_POLL: Duration = Duration::from_millis(250);
/// Total polls before giving up (~5s), with the escalation to `SIGKILL` at the halfway mark.
const STOP_POLLS: u32 = 20;

/// What `bn serve stop` actually achieved. Unregistering a service and stopping a server are
/// different things, and the old command reported the first as if it were the second.
#[derive(Debug, PartialEq, Eq)]
enum StopOutcome {
    /// No service installed and nothing on the port.
    Nothing,
    /// A managed service was unregistered; no stray listener was left behind.
    Unregistered,
    /// Listeners on the port were signalled and are gone.
    Terminated(Vec<i32>),
    /// Something still holds the port after unregistering *and* signalling it.
    Stuck(Vec<i32>),
}

/// Decide what actually happened, so the report can be honest about it. Pure: `reaped` is what was
/// listening before we signalled, `still` what is listening after.
fn stop_outcome(was_installed: bool, reaped: &[i32], still: &[i32]) -> StopOutcome {
    if !still.is_empty() {
        return StopOutcome::Stuck(still.to_vec());
    }
    if !reaped.is_empty() {
        return StopOutcome::Terminated(reaped.to_vec());
    }
    if was_installed {
        StopOutcome::Unregistered
    } else {
        StopOutcome::Nothing
    }
}

/// Signal every listener on `port` and wait for the port to actually free up: `SIGTERM` first so
/// bn-web can close its listener cleanly, escalating to `SIGKILL` only if it is still there.
/// Returns the PIDs that were holding the port when we started.
fn reap_listeners(port: u16) -> Vec<i32> {
    let pids = crate::cmd::killport::listening_pids(port);
    if pids.is_empty() {
        return Vec::new();
    }
    for pid in &pids {
        let _ = crate::cmd::killport::send_signal(*pid, crate::cmd::killport::signal_term());
    }
    for poll in 0..STOP_POLLS {
        std::thread::sleep(STOP_POLL);
        if !port_listening(port) {
            break;
        }
        // Halfway through the budget, stop being polite.
        if poll == STOP_POLLS / 2 {
            for pid in crate::cmd::killport::listening_pids(port) {
                let _ = crate::cmd::killport::send_signal(pid, crate::cmd::killport::signal_kill());
            }
        }
    }
    pids
}

/// `bn serve stop` — take the dashboard *down*: unload the managed service on whichever platform
/// installed it (best-effort on both, since only one applies) **and** terminate whatever is still
/// listening on the port.
///
/// Unregistering alone is not stopping. A bn-web reparented to init (PPID 1 — an orphan left by an
/// earlier plist/unit generation) is no longer tracked by launchd/systemd, so the unload never
/// signals it. It keeps the port, and a subsequently registered service crash-loops on
/// `cannot bind 127.0.0.1:7770: Address already in use` — which from the outside is
/// indistinguishable from nothing happening, while this command printed "stopped and unregistered".
fn stop(json: bool) {
    let port = resolved_port(None);
    let was_installed = crate::launchd::web_installed() || crate::systemd::web_installed();
    crate::launchd::web_uninstall();
    crate::systemd::web_uninstall();

    let reaped = reap_listeners(port);
    let still = crate::cmd::killport::listening_pids(port);
    let outcome = stop_outcome(was_installed, &reaped, &still);

    if json {
        let data = serde_json::json!({
            // `stopped` now means what it says: nothing is serving this port any more.
            "stopped": !matches!(outcome, StopOutcome::Stuck(_)),
            "was_installed": was_installed,
            "port": port,
            "terminated": reaped,
            "still_listening": still,
        });
        println!("{}", Envelope::success("serve", data).to_json());
        return;
    }

    match outcome {
        StopOutcome::Nothing => {
            println!("bn serve: no managed service was installed (nothing to stop).");
        }
        StopOutcome::Unregistered => {
            println!("bn serve: managed dashboard service stopped and unregistered.");
        }
        StopOutcome::Terminated(pids) => {
            let list = pids
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if was_installed {
                println!("bn serve: managed dashboard service stopped and unregistered.");
            }
            println!("  terminated the server still holding port {port} (pid {list}).");
        }
        StopOutcome::Stuck(pids) => {
            let list = pids
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("bn serve: port {port} is STILL held after unregistering (pid {list}).");
            println!("  bn-web will fail to bind until that process exits — inspect it with:");
            println!("    bn killport {port}");
        }
    }
}

/// The port `bn serve status` probes: an explicit `--port`, else the resolved `[web].port` default.
fn resolved_port(port: Option<u16>) -> u16 {
    port.unwrap_or_else(|| Config::load().web.port)
}

/// Whether something is accepting connections on loopback `port` — a cheap liveness probe that
/// needs no HTTP client (a successful TCP connect is enough to know the server is up).
pub(crate) fn port_listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
}

/// Spawn `bn-web` as a detached background process (its own session) so it keeps running after the
/// launching shell exits — the fallback when launchd is unavailable. Mirrors orchestrate's
/// `spawn_detached_run`. Returns whether the spawn succeeded.
#[allow(
    unsafe_code,
    reason = "CommandExt::pre_exec is required to detach the fallback server before exec"
)]
fn spawn_detached(bin: &std::path::Path, args: &[String]) -> bool {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New session so it detaches from the controlling terminal / process group.
        unsafe {
            cmd.pre_exec(|| {
                setsid_detached();
                Ok(())
            });
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().is_ok()
}

/// setsid(2) via raw syscall — detach into a new session without pulling in the `libc` crate
/// (same trick as cmd/orchestrate.rs).
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "setsid has no standard-library wrapper and is isolated to this FFI boundary"
)]
fn setsid_detached() {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The bug this file exists to fix: `bn serve stop` unregistered the service and reported
    // "stopped and unregistered" while an orphaned bn-web kept the port. `Stuck` is the state that
    // used to be indistinguishable from success.
    #[test]
    fn a_listener_surviving_the_unload_is_reported_stuck_not_stopped() {
        assert_eq!(
            stop_outcome(true, &[96147], &[96147]),
            StopOutcome::Stuck(vec![96147]),
        );
    }

    #[test]
    fn stuck_wins_even_when_no_service_was_registered() {
        // The orphan case exactly: nothing installed any more, yet the port is still held.
        assert_eq!(
            stop_outcome(false, &[], &[4242]),
            StopOutcome::Stuck(vec![4242]),
        );
    }

    #[test]
    fn a_listener_that_died_on_signal_is_reported_terminated() {
        assert_eq!(
            stop_outcome(true, &[123, 456], &[]),
            StopOutcome::Terminated(vec![123, 456]),
        );
    }

    #[test]
    fn unregistering_with_nothing_on_the_port_is_still_a_clean_stop() {
        assert_eq!(stop_outcome(true, &[], &[]), StopOutcome::Unregistered);
    }

    #[test]
    fn nothing_installed_and_nothing_listening_is_a_no_op() {
        assert_eq!(stop_outcome(false, &[], &[]), StopOutcome::Nothing);
    }
}
