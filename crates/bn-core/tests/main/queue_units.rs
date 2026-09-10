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
use bn_core::backlog::{
    AddSpec, AgentSpec, Backlog, BacklogEntry, BacklogStatus, ClassOverride, LaunchFailure,
    Recurrence, Substrate,
};
use bn_core::pipeline::stage::Stage;
use bn_core::queue::classify::{classify, QueueMode, QueueSubtype};
use bn_core::queue::kind::{BacklogKind, ExecSpec, Requirements, WorkClass};
use bn_core::queue::tick::{launch_retry_delay, LAUNCH_ATTEMPT_CAP};
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
fn the_explanation_states_the_arithmetic_in_the_order_it_happens() {
    // The reserve comes off before the division; a sentence that divides first reads as false
    // arithmetic ("16 ÷ 2 = 6") and undermines the number it is there to justify.
    let (coding, _, why) = capacity::derive(16384, 10);
    assert_eq!(
        why,
        "(16 GB RAM − 4 GB reserved) ÷ 2 GB/agent = 6; 10 cores ÷ 2 = 5; the lower wins"
    );
    assert_eq!(coding, 5, "the sentence's smaller bound is the lane cap");
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

fn leased_heavy(id: &str, exec: ExecSpec, updated_at: i64) -> BacklogEntry {
    let mut e = leased(id, exec, updated_at);
    e.heavy = true;
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
fn heavy_headless_work_uses_the_long_running_ttl() {
    // Heavy jobs are spawned headlessly, so the agent liveness set cannot see them. They still run
    // for minutes like agents; using the short express TTL requeues the same build while it is
    // still waiting on Cargo's lock, and the duplicate nightly run kills the first tmux session.
    let entries = vec![leased_heavy(
        "pool-warm",
        ExecSpec::Job {
            job: "pool-warm".into(),
            repo: None,
            branch: None,
        },
        1000,
    )];
    let ttl = |long_lived: bool| {
        if long_lived {
            reap::AGENT_LEASE_TTL_SECS
        } else {
            reap::EXPRESS_LEASE_TTL_SECS
        }
    };
    let after_express = 1000 + reap::EXPRESS_LEASE_TTL_SECS + 1;
    assert!(
        reap::stale(&entries, &[], after_express, ttl).is_empty(),
        "heavy work must not be reclaimed on the express TTL"
    );
    let past_long_running = 1000 + reap::AGENT_LEASE_TTL_SECS + 1;
    assert_eq!(reap::stale(&entries, &[], past_long_running, ttl).len(), 1);
}

#[test]
fn only_leased_entries_are_candidates() {
    let spec = AddSpec::default();
    let pending = BacklogEntry::new("repo", "pending-item", &spec, 0);
    assert_eq!(pending.status, BacklogStatus::Pending);
    let entries = vec![pending];
    assert!(reap::stale(&entries, &[], 10_000_000, |_| 1).is_empty());
}

// ---------------------------------------------------------------------------- queue class

/// The real repo-setup prompt's opening, verbatim in the shape that caused the bug: the goal text
/// becomes the entry's `item`, and it talks about warm pools because that is what the scripts it
/// writes are for.
const REPO_SETUP_ITEM: &str = "You are the repo setup agent for `nvim`.\n\nWrite bn setup scripts \
    that stay idempotent for future agents and warm pools.";

fn dispatched(item: &str) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        priority: 100,
        source: "dispatch".to_string(),
        ..Default::default()
    };
    BacklogEntry::new("nvim", item, &spec, 1000)
}

fn warming_job(job: &str) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        source: format!("cron:{job}"),
        exec: ExecSpec::Job {
            job: job.to_string(),
            repo: None,
            branch: None,
        },
        heavy: true,
        ..Default::default()
    };
    BacklogEntry::new("machine", &format!("run {job}"), &spec, 1000)
}

#[test]
fn a_dispatch_is_foreground_however_its_prompt_reads() {
    // The bug: `is_warming` substring-matched the item text for "warm". An entry's item is a short
    // job name for a job and the *entire prompt* for an agent, so a repo-setup agent that mentioned
    // warm pools was classified opportunistic and held for an away window — priority-100 work a
    // person had just clicked, frozen by a word in its own instructions.
    let class = classify(&dispatched(REPO_SETUP_ITEM));
    assert_eq!(class.mode, QueueMode::Foreground, "{REPO_SETUP_ITEM}");
    assert_eq!(class.subtype, QueueSubtype::Foreground);
}

