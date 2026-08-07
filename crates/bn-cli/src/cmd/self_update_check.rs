//! Checking that a self-update did not remove something the machine depends on.
//!
//! `self-update` rebuilds bn from main and installs it. That is the right default — a machine that
//! drifts from main is a machine nobody can reason about — but it means an unattended job replaces
//! the binary every long-running bn service on the box is executing.
//!
//! Which is how the dispatcher died: main predated the queue, so the freshly installed bn had no
//! `queue` subcommand, and `bn queue run` became `unrecognized subcommand` on every restart until
//! launchd stopped trying. Nothing failed loudly. `self-update` reported success, because from its
//! point of view it had done exactly what it was asked.
//!
//! The queue now defends itself — it will not retire in favour of a binary that cannot replace it,
//! and will not install a unit pointing at one. This is the other half: the update that *caused*
//! it should say so, rather than leaving the discovery to whoever notices the machine is quiet.
//!
//! It reports rather than rolls back. By the time this runs `install.sh` has already replaced the
//! binary, and a job that silently reverted the update you asked for would be its own surprise. A
//! failed cron is visible in `bn cron` and carries its reason in the run's output tail, which is
//! what "loud" means for something that runs at midnight.
use std::path::Path;

/// The bn subcommand each supervised service needs, paired with the unit that would be installed
/// for it. If the unit is on the box, the binary has to be able to run it.
const SERVICES: [(&str, &str); 3] = [
    ("com.bn.queue", "queue"),
    ("com.bn.web", "web"),
    ("com.bn.routine", "routine"),
];

/// The systemd equivalents, by unit file name.
const UNITS: [(&str, &str); 3] = [
    ("bn-queue.service", "queue"),
    ("bn-web.service", "web"),
    ("bn-routine.service", "routine"),
];

/// Subcommands this machine's installed services depend on.
///
/// Derived from what is actually installed rather than a fixed list, so a box that never registered
/// the dashboard is not held to whether `bn web` still exists.
fn needed() -> Vec<&'static str> {
    let mut out = Vec::new();
    let agents = crate::launchd::agents_dir();
    for (label, sub) in SERVICES {
        if agents.join(format!("{label}.plist")).exists() {
            out.push(sub);
        }
    }
    let units = crate::cmd::cron::systemd_user_dir();
    for (unit, sub) in UNITS {
        if units.join(unit).exists() && !out.contains(&sub) {
            out.push(sub);
        }
    }
    out
}

/// Whether `bn` still understands `sub`.
///
/// `--help` proves the subcommand is present without running it, so the check cannot have side
/// effects. clap exits non-zero on an unknown subcommand, which is the discriminator wanted.
fn supports(bn: &Path, sub: &str) -> bool {
    std::process::Command::new(bn)
        .args([sub, "--help"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Verify the freshly installed `bn` can still run every service installed on this machine.
///
/// Returns the job's exit code: 0 when nothing is missing (including when the binary is absent,
/// which is a different failure the installer already reported).
pub(crate) fn verify(bn: &Path) -> i32 {
    if !bn.exists() {
        return 0;
    }
    let missing: Vec<&str> = needed()
        .into_iter()
        .filter(|sub| !supports(bn, sub))
        .collect();
    if missing.is_empty() {
        return 0;
    }
    crate::cmd::jobs::native_job_line(
        &format!(
            "self-update: the installed bn no longer supports {} — {} service(s) on this machine \
             execute it and will fail to start",
            missing.join(", "),
            missing.len()
        ),
        true,
    );
    crate::cmd::jobs::native_job_line(
        "  the update landed a revision older than the services registered here; \
         reinstall from a revision that has them, or unregister the service",
        true,
    );
    1
}
