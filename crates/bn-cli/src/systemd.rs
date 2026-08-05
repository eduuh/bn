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

/// The per-repo orchestrate `--user` unit name: `bn-orchestrate-<repo>.service`. Uses the same
/// `sanitize_run_key` as the launchd label so the repo token is a valid systemd unit name.
pub(crate) fn orchestrate_unit(repo: &str) -> String {
    format!(
        "bn-orchestrate-{}.service",
        bn_core::orchestrate::sanitize_run_key(repo)
    )
}

/// The cross-repo (#455) orchestrate supervisor `--user` unit name: `bn-orchestrate-__all__.service`.
/// Uses the reserved `__all__` token (matching the launchd label) so a repo literally named `all`
/// never collides with the cross-repo supervisor's unit.
pub(crate) fn orchestrate_all_unit() -> String {
    "bn-orchestrate-__all__.service".to_string()
}

/// The pre-#395 machine-wide singleton orchestrate `--user` unit name — retired by the legacy
/// migration so the old daemon can't keep driving (or resurrecting) the moved state file.
pub(crate) fn orchestrate_legacy_unit() -> String {
    "bn-orchestrate.service".to_string()
}

fn orchestrate_unit_path(unit: &str) -> PathBuf {
    user_unit_dir().join(unit)
}

