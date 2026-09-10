//! Switching the queue off — "stop holding my work back."
//!
//! The queue exists to keep the machine's own workload from over-committing the box, and that is
//! the right default for a machine minding its own business. It is the wrong one when somebody is
//! sitting in front of it: every trigger becomes an entry, every entry waits for a pass, and the
//! thing you just asked for is a row rather than a result. This is the switch that says so. With it
//! on, no producer captures — every trigger runs its work on the spot, exactly as it did before the
//! queue existed.
//!
//! **Not a pause, and the difference is the whole point.** A pause holds work: it keeps
//! accumulating in the backlog and starts when the pause lifts. This stops work being *filed at
//! all*, so there is nothing to accumulate and nothing to catch up on. The two look similar from the
//! outside — neither starts anything new through the dispatcher — and produce opposite backlogs.
//!
//! **Not `[machine] queue = false` either.** That removes the machine from the queue
//! entirely: no dispatcher, no `bn queue` surface, the managed service torn down. This leaves all of
//! that standing, so whatever was already filed keeps draining and switching back on is instant.
//!
//! Stored the way every other live control is: two events in the shared log (`queue_off` /
//! `queue_on`), folded on read, no new store and no migration. The same shape as [`crate::away`],
//! [`crate::focus`] and [`crate::queue::pause`], because it is the same kind of claim — something a
//! person just decided, which the machine should honour until they decide otherwise.
//!
//! Two decisions worth knowing before reading the code:
//!
//! * **Expiry is folded at read time, not fired by a timer.** `bn queue off --for 2h` on a laptop
//!   that is then shut for the night comes back on by itself, rather than waiting for a timer that
//!   never fired to un-stick it. [`crate::queue::pause`] makes the same choice for the same reason.
//! * **The log-derived half is memoised; `now` is applied outside it.** [`is_off`] sits on the
//!   producer gate and on the picker's repaint path, and `events::read_all()` is a full parse of a
//!   log that runs to tens of megabytes. So [`declared`] goes through [`crate::event_memo`], keyed
//!   on the log's identity, and [`active`] applies the clock to its answer — the doctrine
//!   [`crate::focus::active`] states.
use crate::events::{self, Event};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The event kinds this module folds. Public so `bn queue log` and the event reader can name them.
pub const OFF_EVENT: &str = "queue_off";
/// See [`OFF_EVENT`].
pub const ON_EVENT: &str = "queue_on";

/// The queue switched off, as the log records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Off {
    /// Epoch secs it was switched off.
    pub since: i64,
    /// Free text: why. Shown by `bn queue status`, on the picker's settings row and in
    /// `bn queue log` — so a machine that is not queueing is never anonymously not queueing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Epoch secs it switches back on by itself. `None` = until switched on explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl Off {
    /// Whether this has lapsed as of `now`.
    pub fn expired(&self, now: i64) -> bool {
        self.until.is_some_and(|until| now >= until)
    }

    /// Whole seconds until it switches itself back on, or `None` when it is indefinite.
    pub fn remaining(&self, now: i64) -> Option<i64> {
        self.until.map(|until| (until - now).max(0))
    }
}

/// Fold `queue_off` / `queue_on` into the switch's position, ignoring the clock.
///
/// Latest event wins, so flipping twice is safe and the second flip re-arms rather than erroring —
/// the [`crate::queue::pause::pause`] contract. Scanned in reverse because the log is appended
/// chronologically and only the last of the pair matters.
///
/// Deliberately clock-free: expiry belongs to [`active`], so this value is pure log-derived state
/// and therefore cacheable. Folding `until` in here would make the answer depend on when it was
/// asked, which is precisely the thing [`crate::event_memo`] must not be handed.
pub fn fold(events: &[Event]) -> Option<Off> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            ON_EVENT => Some(None),
            OFF_EVENT => Some(Some(Off {
                since: event.ts,
                reason: event
                    .fields
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                until: event.fields.get("until").and_then(Value::as_i64),
            })),
            _ => None,
        })
        .flatten()
}

/// The switch's position as the log records it, before `now` is taken into account.
///
/// Memoised against the event log's identity, so the common case — the switch has not been touched
/// since the last read — costs a small file read rather than a full parse. See
/// [`crate::event_memo`] for why that key is exact rather than a TTL.
pub fn declared() -> Option<Off> {
    crate::event_memo::memoised("queue-off.json", "", || fold(&events::read_all()))
}

/// The queue's off switch if it is in force at `now`, or `None` when the queue is on.
///
/// `now` is applied here rather than inside the memo: the switch's position is pure log-derived
/// state and so is cacheable, while "has it lapsed *yet*" changes every second and is not.
pub fn active(now: i64) -> Option<Off> {
    declared().filter(|off| !off.expired(now))
}

/// Whether the queue is switched off right now — the single question every producer gate asks.
pub fn is_off(now: i64) -> bool {
    active(now).is_some()
}

/// Append `queue_off`.
///
/// Switching off while already off re-opens it with the new reason and expiry (latest wins on
/// read), so calling it repeatedly is safe.
pub fn off(reason: &str, until: Option<i64>, now: i64) -> std::io::Result<()> {
    let mut fields = Map::new();
    if !reason.trim().is_empty() {
        fields.insert(
            "reason".to_string(),
            Value::String(reason.trim().to_string()),
        );
    }
    if let Some(until) = until {
        fields.insert("until".to_string(), Value::from(until));
    }
    events::append_at(OFF_EVENT, now, fields)
}

