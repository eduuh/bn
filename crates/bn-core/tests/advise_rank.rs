//! What the ranking core does **today**, pinned before anything moves it.
//!
//! `advise` and `select_wave` decide what the machine works on next, and between them they had no
//! tests at all — despite `select.rs`'s own module doc promising "the whole select→skip decision is
//! unit-tested directly here". The 32 tests in `queue_tick_policy.rs` reach this code only through
//! `plan_tick`, and exercise its ordering incidentally.
//!
//! So this file is a characterization net, not an endorsement. Several of the behaviours asserted
//! here are ones the queue is being changed to stop doing — the tie broken alphabetically, the
//! unbounded priority summed into a bounded strategic score. They are written down so that when
//! they change, it is because somebody meant to change them, and the diff says which.
use bn_core::advise::{advise, AdviseInputs, AttnSignal, Candidate, GoalContext};
use bn_core::queue::kind::BacklogKind;
use bn_core::rank::{Band, Score, BAND_STRIDE};
use bn_core::select::{select_wave, SelectCandidate, SelectionInputs, SkipReason};

const NOW: i64 = 1_000_000;

fn goal(key: &str, text: &str, status: &str) -> GoalContext {
    GoalContext {
        key: key.to_string(),
        text: text.to_string(),
        status: status.to_string(),
    }
}

/// Every candidate arrives at the same instant with no deadline, so the two time terms contribute
/// an identical constant and cannot reorder anything. Tests that care about time set them.
fn cand(id: &str, title: &str, priority: i64) -> Candidate {
    Candidate {
        id: id.to_string(),
        repo: "bn".to_string(),
        title: title.to_string(),
        kind: BacklogKind::Ship,
        priority,
        goal: None,
        band: None,
        added_at: NOW,
        not_after: None,
        est_secs: 0,
    }
}

fn attn(repo: &str, severity: &str) -> AttnSignal {
    AttnSignal {
        repo: Some(repo.to_string()),
        severity: severity.to_string(),
    }
}

fn ranked(
    goals: &[GoalContext],
    candidates: &[Candidate],
    attention: &[AttnSignal],
) -> Vec<String> {
    advise(&AdviseInputs {
        goals,
        candidates,
        attention,
        capacity: candidates.len(),
        now: NOW,
    })
    .into_iter()
    .map(|a| a.id)
    .collect()
}

/// The score's components, which is what these tests are really about. The flat total is
/// `band * BAND_STRIDE + within`, so asserting it would pin the band arithmetic into every test
/// about a goal weight.
fn breakdown(goals: &[GoalContext], candidates: &[Candidate], id: &str) -> Score {
    advise(&AdviseInputs {
        goals,
        candidates,
        attention: &[],
        capacity: candidates.len(),
        now: NOW,
    })
    .into_iter()
    .find(|a| a.id == id)
    .map(|a| a.breakdown)
    .expect("candidate must be ranked")
}

fn align_of(goals: &[GoalContext], candidates: &[Candidate], id: &str) -> i64 {
    breakdown(goals, candidates, id).align
}

// ------------------------------------------------------------------ the weights, exactly

/// The status ladder, stated as the module doc states it: a candidate advancing an off-track goal
/// outranks one advancing an at-risk goal, which outranks unscored, on-track, and unaligned.
#[test]
fn goal_status_orders_the_ladder() {
    let goals = vec![
        goal("alpha", "alpha", "off-track"),
        goal("bravo", "bravo", "at-risk"),
        goal("charlie", "charlie", "unscored"),
        goal("delta", "delta", "on-track"),
        goal("echo", "echo", "done"),
    ];
    let candidates = vec![
        cand("e", "echo", 0),
        cand("d", "delta", 0),
        cand("c", "charlie", 0),
        cand("b", "bravo", 0),
        cand("a", "alpha", 0),
    ];
    assert_eq!(ranked(&goals, &candidates, &[]), ["a", "b", "c", "d", "e"]);
}

/// The exact numbers, so a change to any of them is a visible diff rather than a reordering
/// somebody notices in production.
#[test]
fn the_status_weights_are_what_they_are() {
    for (status, weight) in [
        ("off-track", 100),
        ("at-risk", 80),
        ("unscored", 55),
        ("on-track", 40),
        ("done", 0),
        ("something-nobody-defined", 0),
    ] {
        let goals = vec![goal("g", "widget", status)];
        let candidates = vec![cand("x", "widget", 0)];
        assert_eq!(
            align_of(&goals, &candidates, "x"),
            weight,
            "status {status}"
        );
    }
}

