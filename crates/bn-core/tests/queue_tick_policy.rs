//! The dispatcher's decision policy, tested directly.
//!
//! `plan_tick` is pure — no clock, no disk, no processes — precisely so its whole policy can be
//! exercised like this rather than inferred from what a machine happened to do. Every case here is
//! a question an operator asks out loud: why did that not start, why did that one go first, why is
//! the queue not moving.
use bn_core::backlog::{AddSpec, BacklogEntry, ExecutionTarget};
use bn_core::hosts::{NodeRecord, NodeStatus};
use bn_core::queue::kind::{BacklogKind, ExecSpec, WorkClass};
use bn_core::queue::pause::PauseState;
use bn_core::queue::tick::{
    plan_tick, DeferReason, InFlight, KindCaps, Presence, TickInputs, TickPlan,
};
use bn_core::scheduler::{Caps, Observed};

const NOW: i64 = 1_000_000;

fn caps(coding: usize, research: Option<usize>) -> Caps {
    Caps {
        max_local: coding,
        research,
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
    }
}

/// A local node with plenty of room and no special labels.
fn local_node() -> NodeRecord {
    NodeRecord {
        node_id: "testbox".to_string(),
        target: ExecutionTarget::Local,
        status: NodeStatus::Ready,
        slots: 8,
        last_heartbeat: NOW,
        ..Default::default()
    }
}

fn pending(id: &str, kind: BacklogKind, priority: i64, exec: ExecSpec) -> BacklogEntry {
    let spec = AddSpec {
        kind,
        priority,
        exec,
        ..Default::default()
    };
    let mut e = BacklogEntry::new("repo", id, &spec, NOW - 60);
    e.id = id.to_string();
    e
}

fn shell(id: &str, priority: i64) -> BacklogEntry {
    pending(
        id,
        BacklogKind::Cron,
        priority,
        ExecSpec::Shell {
            cmd: "true".to_string(),
        },
    )
}

fn heavy_job(id: &str, priority: i64) -> BacklogEntry {
    let mut e = pending(
        id,
        BacklogKind::Cron,
        priority,
        ExecSpec::Job {
            job: "pool-warm".to_string(),
            repo: None,
            branch: None,
        },
    );
    e.heavy = true;
    e
}

fn targeted_pool_warm(id: &str, priority: i64) -> BacklogEntry {
    let mut e = heavy_job(id, priority);
    e.exec = ExecSpec::Job {
        job: "pool-warm".to_string(),
        repo: Some("repo".to_string()),
        branch: Some("main".to_string()),
    };
    e
}

fn agent(id: &str, priority: i64) -> BacklogEntry {
    pending(id, BacklogKind::Ship, priority, ExecSpec::Agent)
}

/// The inputs a healthy, unconstrained machine presents.
struct Fixture {
    pending: Vec<BacklogEntry>,
    nodes: Vec<NodeRecord>,
    caps: Caps,
    kind_caps: KindCaps,
    observed: Observed,
    max_express: usize,
    presence: Presence,
    pauses: PauseState,
    mandatory_warming: bool,
    in_flight: Vec<InFlight>,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture {
            pending: Vec::new(),
            nodes: vec![local_node()],
            caps: caps(4, Some(2)),
            kind_caps: KindCaps(Default::default()),
            observed: Observed::default(),
            max_express: 20,
            presence: Presence {
                idle: true,
                away_window: false,
            },
            // Empty by default, which is the whole safety property: a machine that has never
            // paused anything plans exactly as it did before pauses were wired in.
            pauses: PauseState::default(),
            mandatory_warming: false,
            in_flight: Vec::new(),
        }
    }
}

impl Fixture {
    fn plan(&self) -> TickPlan {
        plan_tick(&TickInputs {
            now: NOW,
            machine: "testbox",
            pending: &self.pending,
            in_flight: &self.in_flight,
            nodes: &self.nodes,
            caps: &self.caps,
            observed: &self.observed,
            kind_caps: &self.kind_caps,
            kinds: None,
            goals: &[],
            attention: &[],
            pauses: &self.pauses,
            max_express: self.max_express,
            windows: &[],
            presence: self.presence,
            mandatory_warming: self.mandatory_warming,
        })
    }
}

