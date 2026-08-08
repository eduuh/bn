//! Priority windows: "prefer warming the pool while I am at lunch, and again before I stop".
//!
//! A window is a recurring span plus a priority delta, applied at *ranking* rather than written
//! onto the entry — so the boost appears and disappears with the clock and nothing has to be undone
//! when the window closes. That makes it the kind of feature that can quietly stop working without
//! anything looking broken: the work still runs, just never preferentially, and the only symptom is
//! that the thing you wanted at lunch happened at four.
//!
//! Every time is derived from the spec itself via `previous_occurrence`, never hardcoded, so these
//! do not depend on the machine's timezone.
use bn_core::backlog::{AddSpec, BacklogEntry, BacklogKind, ExecSpec, ExecutionTarget};
use bn_core::hosts::{NodeRecord, NodeStatus};
use bn_core::queue::tick::{plan_tick, KindCaps, TickInputs};
use bn_core::queue::window::{self, PriorityWindow};
use bn_core::scheduler::{Caps, Observed};
use bn_core::staff_schedule::previous_occurrence;

/// Noon on a weekday, expressed as "whenever this spec last fired" so the test carries no timezone
/// assumption of its own.
const LUNCH: &str = "*-*-* 12:30:00";
const HALF_HOUR: i64 = 1800;

/// A reference instant, and the start of the lunch window that precedes it.
fn lunch_start() -> i64 {
    // Any instant will do; `previous_occurrence` walks back to the spec's last firing from here.
    previous_occurrence(LUNCH, 1_800_000_000).expect("the lunch spec must be parseable")
}

fn window(name: &str, boost: i64, matches: &[&str]) -> PriorityWindow {
    PriorityWindow {
        name: name.to_string(),
        at: LUNCH.to_string(),
        secs: HALF_HOUR,
        boost,
        matches: matches.iter().map(|s| s.to_string()).collect(),
        mode: Default::default(),
    }
}

fn entry(id: &str, kind: BacklogKind, priority: i64, source: &str) -> BacklogEntry {
    let spec = AddSpec {
        kind,
        priority,
        source: source.to_string(),
        exec: ExecSpec::Shell {
            cmd: "true".to_string(),
        },
        ..Default::default()
    };
    let mut e = BacklogEntry::new("repo", id, &spec, 0);
    e.id = id.to_string();
    e
}

// ---------------------------------------------------------------------------- opening and closing

#[test]
fn a_window_is_open_from_its_firing_until_its_duration_elapses() {
    let w = window("lunch", 50, &["*"]);
    let start = lunch_start();
    assert!(w.is_open(start), "open at the instant it fires");
    assert!(
        w.is_open(start + HALF_HOUR - 1),
        "open until the last second"
    );
    assert!(!w.is_open(start + HALF_HOUR), "closed once it elapses");
    assert!(!w.is_open(start - 1), "not open before it fires");
}

#[test]
fn a_zero_length_window_is_never_open() {
    // Guards the `secs.max(0)` clamp: `now >= start && now < start` is empty, so a misconfigured
    // window is inert rather than permanently open.
    let mut w = window("broken", 50, &["*"]);
    w.secs = 0;
    assert!(!w.is_open(lunch_start()));
    w.secs = -60;
    assert!(!w.is_open(lunch_start()));
}

#[test]
fn closes_in_counts_down_while_open_and_is_absent_otherwise() {
    let w = window("lunch", 50, &["*"]);
    let start = lunch_start();
    assert_eq!(w.closes_in(start), Some(HALF_HOUR));
    assert_eq!(w.closes_in(start + 600), Some(HALF_HOUR - 600));
    assert_eq!(w.closes_in(start + HALF_HOUR), None);
}

#[test]
fn open_reports_only_the_windows_in_force() {
    let mut shut = window("evening", 50, &["*"]);
    shut.at = "*-*-* 23:59:00".to_string();
    shut.secs = 60;
    let windows = vec![window("lunch", 50, &["*"]), shut];
    let names: Vec<&str> = window::open(&windows, lunch_start())
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(names, vec!["lunch"]);
}

// ---------------------------------------------------------------------------- what it applies to

#[test]
fn a_window_matches_by_kind_repo_source_prefix_or_everything() {
    let e = entry("warm", BacklogKind::Cron, 0, "cron:pool-warm");
    assert!(window("w", 1, &["*"]).applies_to(&e));
    assert!(window("w", 1, &["kind:cron"]).applies_to(&e));
    assert!(window("w", 1, &["repo:repo"]).applies_to(&e));
    // Prefix, so one entry covers every scheduled job rather than listing them.
    assert!(window("w", 1, &["source:cron:"]).applies_to(&e));
    assert!(window("w", 1, &["source:cron:pool-warm"]).applies_to(&e));

    assert!(!window("w", 1, &["kind:ship"]).applies_to(&e));
    assert!(!window("w", 1, &["repo:elsewhere"]).applies_to(&e));
    assert!(!window("w", 1, &["source:attn:"]).applies_to(&e));
    assert!(!window("w", 1, &["nonsense"]).applies_to(&e));
}

