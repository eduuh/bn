//! The Today↔queue tie: accepting a suggestion is what feeds the queue, and the round trip back.
//!
//! Covers the pure, load-bearing half — the source lookup that makes accepting idempotent, and the
//! action-feed fold that must stop offering to start work already in flight. The producer itself
//! lives in `bn-cli` (it needs a task store and a config), but everything it *depends* on for
//! correctness is here, because these are the invariants whose violation is silent: a second accept
//! that quietly duplicates work, and a CTA that launches an agent onto work already running.
//!
//! An integration file rather than `#[cfg(test)]` modules, matching `queue_units.rs`: the repo runs a
//! 1000-line ratchet over every tracked `.rs`, and `action_center.rs` needs its remaining headroom.
use bn_core::action_center::{build_actions, ActionKind, ActionSources};
use bn_core::backlog::{AddSpec, Backlog, BacklogEntry, BacklogStatus};
use bn_core::queue::kind::BacklogKind;
use bn_core::suggestions::{Suggestion, SuggestionStatus};

/// A suggestion in a given state, with the queue link a producer would have cached on it.
fn suggestion(id_key: &str, status: SuggestionStatus) -> Suggestion {
    let mut s = Suggestion::new(
        "pr-queue",
        id_key,
        "fix the flaky queue tick under load",
        "surfaced by the pr-queue cron",
        "https://github.com/eduuh/bn/issues/577",
    );
    s.status = status;
    s
}

/// An entry as a producer would have written it, at `status`.
fn entry(
    repo: &str,
    subject: &str,
    source: &str,
    kind: BacklogKind,
    status: BacklogStatus,
) -> BacklogEntry {
    let spec = AddSpec {
        kind,
        priority: 80,
        source: source.to_string(),
        task: "t1".to_string(),
        ..Default::default()
    };
    let mut e = BacklogEntry::new(repo, subject, &spec, 1_000);
    e.status = status;
    e
}

fn backlog(entries: Vec<BacklogEntry>) -> Backlog {
    let mut bl = Backlog::new();
    for e in entries {
        bl.apply(e);
    }
    bl
}

// ---------------------------------------------------------------- the idempotency key

#[test]
fn a_source_finds_the_work_it_queued() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    let bl = backlog(vec![entry(
        "bn",
        "fix the flaky queue tick under load",
        &s.queue_source(),
        BacklogKind::Ship,
        BacklogStatus::Pending,
    )]);
    let found = bl
        .active_for_source(&s.queue_source())
        .expect("the entry this suggestion produced");
    assert_eq!(found.repo, "bn");
}

/// The failure this prevents: a second accept queueing the same work again because the first one
/// crashed before caching the entry id on the suggestion. The lookup is by source precisely so the
/// cache is never load-bearing.
#[test]
fn the_lookup_does_not_depend_on_the_cached_link() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    assert!(s.queued_id.is_empty(), "nothing cached on it");
    let bl = backlog(vec![entry(
        "bn",
        "fix it",
        &s.queue_source(),
        BacklogKind::Ship,
        BacklogStatus::Pending,
    )]);
    assert!(bl.active_for_source(&s.queue_source()).is_some());
}

/// A resolved entry is not in flight, so the work is queueable again. Treating "an entry exists" as
/// "something is running" would strand a proposal whose entry was cancelled: nothing would ever run
/// it, and nothing would ever say so.
#[test]
fn a_resolved_entry_is_not_in_flight() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    for done in [BacklogStatus::Done, BacklogStatus::Cancelled] {
        let bl = backlog(vec![entry(
            "bn",
            "fix it",
            &s.queue_source(),
            BacklogKind::Ship,
            done,
        )]);
        assert!(
            bl.active_for_source(&s.queue_source()).is_none(),
            "{done:?} must not read as in flight"
        );
        assert!(
            bl.for_source(&s.queue_source()).is_some(),
            "{done:?} is still reportable"
        );
    }
}

