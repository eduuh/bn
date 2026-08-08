//! `bn orchestrate` durable, prioritized backlog (#375).
//!
//! Splits *what to work on* from *what's running*. The live
//! [`crate::orchestrate::OrchestrateState`] is a small, per-repo **execution ledger** a run drains
//! and then terminates; this module is the durable, prioritized, add-anytime **backlog** that
//! survives run start/stop and reboots and feeds every run. `bn orchestrate add` appends here at
//! any time (run active or not); the daemon's tick pulls the highest-priority pending entries into
//! the run state as concurrency slots free — no new IPC, because the daemon already reloads state
//! from disk every poll (#368/#348 consume this store, #343 keys it by `(repo, id)`).
//!
//! On disk it is one append-only `backlog.jsonl` in [`crate::resolver::state_dir`]: one JSON object
//! per line. Appends are cheap — a run's `add` never rewrites the file. Reads fold the log into the
//! current per-`(repo, id)` view (the **latest** line for a key wins, log-structured); periodic
//! [`compact`] rewrites the file dropping terminal entries and collapsing superseded lines so it
//! stays bounded (deferred SQLite territory begins ~1e5 items). The ordering / dedup / lease
//! *policy* is pure and unit-tested on the in-memory [`Backlog`]; the CLI only does the file I/O.

use crate::orchestrate::derive_id;
use crate::store::FileLock;
use std::cmp::Reverse;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// The work-kind taxonomy, plus the executor and requirement axes that travel with it, now live in
/// [`crate::queue::kind`] — re-exported here so every existing `backlog::BacklogKind` path (and the
/// serde shape of a stored line) is unchanged.
pub use crate::queue::kind::{BacklogKind, ExecSpec, Requirements};

pub mod entry;
pub use entry::*;

impl Backlog {
    /// Live entries that name no task — the pre-#1001 grandfathers.
    ///
    /// Reported by `bn doctor` rather than deleted: an in-flight run must not be orphaned by an
    /// upgrade. Once this is empty the invariant holds for the whole store, and a caller can say
    /// so honestly rather than assuming.
    pub fn untasked(&self) -> Vec<&BacklogEntry> {
        self.entries
            .iter()
            .filter(|e| e.status.is_active())
            .filter(|e| e.task.trim().is_empty())
            .collect()
    }
}

/// The in-memory current view of the backlog: the folded, latest-per-`(repo, id)` set of entries in
/// first-seen order. Every ordering / dedup / lease decision is a pure method here — the file I/O
/// ([`load`], [`add`], [`lease`], [`mark`], [`compact`]) is a thin wrapper that folds the log in and
/// appends the decision back out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Backlog {
    /// Lines the fold could not parse — entries that are, in effect, lost. Surfaced rather than
    /// swallowed; see [`Backlog::from_lines`].
    pub unreadable: usize,
    entries: Vec<BacklogEntry>,
}

impl Backlog {
    /// An empty backlog.
    pub fn new() -> Backlog {
        Backlog {
            unreadable: 0,
            entries: Vec::new(),
        }
    }

