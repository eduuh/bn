//! What happens when a scheduled job fires again while its last firing is still waiting.
//!
//! Every cron in the queue eventually hits this: the machine is busy, the 10:00 instance has not
//! started, and 11:00 comes round. The queue has always deduped by id, so there was never a
//! *duplicate* problem — but deduping is not a policy, and the two kinds of scheduled work want
//! opposite answers. Refresh work (warm a pool, sync a cache) wants the newest firing to keep the
//! entry alive; time-critical work (a 09:00 digest) wants the stale one to lapse rather than run at
//! the wrong hour.
use bn_core::backlog::{self, AddSpec, BacklogKind, BacklogStatus, ExecSpec, Recurrence};
use bn_core::queue::enqueue::{enqueue, Enqueued};

const T: i64 = 1_000_000;
const HOUR: i64 = 3600;

/// A backlog file of this test's own.
///
/// Named per test rather than per thread: the harness runs tests in parallel and reuses threads, so
/// a thread-keyed path lets two tests share a file — each truncating the other's backlog mid-run.
/// That is a flaky suite, which is worse than no suite, because it teaches you to ignore it.
fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bn-recurrence-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir.join("backlog.jsonl")
}

fn cron(not_after: i64, recurrence: Recurrence) -> AddSpec {
    AddSpec {
        kind: BacklogKind::Cron,
        exec: ExecSpec::Job {
            job: "digest".to_string(),
            repo: None,
            branch: None,
        },
        not_after: Some(not_after),
        recurrence,
        ..Default::default()
    }
}

fn pending_entry(path: &std::path::Path) -> bn_core::backlog::BacklogEntry {
    let bl = backlog::load(path);
    let active: Vec<_> = bl
        .entries()
        .iter()
        .filter(|e| e.status == BacklogStatus::Pending)
        .cloned()
        .collect();
    assert_eq!(active.len(), 1, "recurrence must not duplicate the entry");
    active[0].clone()
}

#[test]
fn a_recurrence_does_not_duplicate_the_pending_instance() {
    let path = tmp("a_recurrence_does_not_duplicate_the_pending_instance");
    enqueue(
        &path,
        "repo",
        "run digest",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let second = enqueue(
        &path,
        "repo",
        "run digest",
        &cron(T + 2 * HOUR, Recurrence::Coalesce),
        T + HOUR,
    )
    .unwrap();
    assert!(matches!(
        second,
        Enqueued::Existing(_) | Enqueued::Reprioritized(_)
    ));
    pending_entry(&path); // asserts exactly one
}

#[test]
fn coalescing_carries_the_newest_deadline_forward() {
    // The bug this policy exists to name. `not_after` for a cron is its own next firing, so an
    // entry held past it is dropped as "its window passed". Without refreshing, the pending entry
    // keeps the FIRST firing's deadline while every later firing folds silently into it — so a
    // machine busy for one interval stops running that job entirely, and reports the drop as if the
    // work had simply arrived too late. Refresh work would rather run late than not at all.
    let path = tmp("coalescing_carries_the_newest_deadline_forward");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + 2 * HOUR, Recurrence::Coalesce),
        T + HOUR,
    )
    .unwrap();
    let e = pending_entry(&path);
    assert_eq!(
        e.not_after,
        Some(T + 2 * HOUR),
        "the live deadline is the newest firing's, not the one that has already passed"
    );
}

#[test]
fn coalescing_never_pulls_a_deadline_in() {
    // Only ever extended. A firing that somehow carries an earlier deadline must not shorten the
    // window of work already waiting.
    let path = tmp("coalescing_never_pulls_a_deadline_in");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + 2 * HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T + 60,
    )
    .unwrap();
    assert_eq!(pending_entry(&path).not_after, Some(T + 2 * HOUR));
}

#[test]
fn skip_leaves_the_waiting_instance_exactly_as_it_was() {
    // The other half: for work whose value is tied to its hour, a later firing must not keep the
    // stale one alive. It lapses, and tomorrow's runs on time.
    let path = tmp("skip_leaves_the_waiting_instance_exactly_as_it_was");
    enqueue(
        &path,
        "repo",
        "run digest",
        &cron(T + HOUR, Recurrence::Skip),
        T,
    )
    .unwrap();
    let second = enqueue(
        &path,
        "repo",
        "run digest",
        &cron(T + 2 * HOUR, Recurrence::Skip),
        T + HOUR,
    )
    .unwrap();
    assert!(matches!(second, Enqueued::Existing(_)));
    assert_eq!(
        pending_entry(&path).not_after,
        Some(T + HOUR),
        "skip keeps the original deadline so a late instance lapses"
    );
}

