//! What the queue learns about its own work, tested directly.
//!
//! `Durations::observe` is pure over a list of runs, which is the whole reason this is testable
//! without a state dir or a clock. The cases here are the ones that decide whether an estimate is
//! worth acting on: a bucket too thin to believe, a run that measures a failure rather than work,
//! and the fallback chain that has to end at a flat default rather than at nothing.
use bn_core::backlog::{AddSpec, BacklogEntry, ExecutionTarget};
use bn_core::queue::durations::{
    Durations, Source, DEFAULT_EST_SECS, MAX_PLAUSIBLE_SECS, MIN_SAMPLES,
};
use bn_core::queue::kind::BacklogKind;
use bn_core::queue::log::{Run, Tone};

const NOW: i64 = 1_000_000;

/// A completed run of `id` in `repo` that took `secs`.
fn run(repo: &str, id: &str, kind: BacklogKind, secs: i64) -> Run {
    Run {
        key: format!("{repo}/{id}"),
        started_at: NOW,
        ended_at: Some(NOW + secs),
        duration_secs: secs,
        repo: repo.to_string(),
        id: id.to_string(),
        work: format!("run {id}"),
        work_kind: kind.label().to_string(),
        lane: String::new(),
        exec: "job".to_string(),
        node: String::new(),
        harness: String::new(),
        priority: None,
        rationale: String::new(),
        task: String::new(),
        entry_status: String::new(),
        outcome: "completed".to_string(),
        summary: String::new(),
        tone: Tone::Info,
        transitions: Vec::new(),
    }
}

fn entry(repo: &str, id: &str, kind: BacklogKind) -> BacklogEntry {
    let spec = AddSpec {
        kind,
        ..Default::default()
    };
    let mut e = BacklogEntry::new(repo, id, &spec, NOW);
    e.id = id.to_string();
    e
}

/// Enough runs of one job, all the same length, and the estimator says so.
#[test]
fn a_job_that_has_run_enough_times_teaches_its_own_duration() {
    let runs: Vec<Run> = (0..MIN_SAMPLES)
        .map(|_| run("machine", "run-watch", BacklogKind::Cron, 10))
        .collect();
    let learned = Durations::observe(&runs);
    let est = learned.estimate(&entry("machine", "run-watch", BacklogKind::Cron));
    assert_eq!(est.secs, 10);
    assert_eq!(est.source, Source::LearnedWork);
}

/// One run is an anecdote. The whole point of the sample floor is that a machine cannot squint at
/// an `n` the way a person reading `bn calib` can.
#[test]
fn a_bucket_below_the_sample_floor_is_not_trusted() {
    let runs: Vec<Run> = (0..MIN_SAMPLES - 1)
        .map(|_| run("machine", "run-watch", BacklogKind::Cron, 10))
        .collect();
    let learned = Durations::observe(&runs);
    let est = learned.estimate(&entry("machine", "run-watch", BacklogKind::Cron));
    assert_eq!(est.secs, DEFAULT_EST_SECS);
    assert_eq!(est.source, Source::Default);
}

/// The median, not the mean — and this is the case that decides it. `keep-warm` on the machine
/// this was written for has a median of 3s against a p90 of 304s; a mean would describe neither.
#[test]
fn one_slow_run_does_not_move_the_estimate() {
    let mut runs: Vec<Run> = (0..4)
        .map(|_| run("machine", "run-keep-warm", BacklogKind::Cron, 3))
        .collect();
    runs.push(run("machine", "run-keep-warm", BacklogKind::Cron, 3000));
    let learned = Durations::observe(&runs);
    assert_eq!(
        learned.for_entry(&entry("machine", "run-keep-warm", BacklogKind::Cron)),
        3,
        "the median must survive an outlier a mean would not"
    );
}

/// A failure measures how long the machine took to give up, not how long the work takes.
#[test]
fn only_completions_teach_anything() {
    for outcome in ["failed", "expired", "reclaimed", "in-flight", "skipped"] {
        let runs: Vec<Run> = (0..MIN_SAMPLES + 2)
            .map(|_| {
                let mut r = run("machine", "run-watch", BacklogKind::Cron, 7);
                r.outcome = outcome.to_string();
                r
            })
            .collect();
        let learned = Durations::observe(&runs);
        assert!(
            learned.is_empty(),
            "{outcome} runs must not teach a duration"
        );
    }
}