#[test]
fn a_generated_dispatch_is_autonomous_because_it_says_so() {
    // The other half of the rule above. Prompt text is not evidence — but the producer's own
    // declaration is, and it is the only thing that can tell a repo-setup agent the dashboard
    // generated apart from the identical-looking sentence a person typed.
    //
    // Left foreground, six repos' setup agents sat pending for five hours behind the watched coding
    // lane, deferred `at local concurrency cap` by the work their owner was sitting in front of.
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        priority: 100,
        source: "dispatch".to_string(),
        machine: true,
        ..Default::default()
    };
    let entry = BacklogEntry::new("nvim", REPO_SETUP_ITEM, &spec, 1000);
    assert!(bn_core::queue::classify::is_machine(&entry));
    let class = classify(&entry);
    assert_eq!(class.mode, QueueMode::Autonomous);
    assert_eq!(class.subtype, QueueSubtype::Maintenance);
}

#[test]
fn a_person_still_outranks_what_the_producer_declared() {
    // `machine` is provenance; `class_override` is a person's later opinion. Someone who runs
    // `bn queue class <id> foreground` has looked at this entry and decided they want to watch it,
    // and a durable fact recorded at enqueue time must not quietly overrule them.
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        source: "dispatch".to_string(),
        machine: true,
        class_override: Some(ClassOverride::Foreground),
        ..Default::default()
    };
    let class = classify(&BacklogEntry::new("nvim", REPO_SETUP_ITEM, &spec, 1000));
    assert_eq!(class.mode, QueueMode::Foreground);
}

#[test]
fn a_job_is_machine_work_without_having_to_declare_it() {
    // What an entry *runs* answers the question on its own for jobs and shell commands: they launch
    // no agent and nobody types them. Only the agent-bearing producers need the explicit bit, which
    // is why `bn queue add --job pool-warm` needed no change to stop landing on the day's list.
    assert!(bn_core::queue::classify::is_machine(&warming_job(
        "pool-warm"
    )));
    assert!(!bn_core::queue::classify::is_machine(&dispatched(
        REPO_SETUP_ITEM
    )));
}

#[test]
fn warming_is_read_from_what_the_entry_declares() {
    for job in jobs::WARMING_JOBS {
        let class = classify(&warming_job(job));
        assert_eq!(class.mode, QueueMode::Opportunistic, "{job} mode");
        assert_eq!(class.subtype, QueueSubtype::Warming, "{job} subtype");
    }
}

#[test]
fn heavy_is_not_the_same_claim_as_warming() {
    // `verify` costs as much as a warm and is admitted the same way, but it is not warming and
    // must not inherit warming's away-only admission — it would never run on a working day, and
    // it is a verb someone asked for from the picker and is watching.
    assert!(jobs::is_heavy("verify"));
    assert!(!jobs::is_warming("verify"));
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        source: "cron:verify".to_string(),
        exec: ExecSpec::Job {
            job: "verify".to_string(),
            repo: None,
            branch: None,
        },
        heavy: true,
        ..Default::default()
    };
    let class = classify(&BacklogEntry::new("machine", "run verify", &spec, 1000));
    assert_eq!(class.subtype, QueueSubtype::Cron);
    assert_eq!(class.mode, QueueMode::Autonomous);
}

// ---------------------------------------------------------------------------- agent identity

fn setup_entry() -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        priority: 100,
        source: "dispatch".to_string(),
        agent: AgentSpec {
            branch: Some("repo-setup".to_string()),
            name: Some("repo-setup".to_string()),
            yolo: true,
            no_plan_review: true,
            with_main: false,
            stage: Some(Stage::Critique),
        },
        ..Default::default()
    };
    BacklogEntry::new("nvim", REPO_SETUP_ITEM, &spec, 1000)
}

#[test]
fn a_queued_agent_keeps_the_identity_it_was_declared_with() {
    // The whole point of `AgentSpec`: what comes out of the queue must be the work that went in.
    // Losing the branch put the agent on a slug of its own prompt; losing the name meant no
    // already-running guard could recognise it; losing yolo meant it stopped for an approval nobody
    // had been asked for.
    let entry = setup_entry();
    assert_eq!(entry.agent.branch.as_deref(), Some("repo-setup"));
    assert_eq!(entry.agent.name.as_deref(), Some("repo-setup"));
    assert!(entry.agent.yolo);
    // Losing this one reinstates a plan-review gate the caller declined, and the agent then waits
    // on a review nobody expected — the same class of silent-drop failure as the three above.
    assert!(entry.agent.no_plan_review);
    // And the stage. Dispatch's fallback can only infer investigate/plan/execute, so an entry that
    // loses `critique` does not fail — it launches on the *execute* stage's model, effort and
    // harness. Six of the nine stages are unreachable by inference, which is why the stage has to
    // be carried rather than re-derived.
    assert_eq!(entry.agent.stage, Some(Stage::Critique));
}

