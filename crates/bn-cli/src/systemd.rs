//! systemd `--user` service management for bn's long-running daemons — the Linux twin of the
//! `com.bn.*` launchd agents in [`crate::launchd`]. The bn-web dashboard (#499) and the orchestrate
//! loops (#608) each get an enabled `--user` unit. Where launchd uses a `RunAtLoad` +
//! `KeepAlive {SuccessfulExit=false}` plist, systemd uses a `--user` unit with `Restart=on-failure`
//! (crash → relaunch, clean stop → stays down) enabled so it survives logout and reboot. Every
//! entry point degrades to `Err`/no-op when `systemctl` is absent (macOS / no user bus), so
//! `bn serve --daemon` falls back to a detached child exactly as it does when launchctl is missing.

use std::path::PathBuf;
use std::process::Command;

/// The bn-web `--user` unit name.
pub(crate) const WEB_UNIT: &str = "bn-web.service";

/// Whether systemd's per-user manager is actually usable (Linux with a *reachable* user bus).
/// Probes with `show-environment` — a side-effect-free read that needs the bus — so it's `false`
/// both where `systemctl` is absent (macOS — the spawn fails) and on a systemd host whose per-user
/// manager isn't reachable (no login session, lingering disabled, many containers). The old
/// `--version` probe never touched the bus and so wrongly reported `true` on a bus-less host,
/// making every `daemon-reload`/`enable` below fail with "Failed to connect to bus"; gating on the
/// bus here lets callers fall back cleanly (detached child / no-op) without special-casing.
pub(crate) fn have_systemctl() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The per-user systemd unit dir (`$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`).
pub(crate) fn user_unit_dir() -> PathBuf {
    unit_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok(),
        std::env::var("HOME").unwrap_or_default(),
    )
}

/// Pure resolution of the unit dir from the two env inputs, so the XDG-vs-`~/.config` precedence is
/// unit-testable without mutating process-global environment.
fn unit_dir_from(xdg_config_home: Option<String>, home: String) -> PathBuf {
    let base = xdg_config_home
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config"));
    base.join("systemd").join("user")
}

fn web_unit_path() -> PathBuf {
    user_unit_dir().join(WEB_UNIT)
}

/// The bn-web `--user` unit text. `Restart=on-failure` is the systemd analogue of launchd's
/// `KeepAlive {SuccessfulExit=false}` — a crash (non-zero/signal) relaunches, a clean stop
/// (`systemctl stop` → SIGTERM → clean exit) stays down; `RestartSec` throttles a hot loop.
/// `ExecStart` runs through a login shell (`/bin/sh -lc 'exec …'`) so `$PATH` finds `bn`/`gh`/`git`
/// for bn-web's subprocess calls, matching the launchd plist. `extra_args` are appended to the
/// `bn-web` invocation (e.g. `--port 7770`, `--lan`).
pub(crate) fn web_unit(bn_web_path: &str, extra_args: &[String]) -> String {
    let joined = extra_args.join(" ");
    let invocation = if joined.is_empty() {
        format!("exec \"{bn_web_path}\"")
    } else {
        format!("exec \"{bn_web_path}\" {joined}")
    };
    format!(
        "[Unit]\n\
         Description=bn-web dashboard server (bn serve --daemon, #499)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=/bin/sh -lc '{invocation}'\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Run `systemctl --user <args>`, mapping a non-zero exit (or a spawn failure) to its stderr text.
fn run_user(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Install (write + enable + start) the bn-web `--user` service: write the unit, `daemon-reload`,
/// then `enable --now` so it starts immediately *and* on every login (surviving reboot). `Err` when
/// systemctl is unavailable or the user bus rejects it — the caller then falls back to a detached
/// child.
pub(crate) fn web_install(bn_web_path: &str, extra_args: &[String]) -> Result<(), String> {
    if !have_systemctl() {
        return Err("systemctl unavailable".into());
    }
    let path = web_unit_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, web_unit(bn_web_path, extra_args)).map_err(|e| e.to_string())?;
    run_user(&["daemon-reload"])?;
    run_user(&["enable", "--now", WEB_UNIT])
}

/// Whether the bn-web `--user` unit is *installed* (its unit file is on disk) — the file-presence
/// source of truth `bn serve status` reads, matching [`crate::launchd::web_installed`].
pub(crate) fn web_installed() -> bool {
    web_unit_path().exists()
}

/// Stop, disable, and remove the bn-web `--user` service (best-effort). The inverse of
/// [`web_install`], called by `bn serve stop`; `disable --now` before the `remove_file` so the unit
/// is stopped before its file disappears.
pub(crate) fn web_uninstall() {
    // Nothing installed means nothing to reconcile, and `daemon-reload` is ~250ms of real work —
    // paid by *every* `bn` invocation via `serve::reconcile`, and so by every fzf keystroke that
    // re-execs `bn` for a picker preview. The unit file is already the source of truth
    // [`web_installed`] and `bn serve status` read, so this guard also makes uninstall agree with
    // the predicate the rest of this module uses.
    if !web_unit_path().exists() {
        return;
    }
    let _ = run_user(&["disable", "--now", WEB_UNIT]);
    let _ = std::fs::remove_file(web_unit_path());
    let _ = run_user(&["daemon-reload"]);
}

// --- `bn orchestrate` daemons (#608) -----------------------------------------
// The Linux twin of the launchd orchestrate daemons in [`crate::launchd`] (#338/#395/#455). Each
// orchestrate loop is a long-running daemon: on macOS it's a `RunAtLoad` + `KeepAlive
// {SuccessfulExit=false}` plist; on Linux it's an enabled `--user` unit with `Restart=on-failure`.
// `bn orchestrate` prefers launchd, then systemd here, then a detached child — so a Linux run
// survives the launching shell, a crash (Restart=on-failure), AND a reboot (WantedBy=default.target).

// --- `bn routine watch` managed daemon (#801 amendment) ----------------------
// The Linux twin of the launchd routine watcher daemon: an enabled `--user` unit with
// `Restart=on-failure` (crash relaunches, clean stop stays down), coexisting safely with the
// `routine-tick` timer fallback (both share the same transaction lock and dedupe state).

/// The `bn queue run` systemd user service — a resident loop, not a timer.
///
/// `Restart=always` is the point: the dispatcher is meant to be up, so systemd's job is to keep it
/// there rather than to poke it once a minute.
pub(crate) fn queue_unit(bn_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=bn queue dispatcher — resident loop draining the machine-wide work queue\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={bn_path} queue run\n\
         Restart=always\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}
