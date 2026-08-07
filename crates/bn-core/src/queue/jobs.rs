//! What a native job *is* to the queue: ordinary, recovery, heavy, or unqueueable.
//!
//! These are properties of the job itself, so they live where every enqueue path can reach them.
//! They began life inside the cron capture shim, which meant the classification was a property of
//! *how work was queued* rather than of the work: `bn queue add --job agent-supervise` produced an
//! entry that looked identical to the captured one and silently was not exempt from pause. The same
//! job, two answers, decided by which door it came through.
//!
//! That is the same shape as the entry-point trap that bit the dispatcher twice, and it has the same
//! fix: state the rule once, in the place everyone already has to consult.
use crate::backlog::AddSpec;

/// The jobs the queue **depends on**, and therefore cannot contain.
///
/// Distinct from [`RECOVERY_JOBS`]: those go through the queue and are merely exempt from pause.
/// These cannot be queued at all, because queueing them is circular — the thing that drains the
/// queue would be waiting in it.
pub const NEVER_CAPTURE: &[&str] = &["queue-tick", "tick-5m", "tick-10m", "tick-15m"];

/// The jobs that keep the machine recoverable.
///
/// They still go through the queue — so they appear on the timeline and respect capacity like
/// everything else — but they are exempt from pause, because a pause is a statement about *new
/// work* and these are what make stopping safe. Pausing because the machine is unhappy must not
/// switch off the passes that fix it: `agent-supervise` finds stuck agents holding lane capacity,
/// `fleet-reap` frees the pool slots and leases those agents stranded, `agent-observe` folds
/// finished sessions into the metrics the queue reads back.
///
/// `wt-cleanup` joins them for the same reason `fleet-reap` does — it reclaims worktrees, and
/// reclamation is precisely what a wedged machine needs and precisely what a pause would stop.
pub const RECOVERY_JOBS: &[&str] = &[
    "agent-supervise",
    "fleet-reap",
    "agent-observe",
    "wt-cleanup",
];

/// Jobs that cost like an agent although they launch none.
///
/// `pool-warm` cuts checkouts and runs each repo's full build; `keep-warm` supplies runs from open
/// issues. They are among the most expensive things bn does, and until the queue existed no cap
/// touched them — `scheduler::admit` was reachable from exactly one place, so every guard an
/// operator set throttled agents and nothing else. The heaviest workload on the machine was the one
/// with no admission control, free to starve the agents the caps existed to protect.
///
/// Marked heavy, so they are admitted like an agent and spawned like one rather than running inline
/// in the tick — where minutes of build would hold the dispatcher and make every pass behind it
/// skip on the lock.
pub const HEAVY_JOBS: &[&str] = &["pool-warm", "keep-warm"];

/// Where recovery work sits in the ordering.
///
/// Above routine scheduled work, which sits below zero, and below anything a person is waiting on.
/// Exempting recovery from pause but leaving it at the back of a long cron backlog would be an
/// exemption in name only — it would still not run when it was needed.
pub const RECOVERY_PRIORITY: i64 = 50;

/// Priority for heavy background work: the floor, deliberately below routine scheduled work.
///
/// Warming exists to make room for agents, so a warm that keeps building while an agent waits on
/// RAM is the same starvation with more steps. Starving it is safe — dispatch falls back to a cold
/// worktree — so it degrades to *slow*, never to *stuck*.
pub const HEAVY_PRIORITY: i64 = -50;

/// Routine scheduled work: below anything a person is waiting on, above heavy background work.
pub const CRON_PRIORITY: i64 = -10;

/// Whether this job is exempt from pause.
pub fn is_recovery(name: &str) -> bool {
    RECOVERY_JOBS.contains(&name)
}

/// Whether this job must be admitted and spawned rather than run inline on the express path.
pub fn is_heavy(name: &str) -> bool {
    HEAVY_JOBS.contains(&name)
}

/// Whether queueing this job would be circular.
pub fn never_capture(name: &str) -> bool {
    NEVER_CAPTURE.contains(&name)
}

/// Where this job sits in the ordering, given what it is.
pub fn priority_for(name: &str) -> i64 {
    match (is_recovery(name), is_heavy(name)) {
        (true, _) => RECOVERY_PRIORITY,
        (_, true) => HEAVY_PRIORITY,
        _ => CRON_PRIORITY,
    }
}

/// Stamp a spec with the classification its job implies.
///
/// The single call every producer makes, so a new enqueue path gets pause-exemption, admission and
/// ordering right by construction instead of by remembering three separate lookups. Leaves
/// `priority` alone when the caller has already set one deliberately.
pub fn classify(spec: &mut AddSpec, job: &str, override_priority: bool) {
    spec.recovery = is_recovery(job);
    spec.heavy = is_heavy(job);
    if override_priority {
        spec.priority = priority_for(job);
    }
}