/// Append `queue_on`.
///
/// Returns the epoch it was switched off (for the "off for 4h" line), or `None` when it was not off
/// — so the caller can say so without writing a dangling event. The [`crate::queue::pause::resume`]
/// contract. A lapsed switch counts as already on, which is what makes `--for` and this agree.
pub fn on(now: i64) -> std::io::Result<Option<i64>> {
    let Some(since) = active(now).map(|off| off.since) else {
        return Ok(None);
    };
    events::append_at(ON_EVENT, now, Map::new())?;
    Ok(Some(since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, ts: i64, fields: Vec<(&str, Value)>) -> Event {
        Event {
            kind: kind.to_string(),
            ts,
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn an_empty_log_reads_as_on() {
        assert_eq!(fold(&[]), None);
        assert_eq!(fold(&[event("queue_tick", 10, vec![])]), None);
    }

    /// The reason travels with the switch, because a machine that is not queueing should never be
    /// anonymously not queueing.
    #[test]
    fn a_reason_and_an_expiry_round_trip() {
        let folded = fold(&[event(
            OFF_EVENT,
            100,
            vec![("reason", json!("shipping")), ("until", json!(7300))],
        )]);
        assert_eq!(
            folded,
            Some(Off {
                since: 100,
                reason: "shipping".to_string(),
                until: Some(7300),
            })
        );
    }

    /// Latest wins in both directions, so flipping twice is safe and the switch is not sticky.
    #[test]
    fn the_latest_flip_wins() {
        let off_then_on = [event(OFF_EVENT, 10, vec![]), event(ON_EVENT, 20, vec![])];
        assert_eq!(fold(&off_then_on), None);

        let on_then_off = [event(ON_EVENT, 10, vec![]), event(OFF_EVENT, 20, vec![])];
        assert_eq!(fold(&on_then_off).map(|o| o.since), Some(20));

        // Switching off twice re-arms with the newer reason rather than erroring.
        let twice = [
            event(OFF_EVENT, 10, vec![("reason", json!("first"))]),
            event(OFF_EVENT, 20, vec![("reason", json!("second"))]),
        ];
        assert_eq!(fold(&twice).map(|o| o.reason), Some("second".to_string()));
    }

    /// The property the whole read-time-expiry choice exists for: a machine that slept past `until`
    /// wakes up queueing again, with no timer having fired.
    #[test]
    fn an_expiry_that_has_passed_reads_as_on() {
        let off = Off {
            since: 100,
            reason: String::new(),
            until: Some(200),
        };
        assert!(!off.expired(199));
        assert!(off.expired(200), "at the boundary it is over");
        assert!(off.expired(9_999));

        let indefinite = Off { until: None, ..off };
        assert!(!indefinite.expired(9_999), "no expiry never lapses");
    }

    /// Fold is clock-free on purpose — that is what makes it safe to memoise. A lapsed switch is
    /// still *recorded*; it is only [`active`] that calls it over.
    #[test]
    fn fold_reports_a_lapsed_switch_and_leaves_the_clock_to_the_caller() {
        let folded = fold(&[event(OFF_EVENT, 10, vec![("until", json!(20))])]);
        assert!(folded.is_some(), "the log still records it");
        assert!(folded.expect("recorded").expired(50));
    }

    /// The whole path, through the real log and the real memo: flip it, read it back, flip it
    /// again. This is what proves the memo cannot serve a stale answer — `event_memo` keys on the
    /// log's `(mtime, len)`, and every one of these writes appends.
    #[test]
    fn the_switch_round_trips_through_the_log_and_the_memo() {
        crate::test_support::with_state_dir(|| {
            let now = 1_000_000;
            assert!(!is_off(now), "a fresh machine is queueing");

            off("shipping", None, now).expect("switch off");
            let held = active(now).expect("off");
            assert_eq!(held.reason, "shipping");
            assert_eq!(held.until, None);
            assert!(is_off(now + 86_400), "an indefinite switch does not lapse");

            let since = on(now + 60).expect("switch on");
            assert_eq!(since, Some(now), "it reports how long it was off");
            assert!(
                !is_off(now + 60),
                "and the memo did not serve the old answer"
            );

            assert_eq!(on(now + 120).expect("already on"), None, "a no-op says so");
        });
    }

    /// `--for` end to end: off now, on by itself later, with nothing having fired a timer.
    #[test]
    fn a_timed_switch_lapses_without_a_timer() {
        crate::test_support::with_state_dir(|| {
            let now = 2_000_000;
            off("lunch", Some(now + 3600), now).expect("switch off");
            assert!(is_off(now));
            assert!(is_off(now + 3599));
            assert!(!is_off(now + 3600), "it is over at the boundary");
            assert!(!is_off(now + 999_999), "and stays over");
            // Still recorded, so `on` after it lapsed is honestly a no-op rather than an error.
            assert!(declared().is_some());
            assert_eq!(on(now + 999_999).expect("already on"), None);
        });
    }

    #[test]
    fn remaining_counts_down_and_never_goes_negative() {
        let off = Off {
            since: 0,
            reason: String::new(),
            until: Some(100),
        };
        assert_eq!(off.remaining(40), Some(60));
        assert_eq!(off.remaining(500), Some(0));
        assert_eq!(Off { until: None, ..off }.remaining(40), None);
    }
}
