//! Fit: whether work will finish before the machine is claimed.
//!
//! The dispatcher had no notion of this. It would start a three-hour agent twenty minutes before a
//! window it was supposed to leave free, because admission asked "is there a slot?" and never "is
//! there time?". The cases here are the ones that decide whether such a gate is safe to have at
//! all: it must be inert on a machine that configured no window, it must never apply to work that
//! costs nothing, and it must have a timeout — a preference that can hold work forever is a
//! deadlock wearing better manners.
use bn_core::advise::{AttnSignal, GoalContext};
use bn_core::backlog::{AddSpec, BacklogEntry, ExecutionTarget};
use bn_core::hosts::{NodeRecord, NodeStatus};
use bn_core::meetings::FIT_BUFFER_SECS;
use bn_core::queue::durations::Durations;
use bn_core::queue::kind::{BacklogKind, ExecSpec};
use bn_core::queue::pause::PauseState;
use bn_core::queue::tick::{
    plan_tick, DeferReason, Horizon, KindCaps, Presence, TickInputs, TickPlan, FIT_PATIENCE_SECS,
};
use bn_core::scheduler::{Caps, Observed};

const NOW: i64 = 1_000_000;
const HOUR: i64 = 3600;

/// An agent — work that occupies the machine, and so the only kind fit has an opinion about.
fn agent(id: &str, est_secs: i64) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        exec: ExecSpec::Agent,
        ..Default::default()
    };
    let mut e = BacklogEntry::new("bn", id, &spec, NOW - 60);
    e.id = id.to_string();
    e.est_secs = est_secs;
    e
}

/// A headless cron — express work, which runs inline in seconds and cannot overrun anything.
fn express(id: &str, est_secs: i64) -> BacklogEntry {
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        exec: ExecSpec::Job {
            job: id.to_string(),
            repo: None,
            branch: None,
        },
        ..Default::default()
    };
    let mut e = BacklogEntry::new("machine", id, &spec, NOW - 60);
    e.id = id.to_string();
    e.est_secs = est_secs;
    e
}

struct Fixture {
    pending: Vec<BacklogEntry>,
    horizon: Option<Horizon>,
}

impl Fixture {
    fn plan(&self) -> TickPlan {
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
        let durations = Durations::default();
        let goals: Vec<GoalContext> = Vec::new();
        let attention: Vec<AttnSignal> = Vec::new();
        plan_tick(&TickInputs {
            now: NOW,
            machine: "testbox",
            pending: &self.pending,
            in_flight: &[],
            nodes: &nodes,
            caps: &caps,
            observed: &observed,
            kind_caps: &kind_caps,
            durations: &durations,
            horizon: self.horizon.as_ref(),
            kinds: None,
            goals: &goals,
            attention: &attention,
            max_express: 20,
            windows: &[],
            presence: Presence {
                idle: true,
                away_window: false,
            },
            mandatory_warming: false,
            pauses: &pauses,
        })
    }
}

fn horizon(in_secs: i64) -> Option<Horizon> {
    Some(Horizon {
        at: NOW + in_secs,
        source: "pool-warm-after-work".to_string(),
    })
}

fn started(plan: &TickPlan) -> Vec<String> {
    plan.start.iter().map(|d| d.id.clone()).collect()
}

fn reason_for<'a>(plan: &'a TickPlan, id: &str) -> Option<&'a DeferReason> {
    plan.deferred.iter().find(|d| d.id == id).map(|d| &d.reason)
}

/// The case in the brief: a three-hour agent, twenty minutes before the window opens.
#[test]
fn work_that_would_overrun_the_window_is_held() {
    let fx = Fixture {
        pending: vec![agent("long-agent", 3 * HOUR)],
        horizon: horizon(20 * 60),
    };
    let plan = fx.plan();
    assert!(started(&plan).is_empty());
    assert_eq!(reason_for(&plan, "long-agent"), Some(&DeferReason::WontFit));
}

/// And the same window is no obstacle to work that fits inside it.
#[test]
fn work_that_finishes_in_time_still_starts() {
    let fx = Fixture {
        pending: vec![agent("short-agent", 5 * 60)],
        horizon: horizon(60 * 60),
    };
    assert_eq!(started(&fx.plan()), vec!["short-agent"]);
}

