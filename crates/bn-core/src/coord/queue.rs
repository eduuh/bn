//! The queue verbs. Each one runs in a single `BEGIN IMMEDIATE` transaction that changes the row
//! and appends its transition together, so the history can never disagree with the state.

use super::queue_read::load;
use super::{is_human, normalize_seat, qid, CoordDb, CoordError, QItem, QState, Result, Tier};
use crate::clock::Clock;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

/// A row to create.
#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub dest: String,
    pub source: String,
    pub body: String,
    pub tier: Tier,
    /// A caller-chosen key: creating again with the same key returns the existing row.
    pub idem_key: Option<String>,
    pub ref_: Option<String>,
}

/// Where a handoff sends the work.
#[derive(Debug, Clone, Default)]
pub struct Handoff {
    pub to: String,
    pub note: String,
    /// The new row's tier; defaults to the source row's.
    pub tier: Option<Tier>,
    pub ref_: Option<String>,
    /// Close the source as `done` rather than `handed-off`: this seat finished its part.
    pub complete: bool,
}

/// How a row ends when it is not handed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    Done,
    Failed,
    Denied,
    Canceled,
}

impl CloseOutcome {
    pub fn state(self) -> QState {
        match self {
            CloseOutcome::Done => QState::Done,
            CloseOutcome::Failed => QState::Failed,
            CloseOutcome::Denied => QState::Denied,
            CloseOutcome::Canceled => QState::Canceled,
        }
    }

    fn verb(self) -> &'static str {
        match self {
            CloseOutcome::Done => "done",
            CloseOutcome::Failed => "failed",
            CloseOutcome::Denied => "denied",
            CloseOutcome::Canceled => "canceled",
        }
    }
}

/// One history entry, written inside the caller's transaction.
pub(super) struct Entry<'a> {
    pub(super) id: i64,
    pub(super) actor: &'a str,
    pub(super) verb: &'a str,
    pub(super) from: Option<QState>,
    pub(super) to: Option<QState>,
    pub(super) note: Option<&'a str>,
    pub(super) ref_: Option<&'a str>,
}

pub(super) fn record(tx: &Transaction<'_>, now: i64, e: Entry<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO qtransitions(qitem_id, ts, actor, verb, from_state, to_state, note, ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            e.id,
            now,
            e.actor,
            e.verb,
            e.from.map(QState::as_str),
            e.to.map(QState::as_str),
            e.note,
            e.ref_
        ],
    )?;
    Ok(())
}

/// A note is how a seat says where the work went; an empty one says nothing.
pub(super) fn require_note(note: &str, verb: &str) -> Result<()> {
    if note.trim().is_empty() {
        return Err(CoordError::Invalid(format!(
            "{verb} needs a note saying where the work went or why"
        )));
    }
    Ok(())
}

/// The seat the row is addressed to, its owner, or a person may act on it. This is attribution,
/// not security: it stops an agent closing another agent's work by accident.
pub(super) fn may_act(item: &QItem, actor: &str) -> Result<()> {
    if is_human(actor) || item.dest == actor || item.owner.as_deref() == Some(actor) {
        return Ok(());
    }
    Err(CoordError::Forbidden(format!(
        "{} belongs to {}; {actor} cannot act on it",
        item.id,
        item.owner.as_deref().unwrap_or(&item.dest)
    )))
}

pub(super) fn illegal(item: &QItem, verb: &'static str) -> CoordError {
    CoordError::Illegal {
        id: item.id.clone(),
        state: item.state.clone(),
        verb,
    }
}