/// A line written before the field existed must still read, and must read as "no declared stage" so
/// the inference stays in charge of it.
#[test]
fn an_entry_from_before_stages_were_carried_still_infers_its_own() {
    let line = r#"{"id":"x","repo":"nvim","item":"do a thing","priority":0,"status":"pending","source":"dispatch","added_at":1000,"updated_at":1000,"agent":{"branch":"b","yolo":true}}"#;
    let entry: BacklogEntry = serde_json::from_str(line).expect("old line must still parse");
    assert_eq!(entry.agent.stage, None);
    assert_eq!(entry.agent.branch.as_deref(), Some("b"));
}

/// An entry that declares no stage must serialize exactly as it did before the field existed.
#[test]
fn an_undeclared_stage_adds_nothing_to_the_line() {
    let spec = AddSpec {
        source: "dispatch".to_string(),
        ..Default::default()
    };
    let entry = BacklogEntry::new("nvim", "plain work", &spec, 1000);
    let line = serde_json::to_string(&entry).expect("serialize");
    assert!(!line.contains("stage"), "stage must be omitted: {line}");
}

#[test]
fn identity_survives_a_round_trip_through_the_backlog_file() {
    let entry = setup_entry();
    let line = serde_json::to_string(&entry).expect("serialize");
    let back: BacklogEntry = serde_json::from_str(&line).expect("deserialize");
    assert_eq!(back.agent, entry.agent);
}

#[test]
fn a_line_written_before_identity_existed_still_reads() {
    // The back-compat guarantee. `agent` is skipped when empty, so every pre-existing backlog line
    // has no such key; reading one must produce "let dispatch derive it", not an error.
    let line = r#"{"id":"338","repo":"bn","item":"338","kind":"ship","status":"pending"}"#;
    let entry: BacklogEntry = serde_json::from_str(line).expect("legacy line must still parse");
    assert!(entry.agent.is_empty());
    assert_eq!(entry.agent.branch, None);
}

#[test]
fn an_entry_declaring_no_identity_writes_no_identity_key() {
    // The other half of back-compat: adding the field must not rewrite every existing line.
    let entry = BacklogEntry::new("bn", "338", &AddSpec::default(), 1000);
    let line = serde_json::to_string(&entry).expect("serialize");
    // The key, not the word: `exec` legitimately serializes as `{"run":"agent"}`.
    assert!(!line.contains("\"agent\":"), "{line}");
}

#[test]
fn a_re_add_carries_the_identity_across() {
    // `AddSpec::from_entry` exists so a reprioritize or re-add cannot silently drop an axis. A new
    // axis that it forgets is exactly the bug it was written to prevent.
    let spec = AddSpec::from_entry(&setup_entry());
    assert_eq!(spec.agent, setup_entry().agent);
}

#[test]
fn an_entry_is_named_by_its_declared_identity_not_its_prompt() {
    // Ten queued setup agents all rendered as "You are the repo setup agent for…", which is how a
    // queue full of work could look like a queue full of nothing in particular.
    assert_eq!(setup_entry().title(), "repo-setup");
    // With nothing declared, the first non-empty line is still far better than the whole prompt.
    let bare = BacklogEntry::new(
        "bn",
        "fix the parser\n\nlots of detail",
        &AddSpec::default(),
        0,
    );
    assert_eq!(bare.title(), "fix the parser");
}

// ---------------------------------------------------------------------------- nothing is lost