#[test]
fn matching_is_case_insensitive_for_kind_and_repo() {
    let e = entry("warm", BacklogKind::Cron, 0, "cron:pool-warm");
    assert!(window("w", 1, &["kind:CRON"]).applies_to(&e));
    assert!(window("w", 1, &["repo:REPO"]).applies_to(&e));
}

// ---------------------------------------------------------------------------- the boost

#[test]
fn boost_applies_only_while_open_and_only_to_what_matches() {
    let e = entry("warm", BacklogKind::Cron, 0, "cron:pool-warm");
    let w = vec![window("lunch", 40, &["source:cron:"])];
    let start = lunch_start();
    assert_eq!(window::boost_for(&e, &w, start), 40);
    assert_eq!(
        window::boost_for(&e, &w, start + HALF_HOUR),
        0,
        "a closed window contributes nothing"
    );
    let other = entry("ship", BacklogKind::Ship, 0, "manual");
    assert_eq!(
        window::boost_for(&other, &w, start),
        0,
        "an open window does not boost work it does not name"
    );
}

#[test]
fn overlapping_windows_sum_rather_than_max() {
    // "It is both lunch and a Friday" is a deliberate statement, and the arithmetic should say what
    // the configuration says.
    let e = entry("warm", BacklogKind::Cron, 0, "cron:pool-warm");
    let w = vec![
        window("lunch", 40, &["source:cron:"]),
        window("also-lunch", 5, &["kind:cron"]),
    ];
    assert_eq!(window::boost_for(&e, &w, lunch_start()), 45);
}

// ---------------------------------------------------------------------------- the point of it all

fn plan_order(pending: &[BacklogEntry], windows: &[PriorityWindow], now: i64) -> Vec<String> {
    let node = NodeRecord {
        node_id: "testbox".to_string(),
        target: ExecutionTarget::Local,
        status: NodeStatus::Ready,
        slots: 8,
        last_heartbeat: now,
        ..Default::default()
    };
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
    let plan = plan_tick(&TickInputs {
        now,
        machine: "testbox",
        pending,
        in_flight: &[],
        nodes: &[node],
        caps: &caps,
        observed: &observed,
        kind_caps: &kind_caps,
        kinds: None,
        goals: &[],
        attention: &[],
        max_express: 20,
        windows,
        presence: bn_core::queue::tick::Presence {
            idle: true,
            away_window: false,
        },
    });
    plan.start.iter().map(|d| d.id.clone()).collect()
}

#[test]
fn an_open_window_actually_reorders_the_dispatch() {
    // The whole feature, end to end. Without the window the higher base priority goes first;
    // during lunch the boosted class overtakes it. Anything less than this passing means windows
    // are configuration that reads well and changes nothing.
    let pending = vec![
        entry("warm", BacklogKind::Cron, 0, "cron:pool-warm"),
        entry("routine", BacklogKind::Cron, 10, "cron:other"),
    ];
    let windows = vec![window("lunch", 50, &["source:cron:pool-warm"])];
    let start = lunch_start();

    assert_eq!(
        plan_order(&pending, &windows, start + HALF_HOUR),
        vec!["routine", "warm"],
        "outside the window, base priority decides"
    );
    assert_eq!(
        plan_order(&pending, &windows, start),
        vec!["warm", "routine"],
        "inside it, the boosted work is preferred"
    );
}

#[test]
fn a_window_prefers_rather_than_flattens() {
    // The boost is a delta, so relative order *within* the boosted set survives. A window says
    // "prefer this class now", not "make it all equally urgent".
    let pending = vec![
        entry("warm-a", BacklogKind::Cron, 1, "cron:pool-warm"),
        entry("warm-b", BacklogKind::Cron, 2, "cron:pool-warm"),
    ];
    let windows = vec![window("lunch", 50, &["source:cron:"])];
    assert_eq!(
        plan_order(&pending, &windows, lunch_start()),
        vec!["warm-b", "warm-a"]
    );
}

#[test]
fn no_windows_configured_changes_nothing() {
    let pending = vec![
        entry("a", BacklogKind::Cron, 5, "cron:x"),
        entry("b", BacklogKind::Cron, 9, "cron:y"),
    ];
    assert_eq!(plan_order(&pending, &[], lunch_start()), vec!["b", "a"]);
}