/// An explicit `--q` tag is worth more than the same alignment inferred from words.
#[test]
fn an_explicit_tag_beats_a_keyword_match_by_fifteen() {
    let goals = vec![goal("g", "widget", "on-track")];
    let keyword = vec![cand("x", "widget", 0)];
    let mut tagged = vec![cand("x", "something unrelated entirely", 0)];
    tagged[0].goal = Some("g".to_string());
    assert_eq!(align_of(&goals, &keyword, "x"), 40);
    assert_eq!(align_of(&goals, &tagged, "x"), 55);
}

/// Attention pressure is per-repo and takes the highest open severity, not the sum.
#[test]
fn attention_pressure_takes_the_worst_open_item() {
    let candidates = vec![cand("x", "anything", 0)];
    for (severity, weight) in [("urgent", 30), ("warn", 15), ("info", 5), ("garbled", 5)] {
        let ranks = advise(&AdviseInputs {
            goals: &[],
            candidates: &candidates,
            attention: &[attn("bn", severity)],
            capacity: 1,
            now: NOW,
        });
        assert_eq!(ranks[0].breakdown.pressure, weight, "severity {severity}");
    }
    let many = advise(&AdviseInputs {
        goals: &[],
        candidates: &candidates,
        attention: &[attn("bn", "info"), attn("bn", "urgent"), attn("bn", "warn")],
        capacity: 1,
        now: NOW,
    });
    assert_eq!(
        many[0].breakdown.pressure, 30,
        "the worst severity wins; they do not add"
    );
}

/// Pressure applies only to the repo it names.
#[test]
fn attention_on_another_repo_is_not_pressure_here() {
    let candidates = vec![cand("x", "anything", 0)];
    let ranks = advise(&AdviseInputs {
        goals: &[],
        candidates: &candidates,
        attention: &[attn("arena", "urgent")],
        capacity: 1,
        now: NOW,
    });
    assert_eq!(ranks[0].breakdown.pressure, 0);
}

/// The composition, stated whole: a band decides the thousands, and everything else competes
/// inside it. Priority is no longer *summed* into the score — it is read as the band it implies
/// plus a nudge above that band's floor.
#[test]
fn the_score_is_a_band_plus_the_terms_that_compete_inside_it() {
    let goals = vec![goal("g", "widget", "at-risk")];
    let candidates = vec![cand("x", "widget", 7)];
    let ranks = advise(&AdviseInputs {
        goals: &goals,
        candidates: &candidates,
        attention: &[attn("bn", "warn")],
        capacity: 1,
        now: NOW,
    });
    let b = ranks[0].breakdown;
    assert_eq!(b.band, Band::Scheduled, "priority 7 is ordinary backlog");
    assert_eq!(b.align, 80);
    assert_eq!(b.pressure, 15);
    assert_eq!(b.nudge, 47, "7 sits 47 above the scheduled floor of -40");
    assert_eq!(b.within, 47 + 80 + 15);
    assert_eq!(b.total, Band::Scheduled.ord() * BAND_STRIDE + b.within);
}

/// The property the whole band model exists for: no combination of within-band terms can lift work
/// out of its band. The arithmetic is asserted at compile time in `rank.rs`; this is the same
/// claim made behaviourally, through the real ranker.
#[test]
fn nothing_inside_a_band_can_reach_the_next_one() {
    let goals = vec![goal("g", "widget", "off-track")];
    // Everything a background item could possibly accumulate: maximum nudge, perfect alignment,
    // urgent attention, an expired deadline, and a fortnight of waiting.
    let mut loaded = cand("loaded", "widget", -1);
    loaded.added_at = NOW - 14 * 24 * 3600;
    loaded.not_after = Some(NOW - 1);
    loaded.band = Some(Band::Background);
    // A bare scheduled item with nothing going for it at all.
    let mut bare = cand("bare", "nothing in common", -40);
    bare.band = Some(Band::Scheduled);
    let order = advise(&AdviseInputs {
        goals: &goals,
        candidates: &[loaded, bare],
        attention: &[attn("bn", "urgent")],
        capacity: 2,
        now: NOW,
    });
    assert_eq!(
        order[0].id, "bare",
        "a maximally-loaded background item must still lose to a bare scheduled one"
    );
}

