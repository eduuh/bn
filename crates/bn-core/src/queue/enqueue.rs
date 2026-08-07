//! The one way work enters the queue.
//!
//! Wraps [`crate::backlog::add`] with the invariants every producer would otherwise have to
//! remember individually — which is how seven launch sites each grew their own slightly different
//! version. A producer should have to say *what* the work is, not re-derive the rules about how
//! work is recorded.
use crate::backlog::{self, AddSpec, BacklogEntry, Recurrence};
use std::path::Path;

/// What happened to an enqueue request.
///
/// Distinguishing "created" from "already there" is what lets a producer that fires every ten
/// minutes say `already queued` instead of looking like it minted a fresh entry each time — the
/// difference between a readable log and a log nobody trusts.
#[derive(Debug, Clone, PartialEq)]
pub enum Enqueued {
    /// A new entry was appended.
    Created(BacklogEntry),
    /// An active entry for this `(repo, id)` already existed and was left alone.
    Existing(BacklogEntry),
    /// An active entry existed at a lower priority and was raised to this request's.
    Reprioritized(BacklogEntry),
}

impl Enqueued {
    /// The entry either way.
    pub fn entry(&self) -> &BacklogEntry {
        match self {
            Enqueued::Created(e) | Enqueued::Existing(e) | Enqueued::Reprioritized(e) => e,
        }
    }

    /// The kebab-case outcome label, for events and status lines.
    pub fn label(&self) -> &'static str {
        match self {
            Enqueued::Created(_) => "created",
            Enqueued::Existing(_) => "existing",
            Enqueued::Reprioritized(_) => "reprioritized",
        }
    }

    /// Whether this call actually appended a new entry.
    pub fn is_new(&self) -> bool {
        matches!(self, Enqueued::Created(_))
    }
}

/// Enqueue `item` for `repo`, deduping on `(repo, id)` against entries that are still active.
///
/// Raising the priority of an entry that already exists is deliberate and is the behaviour a
/// repeating producer needs: an attention item that escalates from `warn` to `urgent`, or a PR
/// whose review has now been waiting six hours, should move up the queue rather than either being
/// ignored or being duplicated. Priority is only ever raised here, never lowered — a later, calmer
/// enqueue must not quietly de-escalate work something else escalated.
pub fn enqueue(
    path: &Path,
    repo: &str,
    item: &str,
    spec: &AddSpec,
    now: i64,
) -> std::io::Result<Enqueued> {
    let bl = backlog::load(path);
    let id = crate::orchestrate::derive_id(item);
    if let Some(existing) = bl
        .entries()
        .iter()
        .find(|e| e.repo == repo && e.id == id && e.status.is_active())
    {
        // A recurrence firing while its last firing still waits. Dedupe alone is not a policy —
        // see [`crate::backlog::Recurrence`] for why the answer differs by kind of work.
        let existing = match spec.recurrence {
            Recurrence::Skip => existing.clone(),
            Recurrence::Coalesce => match extended_deadline(existing, spec) {
                Some(deadline) => {
                    backlog::set_deadline(path, repo, &id, deadline, now)?;
                    let mut refreshed = existing.clone();
                    refreshed.not_after = Some(deadline);
                    refreshed.updated_at = now;
                    refreshed
                }
                None => existing.clone(),
            },
        };
        if spec.priority > existing.priority {
            backlog::reprioritize(path, repo, &id, spec.priority, now)?;
            let mut raised = existing;
            raised.priority = spec.priority;
            raised.updated_at = now;
            return Ok(Enqueued::Reprioritized(raised));
        }
        return Ok(Enqueued::Existing(existing));
    }
    let entry = BacklogEntry::new(repo, item, spec, now);
    backlog::add(
        path,
        repo,
        std::slice::from_ref(&item.to_string()),
        spec,
        now,
    )?;
    Ok(Enqueued::Created(entry))
}

/// The deadline a coalescing firing should carry forward, or `None` when nothing changes.
///
/// Only ever **extended**. A cron's `not_after` is its own next firing, so the newest firing is the
/// one whose window is still open; without carrying it forward the waiting entry keeps the first
/// firing's deadline while every later firing folds silently into it. A machine busy for a single
/// interval then stops running that job entirely — and reports the drop as "its window passed", as
/// though the work had merely arrived late rather than been superseded by a firing the queue threw
/// away. Never shortened, because a firing that somehow carries an earlier deadline must not close
/// the window on work already waiting.
fn extended_deadline(existing: &BacklogEntry, spec: &AddSpec) -> Option<i64> {
    let next = spec.not_after?;
    match existing.not_after {
        // Work queued as open-ended does not acquire a deadline from a later firing: narrowing that
        // retroactively could drop work that was never given a window to miss.
        None => None,
        Some(current) if next > current => Some(next),
        Some(_) => None,
    }
}