#[test]
fn a_resolved_instance_does_not_block_the_next_firing() {
    // Dedupe applies to ACTIVE entries only — once an instance is done, the next firing is a fresh
    // one. Otherwise a job would run exactly once, ever.
    let path = tmp("a_resolved_instance_does_not_block_the_next_firing");
    let first = enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = match &first {
        Enqueued::Created(e) => e.id.clone(),
        other => panic!("expected Created, got {other:?}"),
    };
    backlog::resolve_many(&path, "repo", &[(id.as_str(), BacklogStatus::Done)], T + 60).unwrap();
    let second = enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + 2 * HOUR, Recurrence::Coalesce),
        T + HOUR,
    )
    .unwrap();
    assert!(
        matches!(second, Enqueued::Created(_)),
        "a finished instance must not suppress the next firing"
    );
}

#[test]
fn a_higher_priority_firing_still_raises_the_waiting_one() {
    // Pre-existing behaviour, kept: an urgent re-request of queued work promotes it rather than
    // silently doing nothing.
    let path = tmp("a_higher_priority_firing_still_raises_the_waiting_one");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let spec = AddSpec {
        priority: 90,
        ..cron(T + HOUR, Recurrence::Coalesce)
    };
    let second = enqueue(&path, "repo", "run warm", &spec, T + 60).unwrap();
    assert!(matches!(second, Enqueued::Reprioritized(_)));
    assert_eq!(pending_entry(&path).priority, 90);
}

// ---------------------------------------------------------------------------- taking work back out

/// Cancelling is the verb the queue was missing entirely: `BacklogStatus` had no terminal state for
/// "a person decided this should not run", so the only way to unqueue something was to let it run or
/// hand-edit the backlog file.
#[test]
fn pending_work_can_be_cancelled() {
    let path = tmp("pending_work_can_be_cancelled");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = bn_core::orchestrate::derive_id("run warm");
    assert!(backlog::cancel(&path, "repo", &id, T + 10).unwrap());

    let bl = backlog::load(&path);
    let e = bl.entries().iter().find(|e| e.id == id).unwrap();
    assert_eq!(e.status, BacklogStatus::Cancelled);
    assert!(e.status.is_terminal(), "cancelled is terminal");
    assert!(!e.status.is_active(), "a cancelled entry is not live work");
}

#[test]
fn cancelling_is_distinct_from_failing() {
    // Collapsing the two would make "why did this never run" unanswerable for the case where the
    // answer is "you said so".
    assert_ne!(BacklogStatus::Cancelled, BacklogStatus::Failed);
    assert_eq!(BacklogStatus::Cancelled.label(), "cancelled");
}

#[test]
fn a_cancelled_entry_frees_its_id_for_the_next_firing() {
    // Dedupe keys on ACTIVE entries, so cancelling must not permanently block the work from being
    // queued again — otherwise cancelling one instance would silently retire the job.
    let path = tmp("a_cancelled_entry_frees_its_id_for_the_next_firing");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = bn_core::orchestrate::derive_id("run warm");
    backlog::cancel(&path, "repo", &id, T + 10).unwrap();
    let again = enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + 2 * HOUR, Recurrence::Coalesce),
        T + 20,
    )
    .unwrap();
    assert!(matches!(again, Enqueued::Created(_)));
}

#[test]
fn running_work_cannot_be_cancelled_out_from_under_its_executor() {
    // A leased entry has an agent in a worktree. Flipping its row would stop the queue describing
    // something that is still happening, which is worse than not offering the verb.
    let path = tmp("running_work_cannot_be_cancelled_out_from_under_its_executor");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = bn_core::orchestrate::derive_id("run warm");
    backlog::lease_selected(&path, "repo", std::slice::from_ref(&id), T + 5).unwrap();
    assert!(
        !backlog::cancel(&path, "repo", &id, T + 10).unwrap(),
        "a leased entry is refused, and the caller reports why"
    );
    let bl = backlog::load(&path);
    assert_eq!(
        bl.entries().iter().find(|e| e.id == id).unwrap().status,
        BacklogStatus::Leased
    );
}

#[test]
fn priority_moves_in_both_directions() {
    // `reprioritize` only ever raises, because a producer re-declaring work means "sooner" and a
    // lower number means "no change". A person moving something down is saying the opposite.
    let path = tmp("priority_moves_in_both_directions");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = bn_core::orchestrate::derive_id("run warm");

    assert!(backlog::set_priority(&path, "repo", &id, 90, T + 1).unwrap());
    assert_eq!(pending_entry(&path).priority, 90);
    assert!(backlog::set_priority(&path, "repo", &id, -20, T + 2).unwrap());
    assert_eq!(pending_entry(&path).priority, -20);
    // Idempotent: setting it to what it already is writes nothing.
    assert!(!backlog::set_priority(&path, "repo", &id, -20, T + 3).unwrap());
}

#[test]
fn a_terminal_entry_cannot_be_reprioritized() {
    let path = tmp("a_terminal_entry_cannot_be_reprioritized");
    enqueue(
        &path,
        "repo",
        "run warm",
        &cron(T + HOUR, Recurrence::Coalesce),
        T,
    )
    .unwrap();
    let id = bn_core::orchestrate::derive_id("run warm");
    backlog::cancel(&path, "repo", &id, T + 5).unwrap();
    assert!(!backlog::set_priority(&path, "repo", &id, 50, T + 10).unwrap());
}
