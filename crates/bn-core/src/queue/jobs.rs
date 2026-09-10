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
pub const NEVER_CAPTURE: &[&str] = &[
    "queue-tick",
    "agent-scheduler",
    "tick-5m",
    "tick-10m",
    "tick-15m",
];

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
///
/// `build`, `build-main`, `pool-grow` and `verify` are the picker's verbs ([`crate::wt_actions`]) that run a
/// repo's `build.sh`, which is the same minutes-long cost `pool-warm` pays per slot — the reason
/// heaviness is a property of the work and not of the door it came through. They are pointedly
/// **not** in [`WARMING_JOBS`]: someone asked for these from a picker and is watching the row.
pub const HEAVY_JOBS: &[&str] = &[
    "pool-warm",
    "keep-warm",
    "build",
    "build-main",
    "pool-grow",
    "verify",
];

/// The jobs that *are* pool warming — the work [`crate::queue::classify`] holds to idle and away
/// windows.
///
/// A subset of [`HEAVY_JOBS`] rather than the same list: the picker's build verbs are every bit as
/// expensive, but they are not warming and must not inherit warming's away-only admission.
///
/// It is a declared list because the alternative was a substring test on the entry's item text,
/// which classified anything whose *subject* happened to say "warm" as pool warming — including a
/// repo-setup agent whose prompt mentions keeping scripts idempotent for warm pools. Work is what
/// it declares itself to be, never what its prose happens to contain.
pub const WARMING_JOBS: &[&str] = &["pool-warm", "keep-warm"];

/// How long a mandatory pool-refresh firing stays meaningful.
///
/// The sweep is daily, so a firing that has not run within a day has been superseded by the next
/// one — exactly the reasoning [`crate::backlog::BacklogEntry::not_after`] already applies to every
/// other scheduled job. It was the one cron enqueued with no deadline at all, which is how a single
/// stuck firing came to own machine-wide admission indefinitely: alone among scheduled work, it
/// could never lapse.
pub const MANDATORY_WARM_TTL_SECS: i64 = 24 * 3600;

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

/// Whether this job is pool warming, and so runs only while you are idle or away.
pub fn is_warming(name: &str) -> bool {
    WARMING_JOBS.contains(&name)
}

/// The prefix a scheduled firing stamps on an entry's `source` — `cron:<job>`.
pub const CRON_SOURCE_PREFIX: &str = "cron:";

/// Whether `source` is the *scheduled firing* of `job`, rather than someone asking for it by hand.
///
/// The producer is the honest answer to "is this the machine's own sweep or a person's one-off?".
/// A person's `bn queue add --job <name>` stamps `source = "queue"`; only the cron stamps
/// `cron:<job>`. Exact match after the prefix, so `cron:pool-warm-extra` is not `pool-warm`.
pub fn is_scheduled_firing(source: &str, job: &str) -> bool {
    source.strip_prefix(CRON_SOURCE_PREFIX) == Some(job)
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

#[cfg(test)]
mod scheduled_firing_tests {
    use super::*;

    #[test]
    fn only_the_cron_prefix_and_only_an_exact_job_name() {
        assert!(is_scheduled_firing("cron:pool-warm", "pool-warm"));
        // A person's `bn queue add --job pool-warm` — same job, different producer.
        assert!(!is_scheduled_firing("queue", "pool-warm"));
        // Exact, not prefix: a longer job name must not answer for a shorter one.
        assert!(!is_scheduled_firing("cron:pool-warm-extra", "pool-warm"));
        assert!(!is_scheduled_firing("cron:keep-warm", "pool-warm"));
        assert!(!is_scheduled_firing("pool-warm", "pool-warm"));
    }
}
