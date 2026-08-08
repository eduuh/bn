//! `bn routine` — scheduled agent runs (the "routines" surface), and the background service that
//! fires them.
//!
//! Split from `cli.rs` under the file-size ratchet, matching `cli/cron_cmd.rs` and
//! `cli/staff_cmd.rs`: the command *shapes* live beside each other here, the handlers stay in
//! `cmd/`.
use crate::*;

/// The `bn routine` subcommands (#801) — the routine-aware personal schedule coach.
#[derive(Subcommand, Debug)]
pub(crate) enum RoutineCmd {
    /// The block covering right now, if any (default when no subcommand is given).
    Now,
    /// The next upcoming block (today or later).
    Next,
    /// Full status: current + next block, remaining/until-next minutes, alignment, timezone,
    /// UTC offset, DST adjustment, active skip/snooze/override.
    Status,
    /// Record that you checked in with the routine right now (an adherence acknowledgment;
    /// never touches the recurring schedule).
    CheckIn {
        /// Optional free-form note (e.g. what you actually did instead).
        #[arg(long)]
        note: Option<String>,
    },
    /// Silence notifications until `--minutes` from now (status/`now`/`next` stay unaffected —
    /// only outbound reminders are gated).
    Snooze {
        #[arg(long, default_value_t = 30)]
        minutes: i64,
    },
    /// Skip just today's occurrence of a block (future occurrences are unaffected).
    Skip {
        /// Block id (e.g. `b0`) — defaults to whichever block is current right now.
        #[arg(long)]
        block: Option<String>,
        /// Calendar date (routine timezone, `YYYY-MM-DD`) — defaults to today.
        #[arg(long)]
        date: Option<String>,
    },
    /// Declare a temporary status that supersedes the recurring schedule until it expires —
    /// without ever rewriting `[routine.blocks]`.
    Override {
        /// Free-form label (e.g. `"family emergency"`).
        name: String,
        /// Category (e.g. `family`, `work`) — informational only, doesn't affect suppression.
        #[arg(long, default_value = "override")]
        kind: String,
        /// Minutes until the override expires and the recurring schedule resumes.
        #[arg(long, default_value_t = 60)]
        minutes: i64,
    },
    /// End the active override early, reverting to the recurring schedule immediately.
    ClearOverride,
    /// Run one shared evaluation: deliver any due reminder/start events (at-least-once, deduped
    /// by stable block id + resolved instant + event kind, bounded by `catchup_horizon_minutes`),
    /// persist health, and report evaluated_at/active/due/sent/deduped/expired/backend/next-wake.
    /// This is the native `routine-tick` cron/timer job; `bn routine watch` and the managed
    /// `routine` service call the exact same engine on a deadline-driven loop instead of a fixed
    /// interval.
    Tick,
    /// Compatibility alias for [`RoutineCmd::Tick`] — identical delivery, kept for existing
    /// muscle memory / scripts.
    Notify,
    /// Foreground deadline-driven reconciliation loop: run the shared tick, then wait for schedule
    /// state/config changes or the earliest reminder/start/end/snooze/override deadline (with a
    /// bounded reconciliation cap). Re-resolves `timezone = "local"`/config on every wake, so
    /// suspend/resume, clock jumps, DST transitions, and an invalid-then-fixed config are all
    /// handled by the same reconciliation. Exits cleanly on SIGTERM/Ctrl-C.
    Watch,
    /// Manage the long-running `bn routine watch` as a machine-managed service (launchd on
    /// macOS, systemd `--user` on Linux) — starts at login/reboot and restarts on crash.
    Service {
        #[command(subcommand)]
        action: RoutineServiceCmd,
    },
}

/// The `bn routine service` subcommands (#801 amendment) — managed lifecycle for
/// `bn routine watch`.
#[derive(Subcommand, Debug)]
pub(crate) enum RoutineServiceCmd {
    /// Register + start the managed watch service (launchd/systemd), or report that no
    /// supported service manager is available — never a detached fallback claiming reboot/crash
    /// coverage it doesn't have; use `bn cron register` for the `routine-tick` timer instead.
    Install,
    /// Stop and unregister the managed watch service.
    Stop,
    /// Restart the managed watch service (stop, then install again).
    Restart,
    /// Report installed/active state, health freshness, and coverage (covered/degraded/
    /// uncovered).
    Status,
}