// ------------------------------------------------------------------ keyword alignment

/// Overlap is on words of three characters or more, with a stopword list, so a shared "the" is
/// not an alignment.
#[test]
fn stopwords_and_short_words_do_not_align_anything() {
    let goals = vec![goal("g", "the and for with that this", "off-track")];
    let candidates = vec![cand("x", "the and for with that this", 0)];
    assert_eq!(
        align_of(&goals, &candidates, "x"),
        0,
        "a title sharing only stopwords must not align"
    );
}

#[test]
fn alignment_is_case_insensitive() {
    let goals = vec![goal("g", "Rewrite The Scheduler", "off-track")];
    let candidates = vec![cand("x", "scheduler rewrite", 0)];
    assert_eq!(align_of(&goals, &candidates, "x"), 100);
}

/// A tag naming a goal that is not open falls back to keyword matching rather than scoring as
/// tagged against nothing.
#[test]
fn a_tag_for_an_unknown_goal_falls_back_to_keywords() {
    let goals = vec![goal("real", "widget", "on-track")];
    let mut candidates = vec![cand("x", "widget", 0)];
    candidates[0].goal = Some("does-not-exist".to_string());
    assert_eq!(align_of(&goals, &candidates, "x"), 40);
}

// ------------------------------------------------------------------ the ordering contract

/// Saturating, not wrapping. `queue_tick_policy` deliberately ranks an `i64::MAX` priority to
/// prove the arithmetic cannot wrap and sort the highest-priority item last; the band mapping has
/// to survive the same input, which is why `nudge_from_legacy` subtracts saturatingly.
#[test]
fn an_extreme_priority_saturates_instead_of_wrapping() {
    let goals = vec![goal("g", "widget", "off-track")];
    let candidates = vec![cand("huge", "widget", i64::MAX), cand("small", "widget", 0)];
    assert_eq!(ranked(&goals, &candidates, &[]), ["huge", "small"]);
    let b = breakdown(&goals, &candidates, "huge");
    assert_eq!(b.band, Band::Interrupt);
    assert!(
        b.total < BAND_STRIDE * 7,
        "an absurd priority must land in the top band, not off the end of the scale"
    );
}

/// The FIFO tiebreak, finally honoured. `BacklogEntry`'s doc and `bn queue add --priority`'s help
/// have always promised "ties break FIFO by `added_at`", and `Backlog::pending_for` implements
/// exactly that — but this ranker never read `added_at` at all and broke ties on repo then id,
/// alphabetically. On a machine whose scheduled work all shares one priority (27 of 33 entries, on
/// the one this was written for) the alphabet decided what ran.
#[test]
fn equal_scores_break_oldest_first() {
    let mut zulu = cand("zulu", "anything", 0);
    let mut alpha = cand("alpha", "anything", 0);
    let mut mike = cand("mike", "anything", 0);
    // Same band, same nudge, same everything — except when they arrived.
    zulu.added_at = NOW - 300;
    mike.added_at = NOW - 200;
    alpha.added_at = NOW - 100;
    assert_eq!(
        ranked(&[], &[alpha, mike, zulu], &[]),
        ["zulu", "mike", "alpha"],
        "the one that has waited longest goes first, whatever it is called"
    );
}

/// Waiting lifts work, and the lift saturates rather than growing without bound — that bound is
/// what keeps aging inside its band.
#[test]
fn waiting_lifts_work_and_then_stops() {
    let fresh = breakdown(&[], &[cand("fresh", "x", 0)], "fresh");
    assert_eq!(fresh.age, 0);

    let mut half = cand("half", "x", 0);
    half.added_at = NOW - 12 * 3600;
    let mut old = cand("old", "x", 0);
    old.added_at = NOW - 24 * 3600;
    let mut ancient = cand("ancient", "x", 0);
    ancient.added_at = NOW - 30 * 24 * 3600;
    let pool = [half, old, ancient];
    assert!(breakdown(&[], &pool, "half").age > 0);
    assert!(breakdown(&[], &pool, "half").age < breakdown(&[], &pool, "old").age);
    assert_eq!(
        breakdown(&[], &pool, "old").age,
        breakdown(&[], &pool, "ancient").age,
        "past saturation, waiting longer buys nothing more"
    );
}

