//! Property / randomized invariant tests for the resource-aware scheduler + orchestrate state
//! machine (#462, validating #373/#338).
//!
//! The `bn orchestrate` daemon enforces several **invariants** whose unit tests (in
//! `scheduler.rs` / `orchestrate.rs`) pin specific hand-built cases. This file adds the missing
//! *property-based* layer: it drives **arbitrary, seeded event sequences** — random items with
//! random `touches` / work-class / substrate, random caps + machine/budget pressure, dispatch,
//! completion, reboot, and terminal reaping — through the same pure transitions the daemon uses
//! (`scheduler::pick`/`admit`, and the `OrchestrateState` lifecycle mutators), asserting after
//! every tick that the invariants hold:
//!
//!   * **never-exceed-caps** (#377/#385) — local in-flight ≤ the effective cap, global *and*
//!     per typed lane; a resource/budget pause dispatches nothing locally.
//!   * **no-double-dispatch** — a pick plan only ever selects distinct, still-`Queued` items,
//!     and `local`/`remote` are disjoint.
//!   * **conflict-exclusion** (#376) — no locally-dispatched item overlaps the `touches` of any
//!     already-in-flight or earlier-picked item; no two *local* in-flight items ever overlap.
//!   * **reserved ask lane** (#378) — an untyped run never lets ships consume the last shared slot
//!     while a scout/ask is queued: a local slot is always reserved for it.
//!   * **reboot-safe attempt reset** (#380) — a simulated reboot resets `attempts` for in-flight
//!     items only, never queued or terminal ones.
//!   * **terminal reaping + bounded state** (#380) — terminal items are evicted and both the queue
//!     and the recovery trail stay bounded regardless of run length.
//!   * **monotonic progress** — an item's lifecycle only advances (queued → dispatched → terminal),
//!     never regresses, and no in-flight item silently disappears.
//!
//! Everything is **deterministic**: a `splitmix64` PRNG seeded per case, so any failure reproduces
//! from the seed printed in its assertion message. No `proptest`/`quickcheck` dev-dep exists in
//! this workspace, so — per the issue — this is a seeded table-driven generator, not a macro DSL.
//! Pure API only (no env, clock, or disk), so it never touches `BN_JOBS_DIR` or orchestrate logic.

use std::collections::{HashMap, HashSet};

use bn_core::backlog::Substrate;
use bn_core::orchestrate::{
    touches_conflict, ItemStatus, OrchestrateState, PaneState, SupervisorAction, WorkClass,
    HISTORY_RETAIN_SECS, RECOVERY_CAP,
};
use bn_core::scheduler::{pick, Caps, Observed, Plan};

// ---------------------------------------------------------------------------
// Deterministic PRNG (splitmix64) — seeded so every failure reproduces.
// ---------------------------------------------------------------------------

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng {
            state: seed.wrapping_add(0x9E37_79B9_7F4A_7C15),
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (n clamped to ≥1 so it never divides by zero).
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// An inclusive `lo..=hi` (caller ensures `hi >= lo`).
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below((hi - lo + 1) as u64) as usize
    }

    /// True with probability `num/den`.
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    fn choose<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        let i = self.below(xs.len() as u64) as usize;
        &xs[i]
    }
}

// ---------------------------------------------------------------------------
// Scenario generation.
// ---------------------------------------------------------------------------

/// The touch-hint vocabulary the generator draws from — a mix of "no hint" (schedulable with
/// anything), disjoint files, overlapping sets, a directory glob, and the `**` solo case (overlaps
/// everything). Chosen so random draws actually produce conflicts to exclude.
const TOUCH_VOCAB: &[&[&str]] = &[
    &[],
    &[],
    &["crates/bn-cli/src/cli.rs"],
    &["crates/bn-core/src/staff.rs"],
    &[
        "crates/bn-cli/src/cli.rs",
        "crates/bn-core/src/orchestrate.rs",
    ],
    &["crates/bn-cli/**"],
    &["docs/*.md"],
    &["**"],
];

