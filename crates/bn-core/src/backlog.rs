//! The durable, prioritized backlog store (#375): one append-only `backlog.jsonl` in
//! [`crate::resolver::state_dir`], one JSON object per line.
//!
//! Its one consumer is the controller agent's request queue ([`crate::controller`]): requests are
//! added, leased one at a time, and reconciled to a terminal state. Reads fold the log into the
//! current per-`(repo, id)` view (the **latest** line for a key wins, log-structured);
//! [`compact_if_needed`] rewrites the file dropping terminal entries and collapsing superseded
//! lines so it stays bounded. The ordering / dedup / lease *policy* is pure and unit-tested on the
//! in-memory [`Backlog`]; the file functions only do the I/O.

use crate::orchestrate::derive_id;
use crate::store::FileLock;
use std::cmp::Reverse;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub mod entry;
pub mod kind;
pub use entry::*;
pub use kind::{BacklogKind, WorkClass};
mod writes;
pub use writes::*;

/// The in-memory current view of the backlog: the folded, latest-per-`(repo, id)` set of entries in
/// first-seen order. Every ordering / dedup / lease decision is a pure method here — the file I/O
/// ([`load`], [`add`], [`lease`], [`mark_leased`]) is a thin wrapper that folds the log in and
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
                // line must not take the store down); counting keeps the loss visible.
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

    /// A read view of `repo`'s backlog (#373): entries whose status matches `status_filter`
    /// (`None` = every status), ordered by the same pull policy as [`pending_for`] — priority
    /// descending, then `added_at` ascending (FIFO within a band), then `id`. The read counterpart
    /// to the lease path, generalized over status so a caller can also surface leased / terminal
    /// entries, not just pending ones.
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
    /// them. Pure — the disk [`lease`] appends the returned leased lines.
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

    /// Reconcile `(repo, id)` to a terminal `status` (done/failed) if it is currently active and not
    /// already in that state. Returns the updated entry to append, or `None` when it's a no-op (the
    /// entry is absent or already terminal) — so a repeated reconcile appends nothing.
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

    /// Return a leased entry to pending so its work can be retried.
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
/// small file on every write.
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
/// whole point of the JSONL format: an `add` never rewrites the file.
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

/// Compact only when [`should_compact`] says the file has earned it (bounded churn). Returns whether
/// a rewrite happened. Cheap when there is nothing to do.
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
