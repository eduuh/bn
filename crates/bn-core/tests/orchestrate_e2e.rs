//! End-to-end orchestrate integration harness (#461) — **agent-free, network-free, deterministic**.
//!
//! The `bn orchestrate` daemon is a lot of interacting pure pieces — the elastic-concurrency
//! scheduler ([`scheduler::pick`], #377/#385), the #340 pane state machine ([`classify`] /
//! [`action_for`] / [`plan_relaunch`]), the #376 conflict holds, the #380 durability trims — glued
//! by a CLI executor that realizes each decision against tmux/git/gh. Every piece has its own unit
//! tests, but nothing exercised the *interaction*: a whole run flowing declare → dispatch →
//! supervise → land → drain. A regression in the seam between pieces (e.g. the #443 `log_run_event`
//! signature break) could slip past every per-piece test.
//!
//! This harness closes that gap. It drives a full run end-to-end over the **public pure state
//! machine only**, replacing every IO seam the CLI owns with a deterministic fake:
//!   * **dispatch** — no worktree/tmux/agent; a launch just records a branch and flips the item to
//!     `Dispatched` (mirrors `fill_dispatch_slots`).
//!   * **pane observation** — a *scripted* per-item lifecycle of canned pane tails fed through the
//!     real [`classify`], so each agent walks working → done / dead / outage / limited / question
//!     exactly as the live supervisor would read it (mirrors `supervise_inflight`).
//!   * **gate + land** — a deterministic pass/fail; a passing land flips the item to `Merged`
//!     (mirrors `land_ready` + `reconcile_landed`), a failing one retries under the real
//!     [`Backoff`] cap.
//!
//! The [`Harness::tick`] loop mirrors the CLI's [`tick`] ordering (land → reconcile-remote →
//! supervise → dispatch → durability-trim → drain-check) so the transitions it asserts are the ones
//! the daemon actually makes. Nothing here touches the clock, disk, network, or a real agent, so the
//! whole run is fast and reproducible on any machine — the acceptance's "runs fast enough for CI".

use bn_core::backlog::Substrate;
use bn_core::orchestrate::{
    action_for, classify, derive_id, plan_relaunch, Backoff, ItemStatus, OrchestrateState,
    PaneObservation, PaneState, QueueEntrySpec, RelaunchDecision, SupervisorAction, WorkClass,
    HISTORY_RETAIN_SECS, RECOVERY_CAP,
};
use bn_core::scheduler::{pick, Caps, Observed};
use std::collections::HashMap;

/// The wall-clock the harness advances per tick. Deliberately larger than [`Backoff`]'s 1800s
/// `max_secs` cap so a relaunch's backoff window has *always* elapsed by the next pass — the
/// `RelaunchDecision::Wait` branch never fires, keeping the recovery path deterministic (relaunches
/// proceed every tick). It is also `> HISTORY_RETAIN_SECS / 4`, so a terminal item ages out of the
/// live queue within a few ticks — exercising the #380 eviction under the scale test.
const DT: i64 = 2_000;

/// A single scripted step in an agent's lifecycle — one pane snapshot. Each maps to a canned pane
/// tail (+ window / plane-A / wip flags) that the *real* [`classify`] resolves to the intended
/// [`PaneState`], so the harness drives the genuine #340 classifier rather than asserting against a
/// hand-picked state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A live spinner — `classify` ⇒ [`PaneState::Working`] (leave alone).
    Working,
    /// Plane-A `done` — `classify` ⇒ [`PaneState::Done`] (harvest + land).
    Done,
    /// No live window — `classify` ⇒ [`PaneState::Dead`] (hard relaunch).
    Dead,
    /// A bare idle prompt over a dirty tree — `classify` ⇒ [`PaneState::IdleWithWip`] (soft resume).
    IdleWip,
    /// An API/server error banner — `classify` ⇒ [`PaneState::ApiError`] (relaunch w/ backoff).
    ApiError,
    /// A DNS/name-resolution failure — `classify` ⇒ [`PaneState::DnsOutage`] (relaunch w/ backoff).
    DnsOutage,
    /// A blocking question menu — `classify` ⇒ [`PaneState::QuestionMenu`] (surface for a human).
    Question,
    /// A usage-limit dialog — `classify` ⇒ [`PaneState::Limited`] (switch harness, #382).
    Limited,
}

