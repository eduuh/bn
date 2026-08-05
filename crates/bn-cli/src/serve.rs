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
fn resolve_bn_web() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
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

/// `bn serve stop` — unload the managed service on whichever platform installed it (best-effort on
/// both, since only one applies), so a crashed/looping daemon can be cleanly taken down.
fn stop(json: bool) {
    let was_installed = crate::launchd::web_installed() || crate::systemd::web_installed();
    crate::launchd::web_uninstall();
    crate::systemd::web_uninstall();

    if json {
        let data = serde_json::json!({ "stopped": true, "was_installed": was_installed });
        println!("{}", Envelope::success("serve", data).to_json());
    } else if was_installed {
        println!("bn serve: managed dashboard service stopped and unregistered.");
    } else {
        println!("bn serve: no managed service was installed (nothing to stop).");
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