/// A `queue_paused` event as the pause log records it.
fn pause_event(ts: i64, scope: &str, until: Option<i64>) -> bn_core::events::Event {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "scope".to_string(),
        serde_json::Value::String(scope.to_string()),
    );
    if let Some(u) = until {
        fields.insert("until".to_string(), serde_json::Value::from(u));
    }
    bn_core::events::Event {
        kind: "queue_paused".to_string(),
        ts,
        fields,
    }
}

fn started(plan: &TickPlan) -> Vec<String> {
    plan.start.iter().map(|d| d.id.clone()).collect()
}

fn reason_for<'a>(plan: &'a TickPlan, id: &str) -> Option<&'a DeferReason> {
    plan.deferred.iter().find(|d| d.id == id).map(|d| &d.reason)
}

// ---------------------------------------------------------------------------- ordering

#[test]
fn higher_priority_starts_first() {
    let f = Fixture {
        pending: vec![shell("low", 1), shell("high", 99), shell("mid", 50)],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["high", "mid", "low"]);
}

#[test]
fn priority_may_be_negative_without_wrapping() {
    // The score is `alignment + pressure + priority`, and the first version wrapped in release —
    // which sorted the top priority last. Signed lows must stay below zero-priority work.
    let f = Fixture {
        pending: vec![
            shell("floor", -50),
            shell("normal", 0),
            shell("urgent", i64::MAX),
        ],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["urgent", "normal", "floor"]);
}

// ---------------------------------------------------------------------------- express

#[test]
fn express_work_skips_the_lane_caps() {
    // Five cheap crons against a research lane of one: express is bounded by the pass, not the lane,
    // which is what keeps queueing a two-second housekeeping job from making the machine slower.
    let f = Fixture {
        pending: (0..5).map(|i| shell(&format!("c{i}"), 0)).collect(),
        caps: caps(1, Some(1)),
        ..Default::default()
    };
    assert_eq!(started(&f.plan()).len(), 5);
}

#[test]
fn express_is_bounded_by_the_pass() {
    // Unbounded, a backlog of 200 crons runs 200 inline in one tick: the pass blocks for minutes and
    // every scheduled tick behind it skips on the lock.
    let f = Fixture {
        pending: (0..50).map(|i| shell(&format!("c{i}"), 0)).collect(),
        max_express: 20,
        ..Default::default()
    };
    assert_eq!(started(&f.plan()).len(), 20);
}

#[test]
fn express_still_runs_the_highest_priority_first() {
    let mut items: Vec<BacklogEntry> = (0..30).map(|i| shell(&format!("c{i}"), 0)).collect();
    items.push(shell("winner", 100));
    let f = Fixture {
        pending: items,
        max_express: 5,
        ..Default::default()
    };
    assert!(started(&f.plan()).contains(&"winner".to_string()));
}

#[test]
fn heavy_background_work_uses_spare_lane_capacity_last() {
    // Pool warming is autonomous maintenance: it should happen when capacity exists, but never
    // jump ahead of ordinary queued work and never bypass the lane cap as express cron work does.
    let f = Fixture {
        pending: vec![
            heavy_job("warm", bn_core::queue::jobs::HEAVY_PRIORITY),
            pending(
                "research",
                BacklogKind::Research,
                bn_core::queue::jobs::CRON_PRIORITY,
                ExecSpec::Agent,
            ),
        ],
        caps: caps(1, Some(1)),
        ..Default::default()
    };
    let plan = f.plan();
    assert_eq!(started(&plan), vec!["research"]);
    assert!(matches!(
        reason_for(&plan, "warm"),
        Some(DeferReason::Capacity(_))
    ));
}

// ---------------------------------------------------------------------------- capacity

#[test]
fn agent_work_is_held_at_the_lane_cap() {
    let f = Fixture {
        pending: vec![agent("a", 10), agent("b", 9), agent("c", 8)],
        caps: caps(2, Some(1)),
        ..Default::default()
    };
    let plan = f.plan();
    assert_eq!(started(&plan), vec!["a", "b"]);
    assert!(matches!(
        reason_for(&plan, "c"),
        Some(DeferReason::Capacity(_))
    ));
}

