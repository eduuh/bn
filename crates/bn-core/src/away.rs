//! A declared absence — "I am going for lunch, get on with it."
//!
//! The queue holds opportunistic work until you are gone, and it works out that you are gone by
//! *inference*: [`crate::idle::is_active`] scans the event log and calls the machine idle when
//! nothing has happened for a while. That is the right default and a poor answer to the case you
//! actually want it for, because the wait is the whole cost — you close the laptop and the machine
//! spends the first ten minutes of your break deciding whether you have really left.
//!
//! So this is the other half: saying so. `bn queue away --for 45m` opens a block, every entry
//! waiting for an away window becomes admissible immediately, and the block expires on its own.
//!
//! It is the mirror of [`crate::focus`] and built the same way, deliberately — a `queue_away_start`
//! event in the shared log, folded on read, no new store and no migration. Focus says "I am here,
//! protect me"; this says "I am not, go ahead". They are the same shape because they are the same
//! kind of claim about presence, and a reader who knows one knows the other.
//!
//! **The subtlety, and the reason this cannot simply be another `is_active` signal:** pressing the
//! key writes an event, and an event inside the idle window is exactly what `is_active` reads as
//! *presence*. Declaring absence would mark you present. So a declared block **overrides** the
//! scan rather than joining it — see [`crate::idle::is_active`].

use crate::events::{self, Event};
use serde_json::{json, Map};

/// The event a declared absence appends.
const START: &str = "queue_away_start";
/// The event that ends one early.
const END: &str = "queue_away_end";

/// An open block of declared absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Away {
    /// Unix epoch seconds when it began.
    pub started: i64,
    /// Declared length in seconds.
    pub duration_secs: i64,
    /// `started + duration_secs`.
    pub ends_at: i64,
}

impl Away {
    /// Whole seconds left, clamped at zero.
    pub fn remaining(&self, now: i64) -> i64 {
        (self.ends_at - now).max(0)
    }
}

/// Declare yourself away for `duration_secs` from `now`.
///
/// Starting while one is open simply opens another; the later one wins on read, which extends the
/// break rather than erroring at someone who pressed the key twice.
pub fn start(duration_secs: i64, now: i64) -> std::io::Result<Away> {
    let mut fields = Map::new();
    fields.insert("duration_secs".to_string(), json!(duration_secs));
    events::append_at(START, now, fields)?;
    Ok(Away {
        started: now,
        duration_secs,
        ends_at: now.saturating_add(duration_secs),
    })
}

/// End the open block early — you came back.
///
/// Returns the block that was ended, or `None` when there was none.
pub fn end(now: i64) -> std::io::Result<Option<Away>> {
    let Some(open) = active(now) else {
        return Ok(None);
    };
    events::append_at(END, now, Map::new())?;
    Ok(Some(open))
}

/// The open block at `now`, if any.
///
/// Folded rather than stored, exactly as [`crate::focus::active`] does it: the latest `start` wins,
/// and a later `end` closes it. Expiry is evaluated on read, so a machine asleep past `ends_at`
/// wakes up with the block already over and nothing to reconcile.
pub fn active(now: i64) -> Option<Away> {
    fold(&events::read_all(), now)
}

/// Whether an absence is declared right now.
pub fn is_declared(now: i64) -> bool {
    active(now).is_some()
}

/// The fold, pure over its events so the rules are testable without touching the log.
pub fn fold(all: &[Event], now: i64) -> Option<Away> {
    let mut open: Option<Away> = None;
    for event in all {
        match event.kind.as_str() {
            START => {
                let secs = event
                    .fields
                    .get("duration_secs")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                // A non-positive duration is not a block. Recorded rather than rejected at the
                // write, because the log is append-only and a bad line must not poison the read.
                open = (secs > 0).then_some(Away {
                    started: event.ts,
                    duration_secs: secs,
                    ends_at: event.ts.saturating_add(secs),
                });
            }
            END => open = None,
            _ => {}
        }
    }
    open.filter(|a| a.ends_at > now)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_700_000;

    fn ev(kind: &str, ts: i64, secs: Option<i64>) -> Event {
        let mut fields = Map::new();
        if let Some(s) = secs {
            fields.insert("duration_secs".to_string(), json!(s));
        }
        Event {
            kind: kind.to_string(),
            ts,
            fields,
        }
    }

    #[test]
    fn a_declared_block_is_open_until_it_expires() {
        let log = [ev(START, NOW - 60, Some(600))];
        assert!(fold(&log, NOW).is_some(), "still inside the block");
        assert!(
            fold(&log, NOW + 600).is_none(),
            "expiry is evaluated on read, so it needs no sweep"
        );
    }

    #[test]
    fn coming_back_early_ends_it() {
        let log = [ev(START, NOW - 60, Some(600)), ev(END, NOW - 10, None)];
        assert!(fold(&log, NOW).is_none());
    }

    /// Pressing it twice extends the break rather than erroring — the later block wins.
    #[test]
    fn declaring_again_extends_rather_than_conflicts() {
        let log = [ev(START, NOW - 600, Some(300)), ev(START, NOW, Some(1800))];
        let open = fold(&log, NOW + 60).expect("the later block is open");
        assert_eq!(open.ends_at, NOW + 1800);
    }

    /// And a block ended, then declared again, is open — the fold must not let a stale `end` win.
    #[test]
    fn declaring_after_returning_opens_a_new_block() {
        let log = [
            ev(START, NOW - 600, Some(300)),
            ev(END, NOW - 500, None),
            ev(START, NOW - 10, Some(600)),
        ];
        assert!(fold(&log, NOW).is_some());
    }

    #[test]
    fn a_zero_length_block_is_not_a_block() {
        assert!(fold(&[ev(START, NOW, Some(0))], NOW).is_none());
        assert!(fold(&[ev(START, NOW, None)], NOW).is_none());
    }

    #[test]
    fn remaining_never_goes_negative() {
        let open = Away {
            started: NOW,
            duration_secs: 60,
            ends_at: NOW + 60,
        };
        assert_eq!(open.remaining(NOW + 10), 50);
        assert_eq!(open.remaining(NOW + 600), 0);
    }
}