fn gen_touches(rng: &mut Rng) -> Vec<String> {
    let picked: &[&str] = rng.choose(TOUCH_VOCAB);
    picked.iter().map(|s| (*s).to_string()).collect()
}

/// Build a fresh, all-queued run with random caps and per-item attributes, plus the [`Caps`] the
/// CLI would derive from it (mirroring `build_caps`: `max_local = coding_cap`, `research =
/// research_cap`). Guards/observed pressure are layered on per tick, not here.
fn gen_state(rng: &mut Rng) -> (OrchestrateState, Caps) {
    let n = rng.range(1, 8);
    let ids: Vec<String> = (0..n)
        .map(|i| {
            // Mix issue-linked (numeric) ids with free-text slugs — the two exercise different
            // remote-eligibility paths in `pick`.
            if rng.chance(1, 2) {
                format!("{}", 300 + i)
            } else {
                format!("task-{i}")
            }
        })
        .collect();

    let cap = rng.range(1, 4);
    let solo = rng.chance(1, 6);
    let research = if !solo && rng.chance(1, 2) {
        Some(rng.range(1, 3))
    } else {
        None
    };

    let mut st = OrchestrateState::new("bn", &ids, cap, solo, 0);
    st.research = research;
    for item in st.queue.iter_mut() {
        item.touches = gen_touches(rng);
        item.substrate = *rng.choose(&[Substrate::Auto, Substrate::Local, Substrate::Remote]);
        item.work_class = if rng.chance(1, 3) {
            WorkClass::Research
        } else {
            WorkClass::Coding
        };
    }

    // Mirror the CLI's `build_caps`: the base local/research ceilings come from the state itself
    // (so the state accessors used in post-apply assertions agree with what `pick` enforced).
    let caps = Caps {
        max_local: st.coding_cap(),
        research: st.research_cap(),
        min_free_ram_mb: None,
        max_loadavg: None,
        max_swap_pct: None,
        tokens_per_hour: None,
        daily_cost_usd: None,
        daily_cost_usd_coding: None,
        daily_cost_usd_research: None,
        plan_gated: rng.chance(1, 4),
        remote_overflow: rng.chance(1, 2),
        local_owned: true,
    };
    (st, caps)
}

/// Layer this tick's live machine/budget pressure onto the base caps — sometimes tripping exactly
/// one resource/budget guard so the *pause ⇒ no local dispatch* invariant gets exercised.
fn gen_pressure(rng: &mut Rng, base: Caps) -> (Caps, Observed) {
    let mut caps = base;
    let mut obs = Observed::default();
    match rng.below(6) {
        0 => {
            caps.min_free_ram_mb = Some(4000);
            obs.free_ram_mb = Some(1000);
        }
        1 => {
            caps.max_loadavg = Some(5.0);
            obs.loadavg = Some(9.0);
        }
        2 => {
            caps.tokens_per_hour = Some(1000);
            obs.tokens_this_hour = 5000;
        }
        3 => {
            caps.daily_cost_usd = Some(10.0);
            obs.cost_today_usd = 25.0;
        }
        // The remaining draws leave every guard off (no pressure) — the common case.
        _ => {}
    }
    (caps, obs)
}

/// Whether any configured resource/budget guard is tripped by the observed signals — the condition
/// under which `pick` must dispatch nothing locally (a pause).
fn guard_tripped(caps: &Caps, obs: &Observed) -> bool {
    caps.tokens_per_hour
        .is_some_and(|t| obs.tokens_this_hour >= t)
        || caps.daily_cost_usd.is_some_and(|c| obs.cost_today_usd >= c)
        || caps
            .min_free_ram_mb
            .zip(obs.free_ram_mb)
            .is_some_and(|(min, free)| free < min)
        || caps
            .max_loadavg
            .zip(obs.loadavg)
            .is_some_and(|(max, load)| load > max)
}

// ---------------------------------------------------------------------------
// Applying a plan + shared invariant assertions.
// ---------------------------------------------------------------------------

