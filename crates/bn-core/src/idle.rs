//! Whether bn is being actively worked on right now — the activity signal the auto-review loop
//! gates on (review PRs only while I'm idle, never mid-session). This is the focused descendant
//! of the `is_active` helper that backed the old `when_active` cron gate (removed in #176, where
//! gating *housekeeping* on idleness was backwards). Here idleness is the legitimate trigger, so
//! the threshold is a caller-supplied parameter (the `[autoreview]` idle window), not a global
//! `Config` knob.

use crate::resolver::Config;

/// Whether the user is actively working at `now` (epoch secs), looking back `idle_after_mins`.
/// False outright while an absence is declared ([`crate::away`]); otherwise true when punched in,
/// a focus block is active, or any human/agent event landed in this machine's synced metrics log
/// within the window. `cron_run` / `cron_skipped` are excluded so a
/// gated job never counts its own runs as activity (which would keep it perpetually "active").
pub fn is_active(cfg: &Config, now: i64, idle_after_mins: i64) -> bool {
    // A declared absence wins over everything below, and it has to *override* the scan rather than
    // join it. Saying "I am away" writes an event, and an event inside the window is exactly what
    // the scan reads as presence — so a signal that merely voted would mark you present at the
    // moment you announced you were not. See `crate::away`.
    if crate::away::is_declared(now) {
        return false;
    }
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