#[test]
fn a_kind_default_executor_implies_its_skill_requirement() {
    // `ReviewPr` defaults to the `auto-review-pr` skill, and the implied skill is folded into the
    // entry's requirements — so a node that does not advertise it is correctly not chosen. Note
    // `ExecSpec::Agent` *is* the Default, so passing it cannot be distinguished from leaving it
    // unset: the kind's default wins either way.
    let e = pending("r", BacklogKind::ReviewPr, 0, ExecSpec::Agent);
    assert_eq!(e.requires.skills, vec!["auto-review-pr".to_string()]);
    assert!(matches!(e.exec, ExecSpec::Skill { .. }));
}

#[test]
fn a_per_kind_ceiling_binds_inside_a_lane_that_still_has_room() {
    let mut kc = std::collections::BTreeMap::new();
    kc.insert(BacklogKind::ReviewPr, 1);
    let mut node = local_node();
    node.labels = vec!["skill:auto-review-pr".to_string()];
    let f = Fixture {
        pending: vec![
            pending("r1", BacklogKind::ReviewPr, 10, ExecSpec::Agent),
            pending("r2", BacklogKind::ReviewPr, 9, ExecSpec::Agent),
        ],
        nodes: vec![node],
        caps: caps(4, Some(2)),
        kind_caps: KindCaps(kc),
        ..Default::default()
    };
    let plan = f.plan();
    assert_eq!(started(&plan), vec!["r1"]);
    assert!(matches!(
        reason_for(&plan, "r2"),
        Some(DeferReason::KindCap)
    ));
}

// ---------------------------------------------------------------------------- pause

/// A repo-scoped pause is a **drain**: it holds that repo's pending work and touches nothing else.
#[test]
fn a_repo_pause_holds_that_repos_work_and_only_that_repos() {
    // `pending()` books entries against "repo"; the second is moved to "other" so the test can see
    // the boundary rather than assume it.
    let mut elsewhere = agent("ship the thing", 100);
    elsewhere.repo = "other".to_string();
    let fx = Fixture {
        pending: vec![agent("set up the drained repo", 100), elsewhere],
        pauses: PauseState::fold(&[pause_event(NOW - 10, "repo:repo", None)], NOW),
        ..Default::default()
    };
    let plan = fx.plan();
    assert!(
        plan.start.iter().all(|d| d.repo != "repo"),
        "the drained repo's work must not be admitted"
    );
    assert!(
        plan.start.iter().any(|d| d.repo == "other"),
        "a repo-scoped pause must not stop any other repo"
    );
    assert!(
        plan.deferred
            .iter()
            .any(|d| d.reason.label() == "paused" && d.detail.contains("repo:repo")),
        "the deferral must name the scope holding it, not just say paused"
    );
}

/// The safety property this whole wiring rests on: no pause events, no behaviour change.
#[test]
fn an_empty_pause_state_changes_nothing() {
    let planned = Fixture {
        pending: vec![agent("set up nvim", 100)],
        ..Default::default()
    }
    .plan();
    assert!(
        planned
            .deferred
            .iter()
            .all(|d| d.reason.label() != "paused"),
        "a machine that has never paused anything must plan exactly as it did before"
    );
    assert_eq!(planned.start.len(), 1);
}

/// Recovery is exempt. A pause is a statement about *new* work, and lease reclamation is precisely
/// what a wedged machine needs and precisely what a pause would otherwise stop.
#[test]
fn a_pause_does_not_stop_recovery_work() {
    let mut entry = agent("reap the fleet", 100);
    entry.source = "cron:fleet-reap".to_string();
    entry.exec = bn_core::queue::kind::ExecSpec::Job {
        job: "fleet-reap".to_string(),
        repo: None,
        branch: None,
    };
    let fx = Fixture {
        pending: vec![entry],
        pauses: PauseState::fold(&[pause_event(NOW - 10, "all", None)], NOW),
        ..Default::default()
    };
    assert!(
        fx.plan()
            .deferred
            .iter()
            .all(|d| d.reason.label() != "paused"),
        "recovery must survive even a machine-wide pause"
    );
}