    /// Fold a log (one JSON entry per line) into the current view: later lines for a `(repo, id)`
    /// **supersede** earlier ones, keeping the key's first-seen position. Unparseable lines are
    /// skipped so a torn append never poisons the read.
    pub fn from_lines(lines: impl IntoIterator<Item = String>) -> Backlog {
        let mut bl = Backlog::new();
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<BacklogEntry>(line) {
                Ok(entry) => bl.apply(entry),
                // A line we cannot read is an entry that has vanished — a torn append, a
                // hand-edit, or a newer schema under an older binary. Skipping is right (one bad
                // line must not take the store down) but skipping *silently* means work disappears
                // with no signal at all, which for a queue is the failure that matters. Counted
                // here so `bn queue status` can say so.
                Err(_) => bl.unreadable += 1,
            }
        }
        bl
    }

    /// Fold one entry into the view — supersede an existing `(repo, id)` in place, else append. The
    /// building block of [`from_lines`] and the in-memory mutations.
    pub fn apply(&mut self, entry: BacklogEntry) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.repo == entry.repo && e.id == entry.id)
        {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    /// The folded entries in first-seen order.
    pub fn entries(&self) -> &[BacklogEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The current entry for `(repo, id)`, if any.
    pub fn get(&self, repo: &str, id: &str) -> Option<&BacklogEntry> {
        self.entries.iter().find(|e| e.repo == repo && e.id == id)
    }

    /// Whether `(repo, id)` is already **active** (pending or leased) — the dedup predicate.
    pub fn is_active(&self, repo: &str, id: &str) -> bool {
        self.get(repo, id).is_some_and(|e| e.status.is_active())
    }

    /// The pending entries for `repo`, highest priority first. Ordering is the pure pull policy:
    /// priority descending, then `added_at` ascending (FIFO within a priority band), then `id`
    /// ascending for a total, deterministic order.
    pub fn pending_for(&self, repo: &str) -> Vec<&BacklogEntry> {
        let mut pending: Vec<&BacklogEntry> = self
            .entries
            .iter()
            .filter(|e| e.repo == repo && e.status == BacklogStatus::Pending)
            .collect();
        pending.sort_by(|a, b| {
            Reverse(a.priority)
                .cmp(&Reverse(b.priority))
                .then(a.added_at.cmp(&b.added_at))
                .then(a.id.cmp(&b.id))
        });
        pending
    }

    /// The next `n` pending entries for `repo` to pull, in priority order (clones for ownership).
    pub fn top_pending(&self, repo: &str, n: usize) -> Vec<BacklogEntry> {
        self.pending_for(repo)
            .into_iter()
            .take(n)
            .cloned()
            .collect()
    }

    /// How many pending entries `repo` has waiting — the "backlog depth" the status surface shows.
    pub fn pending_depth(&self, repo: &str) -> usize {
        self.entries
            .iter()
            .filter(|e| e.repo == repo && e.status == BacklogStatus::Pending)
            .count()
    }

    /// Total pending entries across every repo.
    pub fn total_pending(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Pending)
            .count()
    }

    /// A read view of `repo`'s backlog (#373): entries whose status matches `status_filter`
    /// (`None` = every status), ordered by the same pull policy as [`pending_for`] — priority
    /// descending, then `added_at` ascending (FIFO within a band), then `id`. The read counterpart
    /// to the lease path, generalized over status so `bn orchestrate backlog` can also surface
    /// leased / terminal entries, not just pending ones.
    pub fn list_view(
        &self,
        repo: &str,
        status_filter: Option<BacklogStatus>,
    ) -> Vec<&BacklogEntry> {
        let mut view: Vec<&BacklogEntry> = self
            .entries
            .iter()
            .filter(|e| e.repo == repo && status_filter.is_none_or(|s| e.status == s))
            .collect();
        view.sort_by(|a, b| {
            Reverse(a.priority)
                .cmp(&Reverse(b.priority))
                .then(a.added_at.cmp(&b.added_at))
                .then(a.id.cmp(&b.id))
        });
        view
    }

    /// Every repo present in the backlog (any status), sorted and deduped — the machine-wide
    /// `--all-repos` list view (#373). Distinct from [`repos_with_pending`], which drops repos
    /// whose only entries are leased or terminal.
    pub fn repos(&self) -> Vec<String> {
        let mut repos: Vec<String> = self.entries.iter().map(|e| e.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        repos
    }

    /// Every repo that has at least one pending entry, sorted — for a machine-wide status view.
    pub fn repos_with_pending(&self) -> Vec<String> {
        let mut repos: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Pending)
            .map(|e| e.repo.clone())
            .collect();
        repos.sort();
        repos.dedup();
        repos
    }

    /// Plan an `add`: the fresh pending entries to append for `items` in `repo`, deduped by
    /// `(repo, id)` against active entries **and** within the batch itself. Pure — the caller
    /// ([`add`]) is the only thing that touches the file.
    pub fn plan_add(
        &self,
        repo: &str,
        items: &[String],
        spec: &AddSpec,
        now: i64,
    ) -> Vec<BacklogEntry> {
        let mut planned: Vec<BacklogEntry> = Vec::new();
        for item in items {
            let id = derive_id(item);
            // Skip a subject already active in the store, or already staged earlier in this batch.
            if self.is_active(repo, &id) || planned.iter().any(|e| e.id == id) {
                continue;
            }
            planned.push(BacklogEntry::new(repo, item, spec, now));
        }
        planned
    }

    /// Lease the top `n` pending entries for `repo`: flip them to `leased` in the view and return
    /// them (the daemon extends the run state with their `item`s). Pure — the disk [`lease`] appends
    /// the returned leased lines.
    pub fn lease_top(&mut self, repo: &str, n: usize, now: i64) -> Vec<BacklogEntry> {
        let ids: Vec<String> = self
            .top_pending(repo, n)
            .into_iter()
            .map(|e| e.id)
            .collect();
        let mut leased = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(e) = self
                .entries
                .iter_mut()
                .find(|e| e.repo == repo && e.id == id)
            {
                e.status = BacklogStatus::Leased;
                e.updated_at = now;
                leased.push(e.clone());
            }
        }
        leased
    }

    /// Lease specific pending `ids` for `repo`, in the given order (the #459 selection policy picks
    /// *which* candidates to start, not merely a priority prefix). Each id that is currently pending
    /// flips to `leased` and is returned; a non-pending or unknown id is skipped. Pure — the disk
    /// [`lease_selected`] appends the returned leased lines.
    pub fn lease_ids(&mut self, repo: &str, ids: &[String], now: i64) -> Vec<BacklogEntry> {
        let mut leased = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(e) = self
                .entries
                .iter_mut()
                .find(|e| e.repo == repo && &e.id == id && e.status == BacklogStatus::Pending)
            {
                e.status = BacklogStatus::Leased;
                e.updated_at = now;
                leased.push(e.clone());
            }
        }
        leased
    }

    /// Reconcile `(repo, id)` to a terminal `status` (done/failed) if it is currently active and not
    /// already in that state. Returns the updated entry to append, or `None` when it's a no-op (the
    /// entry is absent or already terminal) — so a re-reconcile every tick appends nothing.
    pub fn resolve(
        &mut self,
        repo: &str,
        id: &str,
        status: BacklogStatus,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if e.status == status || e.status.is_terminal() {
            return None;
        }
        e.status = status;
        e.updated_at = now;
        Some(e.clone())
    }

    /// Return a leased entry to pending so a failed handoff into a run can be retried.
    pub fn release(&mut self, repo: &str, id: &str, now: i64) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if e.status != BacklogStatus::Leased {
            return None;
        }
        e.status = BacklogStatus::Pending;
        e.updated_at = now;
        Some(e.clone())
    }

    /// Reprioritize an already-active `(repo, id)` entry to `priority` (the #348 attn-promotion
    /// "reprioritize, don't duplicate" path). Only touches an entry that is currently active
    /// (pending or leased) and only when `priority` is a strict *raise* over its current band — so
    /// a re-flagged attention item bumps its queued work up instead of stacking a duplicate, but a
    /// no-op (equal/lower band, absent, or terminal entry) appends nothing. Returns the updated
    /// entry to append, or `None`.
    /// Carry a later deadline onto an active entry — the coalescing half of
    /// [`crate::backlog::Recurrence`]. Extends only: `None`, a terminal entry, or a deadline that
    /// is not strictly later is a no-op, so a steady state writes nothing.
    pub fn set_deadline(
        &mut self,
        repo: &str,
        id: &str,
        not_after: i64,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if !e.status.is_active() || e.not_after.is_none_or(|cur| not_after <= cur) {
            return None;
        }
        e.not_after = Some(not_after);
        e.updated_at = now;
        Some(e.clone())
    }

    /// Take an entry back out of the queue.
    ///
    /// **Pending only.** A `Leased` entry has an executor — an agent in a worktree, a job mid-run —
    /// and flipping its row to cancelled would not stop any of that; it would only make the queue
    /// stop describing something that is still happening, which is worse than not offering the
    /// verb. Stopping live work is `bn agent kill`, and the reaper returns the lease afterwards.
    ///
    /// `None` when the entry is absent or not pending, so the caller can say which.
    pub fn cancel(&mut self, repo: &str, id: &str, now: i64) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if e.status != BacklogStatus::Pending {
            return None;
        }
        e.status = BacklogStatus::Cancelled;
        e.updated_at = now;
        Some(e.clone())
    }

    /// Set an active entry's priority to exactly `priority` — up **or** down.
    ///
    /// Distinct from [`Backlog::reprioritize`], which only ever raises: that one exists for a
    /// producer re-declaring work it wants sooner, where a lower number means "no change", not
    /// "demote". A person moving something down the queue is saying the opposite, and needs a verb
    /// that believes them.
    pub fn set_priority(
        &mut self,
        repo: &str,
        id: &str,
        priority: i64,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if !e.status.is_active() || e.priority == priority {
            return None;
        }
        e.priority = priority;
        e.updated_at = now;
        Some(e.clone())
    }

    /// Snooze a pending entry until `not_before`.
    pub fn snooze(
        &mut self,
        repo: &str,
        id: &str,
        not_before: i64,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if e.status != BacklogStatus::Pending || e.not_before == Some(not_before) {
            return None;
        }
        e.not_before = Some(not_before);
        e.updated_at = now;
        Some(e.clone())
    }

    /// Override a pending entry's derived queue class.
    pub fn set_class_override(
        &mut self,
        repo: &str,
        id: &str,
        class_override: Option<ClassOverride>,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if e.status != BacklogStatus::Pending || e.class_override == class_override {
            return None;
        }
        e.class_override = class_override;
        e.updated_at = now;
        Some(e.clone())
    }

    pub fn reprioritize(
        &mut self,
        repo: &str,
        id: &str,
        priority: i64,
        now: i64,
    ) -> Option<BacklogEntry> {
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.repo == repo && e.id == id)?;
        if !e.status.is_active() || priority <= e.priority {
            return None;
        }
        e.priority = priority;
        e.updated_at = now;
        Some(e.clone())
    }

    /// The compacted set: drop terminal entries, keeping the live (pending/leased) ones in order.
    /// What [`compact`] rewrites the file to.
    pub fn compacted(&self) -> Vec<BacklogEntry> {
        self.entries
            .iter()
            .filter(|e| !e.status.is_terminal())
            .cloned()
            .collect()
    }
}