/// Urgency is measured in slack, not proximity — so how big the work is decides whether a deadline
/// is close. A four-hour job due in five hours is in more trouble than a five-second job due in one.
#[test]
fn a_deadline_is_urgent_relative_to_how_long_the_work_takes() {
    // The same deadline, so only the size differs. The four-hour job has an hour of slack; the
    // five-second job has five hours of it, which is no urgency at all.
    let mut big = cand("big", "x", 0);
    big.not_after = Some(NOW + 5 * 3600);
    big.est_secs = 4 * 3600;
    let mut small = cand("small", "x", 0);
    small.not_after = Some(NOW + 5 * 3600);
    small.est_secs = 5;
    let pool = [big, small];
    assert!(breakdown(&[], &pool, "big").urgency > 0);
    assert_eq!(
        breakdown(&[], &pool, "small").urgency,
        0,
        "the same deadline is not urgent for work that finishes long before it"
    );
    assert_eq!(
        ranked(&[], &pool, &[]),
        ["big", "small"],
        "the job that barely fits its deadline goes first"
    );

    let mut relaxed = cand("relaxed", "x", 0);
    relaxed.not_after = Some(NOW + 30 * 24 * 3600);
    assert_eq!(
        breakdown(&[], &[relaxed], "relaxed").urgency,
        0,
        "a distant deadline is not urgency"
    );

    let mut missed = cand("missed", "x", 0);
    missed.not_after = Some(NOW - 1);
    assert_eq!(breakdown(&[], &[missed], "missed").urgency, 400);
}

/// Work with no deadline is not urgent, which is most of it — a code change is not less valuable
/// an hour later.
#[test]
fn work_without_a_deadline_is_never_urgent() {
    assert_eq!(breakdown(&[], &[cand("x", "anything", 0)], "x").urgency, 0);
}

/// The scale mismatch, fixed. Priority used to be summed into a strategic score whose own terms
/// cap at 145, so `--priority 200` erased the strategic layer outright. Now the number is read as
/// the claim it implies: 200 says "a person asked", which is a higher band, and inside that band
/// goal alignment competes normally again.
#[test]
fn a_shouted_priority_names_a_band_instead_of_erasing_the_strategy() {
    let goals = vec![goal("g", "widget", "off-track")];

    // Across bands, the person still wins — that is what asking means.
    let across = [
        cand("strategic", "widget", 0),
        cand("shouty", "unrelated", 200),
    ];
    assert_eq!(ranked(&goals, &across, &[]), ["shouty", "strategic"]);
    assert_eq!(breakdown(&goals, &across, "shouty").band, Band::Asked);
    assert_eq!(
        breakdown(&goals, &across, "strategic").band,
        Band::Scheduled
    );

    // Inside one band, it no longer drowns anything: both are `Asked`, and the one advancing an
    // off-track goal leads despite the other shouting a hundred points louder.
    let within = [
        cand("aligned", "widget", 100),
        cand("louder", "unrelated", 199),
    ];
    assert_eq!(ranked(&goals, &within, &[]), ["aligned", "louder"]);
}

/// Rank is 1-based and dense over the whole pool; `within_capacity` marks the shortlist without
/// truncating the ranking.
#[test]
fn capacity_flags_the_shortlist_without_trimming_the_ranking() {
    let candidates = vec![cand("a", "x", 3), cand("b", "x", 2), cand("c", "x", 1)];
    let ranks = advise(&AdviseInputs {
        goals: &[],
        candidates: &candidates,
        attention: &[],
        capacity: 2,
        now: NOW,
    });
    assert_eq!(ranks.len(), 3);
    assert_eq!(
        ranks.iter().map(|a| a.rank).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        ranks.iter().map(|a| a.within_capacity).collect::<Vec<_>>(),
        vec![true, true, false]
    );
}

#[test]
fn an_empty_pool_ranks_nothing_rather_than_erroring() {
    assert!(advise(&AdviseInputs {
        goals: &[],
        candidates: &[],
        attention: &[],
        capacity: 5,
        now: NOW,
    })
    .is_empty());
}

// ------------------------------------------------------------------ select_wave's guardrails

