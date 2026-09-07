//! Every mutation of the store, as a file-level operation.
//!
//! Each function here is the same three steps: take the store's lock, fold the log, apply one
//! change from [`super::Backlog`]'s in-memory API, and append the resulting entry as a new line.
//! The append-only format is what makes that cheap — a change is one line, never a rewrite — and
//! the lock is what makes concurrent producers safe.
//!
//! Split from `backlog.rs` because that file sits on the 1000-line ratchet, whose rule is that a
//! file needing to grow is split rather than compressed. The seam is a real one: everything left
//! behind describes what the store *is* and how it folds; everything here changes it.
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
/// per pulled entry and return them. Locked read-modify-append so the daemon's lease never races a
/// compaction or a concurrent `add`.
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

/// Lease specific pending `ids` for `repo` in one locked pass (#459) — the disk wrapper over
/// [`Backlog::lease_ids`]. The autonomous selection policy ranks + gates the pending pool and hands
/// back the concrete `(repo, id)`s to start; this flips exactly those to `leased` (in the given
/// order) and appends their lines, skipping any that raced to non-pending. Returns the leased
/// entries so the daemon extends the run state with them.
pub fn lease_selected(
    path: &Path,
    repo: &str,
    ids: &[String],
    now: i64,
) -> std::io::Result<Vec<BacklogEntry>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    let leased = bl.lease_ids(repo, ids, now);
    for entry in &leased {
        append_line(path, entry)?;
    }
    Ok(leased)
}

/// Reconcile `(repo, id)` to a terminal `status` (done/failed), appending one line. A no-op — no
/// append — when the entry is absent or already terminal, so a re-reconcile every tick is idempotent
/// and never grows the file.
pub fn mark(
    path: &Path,
    repo: &str,
    id: &str,
    status: BacklogStatus,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.resolve(repo, id, status, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
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

/// Cancel a pending `(repo, id)` entry, appending one line — the disk wrapper over
/// [`Backlog::cancel`]. Returns whether a line was written; `false` means it was absent or not
/// pending (already running, already finished), which the caller should report rather than swallow.
pub fn cancel(path: &Path, repo: &str, id: &str, now: i64) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.cancel(repo, id, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Set an active `(repo, id)` entry's priority exactly, appending one line — the disk wrapper over
/// [`Backlog::set_priority`]. A no-op when the entry is absent, terminal, or already there.
pub fn set_priority(
    path: &Path,
    repo: &str,
    id: &str,
    priority: i64,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.set_priority(repo, id, priority, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Record a failed launch for a leased `(repo, id)` entry — the disk wrapper over
/// [`Backlog::fail_launch`]. `None` means the entry was absent or not leased.
///
/// `delay` is the policy, injected rather than imported so the retry schedule stays in
/// [`crate::queue::tick`] with the rest of the dispatcher's decisions:
/// [`crate::queue::tick::launch_retry_delay`] is the one the tick passes.
pub fn fail_launch(
    path: &Path,
    repo: &str,
    id: &str,
    now: i64,
    delay: fn(u32) -> Option<i64>,
) -> std::io::Result<Option<LaunchFailure>> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.fail_launch(repo, id, now, delay) {
        Some((entry, outcome)) => {
            append_line(path, &entry)?;
            Ok(Some(outcome))
        }
        None => Ok(None),
    }
}

/// Reset `(repo, id)`'s consecutive launch-failure count after a launch that worked — the disk
/// wrapper over [`Backlog::clear_launch_failures`]. Writes nothing when the count is already zero,
/// which is the common case.
pub fn clear_launch_failures(path: &Path, repo: &str, id: &str) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.clear_launch_failures(repo, id) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Snooze a pending `(repo, id)` entry until `not_before`.
pub fn start_next(path: &Path, repo: &str, id: &str, now: i64) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.start_next(repo, id, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub fn snooze(
    path: &Path,
    repo: &str,
    id: &str,
    not_before: i64,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.snooze(repo, id, not_before, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Set or clear a pending `(repo, id)` entry's class override.
pub fn set_class_override(
    path: &Path,
    repo: &str,
    id: &str,
    class_override: Option<ClassOverride>,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.set_class_override(repo, id, class_override, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Extend an active `(repo, id)` entry's deadline, appending one line — the disk wrapper over
/// [`Backlog::set_deadline`]. A no-op (no append) when the deadline is not strictly later.
pub fn set_deadline(
    path: &Path,
    repo: &str,
    id: &str,
    not_after: i64,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.set_deadline(repo, id, not_after, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Reprioritize an active `(repo, id)` backlog entry to `priority`, appending one line — the disk
/// wrapper over [`Backlog::reprioritize`]. A no-op (no append) when the entry is absent, terminal,
/// or the new priority isn't a strict raise. Returns whether a line was written.
pub fn reprioritize(
    path: &Path,
    repo: &str,
    id: &str,
    priority: i64,
    now: i64,
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    match bl.reprioritize(repo, id, priority, now) {
        Some(entry) => {
            append_line(path, &entry)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Reconcile several `(id, status)` terminal outcomes for `repo` in one locked pass — the daemon's
/// per-tick reconcile of the run ledger back onto the backlog. Only entries that are still active
/// and actually change state are appended (a repeated outcome, an already-terminal entry, or an id
/// absent from the backlog is skipped), so a steady state writes nothing. Returns lines written.
pub fn resolve_many(
    path: &Path,
    repo: &str,
    outcomes: &[(&str, BacklogStatus)],
    now: i64,
) -> std::io::Result<usize> {
    let _lock = FileLock::acquire_required(path)?;
    let mut bl = load(path);
    let mut written = 0;
    for (id, status) in outcomes {
        if let Some(entry) = bl.resolve(repo, id, *status, now) {
            append_line(path, &entry)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Return several leased entries to pending in one locked pass after their run-state handoff fails.
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
