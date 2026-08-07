//! The reservoir fold — open tasks grouped by distance from executable.
//!
//! This is the half of the queue `/api/queue` could not see. Since #1001 work is minted as a task
//! and only becomes a backlog entry once it has passed the SMART gate, so a payload built from the
//! backlog alone reported `0 pending` on a machine with thousands of tasks waiting. These tests pin
//! the classification, because the whole value of the section is that its four numbers mean four
//! different things: waiting on the improve agent, waiting on capacity, waiting on a retry, and
//! waiting on a person.
use bn_core::payload::queue::reservoir_of;
use bn_core::todo::TodoFile;

const TODAY: &str = "2026-08-07";

fn counts(input: &str) -> (u64, u64, u64, u64, u64) {
    let file = TodoFile::parse(input);
    let v = reservoir_of(&file, TODAY);
    let n = |k: &str| v[k].as_u64().unwrap_or_default();
    (
        n("needs_scoping"),
        n("ready"),
        n("failed"),
        n("stuck"),
        n("total"),
    )
}

#[test]
fn an_unjudged_task_needs_scoping() {
    // Where every pulled task starts: no `smart:` at all. This is the state that dominates a
    // healthy reservoir, and the one the page most needed to be able to name.
    let (needs, ready, failed, stuck, total) = counts("t1 fix the login bug repo:bn\n");
    assert_eq!((needs, ready, failed, stuck), (1, 0, 0, 0));
    assert_eq!(total, 1);
}

#[test]
fn each_verdict_lands_in_its_own_bucket() {
    // The four states are four different problems. Collapsing any two of them back into one
    // "waiting" number is exactly the loss of resolution this section exists to undo.
    let (needs, ready, failed, stuck, total) = counts(concat!(
        "t1 unjudged repo:bn\n",
        "t2 gated repo:bn smart:ok\n",
        "t3 retrying repo:bn smart:fail\n",
        "t4 needs a human repo:bn smart:stuck\n",
    ));
    assert_eq!((needs, ready, failed, stuck), (1, 1, 1, 1));
    assert_eq!(total, 4);
}

#[test]
fn a_finished_task_is_not_waiting() {
    // Done work is not reservoir depth. Counting it would make the number grow forever and mean
    // nothing — which is precisely how the store this was built against ended up at 2410.
    let (needs, ready, failed, stuck, total) = counts(concat!(
        "x 2026-08-06 shipped it repo:bn\n",
        "t2 still open repo:bn\n",
    ));
    assert_eq!((needs, ready, failed, stuck), (1, 0, 0, 0));
    assert_eq!(total, 1);
}

#[test]
fn an_unrecognised_verdict_reads_as_unjudged() {
    // A `smart:` value the gate never writes must not silently vanish from the totals. Falling
    // back to "needs scoping" keeps it visible and keeps `total` equal to the open count, which is
    // the invariant the header's "N waiting" is read off.
    let (needs, ready, failed, stuck, total) = counts("t1 odd repo:bn smart:banana\n");
    assert_eq!((needs, ready, failed, stuck), (1, 0, 0, 0));
    assert_eq!(total, 1);
}

#[test]
fn the_total_is_always_the_sum_of_the_buckets() {
    // The header reports `total` while the tiles report the parts; if they can disagree, one of the
    // two is lying and there is no way to tell which from the page.
    let file = TodoFile::parse(concat!(
        "t1 a repo:bn\n",
        "t2 b repo:bn smart:ok\n",
        "t3 c repo:bn smart:fail\n",
        "t4 d repo:bn smart:stuck\n",
        "x 2026-08-06 e repo:bn\n",
    ));
    let v = reservoir_of(&file, TODAY);
    let n = |k: &str| v[k].as_u64().unwrap_or_default();
    assert_eq!(
        n("total"),
        n("needs_scoping") + n("ready") + n("failed") + n("stuck")
    );
}

#[test]
fn an_empty_store_is_an_empty_reservoir_not_a_missing_one() {
    // Zeroes, not null: null is reserved for "could not read the store", and the UI renders the
    // two differently on purpose.
    let (needs, ready, failed, stuck, total) = counts("");
    assert_eq!((needs, ready, failed, stuck, total), (0, 0, 0, 0, 0));
}