/// Build the canned pane observation for a step: `(tail, has_window, plane_a, has_wip)`. These are
/// real harness-output fragments the live classifier keys on, so feeding them through [`classify`]
/// reproduces the exact state a supervising daemon would infer from a live pane.
fn step_pane(step: Step) -> (Vec<String>, bool, &'static str, bool) {
    match step {
        Step::Working => (
            vec!["· Working on the fix (esc to interrupt)".to_string()],
            true,
            "working",
            true,
        ),
        Step::Done => (vec!["opened PR, all done".to_string()], true, "done", false),
        Step::Dead => (Vec::new(), false, "", true),
        Step::IdleWip => (vec!["❯".to_string()], true, "", true),
        Step::ApiError => (
            vec!["API Error: 500 Internal Server Error".to_string()],
            true,
            "",
            true,
        ),
        Step::DnsOutage => (
            vec!["Error: getaddrinfo ENOTFOUND api.host".to_string()],
            true,
            "",
            true,
        ),
        Step::Limited => (
            vec!["Usage limit reached — wait for limit to reset".to_string()],
            true,
            "",
            false,
        ),
        Step::Question => (vec!["Shall I proceed? (y/n)".to_string()], true, "", true),
    }
}

/// Resolve a step's [`PaneState`] through the real classifier — the single place the harness turns a
/// scripted step into the state the supervisor acts on.
fn classify_step(step: Step) -> PaneState {
    let (tail, has_window, plane_a, has_wip) = step_pane(step);
    let obs = PaneObservation {
        tail: &tail,
        has_window,
        plane_a,
        has_wip,
    };
    classify(&obs)
}

/// The scripted behavior of one queued item across its whole life — the fake seam for its agent.
struct Lane {
    /// The lifecycle the agent walks while in flight; the cursor clamps at the last step.
    steps: Vec<Step>,
    cursor: usize,
    /// The lifecycle the agent adopts *after a relaunch* — a fresh, re-seeded process resuming the
    /// work (empty ⇒ keep the same steps, e.g. a permanently-dead agent that never recovers).
    recovery: Vec<Step>,
    /// Deterministic gate verdicts consumed on successive land attempts (missing ⇒ pass).
    gate: Vec<bool>,
    gate_idx: usize,
    substrate: Substrate,
    work_class: WorkClass,
}

impl Lane {
    fn current(&self) -> Step {
        self.steps[self.cursor.min(self.steps.len() - 1)]
    }
    /// Advance one step (time passing for an in-flight agent); clamps at the final step.
    fn advance(&mut self) {
        if self.cursor + 1 < self.steps.len() {
            self.cursor += 1;
        }
    }
    /// Adopt the post-relaunch lifecycle from the top — a fresh process resuming the work.
    fn on_relaunch(&mut self) {
        if !self.recovery.is_empty() {
            self.steps = self.recovery.clone();
        }
        self.cursor = 0;
    }
    /// The gate verdict for the next land attempt — deterministic, defaulting to pass once the
    /// scripted verdicts are exhausted.
    fn next_gate(&mut self) -> bool {
        let v = self.gate.get(self.gate_idx).copied().unwrap_or(true);
        self.gate_idx += 1;
        v
    }
}