/// An entry with **every** carried axis set to something other than its default.
///
/// The point is that no field is left at a value a bug could accidentally reproduce: if a rebuild
/// drops `est_secs`, a zero here would still compare equal and the test would pass while the field
/// was being lost. Anything added to `BacklogEntry` should be given a non-default value here.
fn every_axis_set() -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Investigate,
        priority: 77,
        recurrence: Recurrence::Skip,
        touches: vec!["src/**".to_string()],
        substrate: Substrate::Remote,
        source: "dispatch".to_string(),
        controller_id: Some("ctl-1".to_string()),
        task: "t-42".to_string(),
        exec: ExecSpec::Skill {
            skill: "investigate".to_string(),
            arg: Some("the arg".to_string()),
        },
        requires: Requirements {
            skills: vec!["investigate".to_string()],
            harness: vec!["copilot".to_string()],
            mcps: vec!["bn".to_string()],
            capabilities: vec!["home:mac".to_string()],
            excludes: vec!["slow".to_string()],
        },
        est_secs: 900,
        not_after: Some(2000),
        not_before: Some(1500),
        recovery: true,
        heavy: true,
        machine: true,
        class_override: Some(ClassOverride::Opportunistic),
        agent: AgentSpec {
            branch: Some("repo-setup".to_string()),
            name: Some("repo-setup".to_string()),
            yolo: true,
            no_plan_review: true,
            with_main: false,
            stage: Some(Stage::Plan),
        },
    };
    BacklogEntry::new("nvim", "set up nvim", &spec, 1000)
}

#[test]
fn re_adding_an_entry_reproduces_every_axis_of_it() {
    // The guarantee, stated as one equality instead of a checklist someone has to maintain.
    //
    // `AddSpec::from_entry` is what the re-add and reprioritize paths run through, and it is a
    // hand-written field-by-field copy — the exact shape that silently drops a newly added axis.
    // Comparing whole entries means a field added to `BacklogEntry` and forgotten in `from_entry`
    // fails here, rather than in production months later as work that came back subtly different
    // from the work that went in.
    let original = every_axis_set();
    let rebuilt = BacklogEntry::new(
        &original.repo,
        &original.item,
        &AddSpec::from_entry(&original),
        original.added_at,
    );

    // The only legitimate differences are the fields a *fresh* entry derives rather than carries:
    // its lifecycle state and who holds it. Everything else must survive verbatim.
    let mut expected = original.clone();
    expected.status = BacklogStatus::Pending;
    expected.assigned_to = None;
    expected.updated_at = original.added_at;

    assert_eq!(rebuilt, expected);
}

#[test]
fn a_fully_populated_entry_survives_the_backlog_file() {
    // The other way a field is lost: present in memory, absent on disk. A `skip_serializing_if`
    // that is wrong, or a `#[serde(skip)]` added for convenience, loses the value at the first
    // restart — and the backlog is the thing that is supposed to make work durable.
    let original = every_axis_set();
    let line = serde_json::to_string(&original).expect("serialize");
    let back: BacklogEntry = serde_json::from_str(&line).expect("deserialize");
    assert_eq!(back, original);
}

// ---------------------------------------------------------------- launch retry, and its ending

/// The schedule that replaced a flat ten-minute wait. Doubling keeps the early retries close
/// together, where a transient blocker is most likely to have cleared.
#[test]
fn the_wait_after_a_failed_launch_doubles_up_to_a_ceiling() {
    assert_eq!(launch_retry_delay(1), Some(600));
    assert_eq!(launch_retry_delay(2), Some(1200));
    assert_eq!(launch_retry_delay(3), Some(2400));
    // Capped, so doubling stays a backoff rather than a disappearance.
    assert_eq!(launch_retry_delay(4), Some(3600));
}

/// The half the backoff alone never had. `bn dispatch` refuses an item whose branch is already
/// active, and a merged branch's leftover ref satisfies that test forever — so "wait and retry"
/// with no floor meant 15,559 failed launches in 52 hours on the machine this was written for.
#[test]
fn the_attempts_run_out_instead_of_retrying_forever() {
    assert_eq!(launch_retry_delay(LAUNCH_ATTEMPT_CAP), None);
    assert_eq!(launch_retry_delay(LAUNCH_ATTEMPT_CAP + 1), None);
}

fn leased_backlog(id_seed: &str) -> Backlog {
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        exec: ExecSpec::Agent,
        ..Default::default()
    };
    let mut e = BacklogEntry::new("repo", id_seed, &spec, 1000);
    e.status = BacklogStatus::Leased;
    Backlog::from_lines([serde_json::to_string(&e).expect("serialize")])
}