#[test]
fn a_pause_does_not_deadlock_mandatory_pool_warming() {
    let fx = Fixture {
        pending: vec![agent("ordinary-work", 100), heavy_job("pool-warm", -50)],
        pauses: PauseState::fold(&[pause_event(NOW - 10, "all", None)], NOW),
        mandatory_warming: true,
        ..Default::default()
    };
    let plan = fx.plan();
    assert_eq!(started(&plan), vec!["pool-warm"]);
    assert_eq!(
        reason_for(&plan, "ordinary-work"),
        Some(&DeferReason::MandatoryWarming)
    );
}

#[test]
fn an_expired_pause_is_not_in_historical_state() {
    let events = vec![pause_event(NOW - 100, "all", Some(NOW - 1))];
    assert!(!PauseState::fold(&events, NOW).is_paused());
}

#[test]
fn an_unexpired_historical_pause_still_folds_open() {
    let events = vec![pause_event(NOW - 100, "all", Some(NOW + 60))];
    assert!(PauseState::fold(&events, NOW).is_paused());
}

#[test]
fn a_resume_lifts_the_pause_it_names() {
    let events = vec![
        pause_event(NOW - 100, "all", None),
        bn_core::events::Event {
            kind: "queue_resumed".to_string(),
            ts: NOW - 50,
            fields: serde_json::Map::from_iter([(
                "scope".to_string(),
                serde_json::Value::String("all".to_string()),
            )]),
        },
    ];
    assert!(!PauseState::fold(&events, NOW).is_paused());
}

// ---------------------------------------------------------------------------- deadlines

#[test]
fn work_whose_moment_has_passed_is_dropped_not_retried() {
    // A 09:00 digest run at 14:00 is worse than no digest, and left pending it would only get later.
    let mut late = shell("digest", 10);
    late.not_after = Some(NOW - 1);
    let f = Fixture {
        pending: vec![late],
        ..Default::default()
    };
    let plan = f.plan();
    assert!(started(&plan).is_empty());
    assert_eq!(plan.expired.len(), 1);
    assert_eq!(plan.expired[0].id, "digest");
    // Expiry is terminal, so it must not also appear as merely deferred.
    assert!(reason_for(&plan, "digest").is_none());
}

#[test]
fn work_still_inside_its_deadline_runs() {
    let mut soon = shell("digest", 10);
    soon.not_after = Some(NOW + 60);
    let f = Fixture {
        pending: vec![soon],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["digest"]);
}

// ---------------------------------------------------------------------------- placement

#[test]
fn work_needing_a_skill_no_node_offers_says_so() {
    // "No capable node" must name the missing capability — `bn queue status` turning that into an
    // answerable question is the whole point of carrying requirements on the entry.
    let mut needs = agent("needs-skill", 10);
    needs.requires.skills = vec!["auto-review-pr".to_string()];
    let f = Fixture {
        pending: vec![needs],
        ..Default::default()
    };
    let plan = f.plan();
    assert!(started(&plan).is_empty());
    assert!(matches!(
        reason_for(&plan, "needs-skill"),
        Some(DeferReason::NoCapableNode)
    ));
}

#[test]
fn work_runs_once_a_node_advertises_the_skill() {
    let mut needs = agent("needs-skill", 10);
    needs.requires.skills = vec!["auto-review-pr".to_string()];
    let mut node = local_node();
    node.labels = vec!["skill:auto-review-pr".to_string()];
    let f = Fixture {
        pending: vec![needs],
        nodes: vec![node],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["needs-skill"]);
}

#[test]
fn the_local_box_is_an_always_available_fallback_for_unconstrained_work() {
    // Deliberate, and the pre-registry invariant: work with no required capabilities never waits on
    // the node registry, so an offline or empty registry does not strand it on the box it is
    // already running on.
    let mut node = local_node();
    node.status = NodeStatus::Offline;
    let f = Fixture {
        pending: vec![agent("work", 10)],
        nodes: vec![node],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["work"]);

    let empty = Fixture {
        pending: vec![agent("work", 10)],
        nodes: vec![],
        ..Default::default()
    };
    assert_eq!(started(&empty.plan()), vec!["work"]);
}

