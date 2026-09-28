//! Parks: blocking a row on something, with a trigger that will wake it.
//!
//! Closing a blocked row would be a lie, and sitting on it silently looks identical to a crash. So
//! a park always names what it waits on *and* how it will come back: a timer (`wake_after`), an
//! existing watchdog, or a person — who gets a plain-language summary and a durable evidence
//! reference, because a person will judge it.

use super::queue::{illegal, may_act, record, require_note, Entry};
use super::queue_read::load;
use super::watchdog::{insert_wd, load_wd, Kind, NewWatchdog, Spec};
use super::{is_human, normalize_seat, CoordDb, CoordError, QItem, QState, Result};
use crate::clock::Clock;
use rusqlite::params;

/// How a parked row will come back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// A one-shot `wake-row` watchdog, this many seconds out.
    WakeAfter(i64),
    /// An existing watchdog (by id).
    Watchdog(i64),
    /// The person the row is blocked on resolves it. Needs a summary and an evidence ref.
    Human { summary: String, evidence: String },
}

/// What a row is parked on, and what will wake it.
#[derive(Debug, Clone)]
pub struct Block {
    /// A seat, a row (`q<N>`), or free text naming the blocker.
    pub on: String,
    pub trigger: Trigger,
    /// Optional note for the history (a human block uses its summary).
    pub note: Option<String>,
}