fn sel(id: &str, priority: i64) -> SelectCandidate {
    SelectCandidate {
        id: id.to_string(),
        repo: "bn".to_string(),
        title: "some work".to_string(),
        kind: BacklogKind::Ship,
        priority,
        goal: None,
        band: None,
        added_at: NOW,
        not_after: None,
        est_secs: 0,
        touches: Vec::new(),
        mergeable: true,
    }
}

fn wave(candidates: &[SelectCandidate], capacity: usize, inflight: &[Vec<String>]) -> Vec<String> {
    select_wave(&SelectionInputs {
        goals: &[],
        candidates,
        attention: &[],
        capacity,
        inflight_touches: inflight,
        now: NOW,
    })
    .wave
    .into_iter()
    .map(|c| c.id)
    .collect()
}

fn skip_reason(
    candidates: &[SelectCandidate],
    capacity: usize,
    inflight: &[Vec<String>],
    id: &str,
) -> Option<SkipReason> {
    select_wave(&SelectionInputs {
        goals: &[],
        candidates,
        attention: &[],
        capacity,
        inflight_touches: inflight,
        now: NOW,
    })
    .skipped
    .into_iter()
    .find(|s| s.id == id)
    .map(|s| s.reason)
}

#[test]
fn the_wave_follows_the_advise_order() {
    let candidates = vec![sel("low", 1), sel("high", 100), sel("mid", 50)];
    assert_eq!(wave(&candidates, 3, &[]), ["high", "mid", "low"]);
}

/// Work with no path to a clean merge is dropped, and labelled for *why it cannot run* rather than
/// merely "no room" — which is why the intrinsic reasons are checked before capacity.
#[test]
fn unmergeable_work_is_dropped_even_when_there_is_room() {
    let mut candidates = vec![sel("blocked", 100), sel("fine", 1)];
    candidates[0].mergeable = false;
    assert_eq!(wave(&candidates, 5, &[]), ["fine"]);
    assert_eq!(
        skip_reason(&candidates, 5, &[], "blocked"),
        Some(SkipReason::CantMerge)
    );
}

/// A candidate whose touches overlap committed work is held — and does *not* occupy a touch area,
/// so a later disjoint candidate still fills the slot.
#[test]
fn overlapping_touches_are_held_and_do_not_block_the_slot() {
    let mut candidates = vec![sel("collides", 100), sel("disjoint", 1)];
    candidates[0].touches = vec!["src/queue/*".to_string()];
    candidates[1].touches = vec!["docs/*".to_string()];
    let inflight = vec![vec!["src/queue/tick.rs".to_string()]];
    assert_eq!(wave(&candidates, 5, &inflight), ["disjoint"]);
    assert_eq!(
        skip_reason(&candidates, 5, &inflight, "collides"),
        Some(SkipReason::Conflict)
    );
}

/// Two picks in the same wave must not collide with each other either — the first pick joins the
/// conflict map before the second is weighed.
#[test]
fn two_candidates_in_one_wave_cannot_collide_with_each_other() {
    let mut candidates = vec![sel("first", 100), sel("second", 50)];
    candidates[0].touches = vec!["src/queue/*".to_string()];
    candidates[1].touches = vec!["src/queue/*".to_string()];
    assert_eq!(wave(&candidates, 5, &[]), ["first"]);
    assert_eq!(
        skip_reason(&candidates, 5, &[], "second"),
        Some(SkipReason::Conflict)
    );
}

/// An empty touch-set is "no hint", which conflicts with nothing — land-time serialization stays
/// the safety net.
#[test]
fn work_that_declares_no_touches_conflicts_with_nothing() {
    let candidates = vec![sel("a", 2), sel("b", 1)];
    let inflight = vec![vec!["src/**".to_string()]];
    assert_eq!(wave(&candidates, 5, &inflight), ["a", "b"]);
}

#[test]
fn capacity_is_extrinsic_and_reported_as_such() {
    let candidates = vec![sel("a", 3), sel("b", 2), sel("c", 1)];
    assert_eq!(wave(&candidates, 2, &[]), ["a", "b"]);
    assert_eq!(
        skip_reason(&candidates, 2, &[], "c"),
        Some(SkipReason::Capacity)
    );
}

