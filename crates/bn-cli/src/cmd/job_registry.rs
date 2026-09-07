//! The native-job registry: the name → implementation table `run_job` dispatches through, plus
//! the cadence clusters that fold several jobs behind one timer.
//!
//! Split out of [`crate::cmd::jobs`] so the table can grow without the implementations file
//! growing with it — `jobs.rs` is at the 1000-line ratchet's mercy (see `.file-size-baseline`),
//! and a registry is exactly the kind of thing that gains a line per feature. The job *bodies*
//! stay where they are; only the lookup moved.
use crate::*;

/// A natively-implemented job: `(cfg, ctx) -> exit code`.
pub(crate) type NativeJob = fn(&Config, Option<&Resolved>) -> i32;

/// The native implementation for `job`, or `None` if it hasn't been ported (→ fall back to `.sh`).
pub(crate) fn native_job(job: &str) -> Option<NativeJob> {
    match job {
        "wt-cleanup" => Some(job_wt_cleanup),
        "fleet-reap" => Some(job_fleet_reap),
        "attention-sync" => Some(job_attention_sync),
        "daily-rollover" => Some(job_daily_rollover),
        "metrics-snapshot" => Some(job_metrics_snapshot),
        "gather-findings" => Some(job_gather_findings),
        "weekly-insights" => Some(job_weekly_insights),
        "agent-observe" => Some(job_agent_observe),
        "auto-review-poll" => Some(job_auto_review_poll),
        "ensure-tmux" => Some(job_ensure_tmux),
        "notify" => Some(job_notify),
        "standup-push" => Some(job_standup_push),
        "wrap-digest" => Some(job_wrap_digest),
        "auto-review-trigger" => Some(job_auto_review_trigger),
        "attention-auto-trigger" => Some(job_attention_auto_trigger),
        "agent-supervise" => Some(job_agent_supervise),
        "pr-observe" => Some(job_pr_observe),
        "commit-notes" => Some(job_commit_notes),
        "watch" => Some(job_watch),
        "recall-index" => Some(job_recall_index),
        "managed-repos" => Some(job_managed_repos),
        "main-sync" => Some(crate::cmd::main_sync::job_main_sync),
        "refresh-all" => Some(job_refresh_all),
        "self-update" => Some(crate::cmd::self_update::job_self_update),
        "tools-reconcile" => Some(job_tools_reconcile),
        "tmux-reconcile" => Some(crate::cmd::tmux_reconcile::job_tmux_reconcile),
        "pool-warm" => Some(crate::wt_pool::job_pool_warm),
        // The worktree-picker verbs (`bn_core::wt_actions`). Each is a queueable name for work
        // that already existed as a subcommand — which is what lets a picker row show its progress.
        "build" => Some(crate::cmd::wt_action_jobs::job_build),
        "verify" => Some(crate::cmd::wt_action_jobs::job_verify),
        "build-main" => Some(crate::cmd::wt_action_jobs::job_build_main),
        "pool-grow" => Some(crate::cmd::wt_action_jobs::job_pool_grow),
        "fetch-prs" => Some(crate::cmd::wt_action_jobs::job_fetch_prs),
        "pull-prs" => Some(crate::cmd::wt_action_jobs::job_pull_prs),
        "wt-prune" => Some(crate::cmd::wt_action_jobs::job_wt_prune),
        "cron-health" => Some(crate::cmd::attention_workflow::job_cron_health),
        "agent-scheduler" => Some(crate::cmd::cron::job_agent_scheduler),
        "queue-tick" => Some(crate::cmd::queue_tick::job_queue_tick),
        "tick-5m" => Some(job_tick_5m),
        "tick-10m" => Some(job_tick_10m),
        "tick-15m" => Some(job_tick_15m),
        _ => None,
    }
}

/// The `*:0/5` cadence cluster, in run order.
///
/// `notify` deliberately left this cluster: `bn watch` (the `*:0/2` cron) owns attention delivery,
/// and running both meant every urgent item was toasted twice on two independent dedup ledgers.
/// The job still exists for `bn run notify` — see [`crate::cmd::jobs::job_notify`].
pub(crate) const TICK_5M: &[&str] =
    &["tools-reconcile", "routine-tick", "attention-sync", "tmux-reconcile"];

/// The `*:0/10` cadence cluster, in run order.
pub(crate) const TICK_10M: &[&str] = &[
    "auto-review-trigger",
    "attention-auto-trigger",
    "agent-supervise",
];

/// The `*:0/15` cadence cluster, in run order.
///
/// `main-sync` runs first: it is the one that keeps every repo's `main` checkout present and
/// current, so the rest of the tick — and anything cut from main before the next one — reads a
/// fresh base rather than whatever the checkout drifted to.
pub(crate) const TICK_15M: &[&str] = &["main-sync", "auto-review-poll", "cron-health"];