/// The buffer is real: work that would finish exactly as the window opens does not fit it.
#[test]
fn the_buffer_leaves_a_moment_before_the_window() {
    let snug = Fixture {
        pending: vec![agent("snug", HOUR)],
        horizon: horizon(HOUR),
    };
    assert_eq!(
        reason_for(&snug.plan(), "snug"),
        Some(&DeferReason::WontFit)
    );

    let trimmed = Fixture {
        pending: vec![agent("trimmed", HOUR - FIT_BUFFER_SECS)],
        horizon: horizon(HOUR),
    };
    assert_eq!(started(&trimmed.plan()), vec!["trimmed"]);
}

/// The safety property that keeps every existing dispatcher test unchanged: with no deadline
/// window configured there is no horizon, and the gate does not exist.
#[test]
fn no_horizon_means_no_gate() {
    let fx = Fixture {
        pending: vec![agent("very-long-agent", 30 * HOUR)],
        horizon: None,
    };
    assert_eq!(started(&fx.plan()), vec!["very-long-agent"]);
}

/// Fit is about occupying the machine. Express work runs inline in seconds and cannot overrun a
/// window, so asking whether it fits would only ever produce a wrong answer.
#[test]
fn express_work_is_never_asked_whether_it_fits() {
    let fx = Fixture {
        pending: vec![express("run-watch", 10 * HOUR)],
        horizon: horizon(60),
    };
    assert_eq!(started(&fx.plan()), vec!["run-watch"]);
}

/// The escape valve, and the reason this is a preference rather than a wall. Work whose estimate
/// exceeds every window the machine will ever offer would otherwise be deferred forever — the same
/// failure as a deadlock, with better manners.
#[test]
fn work_that_never_fits_runs_anyway_once_it_has_waited() {
    let mut impatient = agent("four-hour-agent", 4 * HOUR);
    impatient.added_at = NOW - FIT_PATIENCE_SECS + 1;
    let held = Fixture {
        pending: vec![impatient],
        horizon: horizon(HOUR),
    };
    assert_eq!(
        reason_for(&held.plan(), "four-hour-agent"),
        Some(&DeferReason::WontFit)
    );

    let mut patient = agent("four-hour-agent", 4 * HOUR);
    patient.added_at = NOW - FIT_PATIENCE_SECS;
    let admitted = Fixture {
        pending: vec![patient],
        horizon: horizon(HOUR),
    };
    assert_eq!(started(&admitted.plan()), vec!["four-hour-agent"]);
}

/// Being held is not being dropped. A three-hour job twenty minutes before a window has not lost
/// its moment — it has lost *this* moment — so it stays pending rather than being resolved.
#[test]
fn work_that_does_not_fit_is_deferred_not_expired() {
    let fx = Fixture {
        pending: vec![agent("long-agent", 3 * HOUR)],
        horizon: horizon(20 * 60),
    };
    let plan = fx.plan();
    assert!(
        plan.expired.is_empty(),
        "fit must never resolve work; that is what not_after is for"
    );
    assert_eq!(plan.deferred.len(), 1);
}

/// The deferral names the window that is holding it and how much room there was, because "why is
/// this not running" should be answerable from the decision alone.
#[test]
fn the_deferral_says_what_it_needed_and_what_was_free() {
    let fx = Fixture {
        pending: vec![agent("long-agent", 3 * HOUR)],
        horizon: horizon(20 * 60),
    };
    let plan = fx.plan();
    let detail = &plan
        .deferred
        .iter()
        .find(|d| d.id == "long-agent")
        .expect("held")
        .detail;
    assert!(detail.contains("pool-warm-after-work"), "detail: {detail}");
    assert!(detail.contains("needs"), "detail: {detail}");
}

/// A shorter job takes the slot a longer one could not use, rather than the window stalling the
/// whole queue.
#[test]
fn a_window_holds_the_long_job_without_stalling_the_short_one() {
    let fx = Fixture {
        pending: vec![agent("long-agent", 3 * HOUR), agent("short-agent", 60)],
        horizon: horizon(20 * 60),
    };
    let plan = fx.plan();
    assert_eq!(started(&plan), vec!["short-agent"]);
    assert_eq!(reason_for(&plan, "long-agent"), Some(&DeferReason::WontFit));
}