/// The declarative catalogue of item behaviors the harness can seed — each expands to a [`Lane`].
#[derive(Debug, Clone, Copy)]
enum Behavior {
    /// Works a pass, then finishes and lands cleanly.
    Happy,
    /// Finishes on the first observation (a fast agent) — lands immediately.
    InstantDone,
    /// Dies (window gone), is hard-relaunched, and the fresh process finishes.
    DiesRecovers,
    /// Hits an API error, is relaunched with backoff, and recovers.
    ApiRecovers,
    /// Hits a DNS outage, is relaunched with backoff, and recovers.
    DnsRecovers,
    /// Stalls idle-with-wip, is soft-resumed, and recovers.
    IdleWipRecovers,
    /// Surfaces a blocking question, then proceeds and lands.
    QuestionThenDone,
    /// Hits a usage limit (harness switch, #382), then proceeds and lands.
    LimitedThenDone,
    /// Finishes, but the first gate fails; the retry passes and it lands.
    GateFlakyThenLands,
    /// Never recovers — relaunches until the [`Backoff`] cap, then is surfaced `failed`.
    NeverRecovers,
    /// A remote fan-out (#377): no local pane, reconciled by its PR completion.
    Remote,
}

fn lane_for(b: Behavior) -> Lane {
    let (steps, recovery, gate, substrate) = match b {
        Behavior::Happy => (
            vec![Step::Working, Step::Done],
            vec![],
            vec![],
            Substrate::Auto,
        ),
        Behavior::InstantDone => (vec![Step::Done], vec![], vec![], Substrate::Auto),
        Behavior::DiesRecovers => (
            vec![Step::Working, Step::Dead],
            vec![Step::Working, Step::Done],
            vec![],
            Substrate::Auto,
        ),
        Behavior::ApiRecovers => (
            vec![Step::Working, Step::ApiError],
            vec![Step::Working, Step::Done],
            vec![],
            Substrate::Auto,
        ),
        Behavior::DnsRecovers => (
            vec![Step::Working, Step::DnsOutage],
            vec![Step::Working, Step::Done],
            vec![],
            Substrate::Auto,
        ),
        Behavior::IdleWipRecovers => (
            vec![Step::Working, Step::IdleWip],
            vec![Step::Working, Step::Done],
            vec![],
            Substrate::Auto,
        ),
        Behavior::QuestionThenDone => (
            vec![Step::Working, Step::Question, Step::Working, Step::Done],
            vec![],
            vec![],
            Substrate::Auto,
        ),
        Behavior::LimitedThenDone => (
            vec![Step::Working, Step::Limited, Step::Working, Step::Done],
            vec![],
            vec![],
            Substrate::Auto,
        ),
        Behavior::GateFlakyThenLands => {
            (vec![Step::Done], vec![], vec![false, true], Substrate::Auto)
        }
        Behavior::NeverRecovers => (
            vec![Step::Working, Step::Dead],
            vec![Step::Dead],
            vec![],
            Substrate::Auto,
        ),
        Behavior::Remote => (
            vec![Step::Working, Step::Working, Step::Done],
            vec![],
            vec![],
            Substrate::Remote,
        ),
    };
    Lane {
        steps,
        cursor: 0,
        recovery,
        gate,
        gate_idx: 0,
        substrate,
        work_class: WorkClass::Coding,
    }
}

/// The end-to-end driver: an [`OrchestrateState`] plus the fake seams (scripted lanes, an event
/// log, and a synthetic clock/backlog), stepped by [`Harness::tick`] until the run drains.
struct Harness {
    st: OrchestrateState,
    lanes: HashMap<String, Lane>,
    /// Items not yet leased into the run — fed in batches to model a self-feeding backlog (#368).
    backlog: Vec<(String, Substrate, WorkClass)>,
    /// Keep at most this many items queued at once (0 ⇒ everything is seeded up front).
    refill_batch: usize,
    /// The mirror of the CLI's `log_event` calls — `(event, id)` — asserted by the tests.
    events: Vec<(&'static str, String)>,
    now: i64,
    concurrency: usize,
    /// Whether to evict aged terminal items to "history" each tick (#380). Off keeps the whole
    /// queue live so a small run's final per-item outcomes stay assertable.
    evict: bool,
    evicted: usize,
    // Observability of the run for the assertions.
    dispatched_per_tick: Vec<usize>,
    peak_queue_len: usize,
    peak_recovery_len: usize,
    peak_json_len: usize,
    peak_local_inflight: usize,
    ticks: usize,
}

impl Harness {
    fn new(repo: &str, concurrency: usize) -> Harness {
        Harness {
            st: OrchestrateState::new(repo, &[], concurrency, false, 1_000_000),
            lanes: HashMap::new(),
            backlog: Vec::new(),
            refill_batch: 0,
            events: Vec::new(),
            now: 1_000_000,
            concurrency,
            evict: false,
            evicted: 0,
            dispatched_per_tick: Vec::new(),
            peak_queue_len: 0,
            peak_recovery_len: 0,
            peak_json_len: 0,
            peak_local_inflight: 0,
            ticks: 0,
        }
    }

