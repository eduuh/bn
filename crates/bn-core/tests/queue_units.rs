//! Unit tests for the queue's pure policy.
//!
//! These cover the arithmetic and the rules — the parts with no clock, no disk and no processes,
//! which is exactly where the subtle bugs turned out to live. Three of this feature's defects were
//! invisible in normal operation and were found only by hand: a reaper predicate that never matched,
//! a score that overflowed in release, and a pool guard that trusted a pid a zombie still owned.
//! None of them needed a machine to reproduce; all of them needed someone to ask.
//!
//! An integration file rather than `#[cfg(test)]` modules: the repo runs a 1000-line ratchet over
//! every tracked `.rs`, and inline tests would spend the remaining headroom of the very files this
//! feature still has to grow.
use bn_core::backlog::{AddSpec, BacklogEntry, BacklogStatus};
use bn_core::queue::kind::{BacklogKind, ExecSpec, WorkClass};
use bn_core::queue::{build, capacity, jobs, reap};

// ---------------------------------------------------------------------------- lane capacity

#[test]
fn lanes_take_the_lower_of_ram_and_cores() {
    // 16 GB / 10 cores: RAM allows (16384-4096)/2048 = 6, CPU allows 5. The tighter one wins.
    assert_eq!(capacity::coding(16384, 10), 5);
    // Same RAM, few cores: now CPU is the binding constraint.
    assert_eq!(capacity::coding(16384, 2), 1);
}

#[test]
fn lanes_reserve_ram_for_everything_that_is_not_an_agent() {
    // Without the reservation an 8 GB box would compute 4 agents and then swap.
    assert_eq!(capacity::coding(8192, 64), 2);
}

#[test]
fn a_machine_always_gets_to_run_one_thing() {
    // A queue that can start nothing is broken, not careful — so both lanes floor at 1 however
    // small the box, including one with less RAM than the reservation.
    assert_eq!(capacity::coding(1024, 1), 1);
    assert_eq!(capacity::research(1024, 1), 1);
    assert_eq!(capacity::coding(0, 0), 1);
}

#[test]
fn lanes_are_capped_however_large_the_machine() {
    // Past this the limit stops being the machine and starts being the human reviewing the output.
    assert_eq!(capacity::coding(1_048_576, 512), capacity::MAX_CODING);
    assert_eq!(capacity::research(1_048_576, 512), capacity::MAX_RESEARCH);
}

#[test]
fn research_is_half_of_coding_but_never_zero() {
    for (ram, cores) in [(16384, 10), (32768, 16), (8192, 8), (4096, 2)] {
        let (c, r, _) = capacity::derive(ram, cores);
        assert!(r >= 1, "research floored at 1 for {ram}MB/{cores}c");
        assert!(r <= c, "research never exceeds coding for {ram}MB/{cores}c");
    }
}

// ---------------------------------------------------------------------------- build pool

#[test]
fn builds_are_sized_apart_from_agents() {
    // A build is not an agent: 4 GB and 4 cores each, against an agent's 2 and 2. On 16 GB / 10
    // cores that is 2 builds where the coding lane holds 5.
    assert_eq!(build::slots(16384, 10), 2);
    assert!(build::slots(16384, 10) < capacity::coding(16384, 10));
    assert_eq!(build::slots(1024, 1), 1);
    assert_eq!(build::slots(1_048_576, 512), build::MAX_BUILDS);
}

fn held(id: &str, pid: u32, since: i64) -> build::Held {
    build::Held {
        id: id.to_string(),
        pid,
        label: "test".to_string(),
        repo: "r".to_string(),
        since,
    }
}

#[test]
fn a_dead_holder_frees_its_slot_at_once() {
    let rows = vec![held("a", 1, 1000), held("b", 2, 1000)];
    let kept = build::prune(rows, 1000, &|pid| pid == 1);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "a");
}

#[test]
fn a_live_holder_keeps_its_slot() {
    let rows = vec![held("a", 1, 1000)];
    assert_eq!(build::prune(rows, 1000, &|_| true).len(), 1);
}

#[test]
fn a_live_pid_past_the_ttl_is_still_reclaimed() {
    // The bug this exists for. A killed process stays a zombie until reaped and answers `kill(pid,
    // 0)` as alive; a reused pid does the same, permanently. Liveness alone would strand the slot.
    let rows = vec![held("a", 1, 1000)];
    let now = 1000 + build::SLOT_TTL_SECS + 1;
    assert!(build::prune(rows, now, &|_| true).is_empty());
}