// ===========================================================================
// On-disk `backlog.jsonl` — append-friendly, folded on read, compacted periodically
// ===========================================================================

/// Rewrite the file once it carries this many total lines *and* more than twice its live-entry
/// count — i.e. superseded / terminal lines have come to dominate. The floor avoids churning a
/// small file on every tick.
const COMPACT_MIN_LINES: usize = 128;

/// The canonical on-disk path for the machine's backlog.
pub fn backlog_path() -> PathBuf {
    crate::resolver::state_dir().join("backlog.jsonl")
}

/// Read every raw line of the log (file order). Missing file → empty. The unfolded input to
/// [`Backlog::from_lines`] and the raw-line count [`compact_if_needed`] weighs.
fn read_lines(path: &Path) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .collect()
}

/// Load the current folded backlog view from `path`.
pub fn load(path: &Path) -> Backlog {
    Backlog::from_lines(read_lines(path))
}

/// Append one entry as a JSON line, creating the parent dir on demand. Cheap and append-only — the
/// whole point of the JSONL format: an `add` under a busy daemon never rewrites the file.
fn append_line(path: &Path, entry: &BacklogEntry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

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

/// Snooze a pending `(repo, id)` entry until `not_before`.
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

/// Whether the file is worth rewriting: it has grown past the floor and superseded/terminal lines
/// now outnumber the live entries better than 2:1. Pure so the compaction trigger is unit-testable.
pub fn should_compact(raw_lines: usize, live_entries: usize) -> bool {
    raw_lines >= COMPACT_MIN_LINES && raw_lines > live_entries.saturating_mul(2)
}

/// Atomically rewrite `path` to the compacted set — live (non-terminal) entries only, one per line —
/// via a temp sibling + rename, so a concurrent reader never sees a torn file. Returns kept count.
fn write_compacted(path: &Path, bl: &Backlog) -> std::io::Result<usize> {
    let kept = bl.compacted();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        for entry in &kept {
            writeln!(f, "{}", serde_json::to_string(entry)?)?;
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(kept.len())
}

/// Force a compaction: drop terminal entries and collapse superseded lines. Returns kept count.
pub fn compact(path: &Path) -> std::io::Result<usize> {
    let _lock = FileLock::acquire_required(path)?;
    let bl = load(path);
    write_compacted(path, &bl)
}

/// Compact only when [`should_compact`] says the file has earned it (bounded churn). Returns whether
/// a rewrite happened. The daemon calls this once per tick — cheap when there's nothing to do.
pub fn compact_if_needed(path: &Path) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let lines = read_lines(path);
    let raw = lines.len();
    let bl = Backlog::from_lines(lines);
    if should_compact(raw, bl.compacted().len()) {
        write_compacted(path, &bl)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Record which machine holds a lease.
///
/// Separate from [`lease_selected`] because leasing is the *claim* and assignment is the *holder*:
/// a lease with no holder cannot be told apart from a healthy one when a box dies, which is how a
/// crashed machine's work becomes permanently invisible.
pub fn assign(path: &Path, repo: &str, id: &str, machine: &str, now: i64) -> std::io::Result<bool> {
    let _lock = FileLock::acquire_required(path)?;
    let bl = load(path);
    let Some(entry) = bl
        .entries()
        .iter()
        .find(|e| e.repo == repo && e.id == id && e.status.is_active())
    else {
        return Ok(false);
    };
    let mut updated = entry.clone();
    updated.assigned_to = Some(machine.to_string());
    updated.updated_at = now;
    append_line(path, &updated)?;
    Ok(true)
}