    /// Seed items directly into the run's queue (all dispatchable from tick one).
    fn seed(&mut self, specs: &[(&str, Behavior)]) {
        let mut entries = Vec::new();
        for (item, b) in specs {
            let lane = lane_for(*b);
            let (sub, wc) = (lane.substrate, lane.work_class);
            self.lanes.insert(derive_id(item), lane);
            entries.push((item.to_string(), Vec::new(), sub, wc, None));
        }
        self.st.extend_with_entries(&entries, self.now);
    }

    /// Stage items in the durable backlog to be leased in batches (self-feeding run).
    fn stage(&mut self, specs: Vec<(String, Behavior)>, refill_batch: usize) {
        self.refill_batch = refill_batch;
        for (item, b) in specs {
            let lane = lane_for(b);
            let (sub, wc) = (lane.substrate, lane.work_class);
            self.lanes.insert(derive_id(&item), lane);
            self.backlog.push((item, sub, wc));
        }
    }

    fn emit(&mut self, kind: &'static str, id: &str) {
        self.events.push((kind, id.to_string()));
    }

    fn count(&self, kind: &str) -> usize {
        self.events.iter().filter(|(k, _)| *k == kind).count()
    }

    fn has_event(&self, kind: &str, id: &str) -> bool {
        self.events.iter().any(|(k, i)| *k == kind && i == id)
    }

    fn status_of(&self, id: &str) -> Option<ItemStatus> {
        self.st.queue.iter().find(|i| i.id == id).map(|i| i.status)
    }

    fn terminal_in_queue(&self) -> usize {
        self.st
            .queue
            .iter()
            .filter(|i| i.status.is_terminal())
            .count()
    }

    /// Lease the next backlog batch when the queue runs low — the self-feeding refill (#368).
    fn refill(&mut self) {
        if self.refill_batch == 0 || self.backlog.is_empty() {
            return;
        }
        let queued = self.st.counts().queued;
        if queued >= self.refill_batch {
            return;
        }
        let take = (self.refill_batch - queued).min(self.backlog.len());
        let entries: Vec<QueueEntrySpec> = self
            .backlog
            .drain(0..take)
            .map(|(item, sub, wc)| (item, Vec::new(), sub, wc, None))
            .collect();
        self.st.extend_with_entries(&entries, self.now);
    }

    /// Land every finished local pane through the fake gate, and reconcile finished remote fan-outs
    /// — mirrors the CLI `tick`'s top-of-pass `land_ready` + `reconcile_landed` + `reconcile_remote`.
    fn land_and_reconcile(&mut self) {
        let now = self.now;
        let local: Vec<String> = self
            .st
            .queue
            .iter()
            .filter(|i| i.status == ItemStatus::Dispatched && i.substrate != Substrate::Remote)
            .map(|i| i.id.clone())
            .collect();
        for id in local {
            if classify_step(self.lanes[&id].current()) != PaneState::Done {
                continue;
            }
            if self.lanes.get_mut(&id).unwrap().next_gate() {
                self.st.mark_merged(&id, Some(format!("PR:{id}")), now);
                self.st.record_recovery(
                    &id,
                    PaneState::Done,
                    SupervisorAction::Land,
                    "gated + landed",
                    now,
                );
                self.emit("orchestrate_merged", &id);
            } else {
                // Gate failed — bound the retries with the same backoff cap the relaunch path uses.
                let attempts = self.st.bump_attempts(&id, now);
                self.emit("orchestrate_gate_fail", &id);
                if !Backoff::default().should_retry(attempts) {
                    self.st.mark_failed(&id, now);
                    self.st.record_recovery(
                        &id,
                        PaneState::Done,
                        SupervisorAction::Land,
                        "gate failed past cap — surfaced",
                        now,
                    );
                    self.emit("orchestrate_failed", &id);
                }
            }
        }
        // Remote fan-outs have no local pane — reconcile them by their (scripted) PR completion.
        let remote: Vec<String> = self
            .st
            .queue
            .iter()
            .filter(|i| i.status == ItemStatus::Dispatched && i.substrate == Substrate::Remote)
            .map(|i| i.id.clone())
            .collect();
        for id in remote {
            let lane = self.lanes.get_mut(&id).unwrap();
            lane.advance();
            if lane.current() == Step::Done {
                self.st.mark_merged(&id, Some(format!("PR:{id}")), now);
                self.emit("orchestrate_merged", &id);
            }
        }
    }