#[test]
fn a_failed_launch_counts_the_attempt_and_holds_the_entry() {
    let mut bl = leased_backlog("work");
    let id = bl.entries()[0].id.clone();

    let (entry, outcome) = bl
        .fail_launch("repo", &id, 5_000, launch_retry_delay)
        .expect("leased entry");

    assert_eq!(
        outcome,
        LaunchFailure::Retrying {
            attempt: 1,
            retry_at: 5_600
        }
    );
    // Back in the running, but not before the wait is up.
    assert_eq!(entry.status, BacklogStatus::Pending);
    assert_eq!(entry.not_before, Some(5_600));
    assert_eq!(entry.launch_failures, 1);
}

/// The entry stops being pending at all, which is the whole point: a permanently blocked item now
/// has a terminal state somebody can see, instead of reading `snoozed` for as long as the box is up.
#[test]
fn the_last_failed_launch_resolves_the_entry_rather_than_holding_it() {
    let mut bl = leased_backlog("doomed");
    let id = bl.entries()[0].id.clone();

    for attempt in 1..LAUNCH_ATTEMPT_CAP {
        // Re-lease between attempts, as the tick does when the snooze expires.
        bl.lease_ids("repo", std::slice::from_ref(&id), 1_000);
        let (_, outcome) = bl
            .fail_launch("repo", &id, 5_000, launch_retry_delay)
            .expect("leased entry");
        assert!(
            matches!(outcome, LaunchFailure::Retrying { .. }),
            "{attempt}"
        );
    }
    bl.lease_ids("repo", std::slice::from_ref(&id), 1_000);
    let (entry, outcome) = bl
        .fail_launch("repo", &id, 5_000, launch_retry_delay)
        .expect("leased entry");

    assert_eq!(
        outcome,
        LaunchFailure::GaveUp {
            attempt: LAUNCH_ATTEMPT_CAP
        }
    );
    assert_eq!(entry.status, BacklogStatus::Failed);
}

/// Consecutive, not lifetime. A lease reclaimed after a healthy launch would otherwise carry old
/// failures into a fresh attempt and give up early on work that had been starting fine.
#[test]
fn a_launch_that_works_forgets_the_failures_before_it() {
    let mut bl = leased_backlog("flaky");
    let id = bl.entries()[0].id.clone();
    bl.fail_launch("repo", &id, 5_000, launch_retry_delay);
    bl.lease_ids("repo", std::slice::from_ref(&id), 1_000);

    assert!(bl.clear_launch_failures("repo", &id).is_some());
    assert_eq!(bl.get("repo", &id).expect("entry").launch_failures, 0);
    // Already zero: nothing changed, so the caller writes no line.
    assert!(bl.clear_launch_failures("repo", &id).is_none());
}

/// A pending entry has no launch in flight to fail, so this cannot be used to resolve work the
/// caller never leased — the same discipline `mark_leased` enforces.
#[test]
fn only_a_leased_entry_can_fail_its_launch() {
    let mut bl = leased_backlog("pending-one");
    let id = bl.entries()[0].id.clone();
    bl.fail_launch("repo", &id, 5_000, launch_retry_delay); // -> Pending
    assert!(bl
        .fail_launch("repo", &id, 6_000, launch_retry_delay)
        .is_none());
}

/// `[queue.jobs]` replaces the built-in heavy list rather than adding to it.
///
/// Subtractive on purpose: the interesting case is a machine whose `build.sh` takes thirty seconds
/// and has a reason to say `build` is not heavy. A list that could only grow could not say it.
#[test]
fn a_machine_can_say_which_jobs_are_expensive_for_it() {
    let mut cfg = bn_core::resolver::QueueConfig::default();
    assert!(cfg.is_heavy("pool-warm"), "the built-in list by default");
    assert!(!cfg.is_heavy("some-cron"));

    cfg.heavy_jobs = Some(vec!["some-cron".to_string()]);
    assert!(cfg.is_heavy("some-cron"), "what the machine declared");
    assert!(
        !cfg.is_heavy("pool-warm"),
        "and only that — the override replaces, so a cheap build can be declared cheap"
    );
}

/// A per-job priority wins over the derived recovery/heavy/cron banding.
#[test]
fn a_machine_can_re_rate_one_job_without_touching_the_rest() {
    let mut cfg = bn_core::resolver::QueueConfig::default();
    assert_eq!(
        cfg.priority_for("pool-warm"),
        jobs::priority_for("pool-warm")
    );

    cfg.job_priority.insert("pool-warm".to_string(), 42);
    assert_eq!(cfg.priority_for("pool-warm"), 42);
    assert_eq!(
        cfg.priority_for("agent-supervise"),
        jobs::priority_for("agent-supervise"),
        "an override names one job, not a policy"
    );
}