/// Runs pair by `(repo, id)` and an id recurs, so a dispatch whose finish was never written can be
/// closed by a much later event. Such a run measures the silence, not the work.
#[test]
fn an_implausibly_long_run_is_not_evidence() {
    let runs: Vec<Run> = (0..MIN_SAMPLES + 1)
        .map(|_| {
            run(
                "machine",
                "run-watch",
                BacklogKind::Cron,
                MAX_PLAUSIBLE_SECS + 1,
            )
        })
        .collect();
    assert!(Durations::observe(&runs).is_empty());
}

/// Work with no history of its own still gets a better answer than the flat default, from the
/// kind it belongs to.
#[test]
fn work_with_no_history_falls_back_to_its_kind() {
    let runs: Vec<Run> = (0..MIN_SAMPLES)
        .map(|i| run("machine", &format!("run-job-{i}"), BacklogKind::Cron, 12))
        .collect();
    let learned = Durations::observe(&runs);
    let est = learned.estimate(&entry("machine", "something-never-seen", BacklogKind::Cron));
    assert_eq!(est.secs, 12);
    assert_eq!(est.source, Source::LearnedKind);
}

/// A producer that took the trouble to size its work knows something the log cannot. If a learned
/// median could override it, the field would be pointless.
#[test]
fn a_declared_estimate_beats_everything_learned() {
    let runs: Vec<Run> = (0..MIN_SAMPLES + 5)
        .map(|_| run("machine", "run-watch", BacklogKind::Cron, 10))
        .collect();
    let learned = Durations::observe(&runs);
    let mut e = entry("machine", "run-watch", BacklogKind::Cron);
    e.est_secs = 4242;
    let est = learned.estimate(&e);
    assert_eq!(est.secs, 4242);
    assert_eq!(est.source, Source::Declared);
}

/// The regression guard for "we made the estimator smarter and quietly changed every projection":
/// with nothing learned, every entry must estimate exactly as it did before this module existed.
#[test]
fn an_empty_history_reproduces_the_old_flat_default() {
    let learned = Durations::observe(&[]);
    assert!(learned.is_empty());
    for kind in [BacklogKind::Cron, BacklogKind::Ship, BacklogKind::ReviewPr] {
        let est = learned.estimate(&entry("bn", "anything", kind));
        assert_eq!(est.secs, DEFAULT_EST_SECS);
        assert_eq!(est.source, Source::Default);
    }
}

/// One-shot work lands in a bucket of one and is never trusted — the sample floor covers it
/// without a special case for "this id will not recur".
#[test]
fn one_shot_work_never_accumulates_a_bucket_of_its_own() {
    let runs = vec![
        run("bn", "fix-the-header", BacklogKind::Ship, 1800),
        run("bn", "add-a-flag", BacklogKind::Ship, 2400),
        run("bn", "rename-a-thing", BacklogKind::Ship, 1200),
    ];
    let learned = Durations::observe(&runs);
    // No single ship id repeats, so none of them teaches its own duration...
    let est = learned.estimate(&entry("bn", "fix-the-header", BacklogKind::Ship));
    assert_eq!(est.source, Source::LearnedKind);
    // ...but together they say what a ship costs: median(1200, 1800, 2400).
    assert_eq!(est.secs, 1800);
}

/// The same id in two repos is two different pieces of work.
#[test]
fn buckets_are_keyed_by_repo_as_well_as_id() {
    let mut runs: Vec<Run> = (0..MIN_SAMPLES)
        .map(|_| run("bn", "run-build", BacklogKind::Cron, 30))
        .collect();
    runs.extend((0..MIN_SAMPLES).map(|_| run("arena", "run-build", BacklogKind::Cron, 300)));
    let learned = Durations::observe(&runs);
    assert_eq!(
        learned.for_entry(&entry("bn", "run-build", BacklogKind::Cron)),
        30
    );
    assert_eq!(
        learned.for_entry(&entry("arena", "run-build", BacklogKind::Cron)),
        300
    );
}