    /// Classify and act on every in-flight local pane — mirrors `supervise_inflight` +
    /// `execute_action`. A `Done` pane whose land already happened this pass is a no-op (`Land`);
    /// every other state runs the real #340 action, including the full [`plan_relaunch`] policy.
    fn supervise(&mut self) {
        let now = self.now;
        let ids: Vec<String> = self
            .st
            .queue
            .iter()
            .filter(|i| i.status == ItemStatus::Dispatched && i.substrate != Substrate::Remote)
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            let step = self.lanes[&id].current();
            let (tail, has_window, plane_a, has_wip) = step_pane(step);
            let obs = PaneObservation {
                tail: &tail,
                has_window,
                plane_a,
                has_wip,
            };
            let state = classify(&obs);
            self.st.set_state(&id, state, now);
            let mut advance = true;
            match action_for(state) {
                SupervisorAction::Leave => {}
                // Landing is handled at the top of the tick; nothing to force here.
                SupervisorAction::Land => {}
                SupervisorAction::AnswerTrustMenu => {
                    self.st.record_recovery(
                        &id,
                        state,
                        SupervisorAction::AnswerTrustMenu,
                        "cleared trust menu",
                        now,
                    );
                    self.emit("orchestrate_trust_menu", &id);
                }
                SupervisorAction::AnswerCommandApproval => {
                    self.st.record_recovery(
                        &id,
                        state,
                        SupervisorAction::AnswerCommandApproval,
                        "surfaced command approval",
                        now,
                    );
                    self.emit("orchestrate_command_approval", &id);
                }
                SupervisorAction::AnswerQuestion => {
                    self.st.record_recovery(
                        &id,
                        state,
                        SupervisorAction::AnswerQuestion,
                        "surfaced question for a human",
                        now,
                    );
                    self.emit("orchestrate_question", &id);
                }
                SupervisorAction::SwitchHarness => {
                    self.st.record_recovery(
                        &id,
                        state,
                        SupervisorAction::SwitchHarness,
                        "switched harness on usage limit",
                        now,
                    );
                    self.emit("orchestrate_switch", &id);
                }
                SupervisorAction::RelaunchToResume => {
                    // A relaunch re-seeds the lane from the top of its recovery script, so we must
                    // not also advance past that fresh first step this pass.
                    advance = false;
                    self.relaunch(&id, state, has_window);
                }
            }
            // Advance only while the item is still in flight (a give-up flipped it to terminal).
            if advance && self.status_of(&id) == Some(ItemStatus::Dispatched) {
                self.lanes.get_mut(&id).unwrap().advance();
            }
        }
    }

