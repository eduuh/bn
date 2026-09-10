//! Keeping this machine's timers matching the crons it declares, so `bn cron register` is never a
//! step anyone has to remember. Split out of `cron.rs` under the 1000-line ratchet.
use crate::*;

/// The bn a generated timer runs: the bindir-independent `<config_dir>/bn` anchor install.sh keeps
/// pointed at the live binary, so units survive a bindir move, else the resolved bin dir. Both are
/// absolute, as ExecStart requires.
pub(super) fn registered_bn_path() -> String {
    let anchor = resolver::config_dir().join("bn");
    if anchor.exists() {
        anchor.to_string_lossy().into_owned()
    } else {
        resolver::bin_dir()
            .join("bn")
            .to_string_lossy()
            .into_owned()
    }
}

/// How often an interactive `bn` may look at whether the timers still match the manifest.
const RECONCILE_EVERY_SECS: u64 = 600;

/// State-dir file holding [`timers_fingerprint`] as of the last clean `bn cron register`.
const TIMERS_FINGERPRINT: &str = "cron-registered.fingerprint";

/// A fingerprint of what registration would install here: every declared cron as registration sees
/// it, whether cron is on, and the bn the units run. It reads manifests only — no `systemctl` or
/// `launchctl` — so comparing it is cheap enough to do from an ordinary command.
pub(super) fn timers_fingerprint(cfg: &Config, ctx: Option<&Resolved>) -> String {
    let mut lines: Vec<String> = super::machine_scope::desired_crons(cfg, ctx)
        .iter()
        .map(|cron| format!("{:?}|{:?}|{:?}", cron.repo, cron.scope, cron.entry))
        .collect();
    lines.sort();
    lines.push(format!("cron_enabled={}", cfg.cron_enabled_here()));
    lines.push(registered_bn_path());
    format!(
        "{:016x}\n",
        bn_core::hash::fnv1a64(lines.join("\n").as_bytes())
    )
}

/// Record what a clean registration left in place, so the background reconcile only fires again
/// when the declared crons move. Recorded even with no scheduler backend: nothing will ever be
/// written there, and retrying every few minutes would change nothing.
pub(super) fn record_registered(cfg: &Config, ctx: Option<&Resolved>) {
    let _ = std::fs::create_dir_all(resolver::state_dir());
    let _ = std::fs::write(
        resolver::state_dir().join(TIMERS_FINGERPRINT),
        timers_fingerprint(cfg, ctx),
    );
}

/// The path anchors that point bn somewhere other than this machine's own layout. Any one of them
/// makes this a sandbox, and `bn cron register` from a sandbox would rewrite the real machine's
/// timers: launchd and systemd `--user` are per-user, and registration unloads every timer its
/// manifest does not declare.
const SANDBOX_ANCHORS: [&str; 5] = [
    "BN_HOME",
    "BN_CONFIG",
    "BN_CONFIG_DIR",
    "BN_STATE_DIR",
    "BN_HOSTNAME",
];

/// Whether the environment rules the background reconcile out: the queue dispatcher's opt-outs, or
/// any sandbox anchor. `queue-shakedown.sh` unsets `BN_NO_AUTO_TICK` with the real `HOME`, so the
/// opt-outs alone do not keep a sandbox away from the real timers.
fn reconcile_blocked() -> bool {
    ["BN_NO_AUTO_TICK", "BN_NO_CATCHUP", "BN_IN_QUEUE_TICK"]
        .iter()
        .chain(SANDBOX_ANCHORS.iter())
        .any(|key| std::env::var_os(key).is_some())
}

/// Keep this machine's timers matching the crons it declares.
///
/// Looks at most once every [`RECONCILE_EVERY_SECS`] (one stat of a stamp), and the look compares
/// [`timers_fingerprint`] with what the last clean registration recorded. When they differ it
/// starts `bn cron register` detached, the way catch-up starts jobs, so the command you ran is
/// never delayed. Never runs in a sandbox (see [`reconcile_blocked`]).
pub(crate) fn maybe_reconcile_timers(cfg: &Config) {
    if reconcile_blocked() {
        return;
    }
    let state = resolver::state_dir();
    let stamp = state.join("cron-reconcile-checked");
    let looked_recently = std::fs::metadata(&stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age.as_secs() < RECONCILE_EVERY_SECS);
    if looked_recently {
        return;
    }
    let _ = std::fs::create_dir_all(&state);
    if std::fs::write(&stamp, b"").is_err() {
        return;
    }
    // No manifest found (bn/main mid-reset, a broken anchor) is not "declares nothing": registering
    // then would unload every timer until the manifest came back.
    if resolver::shipped_config("crons.toml").is_none() {
        return;
    }
    let recorded = std::fs::read_to_string(state.join(TIMERS_FINGERPRINT)).unwrap_or_default();
    if recorded == timers_fingerprint(cfg, None) {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let _ = std::process::Command::new(&exe)
        .args(["cron", "register"])
        .env("BN_NO_CATCHUP", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sandbox anchor alone keeps the reconcile away from the per-user init system.
    #[test]
    fn a_sandbox_never_reconciles_the_real_timers() {
        let _lock = bn_core::test_support::env_lock();
        for anchor in SANDBOX_ANCHORS {
            let _guard = bn_core::test_support::EnvGuard::set(anchor, "/nonexistent/bn-sandbox");
            assert!(reconcile_blocked(), "{anchor}");
        }
    }
}