#[test]
fn constrained_work_does_not_fall_back_to_the_implicit_local_box() {
    // The other half: the implicit box carries no labels, so it cannot satisfy a requirement.
    // Dispatching there anyway would send work to a node guaranteed to fail it.
    let mut needs = agent("needs", 10);
    needs.requires.skills = vec!["some-skill".to_string()];
    let f = Fixture {
        pending: vec![needs],
        nodes: vec![],
        ..Default::default()
    };
    assert!(started(&f.plan()).is_empty());
}

// ---------------------------------------------------------------------------- empty

#[test]
fn an_empty_queue_starts_nothing_and_says_nothing_is_wrong() {
    let plan = Fixture::default().plan();
    assert!(plan.start.is_empty());
    assert!(plan.deferred.is_empty());
    assert!(plan.expired.is_empty());
}

#[test]
fn a_clicked_dispatch_starts_while_the_user_is_present() {
    // The dashboard's "setup needed" button dispatches an agent whose prompt mentions warm pools.
    // Classified by that word, it became opportunistic and the tick held every one of them
    // `awaiting-away`: ten repos' setup agents, clicked by hand at priority 100, waiting for a
    // window that only opens at lunch. Presence must not bear on foreground work at all.
    let item = "You are the repo setup agent for `nvim`. Keep scripts idempotent for warm pools.";
    let mut entry = agent(item, 100);
    entry.source = "dispatch".to_string();
    let fx = Fixture {
        pending: vec![entry],
        presence: Presence {
            idle: false,
            away_window: false,
        },
        ..Default::default()
    };
    let plan = fx.plan();
    assert_eq!(
        reason_for(&plan, item),
        None,
        "a foreground dispatch must not be deferred for presence"
    );
    assert_eq!(started(&plan), vec![item.to_string()]);
}

#[test]
fn warming_still_waits_for_idle_or_an_away_window() {
    // The other half of the same rule: narrowing what counts as warming must not stop warming
    // itself from being held back while you are at the keyboard.
    let fx = Fixture {
        pending: vec![heavy_job("pool-warm", -50)],
        presence: Presence {
            idle: false,
            away_window: false,
        },
        ..Default::default()
    };
    let plan = fx.plan();
    assert!(matches!(
        reason_for(&plan, "pool-warm"),
        Some(DeferReason::AwaitingAway)
    ));
    assert!(started(&plan).is_empty());
}

#[test]
fn mandatory_warming_blocks_new_work_and_starts_the_warm() {
    let fx = Fixture {
        pending: vec![agent("foreground", 100), heavy_job("pool-warm", -50)],
        presence: Presence {
            idle: false,
            away_window: false,
        },
        mandatory_warming: true,
        ..Default::default()
    };
    let plan = fx.plan();
    assert_eq!(started(&plan), vec!["pool-warm"]);
    assert_eq!(
        reason_for(&plan, "foreground"),
        Some(&DeferReason::MandatoryWarming)
    );
}

#[test]
fn keep_warm_does_not_trigger_or_bypass_mandatory_pool_warming() {
    let mut keep_warm = heavy_job("keep-warm", -50);
    keep_warm.exec = ExecSpec::Job {
        job: "keep-warm".to_string(),
        repo: None,
        branch: None,
    };
    let fx = Fixture {
        pending: vec![keep_warm, heavy_job("pool-warm", -50)],
        mandatory_warming: true,
        ..Default::default()
    };
    let plan = fx.plan();
    assert_eq!(started(&plan), vec!["pool-warm"]);
    assert_eq!(
        reason_for(&plan, "keep-warm"),
        Some(&DeferReason::MandatoryWarming)
    );
}

#[test]
fn targeted_pool_warm_does_not_bypass_mandatory_pool_warming() {
    let fx = Fixture {
        pending: vec![
            targeted_pool_warm("targeted-warm", -50),
            heavy_job("pool-warm", -50),
        ],
        mandatory_warming: true,
        ..Default::default()
    };
    let plan = fx.plan();
    assert_eq!(started(&plan), vec!["pool-warm"]);
    assert_eq!(
        reason_for(&plan, "targeted-warm"),
        Some(&DeferReason::MandatoryWarming)
    );
}