    /// Realize a relaunch through the real [`plan_relaunch`] policy — give up past the cap, wait
    /// inside the backoff window, else relaunch (and re-seed the lane's recovery lifecycle).
    fn relaunch(&mut self, id: &str, state: PaneState, has_window: bool) {
        let now = self.now;
        let (attempts, last) = self
            .st
            .queue
            .iter()
            .find(|i| i.id == id)
            .map(|i| (i.attempts, i.last_relaunch_at))
            .unwrap_or((0, 0));
        match plan_relaunch(state, attempts, last, now, has_window, &Backoff::default()) {
            RelaunchDecision::GiveUp => {
                self.st.mark_failed(id, now);
                self.st.record_recovery(
                    id,
                    state,
                    SupervisorAction::RelaunchToResume,
                    "gave up after backoff cap — surfaced",
                    now,
                );
                self.emit("orchestrate_failed", id);
            }
            RelaunchDecision::Wait => {}
            RelaunchDecision::Relaunch(kind) => {
                let n = self.st.bump_attempts(id, now);
                self.st.record_recovery(
                    id,
                    state,
                    SupervisorAction::RelaunchToResume,
                    &format!("{} #{n}", kind.label()),
                    now,
                );
                self.emit("orchestrate_relaunch", id);
                self.lanes.get_mut(id).unwrap().on_relaunch();
            }
        }
    }