/// A `bn orchestrate run …` `--user` unit body. Same shape as [`web_unit`]: a login-shell
/// `ExecStart` so `$PATH` finds `bn`/`gh`/`git`, `Restart=on-failure` (crash relaunches, a clean
/// drain / `bn orchestrate stop` stays down — the launchd `KeepAlive {SuccessfulExit=false}`
/// analogue), `RestartSec` throttling a hot loop, and `WantedBy=default.target` so it survives
/// logout + reboot. `run_args` are the `orchestrate run` arguments (e.g. `--repo bn`,
/// `--all-repos`).
fn orchestrate_unit_text(bn_path: &str, description: &str, run_args: &str) -> String {
    format!(
        "[Unit]\n\
         Description={description}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=/bin/sh -lc 'exec \"{bn_path}\" orchestrate run {run_args}'\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Write + enable + start the per-repo orchestrate `--user` service (#608). `Err` when systemctl is
/// unavailable (macOS / no user bus) so the caller falls back to a detached child. #395: one unit
/// per repo, so a second run never hijacks — or is hijacked by — another repo's daemon.
pub(crate) fn orchestrate_install(repo: &str) -> Result<(), String> {
    let unit = orchestrate_unit(repo);
    let text = orchestrate_unit_text(
        &current_bn()?,
        &format!("bn orchestrate daemon for {repo} (#608)"),
        &format!("--repo \"{repo}\""),
    );
    write_enable_start(&unit, &text)
}

/// Write + enable + start the #455 cross-repo orchestrate supervisor `--user` service. Mirrors
/// [`orchestrate_install`]; its program is `bn orchestrate run --all-repos`.
pub(crate) fn orchestrate_install_all() -> Result<(), String> {
    let unit = orchestrate_all_unit();
    let text = orchestrate_unit_text(
        &current_bn()?,
        "bn cross-repo orchestrate supervisor (#455)",
        "--all-repos",
    );
    write_enable_start(&unit, &text)
}

/// Stop + disable + remove a repo's orchestrate `--user` service (best-effort). The systemd twin of
/// [`crate::launchd::orchestrate_uninstall`]; no-op when systemctl is absent.
pub(crate) fn orchestrate_uninstall(repo: &str) {
    stop_disable_remove(&orchestrate_unit(repo));
}

/// Stop + disable + remove the cross-repo supervisor `--user` service (best-effort).
pub(crate) fn orchestrate_uninstall_all() {
    stop_disable_remove(&orchestrate_all_unit());
}

/// Stop + disable + remove the pre-#395 singleton orchestrate `--user` service (best-effort) —
/// called once by the legacy-state migration.
pub(crate) fn orchestrate_uninstall_legacy() {
    stop_disable_remove(&orchestrate_legacy_unit());
}

/// Disable + remove a repo's orchestrate unit WITHOUT stopping it — used by the daemon itself when
/// its run drains: `disable` only drops the `WantedBy` symlink (so a reboot won't resurrect a loop
/// for an already-finished run), while the still-running service exits cleanly on its own. The
/// systemd twin of [`crate::launchd::orchestrate_forget`]; a `stop` here would kill the very
/// process calling this.
pub(crate) fn orchestrate_forget(repo: &str) {
    let unit = orchestrate_unit(repo);
    let _ = run_user(&["disable", &unit]);
    let _ = std::fs::remove_file(orchestrate_unit_path(&unit));
    let _ = run_user(&["daemon-reload"]);
}

/// `std::env::current_exe()` as a lossy string, or the `systemctl unavailable`/exe error — the
/// `bn` path an orchestrate unit's `ExecStart` runs.
fn current_bn() -> Result<String, String> {
    if !have_systemctl() {
        return Err("systemctl unavailable".into());
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// Write `text` to `<unit_dir>/<unit>`, `daemon-reload`, then `enable --now` — the shared
/// install body for every orchestrate unit (and identical in shape to [`web_install`]).
fn write_enable_start(unit: &str, text: &str) -> Result<(), String> {
    if !have_systemctl() {
        return Err("systemctl unavailable".into());
    }
    let path = orchestrate_unit_path(unit);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    run_user(&["daemon-reload"])?;
    run_user(&["enable", "--now", unit])
}

/// `disable --now` (stop + drop the symlink), remove the unit file, then `daemon-reload` —
/// best-effort teardown shared by the orchestrate `uninstall*` fns.
fn stop_disable_remove(unit: &str) {
    let _ = run_user(&["disable", "--now", unit]);
    let _ = std::fs::remove_file(orchestrate_unit_path(unit));
    let _ = run_user(&["daemon-reload"]);
}

// --- `bn routine watch` managed daemon (#801 amendment) ----------------------
// The Linux twin of the launchd routine watcher daemon: an enabled `--user` unit with
// `Restart=on-failure` (crash relaunches, clean stop stays down), coexisting safely with the
// `routine-tick` timer fallback (both share the same transaction lock and dedupe state).

/// The routine watcher `--user` unit name.
pub(crate) const ROUTINE_UNIT: &str = "bn-routine.service";

fn routine_unit_path() -> PathBuf {
    user_unit_dir().join(ROUTINE_UNIT)
}

/// The routine watcher `--user` unit text: `bn routine watch` under a login shell (so `$PATH`
/// resolves notifier backends), `Restart=on-failure` (the systemd analogue of launchd's
/// `KeepAlive {SuccessfulExit=false}`), and `WantedBy=default.target` so it survives logout+reboot.
pub(crate) fn routine_unit(bn_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=bn routine watcher (bn routine watch, #801 amendment)\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=/bin/sh -lc 'exec \"{bn_path}\" routine watch'\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Write + enable + start the routine watcher `--user` service. `Err` when systemctl is
/// unavailable (macOS / no user bus) — the caller then reports no resident coverage rather than a
/// detached fallback (#801 amendment invariant 6: no fake coverage claims).
pub(crate) fn routine_install(bn_path: &str) -> Result<(), String> {
    if !have_systemctl() {
        return Err("systemctl unavailable".into());
    }
    let path = routine_unit_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, routine_unit(bn_path)).map_err(|e| e.to_string())?;
    run_user(&["daemon-reload"])?;
    run_user(&["enable", "--now", ROUTINE_UNIT])
}

/// Whether the routine watcher `--user` unit is *installed* (its unit file is on disk).
pub(crate) fn routine_installed() -> bool {
    routine_unit_path().exists()
}

/// Stop, disable, and remove the routine watcher `--user` service (best-effort).
pub(crate) fn routine_uninstall() {
    let _ = run_user(&["disable", "--now", ROUTINE_UNIT]);
    let _ = std::fs::remove_file(routine_unit_path());
    let _ = run_user(&["daemon-reload"]);
}