#[test]
fn a_holder_exactly_at_the_ttl_is_kept() {
    // The boundary belongs to the holder: reclaiming a build that is still running would put two
    // of them on the machine, which is the thing the pool exists to prevent.
    let rows = vec![held("a", 1, 1000)];
    let now = 1000 + build::SLOT_TTL_SECS;
    assert_eq!(build::prune(rows, now, &|_| true).len(), 1);
}

// ---------------------------------------------------------------------------- job classification

#[test]
fn recovery_jobs_are_recognized_by_name() {
    for job in jobs::RECOVERY_JOBS {
        assert!(jobs::is_recovery(job), "{job} should be recovery");
        assert!(!jobs::is_heavy(job), "{job} should not be heavy");
    }
    assert!(!jobs::is_recovery("pool-warm"));
    assert!(!jobs::is_recovery("some-other-job"));
}

#[test]
fn heavy_jobs_are_recognized_by_name() {
    for job in jobs::HEAVY_JOBS {
        assert!(jobs::is_heavy(job), "{job} should be heavy");
    }
    assert!(!jobs::is_heavy("agent-supervise"));
}

#[test]
fn the_queue_never_contains_what_drains_it() {
    assert!(jobs::never_capture("queue-tick"));
    for job in jobs::NEVER_CAPTURE {
        assert!(jobs::never_capture(job));
    }
    assert!(!jobs::never_capture("pool-warm"));
}

#[test]
fn priority_orders_recovery_above_routine_above_heavy() {
    // Exempting recovery from pause but leaving it behind a long cron backlog would be an
    // exemption in name only.
    assert!(jobs::priority_for("agent-supervise") > jobs::priority_for("some-cron"));
    assert!(jobs::priority_for("some-cron") > jobs::priority_for("pool-warm"));
}

#[test]
fn classification_is_a_property_of_the_job_not_the_caller() {
    // The bug: `recovery` was decided inside the cron capture shim, so the same job queued through
    // another door was silently not exempt from pause.
    let mut spec = AddSpec::default();
    jobs::classify(&mut spec, "agent-supervise", true);
    assert!(spec.recovery);
    assert_eq!(spec.priority, jobs::RECOVERY_PRIORITY);

    let mut spec = AddSpec::default();
    jobs::classify(&mut spec, "pool-warm", true);
    assert!(spec.heavy);
    assert!(!spec.recovery);

    // An explicit priority from the caller survives classification.
    let mut spec = AddSpec {
        priority: 7,
        ..Default::default()
    };
    jobs::classify(&mut spec, "agent-supervise", false);
    assert!(spec.recovery);
    assert_eq!(spec.priority, 7);
}

// ---------------------------------------------------------------------------- taxonomy

#[test]
fn every_kind_round_trips_through_its_label() {
    for kind in BacklogKind::ALL {
        let label = kind.label();
        assert_eq!(BacklogKind::parse(label), Some(kind), "round-trip {label}");
    }
}

#[test]
fn an_unknown_kind_is_rejected_rather_than_defaulted() {
    // Degrading to `ship` would dispatch a coding agent at a research job.
    assert_eq!(BacklogKind::parse("nonsense"), None);
    assert_eq!(BacklogKind::parse(""), None);
}

#[test]
fn every_kind_maps_to_a_lane() {
    for kind in BacklogKind::ALL {
        let lane = WorkClass::from_kind(kind);
        assert!(matches!(lane, WorkClass::Coding | WorkClass::Research));
    }
}

#[test]
fn only_agent_work_needs_a_worktree() {
    assert!(ExecSpec::Agent.needs_worktree());
    assert!(ExecSpec::Skill {
        skill: "investigate".into(),
        arg: None
    }
    .needs_worktree());
    assert!(!ExecSpec::Job {
        job: "x".into(),
        repo: None,
        branch: None,
    }
    .needs_worktree());
    assert!(!ExecSpec::Shell { cmd: "true".into() }.needs_worktree());
}

fn entry(exec: ExecSpec, heavy: bool) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        exec,
        heavy,
        ..Default::default()
    };
    BacklogEntry::new("repo", "item", &spec, 1000)
}

#[test]
fn heavy_work_is_never_express_however_cheap_its_executor_looks() {
    // The distinction that keeps a minutes-long build out of the tick. `pool-warm` is an
    // `ExecSpec::Job` exactly like a two-second housekeeping cron; only the weight separates them.
    assert!(entry(
        ExecSpec::Job {
            job: "x".into(),
            repo: None,
            branch: None,
        },
        false
    )
    .is_express());
    assert!(!entry(
        ExecSpec::Job {
            job: "pool-warm".into(),
            repo: None,
            branch: None,
        },
        true
    )
    .is_express());
    assert!(!entry(ExecSpec::Agent, false).is_express());
}

