//! The `wake-row` effect: unpark the row and tell its owner.

use crate::cmd::agent::try_deliver_live;
use crate::*;
use bn_core::clock::FixedClock;
use bn_core::coord::watchdog::Watchdog;
use bn_core::coord::watchdog_eval::Outcome;
use bn_core::coord::{self, CoordDb, CoordError};

pub(super) fn wake_row(cfg: &Config, db: &mut CoordDb, wd: &Watchdog, now: i64) -> (Outcome, String) {
    let Some(id) = wd.row.as_deref().and_then(coord::parse_qid) else {
        return (Outcome::Skipped, "no row attached".into());
    };
    let actor = format!("watchdog:{}", wd.id);
    match db.wake(&FixedClock::at(now), id, &actor) {
        Ok(row) => {
            let seat = row.owner.as_deref().unwrap_or(&row.dest);
            let line = format!(
                "[bn queue {}] woke — it was parked on {}. Re-check it, then carry on, close it \
                 with a note, or block it again.",
                row.id,
                row.blocked_on.as_deref().unwrap_or("?")
            );
            // The wake is the row changing state; reaching the owner's pane is a courtesy.
            let _ = try_deliver_live(cfg, seat, &line);
            (Outcome::Fired, format!("woke {}", row.id))
        }
        Err(CoordError::Illegal { state, .. }) => {
            (Outcome::Skipped, format!("row is {state}, not blocked"))
        }
        Err(e) => (Outcome::Error, e.to_string()),
    }
}