/// An even sample count averages the two middle values rather than picking a side.
#[test]
fn an_even_sample_count_takes_the_middle_pair() {
    let runs = vec![
        run("machine", "run-x", BacklogKind::Cron, 10),
        run("machine", "run-x", BacklogKind::Cron, 20),
        run("machine", "run-x", BacklogKind::Cron, 30),
        run("machine", "run-x", BacklogKind::Cron, 40),
    ];
    let learned = Durations::observe(&runs);
    assert_eq!(
        learned.for_entry(&entry("machine", "run-x", BacklogKind::Cron)),
        25
    );
}

// ------------------------------------------------------- what the estimate is actually *for*

use bn_core::advise::{AttnSignal, GoalContext};
use bn_core::hosts::{NodeRecord, NodeStatus};
use bn_core::queue::kind::ExecSpec;
use bn_core::queue::pause::PauseState;
use bn_core::queue::tick::{KindCaps, Presence, TickInputs};
use bn_core::queue::timeline::project;
use bn_core::scheduler::{Caps, Observed};

/// A machine-wide cron the queue can run on the express path (no worktree, no lane slot).
fn cron_entry(id: &str) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        exec: ExecSpec::Job {
            job: id.to_string(),
            repo: None,
            branch: None,
        },
        ..Default::default()
    };
    let mut e = BacklogEntry::new("machine", id, &spec, NOW);
    e.id = id.to_string();
    e
}

fn project_backlog(pending: &[BacklogEntry], est: &Durations, horizon: i64) -> usize {
    let nodes = vec![NodeRecord {
        node_id: "testbox".to_string(),
        target: ExecutionTarget::Local,
        status: NodeStatus::Ready,
        slots: 8,
        last_heartbeat: NOW,
        ..Default::default()
    }];
    let caps = Caps {
        max_local: 4,
        research: Some(2),
        min_free_ram_mb: None,
        max_loadavg: None,
        max_swap_pct: None,
        tokens_per_hour: None,
        daily_cost_usd: None,
        daily_cost_usd_coding: None,
        daily_cost_usd_research: None,
        plan_gated: false,
        remote_overflow: false,
        local_owned: true,
    };
    let observed = Observed::default();
    let kind_caps = KindCaps(Default::default());
    let pauses = PauseState::default();
    let goals: Vec<GoalContext> = Vec::new();
    let attention: Vec<AttnSignal> = Vec::new();
    let inputs = TickInputs {
        now: NOW,
        machine: "testbox",
        pending,
        in_flight: &[],
        horizon: None,
        durations: est,
        nodes: &nodes,
        caps: &caps,
        observed: &observed,
        kind_caps: &kind_caps,
        pauses: &pauses,
        kinds: None,
        goals: &goals,
        attention: &attention,
        heavy_cap: 1,
        max_express: 20,
        windows: &[],
        presence: Presence {
            idle: true,
            away_window: false,
        },
        mandatory_warming: false,
    };
    // Entries the replay never placed carry no start time — "counted, never drawn".
    project(&inputs, est, horizon)
        .iter()
        .filter(|p| p.at.is_none())
        .count()
}

/// The reason this module exists, stated as a projection.
///
/// Thirty ten-second crons are ninety seconds of work for a machine that runs twenty per pass. Told
/// they take twenty minutes each — the flat default — the replay walks the horizon in twenty-minute
/// strides and reports most of them as unreachable. That is how `bn queue timeline` came to say
/// "beyond the horizon at projected throughput" about a backlog that would drain before you could
/// read the sentence.
#[test]
fn a_backlog_of_short_crons_fits_the_horizon_once_their_length_is_known() {
    let pending: Vec<BacklogEntry> = (0..30)
        .map(|i| cron_entry(&format!("run-job-{i}")))
        .collect();
    let horizon = 15 * 60;

    let guessed = project_backlog(&pending, &Durations::observe(&[]), horizon);

    let history: Vec<Run> = pending
        .iter()
        .flat_map(|e| (0..MIN_SAMPLES).map(move |_| run("machine", &e.id, BacklogKind::Cron, 10)))
        .collect();
    let learned = project_backlog(&pending, &Durations::observe(&history), horizon);

    assert!(
        guessed > 0,
        "the flat default should strand work inside a quarter-hour horizon"
    );
    assert_eq!(
        learned, 0,
        "every ten-second cron fits a fifteen-minute horizon; {learned} were reported unreachable"
    );
}
