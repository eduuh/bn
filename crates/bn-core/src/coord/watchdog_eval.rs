//! Evaluating due watchdogs: claiming a due slot, and recording what happened.
//!
//! The claim is a compare-and-set on `next_due_at`: only the caller whose `UPDATE` changes the row
//! gets to evaluate it, so the tick, a precise timer (`bn watchdog fire`) and a second machine's
//! tick can all race and a watchdog still fires at most once per due slot.

use super::watchdog::{load_wd, wd_from_row, Watchdog, WD_COLUMNS};
use super::{CoordDb, Result};
use rusqlite::params;

/// How far a claimed slot is pushed out while it is evaluated, and how long a deferral waits.
pub const DEFER_SECS: i64 = 5 * 60;
/// Deferrals in a row before the caller should raise attention: a timer that can't land is a
/// problem someone needs to see, not a retry loop.
pub const DEFER_ALERT_AFTER: i64 = 6;

/// How an evaluation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Fired,
    Skipped,
    /// Couldn't deliver now (seat not live, on a dialog); try again shortly.
    Deferred,
    Unavailable,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Fired => "fired",
            Outcome::Skipped => "skipped",
            Outcome::Deferred => "deferred",
            Outcome::Unavailable => "unavailable",
            Outcome::Error => "error",
        }
    }
}

impl CoordDb {
    /// Active watchdogs due at `now`, soonest first. Expired ones are retired on the way.
    pub fn due_watchdogs(&mut self, now: i64) -> Result<Vec<Watchdog>> {
        self.conn.execute(
            "UPDATE watchdogs SET state = 'expired', stopped_at = ?1, stop_reason = 'expired'
             WHERE state = 'active' AND expires_at IS NOT NULL AND expires_at <= ?1",
            [now],
        )?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {WD_COLUMNS} FROM watchdogs WHERE state = 'active' AND next_due_at <= ?1
             ORDER BY next_due_at, id"
        ))?;
        let rows = stmt.query_map([now], wd_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Claim `wd`'s due slot. `false` means someone else claimed it first (or it moved on).
    pub fn claim_due(&mut self, wd: &Watchdog, now: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE watchdogs SET next_due_at = ?3, last_eval_at = ?2
             WHERE id = ?1 AND next_due_at = ?4 AND state = 'active' AND next_due_at <= ?2",
            params![wd.num, now, now + DEFER_SECS, wd.next_due_at],
        )?;
        Ok(changed == 1)
    }

    /// Record how a claimed evaluation ended, schedule the next one, and return the watchdog.
    pub fn finish_eval(
        &mut self,
        id: i64,
        outcome: Outcome,
        detail: &str,
        now: i64,
    ) -> Result<Watchdog> {
        let wd = load_wd(&self.conn, id)?;
        let tx = self.conn.transaction()?;
        let every = wd.every_secs;
        match outcome {
            Outcome::Fired => {
                let fires = wd.fires + 1;
                let done = every.is_none() || wd.max_fires.is_some_and(|m| fires >= m);
                tx.execute(
                    "UPDATE watchdogs SET fires = ?2, defer_count = 0,
                         state = CASE WHEN ?3 THEN 'fired' ELSE state END,
                         next_due_at = CASE WHEN ?3 THEN next_due_at ELSE ?4 END
                     WHERE id = ?1",
                    params![id, fires, done, now + every.unwrap_or(0)],
                )?;
            }
            Outcome::Deferred => {
                tx.execute(
                    "UPDATE watchdogs SET defer_count = defer_count + 1, next_due_at = ?2
                     WHERE id = ?1",
                    params![id, now + DEFER_SECS],
                )?;
            }
            Outcome::Skipped | Outcome::Unavailable | Outcome::Error => match every {
                Some(every) => {
                    tx.execute(
                        "UPDATE watchdogs SET defer_count = 0, next_due_at = ?2 WHERE id = ?1",
                        params![id, now + every],
                    )?;
                }
                // A one-shot whose condition no longer holds has nothing left to do.
                None if outcome == Outcome::Skipped => {
                    tx.execute(
                        "UPDATE watchdogs SET state = 'stopped', stopped_at = ?2, stop_reason = ?3
                         WHERE id = ?1",
                        params![id, now, detail],
                    )?;
                }
                // A one-shot that couldn't tell retries, like a deferral.
                None => {
                    tx.execute(
                        "UPDATE watchdogs SET defer_count = defer_count + 1, next_due_at = ?2
                         WHERE id = ?1",
                        params![id, now + DEFER_SECS],
                    )?;
                }
            },
        }
        tx.execute(
            "INSERT INTO watchdog_evals(watchdog_id, ts, outcome, detail) VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                now,
                outcome.as_str(),
                (!detail.is_empty()).then_some(detail)
            ],
        )?;
        tx.commit()?;
        load_wd(&self.conn, id)
    }
}