/// Realize a dispatch plan against the state exactly as the CLI executor would: local picks become
/// dispatched with a worktree branch; remote picks fan out (substrate pinned to `Remote`). Indices
/// point into `st.queue` and nothing is removed, so they stay valid across both loops.
fn apply_plan(st: &mut OrchestrateState, plan: &Plan, now: i64) {
    for &idx in &plan.local {
        let branch = format!("dispatch/{}", st.queue[idx].id);
        st.mark_dispatched(idx, Some(branch), now);
    }
    for &idx in &plan.remote {
        st.mark_dispatched_remote(idx, now);
    }
}

/// #no-double-dispatch: a plan only selects distinct, still-`Queued` items, and never puts the same
/// item in both lanes.
fn assert_plan_wellformed(st: &OrchestrateState, plan: &Plan, seed: u64, tick: usize) {
    let mut seen: HashSet<usize> = HashSet::new();
    for &idx in plan.local.iter().chain(plan.remote.iter()) {
        assert!(
            idx < st.queue.len(),
            "seed {seed} tick {tick}: plan index {idx} out of range"
        );
        assert!(
            seen.insert(idx),
            "seed {seed} tick {tick}: item {idx} double-dispatched (appears twice / in both lanes)"
        );
        assert_eq!(
            st.queue[idx].status,
            ItemStatus::Queued,
            "seed {seed} tick {tick}: pick selected non-queued item {idx}"
        );
    }
}

/// #376 conflict-exclusion (on the plan): every locally-dispatched item is disjoint from the
/// `touches` of anything already in flight *and* of any item planned earlier in queue order (local
/// or remote). This is the exact guarantee `pick` composes with — a would-be collider serializes.
fn assert_conflict_exclusion(st: &OrchestrateState, plan: &Plan, seed: u64, tick: usize) {
    // Touch-sets already in flight before this pass (any substrate — remote counts too).
    let inflight: Vec<&Vec<String>> = st
        .queue
        .iter()
        .filter(|i| i.status.in_flight())
        .map(|i| &i.touches)
        .collect();
    // Everything planned this pass, tagged with its queue index (order = queue order).
    let mut planned: Vec<usize> = plan
        .local
        .iter()
        .chain(plan.remote.iter())
        .copied()
        .collect();
    planned.sort_unstable();

    for &li in &plan.local {
        let cand = &st.queue[li].touches;
        for other in &inflight {
            assert!(
                !touches_conflict(cand, other),
                "seed {seed} tick {tick}: local pick {li} overlaps an in-flight touch-set"
            );
        }
        for &pi in &planned {
            if pi < li {
                assert!(
                    !touches_conflict(cand, &st.queue[pi].touches),
                    "seed {seed} tick {tick}: local pick {li} overlaps earlier-planned {pi}"
                );
            }
        }
    }
}

/// A pause (any tripped guard) must dispatch nothing locally (#377). Remote fan-out may still make
/// progress, but not a single local slot is taken — and any surfaced bind is the pause reason
/// (guards are checked before the local cap in `admit`), never plain `LocalCap`.
fn assert_pause_blocks_local(caps: &Caps, obs: &Observed, plan: &Plan, seed: u64, tick: usize) {
    if !guard_tripped(caps, obs) {
        return;
    }
    assert!(
        plan.local.is_empty(),
        "seed {seed} tick {tick}: guard tripped yet {} local picks",
        plan.local.len()
    );
    if let Some(bind) = plan.bind {
        assert!(
            bind.is_pause(),
            "seed {seed} tick {tick}: guard tripped but bind is {bind:?}, not a pause"
        );
    }
}

