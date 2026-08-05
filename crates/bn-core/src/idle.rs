//! Whether bn is being actively worked on right now — the activity signal the auto-review loop
//! gates on (review PRs only while I'm idle, never mid-session). This is the focused descendant
//! of the `is_active` helper that backed the old `when_active` cron gate (removed in #176, where
//! gating *housekeeping* on idleness was backwards). Here idleness is the legitimate trigger, so
//! the threshold is a caller-supplied parameter (the `[autoreview]` idle window), not a global
//! `Config` knob.

use crate::resolver::Config;

/// Whether the user is actively working at `now` (epoch secs), looking back `idle_after_mins`.
/// True when punched in, a focus block is active, or any human/agent event landed in this
/// machine's synced metrics log within the window. `cron_run` / `cron_skipped` are excluded so a
/// gated job never counts its own runs as activity (which would keep it perpetually "active").
pub fn is_active(cfg: &Config, now: i64, idle_after_mins: i64) -> bool {
    if crate::track::is_punched_in(cfg, now) || crate::focus::is_active(now) {
        return true;
    }
    let cutoff = now - idle_after_mins * 60;
    crate::events::read_events_at(&cfg.metrics_log_path())
        .iter()
        .any(|e| e.ts >= cutoff && e.kind != "cron_run" && e.kind != "cron_skipped")
}

/// Inverse of [`is_active`]: the machine has been quiet for at least `idle_after_mins`.
pub fn is_idle(cfg: &Config, now: i64, idle_after_mins: i64) -> bool {
    !is_active(cfg, now, idle_after_mins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg_with(personal: PathBuf) -> Config {
        Config {
            personal_root: personal,
            work_root: PathBuf::from("/none-work"),
            worktree_dir: PathBuf::from("/none-wt"),
            hostname: "testhost".into(),
            ..Default::default()
        }
    }

    fn write_metric(cfg: &Config, kind: &str, ts: i64) {
        let p = cfg.metrics_log_path();
        crate::events::append_event_at(&p, kind, ts, serde_json::Map::new()).unwrap();
    }

    #[test]
    fn idle_tracks_recent_human_activity_excluding_cron_noise() {
        // Pin an empty state dir so focus::is_active sees no block and the machine-local log is
        // clean; with no punch file the only signal is the metrics log we write below.
        let _g = crate::test_support::env_lock();
        let state = tempfile::tempdir().unwrap();
        let _state_env = crate::test_support::EnvGuard::set("BN_STATE_DIR", state.path());
        let now = 1_750_000_000_i64;
        let window = 5; // the auto-review idle window (minutes)

        // No activity at all → idle.
        let empty = tempfile::tempdir().unwrap();
        assert!(
            is_idle(&cfg_with(empty.path().into()), now, window),
            "no activity → idle"
        );

        // A recent human mutation → active (not idle).
        let recent = tempfile::tempdir().unwrap();
        let cfg = cfg_with(recent.path().into());
        write_metric(&cfg, "activity", now - 60);
        assert!(is_active(&cfg, now, window), "recent mutation → active");
        assert!(!is_idle(&cfg, now, window), "recent mutation → not idle");

        // Activity older than the window → idle.
        let stale = tempfile::tempdir().unwrap();
        let cfg = cfg_with(stale.path().into());
        write_metric(&cfg, "activity", now - 6 * 60);
        assert!(is_idle(&cfg, now, window), "stale activity → idle");

        // Only cron_run / cron_skipped within the window → idle (a gated job must never count
        // its own runs/skips as activity, or it would keep itself perpetually awake).
        let cronly = tempfile::tempdir().unwrap();
        let cfg = cfg_with(cronly.path().into());
        write_metric(&cfg, "cron_run", now - 30);
        write_metric(&cfg, "cron_skipped", now - 10);
        assert!(is_idle(&cfg, now, window), "only cron events → idle");
    }
}
