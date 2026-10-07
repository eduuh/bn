//! Probe systemd's user manager before callers register scheduled work.

use std::process::Command;

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