impl CoordDb {
    pub(super) fn begin(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Open a `pending` row at `new.dest`. With an `idem_key` already used, returns that row and
    /// `false` instead of creating a second.
    pub fn create(&mut self, clock: &dyn Clock, new: &NewItem) -> Result<(QItem, bool)> {
        let dest = normalize_seat(&new.dest)?;
        let source = normalize_seat(&new.source)?;
        if new.body.trim().is_empty() {
            return Err(CoordError::Invalid("a queue row needs a body".into()));
        }
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        if let Some(key) = &new.idem_key {
            let existing: Option<i64> = tx
                .query_row("SELECT id FROM qitems WHERE idem_key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(id) = existing {
                let item = load(&tx, id)?;
                tx.commit()?;
                return Ok((item, false));
            }
        }
        let id = insert_pending(&tx, now, &dest, &source, new, None)?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &source,
                verb: "create",
                from: None,
                to: Some(QState::Pending),
                note: None,
                ref_: new.ref_.as_deref(),
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok((item, true))
    }

    /// `pending` → `in-progress`, owned by `actor`, with a closure deadline from the tier.
    /// Claiming a row you already own is a no-op, so a retried claim is safe.
    pub fn claim(&mut self, clock: &dyn Clock, id: i64, actor: &str) -> Result<QItem> {
        let actor = normalize_seat(actor)?;
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        if item.qstate() == QState::InProgress && item.owner.as_deref() == Some(actor.as_str()) {
            return Ok(item);
        }
        if item.qstate() != QState::Pending {
            return Err(illegal(&item, "claimed"));
        }
        if item.dest != actor && !is_human(&actor) {
            return Err(CoordError::Forbidden(format!(
                "{} is addressed to {}; {actor} cannot claim it",
                item.id, item.dest
            )));
        }
        let due = now + item.qtier().window_secs();
        tx.execute(
            "UPDATE qitems SET state = 'in-progress', owner = ?2, claimed_at = ?3,
                 closure_required_at = ?4, updated_at = ?3 WHERE id = ?1",
            params![id, actor, now, due],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb: "claim",
                from: Some(QState::Pending),
                to: Some(QState::InProgress),
                note: None,
                ref_: None,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }

    /// `in-progress` → `pending`: give a claimed row back, saying why.
    pub fn unclaim(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        note: &str,
    ) -> Result<QItem> {
        require_note(note, "unclaim")?;
        let actor = normalize_seat(actor)?;
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        if item.qstate() != QState::InProgress {
            return Err(illegal(&item, "unclaimed"));
        }
        if item.owner.as_deref() != Some(actor.as_str()) && !is_human(&actor) {
            return Err(CoordError::Forbidden(format!(
                "{} is owned by {}; only its owner can unclaim it",
                item.id,
                item.owner.as_deref().unwrap_or("nobody")
            )));
        }
        tx.execute(
            "UPDATE qitems SET state = 'pending', owner = NULL, claimed_at = NULL,
                 closure_required_at = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb: "unclaim",
                from: Some(QState::InProgress),
                to: Some(QState::Pending),
                note: Some(note),
                ref_: None,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }

    /// Close row `id` and open a `pending` row at `h.to` in one transaction. The source ends
    /// `handed-off` (or `done` with `h.complete`) with `next` pointing at the new row, and the new
    /// row's `parent` points back. Returns `(source, new)`.
    pub fn handoff(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        h: &Handoff,
    ) -> Result<(QItem, QItem)> {
        require_note(&h.note, "handoff")?;
        let actor = normalize_seat(actor)?;
        let to = normalize_seat(&h.to)?;
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        if item.qstate().is_terminal() {
            return Err(illegal(&item, "handed off"));
        }
        may_act(&item, &actor)?;
        if to == item.dest {
            return Err(CoordError::Invalid(format!(
                "{} is already addressed to {to}",
                item.id
            )));
        }
        let new = NewItem {
            dest: to.clone(),
            source: actor.clone(),
            body: item.body.clone(),
            tier: h.tier.unwrap_or(item.qtier()),
            idem_key: None,
            ref_: h.ref_.clone().or_else(|| item.ref_.clone()),
        };
        let new_id = insert_pending(&tx, now, &to, &actor, &new, Some(id))?;
        retire_wake(&tx, id, now, &format!("{} handed off", item.id))?;
        let (end, verb) = if h.complete {
            (QState::Done, "handoff-and-complete")
        } else {
            (QState::HandedOff, "handoff")
        };
        let closure = format!("→ {} at {to}: {}", qid(new_id), h.note.trim());
        tx.execute(
            "UPDATE qitems SET state = ?2, next_id = ?3, closure_note = ?4, closure_ref = ?5,
                 updated_at = ?6 WHERE id = ?1",
            params![id, end.as_str(), new_id, closure, h.ref_, now],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb,
                from: Some(item.qstate()),
                to: Some(end),
                note: Some(&closure),
                ref_: h.ref_.as_deref(),
            },
        )?;
        let opened = format!("from {} at {}: {}", item.id, item.dest, h.note.trim());
        record(
            &tx,
            now,
            Entry {
                id: new_id,
                actor: &actor,
                verb: "create",
                from: None,
                to: Some(QState::Pending),
                note: Some(&opened),
                ref_: new.ref_.as_deref(),
            },
        )?;
        let pair = (load(&tx, id)?, load(&tx, new_id)?);
        tx.commit()?;
        Ok(pair)
    }

    /// End row `id` as `outcome`, with a note saying where the work went or why it stopped.
    pub fn close(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        outcome: CloseOutcome,
        note: &str,
        ref_: Option<&str>,
    ) -> Result<QItem> {
        require_note(note, outcome.verb())?;
        let actor = normalize_seat(actor)?;
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        if item.qstate().is_terminal() {
            return Err(illegal(&item, "closed"));
        }
        may_act(&item, &actor)?;
        let end = outcome.state();
        retire_wake(&tx, id, now, &format!("{} closed", item.id))?;
        tx.execute(
            "UPDATE qitems SET state = ?2, closure_note = ?3, closure_ref = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, end.as_str(), note.trim(), ref_, now],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb: outcome.verb(),
                from: Some(item.qstate()),
                to: Some(end),
                note: Some(note.trim()),
                ref_,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }

    /// Append a note to row `id`'s history without changing its state.
    pub fn update(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        note: &str,
        ref_: Option<&str>,
    ) -> Result<QItem> {
        require_note(note, "update")?;
        let actor = normalize_seat(actor)?;
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        may_act(&item, &actor)?;
        tx.execute(
            "UPDATE qitems SET updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb: "note",
                from: None,
                to: None,
                note: Some(note.trim()),
                ref_,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }
}

fn insert_pending(
    tx: &Transaction<'_>,
    now: i64,
    dest: &str,
    source: &str,
    new: &NewItem,
    parent: Option<i64>,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO qitems(dest, source, body, state, tier, parent_id, ref, idem_key,
                            created_at, updated_at)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            dest,
            source,
            new.body.trim(),
            new.tier.as_str(),
            parent,
            new.ref_,
            new.idem_key,
            now
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// A row leaving `blocked` any way but a wake or resolve (a close, a handoff) takes its own
/// wake-row timer with it, so no watchdog is left pointing at finished work.
pub(super) fn retire_wake(tx: &Transaction<'_>, id: i64, now: i64, why: &str) -> Result<()> {
    tx.execute(
        "UPDATE watchdogs SET state = 'stopped', stopped_at = ?2, stop_reason = ?3
         WHERE id = (SELECT wake_watchdog_id FROM qitems WHERE id = ?1)
           AND state = 'active' AND kind = 'wake-row'",
        params![id, now, why],
    )?;
    tx.execute(
        "UPDATE qitems SET wake_watchdog_id = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}
