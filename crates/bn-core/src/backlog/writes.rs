//! Every mutation of the store, as a file-level operation.
//!
//! Each function here is the same three steps: take the store's lock, fold the log, apply one
//! change from [`super::Backlog`]'s in-memory API, and append the resulting entry as a new line.
//! The append-only format is what makes that cheap — a change is one line, never a rewrite — and
//! the lock is what makes concurrent producers safe.
//!
//! Split from `backlog.rs` along a real seam: that file describes what the store *is* and how it
//! folds; everything here changes it.
use super::*;

/// Append `items` to the backlog for `repo`, deduped by `(repo, id)` against active entries and
/// within the batch. Returns the ids actually added. Holds the shared advisory lock across the
/// fold + append so a concurrent compaction can't drop the fresh lines.
pub fn add(
    path: &Path,
    repo: &str,
    items: &[String],
    spec: &AddSpec,
    now: i64,
) -> std::io::Result<Vec<String>> {
    let _lock = FileLock::acquire_required(path)?;
    let bl = load(path);
    let planned = bl.plan_add(repo, items, spec, now);
    let mut added = Vec::with_capacity(planned.len());
    for entry in &planned {
        append_line(path, entry)?;
        added.push(entry.id.clone());
    }
    Ok(added)
}

/// Lease up to `n` highest-priority pending entries for `repo` into a run: append a `leased` line
/// per pulled entry and return them. Locked read-modify-append so a lease never races a compaction
/// or a concurrent `add`.
pub fn lease(path: &Path, repo: &str, n: usize, now: i64) -> std::io::Result<Vec<BacklogEntry>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    let leased = bl.lease_top(repo, n, now);
    for entry in &leased {
        append_line(path, entry)?;
    }
    Ok(leased)
}

/// Reconcile a leased entry to a terminal state. Pending entries are rejected so a controller
/// cannot complete work it never claimed.
pub fn mark_leased(
    path: &Path,
    repo: &str,
    id: &str,
    status: BacklogStatus,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    if bl.get(repo, id).map(|entry| entry.status) != Some(BacklogStatus::Leased) {
        return Ok(false);
    }
    match bl.resolve(repo, id, status, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Return several leased entries to pending in one locked pass.
pub fn release_many(path: &Path, repo: &str, ids: &[&str], now: i64) -> std::io::Result<usize> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    let mut written = 0;
    for id in ids {
        if let Some(entry) = bl.release(repo, id, now) {
            append_line(path, &entry)?;
            written += 1;
        }
    }
    Ok(written)
}
