//! `bn cron` — the scheduled side of bn: what fires on its own, when it last ran, and how to take
//! a job out of service.
//!
//! Crons are headless. A job never opens a tmux window or steals focus, so its outcome lives in
//! the event log rather than in a terminal you happened to be watching — which is why so much of
//! this surface is about *reading* runs (`status`, `show`, `failures`) rather than starting them.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CronCmd {
    /// Per-job: next scheduled run + last-run status. Same as bare `bn cron`.
    Status,
    /// Show a job's last run — exit, when, duration, and its captured output (including the
    /// error tail of a failed run): bn cron show <job>.
    Show { job: String },
    /// Recent failed runs with their error output: bn cron failures [--limit N].
    Failures {
        /// Max failures to show (newest first).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Stop and unregister the systemd timer driving a job: bn cron unregister <job>.
    Unregister { job: String },
    /// Tombstone a cron name that is gone for good — renamed, deleted, or replaced:
    /// bn cron retire <job> [reason…].
    ///
    /// The watchdog reads health from the event log, which has no way to say "this name will never
    /// run again", so a retired job's old failures keep reading as an ongoing outage. This records
    /// that they are history. Running the job again revives it, so there is nothing to undo.
    Retire {
        job: String,
        /// Why it is gone (e.g. "renamed to task-enhance"). Recorded with the tombstone.
        #[arg(trailing_var_arg = true)]
        reason: Vec<String>,
    },
    /// Generate + install platform timers from the cron manifests (crons.toml), idempotently:
    /// adds new timers, updates changed schedules, and removes bn timers no longer present in any
    /// manifest. The data-driven replacement for hand-editing jobs/systemd/install.sh.
    Register {
        /// Print the add/update/remove plan without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Compact badge of jobs running right now, for the tmux bar: bn cron running.
    /// Prints nothing when idle (keeps the bar clean).
    Running,
    /// Internal: refresh the Windows-task cache (spawned detached by the dashboard).
    #[command(hide = true)]
    RefreshWindows,
}