    /// Fill open slots via the real elastic-concurrency scheduler — mirrors `fill_dispatch_slots`.
    /// No resource pressure and no caps beyond the local concurrency, so [`pick`] fans out up to the
    /// cap (and remote-substrate items regardless), exactly like a healthy daemon pass.
    fn dispatch(&mut self) {
        let caps = Caps {
            max_local: self.st.coding_cap(),
            research: self.st.research_cap(),
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
        let plan = pick(&self.st, &caps, &Observed::default());
        let now = self.now;
        let mut n = 0;
        for idx in plan.local {
            let id = self.st.queue[idx].id.clone();
            self.st
                .mark_dispatched(idx, Some(format!("orch/{id}")), now);
            self.st.record_recovery(
                &id,
                PaneState::Working,
                SupervisorAction::Leave,
                "dispatched",
                now,
            );
            self.emit("orchestrate_dispatch", &id);
            n += 1;
        }
        for idx in plan.remote {
            let id = self.st.queue[idx].id.clone();
            self.st.mark_dispatched_remote(idx, now);
            self.st.record_recovery(
                &id,
                PaneState::Working,
                SupervisorAction::Leave,
                "dispatched remote",
                now,
            );
            self.emit("orchestrate_dispatch_remote", &id);
            n += 1;
        }
        self.dispatched_per_tick.push(n);
        // The scheduler must never exceed the local concurrency cap.
        let local = self.st.local_in_flight();
        assert!(
            local <= self.concurrency,
            "local in-flight {local} exceeded cap {}",
            self.concurrency
        );
        self.peak_local_inflight = self.peak_local_inflight.max(local);
    }

    /// One full supervisor pass, in the CLI `tick`'s order. Returns whether the run has fully
    /// drained (queue all-terminal *and* nothing left staged in the backlog).
    fn tick(&mut self) -> bool {
        self.ticks += 1;
        let now = self.now;
        self.refill();
        self.land_and_reconcile();
        self.supervise();
        self.dispatch();
        // #380 durability: keep the per-tick state rewrite bounded.
        self.st.trim_recovery(RECOVERY_CAP);
        if self.evict {
            self.evicted += self.st.evict_terminal(now, HISTORY_RETAIN_SECS);
        }
        self.peak_queue_len = self.peak_queue_len.max(self.st.queue.len());
        self.peak_recovery_len = self.peak_recovery_len.max(self.st.recovery.len());
        self.peak_json_len = self.peak_json_len.max(self.st.to_json().unwrap().len());
        self.now += DT;
        self.st.is_drained() && self.backlog.is_empty()
    }

    fn run_to_drain(&mut self, max_ticks: usize) {
        loop {
            assert!(
                self.ticks < max_ticks,
                "run did not drain within {max_ticks} ticks (queue={}, backlog={})",
                self.st.queue.len(),
                self.backlog.len()
            );
            if self.tick() {
                break;
            }
        }
    }
}

/// The headline acceptance: a multi-item run covering every branch of the lifecycle — happy lands,
/// death/outage/idle recoveries, a surfaced question, a usage-limit harness switch, a flaky gate, a
/// permanent failure, and a remote fan-out — drives dispatch → supervise → land to **full drain**
/// with zero real agents/network, and every transition + emitted event is as expected.
#[test]
fn full_lifecycle_multi_item_run_drains_with_expected_transitions() {
    let mut h = Harness::new("octo/demo", 3);
    h.seed(&[
        ("101", Behavior::Happy),
        ("102", Behavior::InstantDone),
        ("103", Behavior::DiesRecovers),
        ("104", Behavior::ApiRecovers),
        ("105", Behavior::DnsRecovers),
        ("106", Behavior::IdleWipRecovers),
        ("107", Behavior::QuestionThenDone),
        ("108", Behavior::LimitedThenDone),
        ("109", Behavior::GateFlakyThenLands),
        ("110", Behavior::NeverRecovers),
        ("201", Behavior::Remote),
    ]);

    h.run_to_drain(200);

    // --- final drained state -------------------------------------------------
    assert!(h.st.is_drained(), "every item must be terminal");
    let c = h.st.counts();
    assert_eq!(c.total, 11);
    assert_eq!(c.in_flight, 0, "no stuck lanes");
    assert_eq!(c.queued, 0, "nothing left queued");
    assert_eq!(c.merged, 10, "all but the permanent failure land");
    assert_eq!(c.failed, 1, "the never-recovers item is surfaced failed");

    // --- per-item terminal outcomes -----------------------------------------
    for id in [
        "101", "102", "103", "104", "105", "106", "107", "108", "109", "201",
    ] {
        assert_eq!(
            h.status_of(id),
            Some(ItemStatus::Merged),
            "item {id} should have landed"
        );
    }
    assert_eq!(h.status_of("110"), Some(ItemStatus::Failed));

    // --- key emitted events --------------------------------------------------
    // Every local item was dispatched; the remote one via the remote path.
    assert_eq!(h.count("orchestrate_dispatch"), 10);
    assert_eq!(h.count("orchestrate_dispatch_remote"), 1);
    assert!(h.has_event("orchestrate_dispatch_remote", "201"));
    // Ten items reached a merged/landed state.
    assert_eq!(h.count("orchestrate_merged"), 10);
    // Each recovering item was relaunched at least once, then landed.
    for id in ["103", "104", "105", "106"] {
        assert!(
            h.has_event("orchestrate_relaunch", id),
            "{id} must relaunch"
        );
        assert!(h.has_event("orchestrate_merged", id), "{id} must then land");
    }
    // The question and limit lanes surfaced/switched, then landed.
    assert!(h.has_event("orchestrate_question", "107"));
    assert!(h.has_event("orchestrate_switch", "108"));
    // The flaky gate failed once before landing.
    assert!(h.has_event("orchestrate_gate_fail", "109"));
    assert!(h.has_event("orchestrate_merged", "109"));
    // The permanent failure relaunched repeatedly and was finally surfaced failed.
    assert!(h.has_event("orchestrate_failed", "110"));
    assert!(
        h.events
            .iter()
            .filter(|(k, i)| *k == "orchestrate_relaunch" && i == "110")
            .count()
            >= 2,
        "110 should relaunch several times before giving up"
    );
    // A relaunch never fabricates a spurious failure for a healthy item.
    assert!(!h.has_event("orchestrate_failed", "101"));
}

/// The scheduler forms **waves**: at concurrency N it dispatches at most N at once and refills as
/// slots free, rather than fanning out the whole backlog in one pass — and still drains everything.
#[test]
fn dispatch_respects_concurrency_and_forms_waves() {
    let mut h = Harness::new("octo/demo", 2);
    h.seed(&[
        ("301", Behavior::Happy),
        ("302", Behavior::Happy),
        ("303", Behavior::Happy),
        ("304", Behavior::Happy),
        ("305", Behavior::Happy),
        ("306", Behavior::Happy),
    ]);

    h.run_to_drain(100);

    assert!(h.st.is_drained());
    assert_eq!(h.st.counts().merged, 6);
    // Never more than the cap in flight at once.
    assert_eq!(h.peak_local_inflight, 2);
    // No single pass launched more than the cap...
    assert!(h.dispatched_per_tick.iter().all(|&n| n <= 2));
    // ...and six items at two-per-wave took at least three dispatching passes.
    let dispatching_passes = h.dispatched_per_tick.iter().filter(|&&n| n > 0).count();
    assert!(
        dispatching_passes >= 3,
        "expected >=3 waves, saw {dispatching_passes}"
    );
    assert_eq!(h.dispatched_per_tick.iter().sum::<usize>(), 6);
}

/// Run a synthetic self-feeding backlog of `n` items to full drain and report the run's peak state
/// footprint — the input to the #380 "state rewrite stays O(in-flight + recent)" assertion.
struct ScaleReport {
    peak_queue_len: usize,
    peak_recovery_len: usize,
    peak_json_len: usize,
    evicted: usize,
    failed: usize,
    completed: usize,
}

fn run_scale(n: usize) -> ScaleReport {
    let mut h = Harness::new("octo/scale", 8);
    h.evict = true;
    // Mostly happy ships with a recurring death-recovery every tenth item, so the recovery trail and
    // the relaunch path are exercised at scale (not just a trivial happy drain).
    let specs: Vec<(String, Behavior)> = (1..=n)
        .map(|k| {
            let b = if k % 10 == 0 {
                Behavior::DiesRecovers
            } else if k % 3 == 0 {
                Behavior::InstantDone
            } else {
                Behavior::Happy
            };
            (k.to_string(), b)
        })
        .collect();
    h.stage(specs, 16);

    h.run_to_drain(100_000);

    let terminal_in_queue = h.terminal_in_queue();
    ScaleReport {
        peak_queue_len: h.peak_queue_len,
        peak_recovery_len: h.peak_recovery_len,
        peak_json_len: h.peak_json_len,
        evicted: h.evicted,
        failed: h.st.counts().failed,
        completed: h.evicted + terminal_in_queue,
    }
}

/// The scale/durability acceptance (#380): a large synthetic run drains fully, and its per-tick
/// state footprint stays **bounded and independent of N** — the recovery trail is ring-buffer
/// capped, aged terminal items evict to history, and the serialized state a big run rewrites each
/// tick is no larger than a small one's.
#[test]
fn large_run_stays_bounded_and_drains_fully() {
    let small = run_scale(64);
    let large = run_scale(256);

    // Both runs account for every item — nothing lost, nothing stuck, none spuriously failed.
    for (r, n) in [(&small, 64usize), (&large, 256usize)] {
        assert_eq!(r.failed, 0, "no item should fail in the happy-scale run");
        assert_eq!(
            r.completed, n,
            "every item must reach a terminal, evicted or live"
        );
        assert!(r.evicted > 0, "the #380 history eviction must actually run");
        assert!(
            r.peak_recovery_len <= RECOVERY_CAP,
            "recovery trail must stay within its ring-buffer cap"
        );
    }

    // The large run must have exercised the recovery-trail trim (its lifetime events exceed the cap).
    assert_eq!(
        large.peak_recovery_len, RECOVERY_CAP,
        "a 256-item run should saturate (and thus trim to) the recovery cap"
    );

    // The live state footprint does NOT grow with N — the whole point of #380. A 4x larger backlog
    // peaks at essentially the same queue length and serialized size as the small one.
    assert!(
        large.peak_queue_len <= 96,
        "peak queue {} should stay bounded regardless of N",
        large.peak_queue_len
    );
    assert!(
        large.peak_queue_len <= small.peak_queue_len + 16,
        "peak queue must not scale with N (small={}, large={})",
        small.peak_queue_len,
        large.peak_queue_len
    );
    assert!(
        large.peak_json_len <= small.peak_json_len * 3 / 2 + 8_192,
        "per-tick state rewrite must not scale with N (small={}B, large={}B)",
        small.peak_json_len,
        large.peak_json_len
    );
    // And the bounded queue is far below the total item count — proof it isn't O(N).
    assert!(large.peak_queue_len < 256 / 2);
}
