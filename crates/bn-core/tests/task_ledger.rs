use bn_core::payload::tasks::{task_ledger_data, TaskLedgerFilter};
use bn_core::todo::TodoFile;

const TODAY: &str = "2026-08-07";

#[test]
fn ledger_counts_every_lifecycle_without_purging_history() {
    let file = TodoFile::parse(concat!(
        "open work id:t1 repo:bn mod:10\n",
        "active work @wip id:t2 repo:bn mod:20\n",
        "later work snooze:2026-08-09 id:t3 repo:bn mod:30\n",
        "x 2026-08-06 shipped work id:t4 repo:bn mod:40\n",
        "x 2026-08-06 abandoned work cancelled:1 id:t5 repo:bn mod:50\n",
    ));

    let value = task_ledger_data(
        &file,
        TODAY,
        TaskLedgerFilter {
            limit: 50,
            ..TaskLedgerFilter::default()
        },
    );

    assert_eq!(value["total"], 5);
    assert_eq!(value["counts"]["open"], 1);
    assert_eq!(value["counts"]["wip"], 1);
    assert_eq!(value["counts"]["snoozed"], 1);
    assert_eq!(value["counts"]["done"], 1);
    assert_eq!(value["counts"]["cancelled"], 1);
}

#[test]
fn ledger_searches_filters_and_pages_newest_first() {
    let file = TodoFile::parse(concat!(
        "first matching task id:t1 repo:bn mod:10\n",
        "other repo task id:t2 repo:other mod:30\n",
        "second matching task id:t3 repo:bn mod:20\n",
    ));

    let value = task_ledger_data(
        &file,
        TODAY,
        TaskLedgerFilter {
            query: Some("matching"),
            repo: Some("BN"),
            offset: 1,
            limit: 1,
            ..TaskLedgerFilter::default()
        },
    );

    assert_eq!(value["total"], 2);
    assert_eq!(value["count"], 1);
    assert_eq!(value["tasks"][0]["id"], "t1");
}

#[test]
fn ledger_state_filter_keeps_cross_state_counts() {
    let file = TodoFile::parse(concat!(
        "open matching id:t1 repo:bn mod:10\n",
        "x 2026-08-06 done matching id:t2 repo:bn mod:20\n",
    ));

    let value = task_ledger_data(
        &file,
        TODAY,
        TaskLedgerFilter {
            query: Some("matching"),
            state: Some("done"),
            limit: 50,
            ..TaskLedgerFilter::default()
        },
    );

    assert_eq!(value["total"], 1);
    assert_eq!(value["counts"]["open"], 1);
    assert_eq!(value["counts"]["done"], 1);
    assert_eq!(value["tasks"][0]["status"], "done");
}