/// #378 reserved-ask-lane (on the plan): in an *untyped*, non-paused run with a queued, local-bound
/// scout/ask waiting, ships never consume the last shared slot. If no scout is placed locally this
/// pass and a slot was free, coding picks leave at least one slot open for it (reserve, never
/// preempt). Stated over the *local-bound* scout only — a weaker antecedent than `pick`'s (which
/// reserves for any queued research), so it stays true even if remote-research reservation is later
/// optimized away.
fn assert_reserved_ask_lane(
    st: &OrchestrateState,
    caps: &Caps,
    local_before: usize,
    plan: &Plan,
    seed: u64,
    tick: usize,
) {
    if caps.research.is_some() {
        return; // typed runs reserve nothing — research has its own lane.
    }
    let cap = caps.max_local;
    let has_local_queued_scout = st.queue.iter().any(|i| {
        i.status == ItemStatus::Queued
            && i.work_class == WorkClass::Research
            && i.substrate != Substrate::Remote
    });
    let scout_placed_local = plan
        .local
        .iter()
        .any(|&i| st.queue[i].work_class == WorkClass::Research);
    let coding_picked = plan
        .local
        .iter()
        .filter(|&&i| st.queue[i].work_class == WorkClass::Coding)
        .count();

    if has_local_queued_scout && !scout_placed_local && local_before < cap {
        assert!(
            local_before + coding_picked < cap,
            "seed {seed} tick {tick}: ships took the reserved scout slot \
             (local_before={local_before} coding_picked={coding_picked} cap={cap})"
        );
    }
}

/// #377/#385 never-exceed-caps (post-apply, via the state's own accountants). A typed run caps each
/// lane independently; an untyped run caps one shared local pool.
fn assert_within_caps(st: &OrchestrateState, caps: &Caps, seed: u64, tick: usize) {
    if let Some(research_cap) = caps.research {
        let coding = st.local_in_flight_class(WorkClass::Coding);
        let research = st.local_in_flight_class(WorkClass::Research);
        assert!(
            coding <= caps.max_local,
            "seed {seed} tick {tick}: coding lane {coding} > cap {}",
            caps.max_local
        );
        assert!(
            research <= research_cap,
            "seed {seed} tick {tick}: research lane {research} > cap {research_cap}"
        );
    } else {
        let local = st.local_in_flight();
        assert!(
            local <= caps.max_local,
            "seed {seed} tick {tick}: shared local pool {local} > cap {}",
            caps.max_local
        );
    }
}

/// #376 snapshot: no two *local* in-flight items ever hold overlapping touch-areas. (Remote items
/// are exempt by design — they serialize at the merge-queue — so the guarantee is over locals.)
fn assert_no_local_overlap(st: &OrchestrateState, seed: u64, tick: usize) {
    let locals: Vec<&Vec<String>> = st
        .queue
        .iter()
        .filter(|i| i.status.in_flight() && i.substrate != Substrate::Remote)
        .map(|i| &i.touches)
        .collect();
    for (a, first) in locals.iter().enumerate() {
        for second in locals.iter().skip(a + 1) {
            assert!(
                !touches_conflict(first, second),
                "seed {seed} tick {tick}: two local in-flight items overlap"
            );
        }
    }
}

/// Status rank for the monotonic-progress check: queued < dispatched < terminal.
fn rank(status: ItemStatus) -> u8 {
    match status {
        ItemStatus::Queued => 0,
        ItemStatus::Dispatched => 1,
        ItemStatus::Merged | ItemStatus::Failed => 2,
    }
}

// ---------------------------------------------------------------------------
// Test 1 — the comprehensive per-tick property sweep.
// ---------------------------------------------------------------------------