/// One proposal owns several entries over its life — the pass that placed it, then the work itself.
/// `for_source` has to name the newest, or a report would show the placement pass forever.
#[test]
fn the_newest_entry_is_the_one_reported() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    let mut placement = entry(
        "machine",
        "resolve where t1 happens",
        &s.queue_source(),
        BacklogKind::Enhance,
        BacklogStatus::Done,
    );
    placement.updated_at = 1_000;
    let mut work = entry(
        "bn",
        "fix the flaky queue tick under load",
        &s.queue_source(),
        BacklogKind::Ship,
        BacklogStatus::Pending,
    );
    work.updated_at = 2_000;
    let bl = backlog(vec![placement, work]);
    let view = bn_core::suggestions::queue_view(&s, &bl).expect("a view");
    assert_eq!(view.kind, "ship", "the work, not the pass that placed it");
    assert!(view.live);
}

#[test]
fn a_suggestion_that_never_reached_the_queue_has_no_view() {
    let s = suggestion("577", SuggestionStatus::Suggested);
    assert!(bn_core::suggestions::queue_view(&s, &Backlog::new()).is_none());
}

// ---------------------------------------------------------------- the action-feed fold

fn fold(suggestions: &[Suggestion], bl: &Backlog) -> Vec<bn_core::action_center::ActionItem> {
    build_actions(&ActionSources {
        suggestions,
        backlog: Some(bl),
        now: 10_000,
        ..Default::default()
    })
    .into_iter()
    .filter(|a| a.kind == ActionKind::Suggestion)
    .collect()
}

/// The bug this closes: an accepted suggestion kept offering "Start agent" after accepting it had
/// already queued the work, so one click ran the same job twice.
#[test]
fn work_in_flight_is_reported_not_offered() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    let bl = backlog(vec![entry(
        "bn",
        "fix the flaky queue tick under load",
        &s.queue_source(),
        BacklogKind::Ship,
        BacklogStatus::Pending,
    )]);
    let rows = fold(std::slice::from_ref(&s), &bl);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].suggested_prompt.is_empty(),
        "an empty prompt is the contract for 'nothing to start here'"
    );
    let detail = rows[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("queued bn/"), "says where it is: {detail}");
    assert_eq!(rows[0].repo.as_deref(), Some("bn"));
}

#[test]
fn an_accepted_suggestion_with_nothing_queued_keeps_its_cta() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    let rows = fold(std::slice::from_ref(&s), &Backlog::new());
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].suggested_prompt.is_empty(),
        "nothing is running, so starting it is exactly the right offer"
    );
    // With no entry to describe, the row falls back to the rationale it was reviewed on.
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("surfaced by the pr-queue cron")
    );
}

/// A cancelled entry means nothing is coming. The CTA has to come back, or the proposal is stuck in
/// a state where the machine will not run it and the page will not let anyone else.
#[test]
fn a_cancelled_entry_restores_the_cta() {
    let s = suggestion("577", SuggestionStatus::Accepted);
    let bl = backlog(vec![entry(
        "bn",
        "fix it",
        &s.queue_source(),
        BacklogKind::Ship,
        BacklogStatus::Cancelled,
    )]);
    let rows = fold(std::slice::from_ref(&s), &bl);
    assert!(!rows[0].suggested_prompt.is_empty());
}

#[test]
fn only_accepted_suggestions_become_actions() {
    for status in [
        SuggestionStatus::Suggested,
        SuggestionStatus::Dismissed,
        SuggestionStatus::Done,
    ] {
        let s = suggestion("577", status);
        assert!(
            fold(std::slice::from_ref(&s), &Backlog::new()).is_empty(),
            "{status:?} must not become an action"
        );
    }
}

/// A placement pass in flight is still work in flight — the suggestion is being acted on, just not
/// yet by the agent that will change code. Offering to start it here would race the pass.
#[test]
fn a_placement_pass_counts_as_in_flight() {
    let s = suggestion("n1", SuggestionStatus::Accepted);
    let bl = backlog(vec![entry(
        "machine",
        "resolve where t5 happens",
        &s.queue_source(),
        BacklogKind::Enhance,
        BacklogStatus::Pending,
    )]);
    let rows = fold(std::slice::from_ref(&s), &bl);
    assert!(rows[0].suggested_prompt.is_empty());
    assert!(rows[0]
        .detail
        .as_deref()
        .unwrap_or_default()
        .contains("enhance"));
}