/// Zero capacity still labels each candidate by why it could not run, rather than calling
/// everything "no room".
#[test]
fn zero_capacity_still_names_the_intrinsic_reasons() {
    let mut candidates = vec![sel("blocked", 3), sel("roomless", 1)];
    candidates[0].mergeable = false;
    assert!(wave(&candidates, 0, &[]).is_empty());
    assert_eq!(
        skip_reason(&candidates, 0, &[], "blocked"),
        Some(SkipReason::CantMerge)
    );
    assert_eq!(
        skip_reason(&candidates, 0, &[], "roomless"),
        Some(SkipReason::Capacity)
    );
}

// ------------------------------------------------------------------ one vocabulary, two surfaces

/// `bn next` and `bn queue` now name the same claims with the same words.
///
/// This is what "unified" means here, and what it deliberately does not mean: the two surfaces
/// still *sort* differently, because a person reading a short list wants sections and batching
/// while a dispatcher wants throughput. What they no longer do is disagree about what kind of claim
/// a piece of work has on the machine.
#[test]
fn both_surfaces_name_the_same_bands() {
    use bn_core::action_rank;
    assert_eq!(
        Band::from_action_priority(action_rank::URGENT.priority()),
        Band::Interrupt
    );
    assert_eq!(
        Band::from_action_priority(action_rank::BLOCKED.priority()),
        Band::Interrupt
    );
    // The guard shares Interrupt with broken work: both say "before anything else". What
    // separates them in `bn next` is `is_urgent`, which is about interrupting focus, not rank.
    assert_eq!(
        Band::from_action_priority(action_rank::GUARD.priority()),
        Band::Interrupt
    );
    assert!(!action_rank::GUARD.is_urgent() && action_rank::URGENT.is_urgent());
    assert_eq!(
        Band::from_action_priority(action_rank::WARN.priority()),
        Band::Planned
    );
    assert_eq!(
        Band::from_action_priority(action_rank::INFO.priority()),
        Band::Scheduled
    );
}

/// `bn next`'s own order is preserved by the mapping — a more pressing action never maps to a
/// lower band than a less pressing one.
#[test]
fn the_mapping_preserves_the_attention_lists_order() {
    use bn_core::action_rank;
    let ladder = [
        action_rank::GUARD.priority(),
        action_rank::URGENT.priority(),
        action_rank::WARN.priority(),
        action_rank::INFO.priority(),
    ];
    let mut sorted = ladder;
    sorted.sort_by(|a, b| action_rank::compare_priority(*a, *b));
    let bands: Vec<Band> = sorted
        .iter()
        .map(|p| Band::from_action_priority(*p))
        .collect();
    let mut descending = bands.clone();
    descending.sort_by_key(|b| std::cmp::Reverse(b.ord()));
    assert_eq!(
        bands, descending,
        "a more pressing action must never map below a less pressing one"
    );
}

/// Every legacy priority already on disk maps to the band it was standing in for, and the mapping
/// preserves their relative order — which is what makes this a rename rather than a migration.
#[test]
fn the_live_priority_table_maps_to_the_bands_it_meant() {
    for (priority, band) in [
        (1000, Band::Interrupt), // INTERRUPT_PRIORITY / ASK_PRIORITY
        (100, Band::Asked),      // a typed bn dispatch, attn warn
        (81, Band::Planned),     // the Today enhance pass
        (80, Band::Planned),     // an accepted Today suggestion
        (70, Band::Planned),     // a pipeline investigate stage
        (50, Band::Recovery),    // RECOVERY_PRIORITY
        (4, Band::Scheduled),    // cron-health
        (0, Band::Scheduled),    // the default ship
        (-10, Band::Scheduled),  // CRON_PRIORITY
        (-50, Band::Background), // HEAVY_PRIORITY
    ] {
        assert_eq!(
            Band::from_legacy_priority(priority),
            band,
            "priority {priority}"
        );
    }

    // And the order survives: ranking by band-then-nudge reproduces ranking by the raw integer.
    let live = [1000, 100, 81, 80, 70, 50, 40, 4, 0, -10, -50];
    let by_band: Vec<i64> = {
        let mut v = live.to_vec();
        v.sort_by_key(|p| {
            let band = Band::from_legacy_priority(*p);
            std::cmp::Reverse((band.ord(), band.nudge_from_legacy(*p)))
        });
        v
    };
    let mut by_int = live.to_vec();
    by_int.sort_by_key(|p| std::cmp::Reverse(*p));
    assert_eq!(
        by_band, by_int,
        "the band mapping must not reorder the live table"
    );
}