/// Drive random event sequences and assert the caps / no-double-dispatch / conflict-exclusion /
/// reserved-ask / monotonic-progress invariants after **every** tick, across many seeds.
#[test]
fn invariants_hold_over_random_event_sequences() {
    for seed in 0..600u64 {
        let mut rng = Rng::new(seed);
        let (mut st, base_caps) = gen_state(&mut rng);

        let ids: HashSet<String> = st.queue.iter().map(|i| i.id.clone()).collect();
        let mut max_rank: HashMap<String, u8> = st
            .queue
            .iter()
            .map(|i| (i.id.clone(), rank(i.status)))
            .collect();

        let mut now: i64 = 1_000;
        let ticks = rng.range(6, 24);
        for tick in 0..ticks {
            let (caps, obs) = gen_pressure(&mut rng, base_caps);
            let local_before = st.local_in_flight();

            let plan = pick(&st, &caps, &obs);

            // --- invariants on the plan (pre-apply) ---
            assert_plan_wellformed(&st, &plan, seed, tick);
            assert_conflict_exclusion(&st, &plan, seed, tick);
            assert_pause_blocks_local(&caps, &obs, &plan, seed, tick);
            assert_reserved_ask_lane(&st, &caps, local_before, &plan, seed, tick);

            // --- apply, then invariants on the resulting state ---
            apply_plan(&mut st, &plan, now);
            assert_within_caps(&st, &caps, seed, tick);
            assert_no_local_overlap(&st, seed, tick);

            // The pool never shrinks or loses an item in this sweep (no eviction here).
            let now_ids: HashSet<String> = st.queue.iter().map(|i| i.id.clone()).collect();
            assert_eq!(
                now_ids, ids,
                "seed {seed} tick {tick}: an item vanished from the queue"
            );

            // Monotonic progress: status only advances, never regresses.
            for item in &st.queue {
                let entry = max_rank.entry(item.id.clone()).or_insert(0);
                assert!(
                    rank(item.status) >= *entry,
                    "seed {seed} tick {tick}: item {} regressed {} -> {}",
                    item.id,
                    *entry,
                    item.status.label()
                );
                *entry = rank(item.status);
            }

            // --- random lifecycle churn: complete some in-flight items ---
            let inflight_ids: Vec<String> = st
                .queue
                .iter()
                .filter(|i| i.status.in_flight())
                .map(|i| i.id.clone())
                .collect();
            for id in inflight_ids {
                if rng.chance(2, 5) {
                    st.mark_merged(&id, None, now);
                } else if rng.chance(1, 3) {
                    st.mark_failed(&id, now);
                }
            }
            now += rng.range(60, 600) as i64;
        }
    }
}

// ---------------------------------------------------------------------------
// Test 2 — #378 the reserved ask lane is never starved when a slot is free.
// ---------------------------------------------------------------------------