impl CoordDb {
    /// Park row `id`: pending/in-progress → blocked, with its wake trigger, in one transaction.
    /// A `WakeAfter` creates the row's `wake-row` watchdog inside the same transaction, so a
    /// park can never exist without the timer that ends it.
    pub fn block(&mut self, clock: &dyn Clock, id: i64, actor: &str, b: &Block) -> Result<QItem> {
        let actor = normalize_seat(actor)?;
        if b.on.trim().is_empty() {
            return Err(CoordError::Invalid(
                "block needs --on: what it waits on".into(),
            ));
        }
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        // Only claimed work parks: a wake puts the row back in its owner's hands, so it must have
        // one. An unclaimed row that can't start yet is still just pending.
        if item.qstate() != QState::InProgress {
            return Err(illegal(&item, "blocked (claim it first)"));
        }
        may_act(&item, &actor)?;
        let on = b.on.trim();
        let (wake, summary, evidence) = match &b.trigger {
            Trigger::WakeAfter(secs) => {
                if *secs < 60 {
                    return Err(CoordError::Invalid(
                        "--wake-after must be at least 1m".into(),
                    ));
                }
                let wd = insert_wd(
                    &tx,
                    now,
                    &NewWatchdog {
                        kind: Kind::WakeRow,
                        target: item.owner.clone().unwrap_or_else(|| item.dest.clone()),
                        text: None,
                        spec: Spec::default(),
                        first_in_secs: *secs,
                        every_secs: None,
                        expires_in_secs: None,
                        max_fires: Some(1),
                        qitem_id: Some(id),
                        created_by: actor.clone(),
                    },
                )?;
                (Some(wd), None, None)
            }
            Trigger::Watchdog(wd) => {
                let w = load_wd(&tx, *wd)?;
                if !w.is_active() {
                    return Err(CoordError::Invalid(format!("{} is {}", w.id, w.state)));
                }
                // Only a wake-row watchdog wakes anything; any other kind would leave the park
                // silent while looking guarded. An unattached one is adopted for this row.
                let mine = w.row.as_deref().and_then(super::parse_qid);
                if w.kind() != Some(Kind::WakeRow) || mine.is_some_and(|r| r != id) {
                    return Err(CoordError::Invalid(format!(
                        "{} is a {} watchdog{}; a park needs a wake-row watchdog for {} \
                         (or use --wake-after)",
                        w.id,
                        w.kind,
                        w.row
                            .as_deref()
                            .map(|r| format!(" for {r}"))
                            .unwrap_or_default(),
                        item.id
                    )));
                }
                tx.execute(
                    "UPDATE watchdogs SET qitem_id = ?2 WHERE id = ?1",
                    params![wd, id],
                )?;
                (Some(*wd), None, None)
            }
            Trigger::Human { summary, evidence } => {
                if !is_human(on) {
                    return Err(CoordError::Invalid(
                        "a summary/evidence park must be --on human:<name>".into(),
                    ));
                }
                require_note(summary, "a block on a person")?;
                if evidence.trim().is_empty() {
                    return Err(CoordError::Invalid(
                        "a block on a person needs --evidence: a link or path they can judge"
                            .into(),
                    ));
                }
                (None, Some(summary.trim()), Some(evidence.trim()))
            }
        };
        let on = if is_human(on) {
            normalize_seat(on)?
        } else {
            on.to_string()
        };
        tx.execute(
            "UPDATE qitems SET state = 'blocked', blocked_on = ?2, block_summary = ?3,
                 evidence_ref = ?4, wake_watchdog_id = ?5, updated_at = ?6 WHERE id = ?1",
            params![id, on, summary, evidence, wake, now],
        )?;
        let note = b
            .note
            .clone()
            .or_else(|| summary.map(str::to_string))
            .unwrap_or_else(|| format!("on {on}"));
        record(
            &tx,
            now,
            Entry {
                id,
                actor: &actor,
                verb: "block",
                from: Some(item.qstate()),
                to: Some(QState::Blocked),
                note: Some(&note),
                ref_: evidence,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }

    /// Unpark row `id` by hand: blocked → in-progress, with a fresh deadline, stopping its wake
    /// watchdog. Returns the row.
    pub fn resolve(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        note: &str,
    ) -> Result<QItem> {
        require_note(note, "resolve")?;
        let actor = normalize_seat(actor)?;
        self.unpark(clock, id, &actor, "resolve", note, "stopped")
    }

    /// Unpark row `id` because its watchdog fired. `actor` is `watchdog:wd<N>`.
    pub fn wake(&mut self, clock: &dyn Clock, id: i64, actor: &str) -> Result<QItem> {
        self.unpark(clock, id, actor, "wake", "woken by its watchdog", "fired")
    }

    fn unpark(
        &mut self,
        clock: &dyn Clock,
        id: i64,
        actor: &str,
        verb: &'static str,
        note: &str,
        wd_state: &str,
    ) -> Result<QItem> {
        let now = clock.now_epoch_secs();
        let tx = self.begin()?;
        let item = load(&tx, id)?;
        if item.qstate() != QState::Blocked {
            return Err(illegal(
                &item,
                if verb == "wake" { "woken" } else { "resolved" },
            ));
        }
        if verb == "resolve" {
            may_act(&item, actor)?;
        }
        // A resolved park's own timer is moot; a fired one is already done — retire it. A watchdog
        // the park merely borrowed (`--watchdog`) belongs to someone else and keeps running.
        tx.execute(
            "UPDATE watchdogs SET state = ?2, stopped_at = ?3,
                 stop_reason = COALESCE(stop_reason, ?4)
             WHERE id = (SELECT wake_watchdog_id FROM qitems WHERE id = ?1) AND state = 'active'
                 AND kind = 'wake-row'",
            params![id, wd_state, now, format!("{} {}", past(verb), item.id)],
        )?;
        let due = now + item.qtier().window_secs();
        tx.execute(
            "UPDATE qitems SET state = 'in-progress', wake_watchdog_id = NULL,
                 closure_required_at = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, due, now],
        )?;
        record(
            &tx,
            now,
            Entry {
                id,
                actor,
                verb,
                from: Some(QState::Blocked),
                to: Some(QState::InProgress),
                note: Some(note.trim()),
                ref_: None,
            },
        )?;
        let item = load(&tx, id)?;
        tx.commit()?;
        Ok(item)
    }
}

fn past(verb: &str) -> &'static str {
    if verb == "wake" {
        "woke"
    } else {
        "resolved"
    }
}