// ---------------------------------------------------------------------------- lease reaping

fn leased(id: &str, exec: ExecSpec, updated_at: i64) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        exec,
        ..Default::default()
    };
    let mut e = BacklogEntry::new("repo", id, &spec, updated_at);
    e.id = id.to_string();
    e.status = BacklogStatus::Leased;
    e.updated_at = updated_at;
    e
}

#[test]
fn a_dispatched_branch_contains_the_entry_id_rather_than_equalling_it() {
    // The bug: matching on equality never matched anything, which quietly reduced the reaper to a
    // pure timeout and would have reclaimed healthy agents an hour into their run.
    let e = leased("an-item", ExecSpec::Agent, 1000);
    let live = vec![("repo".to_string(), "dispatch/an-item".to_string())];
    assert!(reap::is_live(&e, &live));

    let unrelated = vec![("repo".to_string(), "dispatch/something-else".to_string())];
    assert!(!reap::is_live(&e, &unrelated));

    // Repo must match too, case-insensitively.
    let other_repo = vec![("elsewhere".to_string(), "dispatch/an-item".to_string())];
    assert!(!reap::is_live(&e, &other_repo));
    let cased = vec![("REPO".to_string(), "dispatch/an-item".to_string())];
    assert!(reap::is_live(&e, &cased));
}

#[test]
fn an_agent_with_no_live_session_is_reclaimed_without_waiting_for_the_ttl() {
    let entries = vec![leased("gone", ExecSpec::Agent, 1000)];
    let now = 1000 + reap::START_GRACE_SECS + 1;
    let stale = reap::stale(&entries, &[], now, |_| reap::AGENT_LEASE_TTL_SECS);
    assert_eq!(
        stale.len(),
        1,
        "a provably absent executor is not waited on"
    );
}

#[test]
fn a_freshly_leased_agent_is_given_time_to_appear() {
    // Launching involves a checkout and a tmux window; reclaiming inside that window would fight
    // the launcher it is waiting for.
    let entries = vec![leased("starting", ExecSpec::Agent, 1000)];
    let now = 1000 + reap::START_GRACE_SECS - 1;
    assert!(reap::stale(&entries, &[], now, |_| reap::AGENT_LEASE_TTL_SECS).is_empty());
}

#[test]
fn a_live_agent_is_left_alone_until_its_ttl() {
    let entries = vec![leased("running", ExecSpec::Agent, 1000)];
    let live = vec![("repo".to_string(), "dispatch/running".to_string())];
    let now = 1000 + reap::AGENT_LEASE_TTL_SECS - 1;
    assert!(reap::stale(&entries, &live, now, |_| reap::AGENT_LEASE_TTL_SECS).is_empty());

    let past = 1000 + reap::AGENT_LEASE_TTL_SECS + 1;
    assert_eq!(
        reap::stale(&entries, &live, past, |_| reap::AGENT_LEASE_TTL_SECS).len(),
        1,
        "the TTL is the backstop even for something that still looks live"
    );
}

#[test]
fn headless_work_is_reclaimed_on_the_ttl_alone() {
    // The liveness check cannot see a shell job, so only the (much shorter) TTL applies — and the
    // grace period must not apply to it either.
    let entries = vec![leased("job", ExecSpec::Shell { cmd: "true".into() }, 1000)];
    let ttl = |worktree: bool| {
        if worktree {
            reap::AGENT_LEASE_TTL_SECS
        } else {
            reap::EXPRESS_LEASE_TTL_SECS
        }
    };
    let now = 1000 + reap::EXPRESS_LEASE_TTL_SECS - 1;
    assert!(reap::stale(&entries, &[], now, ttl).is_empty());
    let past = 1000 + reap::EXPRESS_LEASE_TTL_SECS + 1;
    assert_eq!(reap::stale(&entries, &[], past, ttl).len(), 1);
}

#[test]
fn only_leased_entries_are_candidates() {
    let spec = AddSpec::default();
    let pending = BacklogEntry::new("repo", "pending-item", &spec, 0);
    assert_eq!(pending.status, BacklogStatus::Pending);
    let entries = vec![pending];
    assert!(reap::stale(&entries, &[], 10_000_000, |_| 1).is_empty());
}