/// A queued scout/ask in an untyped, unpaused run dispatches on the *first* tick a local slot is
/// free — ships hold a slot for it rather than starving it. Randomized over the number of ships in
/// flight and the queue position of the ask.
#[test]
fn queued_scout_dispatches_the_moment_a_slot_frees() {
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed);
        let cap = rng.range(1, 4);

        // Some ships already running (0..cap), a stream of queued ships, and one queued ask placed
        // at a random position among them.
        let running = rng.range(0, cap);
        let queued_ships = rng.range(0, 4);
        let ask_pos = rng.range(0, queued_ships);

        let mut rows: Vec<(&str, ItemStatus, WorkClass)> = Vec::new();
        for _ in 0..running {
            rows.push(("ship-run", ItemStatus::Dispatched, WorkClass::Coding));
        }
        for i in 0..queued_ships {
            if i == ask_pos {
                rows.push(("ask", ItemStatus::Queued, WorkClass::Research));
            }
            rows.push(("ship-q", ItemStatus::Queued, WorkClass::Coding));
        }
        if ask_pos == queued_ships {
            rows.push(("ask", ItemStatus::Queued, WorkClass::Research));
        }

        let ids: Vec<String> = (0..rows.len()).map(|i| format!("i{i}")).collect();
        let mut st = OrchestrateState::new("bn", &ids, cap, false, 0);
        for (slot, (_, status, class)) in st.queue.iter_mut().zip(&rows) {
            slot.status = *status;
            slot.work_class = *class;
            // No touch hints — isolate the reservation from conflict holding.
        }

        let caps = Caps {
            max_local: st.coding_cap(),
            research: st.research_cap(),
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
        let plan = pick(&st, &caps, &Observed::default());

        // The ask is the sole research item; find it by lane.
        let ask_idx = st
            .queue
            .iter()
            .position(|i| i.work_class == WorkClass::Research)
            .expect("an ask item exists");

        let slot_free = running < cap;
        let ask_dispatched = plan.local.contains(&ask_idx);
        if slot_free {
            assert!(
                ask_dispatched,
                "seed {seed}: a slot was free (running={running} cap={cap}) yet the ask was starved"
            );
        } else {
            // Pool full — the ask legitimately waits, never preempting a running ship.
            assert!(
                !ask_dispatched,
                "seed {seed}: ask preempted a running ship (running={running} cap={cap})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test 3 — #380 reboot resets attempts for in-flight items only.
// ---------------------------------------------------------------------------

/// After a simulated reboot, `boot_reset_candidates` + `reset_backoff` zero the relaunch backoff of
/// **in-flight** items only (those with a live worktree and prior attempts), never queued or
/// terminal ones — a machine event must not burn a healthy item's failure budget nor touch work
/// that wasn't running.
#[test]
fn reboot_resets_attempts_for_inflight_only() {
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed);
        let (mut st, base_caps) = gen_state(&mut rng);

        // Warm the run up: a few dispatch/complete passes so we have a realistic mix of queued,
        // in-flight, and terminal items.
        let mut now: i64 = 1_000;
        for _ in 0..rng.range(2, 5) {
            let plan = pick(&st, &base_caps, &Observed::default());
            apply_plan(&mut st, &plan, now);
            let inflight: Vec<String> = st
                .queue
                .iter()
                .filter(|i| i.status.in_flight())
                .map(|i| i.id.clone())
                .collect();
            for id in inflight {
                // Some in-flight items accrue relaunch attempts; some complete.
                if rng.chance(1, 2) {
                    st.bump_attempts(&id, now);
                }
                if rng.chance(1, 4) {
                    st.mark_merged(&id, None, now);
                }
            }
            now += 120;
        }

        // Snapshot (status, attempts) before the reboot.
        let before: HashMap<String, (ItemStatus, u32)> = st
            .queue
            .iter()
            .map(|i| (i.id.clone(), (i.status, i.attempts)))
            .collect();

        // A random subset of items has a surviving worktree.
        let worktree_live: HashSet<String> = st
            .queue
            .iter()
            .filter(|_| rng.chance(1, 2))
            .map(|i| i.id.clone())
            .collect();

        // First boot marker recorded → not a fresh boot; a different marker → fresh boot.
        st.record_boot("boot-A", now);
        assert!(
            !st.is_fresh_boot("boot-A"),
            "seed {seed}: same marker is not a fresh boot"
        );
        assert!(
            st.is_fresh_boot("boot-B"),
            "seed {seed}: a changed marker is a fresh boot"
        );

        // Candidate correctness: exactly the in-flight, attempted, live items.
        let candidates = st.boot_reset_candidates(&worktree_live);
        let cand_set: HashSet<&String> = candidates.iter().collect();
        for item in &st.queue {
            let eligible =
                item.status.in_flight() && item.attempts > 0 && worktree_live.contains(&item.id);
            assert_eq!(
                cand_set.contains(&item.id),
                eligible,
                "seed {seed}: candidate membership wrong for {} (status {}, attempts {}, live {})",
                item.id,
                item.status.label(),
                item.attempts,
                worktree_live.contains(&item.id)
            );
        }

        // Reset over *all* ids (a superset) to prove `reset_backoff` itself only touches in-flight,
        // attempted items regardless of what it's handed.
        let all_ids: Vec<String> = st.queue.iter().map(|i| i.id.clone()).collect();
        let reset = st.reset_backoff(&all_ids, now + 5);
        let reset_set: HashSet<&String> = reset.iter().collect();

        for item in &st.queue {
            let (prev_status, prev_attempts) = before[&item.id];
            // Status is never changed by a backoff reset.
            assert_eq!(
                item.status, prev_status,
                "seed {seed}: reset changed status of {}",
                item.id
            );
            let was_inflight_attempted = prev_status.in_flight() && prev_attempts > 0;
            if was_inflight_attempted {
                assert!(
                    reset_set.contains(&item.id),
                    "seed {seed}: in-flight attempted item {} was not reset",
                    item.id
                );
                assert_eq!(
                    item.attempts, 0,
                    "seed {seed}: {} attempts not zeroed",
                    item.id
                );
                assert_eq!(
                    item.last_relaunch_at, 0,
                    "seed {seed}: {} last_relaunch_at not zeroed",
                    item.id
                );
            } else {
                // Queued / terminal / attempt-free items are untouched.
                assert!(
                    !reset_set.contains(&item.id),
                    "seed {seed}: non-in-flight item {} was reset",
                    item.id
                );
                assert_eq!(
                    item.attempts, prev_attempts,
                    "seed {seed}: {} attempts changed",
                    item.id
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test 4 — #380 terminal reaping keeps the state bounded over a long run.
// ---------------------------------------------------------------------------

/// A self-feeding run that dispatches, completes, records recovery events, and evicts terminal
/// items every tick keeps both the live queue and the recovery trail bounded by a constant that
/// depends only on the cap / batch / retain window — *not* on how many ticks (i.e. how long) it has
/// run. Also proves eviction is complete: nothing terminal older than the retain window survives.
#[test]
fn terminal_reaping_keeps_state_bounded() {
    for seed in 0..40u64 {
        let mut rng = Rng::new(seed);
        let cap = rng.range(2, 6);
        let batch = rng.range(1, cap); // add ≤ cap per tick so the queue drains

        let mut st = OrchestrateState::new("bn", &[format!("seed{seed}")], cap, false, 0);
        // Retire the placeholder so the run starts effectively empty.
        st.mark_merged(&format!("seed{seed}"), None, 0);

        let mut now: i64 = 10_000;
        const STEP: i64 = 900; // fixed clock step per tick
        let retention_ticks = (HISTORY_RETAIN_SECS / STEP + 1) as usize;
        // Bound: items evictable only after `retention_ticks` ticks; ≤ `cap` complete per tick;
        // plus a batch of fresh queued + a lane's worth in flight. Constant in the tick count.
        let bound = retention_ticks * cap + batch + cap + 1;

        let mut counter = 0usize;
        const TICKS: usize = 400;
        for tick in 0..TICKS {
            // Feed a fresh batch of queued work.
            let new_ids: Vec<String> = (0..batch)
                .map(|_| {
                    counter += 1;
                    format!("w{counter}")
                })
                .collect();
            st.extend(&new_ids, now);

            // Dispatch and immediately complete this tick's in-flight items.
            let plan = pick(&st, &base_caps_for(&st), &Observed::default());
            apply_plan(&mut st, &plan, now);
            let inflight: Vec<String> = st
                .queue
                .iter()
                .filter(|i| i.status.in_flight() && i.substrate != Substrate::Remote)
                .map(|i| i.id.clone())
                .collect();
            for id in inflight {
                if rng.chance(1, 2) {
                    st.mark_merged(&id, None, now);
                } else {
                    st.mark_failed(&id, now);
                }
            }

            // An unbounded source of recovery events, trimmed to the ring-buffer cap each tick.
            for _ in 0..rng.range(0, 12) {
                st.record_recovery(
                    "w1",
                    PaneState::Dead,
                    SupervisorAction::RelaunchToResume,
                    "reaped",
                    now,
                );
            }
            st.trim_recovery(RECOVERY_CAP);
            assert!(
                st.recovery.len() <= RECOVERY_CAP,
                "seed {seed} tick {tick}: recovery trail {} exceeds cap {RECOVERY_CAP}",
                st.recovery.len()
            );

            // Advance the clock and reap aged terminal items.
            now += STEP;
            st.evict_terminal(now, HISTORY_RETAIN_SECS);

            // Eviction is complete: no aged terminal item survives.
            assert!(
                st.queue.iter().all(|i| {
                    !(i.status.is_terminal()
                        && now.saturating_sub(i.updated_at) >= HISTORY_RETAIN_SECS)
                }),
                "seed {seed} tick {tick}: an aged terminal item escaped eviction"
            );

            // The whole point: the live queue stays bounded by a tick-count-independent constant.
            assert!(
                st.queue.len() <= bound,
                "seed {seed} tick {tick}: queue length {} exceeded bound {bound} (unbounded growth!)",
                st.queue.len()
            );
        }
    }
}

/// The caps a bounded-run state derives (untyped, no guards) — mirrors `build_caps` for the
/// reaping test where research lanes and pressure are irrelevant.
fn base_caps_for(st: &OrchestrateState) -> Caps {
    Caps {
        max_local: st.coding_cap(),
        research: st.research_cap(),
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

// ---------------------------------------------------------------------------
// Test 5 — non-vacuity witness: the generator actually produces the interesting
// scenarios the property tests rely on. Guards the suite against silently going
// vacuous if the generator is later tweaked.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Coverage {
    local_cap_bind: u32,
    resource_pause: u32,
    conflict_hold: u32,
    remote_fanout: u32,
    reserve_fired: u32,
    typed_run: u32,
    multi_local_inflight: u32,
}

#[test]
fn generator_exercises_every_invariant_scenario() {
    let mut cov = Coverage::default();

    for seed in 0..600u64 {
        let mut rng = Rng::new(seed);
        let (mut st, base_caps) = gen_state(&mut rng);
        if base_caps.research.is_some() {
            cov.typed_run += 1;
        }

        let mut now: i64 = 1_000;
        let ticks = rng.range(6, 24);
        for _ in 0..ticks {
            let (caps, obs) = gen_pressure(&mut rng, base_caps);
            let local_before = st.local_in_flight();
            let plan = pick(&st, &caps, &obs);

            if plan.bind == Some(bn_core::scheduler::Bind::LocalCap) {
                cov.local_cap_bind += 1;
            }
            if plan.paused() {
                cov.resource_pause += 1;
            }
            if !plan.remote.is_empty() {
                cov.remote_fanout += 1;
            }

            // A conflict hold: a queued item with real touches that `pick` left unplaced because it
            // overlaps something in flight.
            let planned: HashSet<usize> = plan
                .local
                .iter()
                .chain(plan.remote.iter())
                .copied()
                .collect();
            let inflight: Vec<&Vec<String>> = st
                .queue
                .iter()
                .filter(|i| i.status.in_flight())
                .map(|i| &i.touches)
                .collect();
            for (idx, item) in st.queue.iter().enumerate() {
                if item.status == ItemStatus::Queued
                    && !item.touches.is_empty()
                    && !planned.contains(&idx)
                    && inflight.iter().any(|t| touches_conflict(&item.touches, t))
                {
                    cov.conflict_hold += 1;
                    break;
                }
            }

            // The reserve mechanism actually held a ship back for a queued scout.
            let has_local_queued_scout = caps.research.is_none()
                && st.queue.iter().any(|i| {
                    i.status == ItemStatus::Queued
                        && i.work_class == WorkClass::Research
                        && i.substrate != Substrate::Remote
                });
            let scout_placed_local = plan
                .local
                .iter()
                .any(|&i| st.queue[i].work_class == WorkClass::Research);
            if has_local_queued_scout && !scout_placed_local && local_before < caps.max_local {
                cov.reserve_fired += 1;
            }

            apply_plan(&mut st, &plan, now);
            if st.local_in_flight() >= 2 {
                cov.multi_local_inflight += 1;
            }

            let inflight_ids: Vec<String> = st
                .queue
                .iter()
                .filter(|i| i.status.in_flight())
                .map(|i| i.id.clone())
                .collect();
            for id in inflight_ids {
                if rng.chance(2, 5) {
                    st.mark_merged(&id, None, now);
                } else if rng.chance(1, 3) {
                    st.mark_failed(&id, now);
                }
            }
            now += rng.range(60, 600) as i64;
        }
    }

    assert!(
        cov.local_cap_bind > 0,
        "generator never hit a local-cap bind"
    );
    assert!(
        cov.resource_pause > 0,
        "generator never hit a resource/budget pause"
    );
    assert!(
        cov.conflict_hold > 0,
        "generator never produced a conflict hold"
    );
    assert!(
        cov.remote_fanout > 0,
        "generator never produced a remote fan-out"
    );
    assert!(
        cov.reserve_fired > 0,
        "generator never exercised the reserved ask lane"
    );
    assert!(
        cov.typed_run > 0,
        "generator never produced a typed (research-lane) run"
    );
    assert!(
        cov.multi_local_inflight > 0,
        "generator never had ≥2 local agents in flight"
    );
}
