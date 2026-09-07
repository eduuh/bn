//! `backlog::latest` — one entry, read without folding the whole append-only store.
//!
//! Out of line with the rest of the store's tests because `backlog.rs` is at the 1000-line ratchet:
//! the repo's rule is that a file which needs to grow gets split, never compressed, and a test is
//! the half that moves most cleanly.
fn write(path: &std::path::Path, lines: &[&str]) {
    std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("write the log");
}

fn line(repo: &str, id: &str, status: &str, priority: i64) -> String {
    format!(
        r#"{{"id":"{id}","repo":"{repo}","item":"x","kind":"cron","priority":{priority},"added_at":1,"source":"queue","status":"{status}","updated_at":1}}"#
    )
}

/// The property the picker's side pane depends on: one entry, read without folding the store.
#[test]
fn latest_agrees_with_the_folded_view() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("backlog.jsonl");
    write(
        &path,
        &[
            &line("machine", "a", "pending", 1),
            &line("machine", "b", "pending", 2),
            &line("machine", "a", "leased", 1),
        ],
    );
    // `bn`, not `machine`: a legacy row naming the retired sentinel normalizes to the repo it
    // serves on read, and `latest` has to apply the same rule `load` does or the two disagree
    // about whether an entry exists at all.
    let folded = bn_core::backlog::load(&path);
    let one = bn_core::backlog::latest(&path, "bn", "a").expect("entry a");
    assert_eq!(one.status, folded.get("bn", "a").unwrap().status);
    assert_eq!(
        one.status,
        bn_core::backlog::BacklogStatus::Leased,
        "the last line wins"
    );
}

/// A key that has been superseded by a terminal line reports the terminal state, not the first
/// one — the case that decides whether the pane offers "cancel" or "queue it".
#[test]
fn a_cancelled_entry_reads_as_cancelled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("backlog.jsonl");
    write(
        &path,
        &[
            &line("machine", "a", "pending", 1),
            &line("machine", "a", "cancelled", 1),
        ],
    );
    let one = bn_core::backlog::latest(&path, "bn", "a").expect("entry a");
    assert!(!one.status.is_active());
}

/// A key that is not there is absent, not the nearest match — the substring pre-filter must not
/// let a different id through.
#[test]
fn an_absent_key_is_none_even_when_a_similar_one_exists() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("backlog.jsonl");
    write(&path, &[&line("machine", "alpha", "pending", 1)]);
    assert!(bn_core::backlog::latest(&path, "bn", "alph").is_none());
    assert!(bn_core::backlog::latest(&path, "other", "alpha").is_none());
    assert!(bn_core::backlog::latest(&path, "bn", "alpha").is_some());
}

/// A missing store is empty, not an error — the picker paints before anything has been queued.
#[test]
fn a_missing_store_has_no_entries() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(bn_core::backlog::latest(&dir.path().join("nope.jsonl"), "bn", "a").is_none());
}

/// The rule `load` applies on every row, applied here too. A machine upgrading from before #1317
/// still has `repo = "machine"` lines on disk; if `latest` screened on the caller's repo before
/// normalizing, the picker would report "not queued" for entries the dispatcher can plainly see.
#[test]
fn a_legacy_machine_row_is_found_under_the_repo_it_serves() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("backlog.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            r#"{"id":"warm","repo":"machine","item":"x","kind":"cron","priority":0,"added_at":1,"source":"queue","status":"pending","updated_at":1,"exec":{"run":"job","job":"pool-warm","repo":"arena"}}"#
        ),
    )
    .expect("write the log");

    assert!(
        bn_core::backlog::latest(&path, "arena", "warm").is_some(),
        "the row serves arena, so that is where it lives"
    );
    assert!(
        bn_core::backlog::latest(&path, "machine", "warm").is_none(),
        "and the retired sentinel is not a repo any more"
    );
    assert_eq!(
        bn_core::backlog::load(&path).get("arena", "warm").is_some(),
        bn_core::backlog::latest(&path, "arena", "warm").is_some(),
        "latest and load must agree about whether an entry exists"
    );
}