#[test]
fn mandatory_warming_drains_existing_work_without_starting_more() {
    let fx = Fixture {
        pending: vec![agent("foreground", 100), heavy_job("pool-warm", -50)],
        caps: caps(1, Some(1)),
        mandatory_warming: true,
        in_flight: vec![InFlight {
            repo: "repo".to_string(),
            id: "already-running".to_string(),
            kind: BacklogKind::Ship,
            work_class: WorkClass::Coding,
            heavy: false,
            node: None,
            touches: Vec::new(),
            started_at: NOW - 60,
        }],
        ..Default::default()
    };
    let plan = fx.plan();
    assert!(started(&plan).is_empty());
    assert_eq!(
        reason_for(&plan, "foreground"),
        Some(&DeferReason::MandatoryWarming)
    );
    assert!(matches!(
        reason_for(&plan, "pool-warm"),
        Some(DeferReason::Capacity(_))
    ));
}

#[test]
fn the_dispatcher_hands_the_executor_the_declared_identity() {
    // The gap this closes sat between admission and launch: the entry knew its branch, the plan
    // dropped it, and the executor rebuilt an invocation from the subject line alone. Everything
    // upstream was correct and the agent still ran as the wrong piece of work.
    let mut entry = agent("set up nvim", 100);
    entry.agent = bn_core::backlog::AgentSpec {
        branch: Some("repo-setup".to_string()),
        name: Some("repo-setup".to_string()),
        yolo: true,
        no_plan_review: true,
        with_main: true,
        stage: Some(bn_core::pipeline::stage::Stage::Critique),
    };
    let fx = Fixture {
        pending: vec![entry],
        ..Default::default()
    };
    let plan = fx.plan();
    let d = plan.start.first().expect("the entry should start");
    assert_eq!(d.agent.branch.as_deref(), Some("repo-setup"));
    assert_eq!(d.agent.name.as_deref(), Some("repo-setup"));
    assert!(d.agent.yolo);
    assert!(d.agent.no_plan_review);
    assert!(d.agent.with_main);
    // The stage has to cross the same boundary: the plan is what the executor reads, and a stage
    // dropped here is silently re-inferred as `execute` — a different model, effort and harness than
    // the one this entry declared.
    assert_eq!(
        d.agent.stage,
        Some(bn_core::pipeline::stage::Stage::Critique)
    );
}

// ---------------------------------------------------------------------------- launch backoff

/// The contract the tick's launch-failure backoff rests on: an entry snoozed into the future is
/// held, not started.
///
/// Worth pinning because of what it prevents. `bn dispatch` refuses an item whose branch is
/// already active, and a failed launch used to go straight back to pending — so the next pass,
/// seconds later, tried the identical thing and failed identically. One entry burned 57 launch
/// attempts in under an hour, each spawning a subprocess that did real git work before failing.
/// The fix sets `not_before`; this is the half that makes the fix mean anything.
#[test]
fn a_snoozed_entry_is_held_rather_than_started() {
    let mut blocked = shell("blocked", 99);
    blocked.not_before = Some(NOW + 600);
    let f = Fixture {
        pending: vec![blocked, shell("free", 1)],
        ..Default::default()
    };
    let plan = f.plan();

    // Highest priority in the queue, and still held — the snooze outranks the score.
    assert_eq!(started(&plan), vec!["free"]);
    assert_eq!(reason_for(&plan, "blocked"), Some(&DeferReason::Snoozed));
}

/// And the backoff is a delay, not a cancellation: once the window passes the entry competes
/// normally again. A retry that never came would be a worse bug than the one being fixed.
#[test]
fn a_snooze_that_has_expired_starts_normally() {
    let mut waited = shell("waited", 99);
    waited.not_before = Some(NOW - 1);
    let f = Fixture {
        pending: vec![waited],
        ..Default::default()
    };
    assert_eq!(started(&f.plan()), vec!["waited"]);
}
