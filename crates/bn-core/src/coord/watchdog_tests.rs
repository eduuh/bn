use super::policy::{decide, Decision, Facts, DEFAULT_IDLE_SECS};
use super::watchdog::{parse_wdid, wdid, Kind, NewWatchdog, Spec};
use super::watchdog_eval::{Outcome, DEFER_SECS};
use super::*;
use crate::clock::FixedClock;

const T0: i64 = 1_790_000_000;

fn db() -> CoordDb {
    CoordDb::open_in_memory().unwrap()
}

fn wd(kind: Kind) -> NewWatchdog {
    NewWatchdog {
        kind,
        target: "bn:y".into(),
        text: None,
        spec: Spec::default(),
        first_in_secs: 60,
        every_secs: None,
        expires_in_secs: None,
        max_fires: None,
        created_by: "bn:x".into(),
    }
}

fn register(db: &mut CoordDb, new: NewWatchdog) -> watchdog::Watchdog {
    db.register_watchdog(&FixedClock::at(T0), &new).unwrap()
}

#[test]
fn ids_round_trip() {
    assert_eq!(wdid(3), "wd3");
    assert_eq!(parse_wdid("wd3"), Some(3));
    assert_eq!(parse_wdid("3"), Some(3));
    assert_eq!(parse_wdid("q3"), None);
}

#[test]
fn policy_table() {
    let mut db = db();
    let mut at = |kind| register(&mut db, wd(kind));
    let (rem, keep, ctx) = (at(Kind::Reminder), at(Kind::Keepalive), at(Kind::Context));
    let idle = Some(DEFAULT_IDLE_SECS);
    let busy = Some(10);
    let f = |idle_secs, context_pct| Facts {
        idle_secs,
        context_pct,
    };
    assert!(matches!(decide(&rem, &Facts::default()), Decision::Fire(_)));
    assert!(matches!(decide(&keep, &f(idle, None)), Decision::Fire(_)));
    assert!(matches!(decide(&keep, &f(busy, None)), Decision::Skip(_)));
    assert!(matches!(
        decide(&keep, &f(None, None)),
        Decision::Unavailable(_)
    ));
    // Unknown context never fires — it is not 0%.
    assert!(matches!(
        decide(&ctx, &f(None, None)),
        Decision::Unavailable(_)
    ));
    assert!(matches!(
        decide(&ctx, &f(None, Some(79))),
        Decision::Skip(_)
    ));
    assert!(matches!(
        decide(&ctx, &f(None, Some(80))),
        Decision::Fire(_)
    ));
}

#[test]
fn only_due_watchdogs_are_returned_and_a_slot_is_claimed_once() {
    let mut db = db();
    let w = register(&mut db, wd(Kind::Reminder));
    assert!(db.due_watchdogs(T0 + 59).unwrap().is_empty());
    let due = db.due_watchdogs(T0 + 60).unwrap();
    assert_eq!(due.len(), 1);
    assert!(db.claim_due(&due[0], T0 + 60).unwrap());
    // The same slot, claimed again (a second tick, or `bn watchdog fire`), loses.
    assert!(!db.claim_due(&due[0], T0 + 61).unwrap());
    assert!(db.due_watchdogs(T0 + 61).unwrap().is_empty(), "leased out");
    let _ = w;
}

#[test]
fn a_one_shot_fires_once_and_a_periodic_one_reschedules() {
    let mut db = db();
    let once = register(&mut db, wd(Kind::Reminder));
    let every = register(
        &mut db,
        NewWatchdog {
            every_secs: Some(600),
            max_fires: Some(2),
            ..wd(Kind::Reminder)
        },
    );
    let now = T0 + 60;
    for w in db.due_watchdogs(now).unwrap() {
        assert!(db.claim_due(&w, now).unwrap());
        db.finish_eval(w.num, Outcome::Fired, "", now).unwrap();
    }
    assert_eq!(db.watchdog(once.num).unwrap().state, "fired");
    let e = db.watchdog(every.num).unwrap();
    assert_eq!(
        (e.state.as_str(), e.fires, e.next_due_at),
        ("active", 1, now + 600)
    );
    // Second fire hits max_fires.
    let w = db.due_watchdogs(now + 600).unwrap().pop().unwrap();
    assert!(db.claim_due(&w, now + 600).unwrap());
    let e = db
        .finish_eval(w.num, Outcome::Fired, "", now + 600)
        .unwrap();
    assert_eq!((e.state.as_str(), e.fires), ("fired", 2));
    assert_eq!(db.watchdog_evals(every.num, 10).unwrap().len(), 2);
}

#[test]
fn deferrals_back_off_and_count() {
    let mut db = db();
    let w = register(&mut db, wd(Kind::Reminder));
    let now = T0 + 60;
    let due = db.due_watchdogs(now).unwrap().pop().unwrap();
    db.claim_due(&due, now).unwrap();
    let after = db
        .finish_eval(w.num, Outcome::Deferred, "not live", now)
        .unwrap();
    assert_eq!(
        (after.defer_count, after.next_due_at),
        (1, now + DEFER_SECS)
    );
    assert_eq!(after.state, "active");
    // Firing resets the count.
    let due = db.due_watchdogs(now + DEFER_SECS).unwrap().pop().unwrap();
    db.claim_due(&due, now + DEFER_SECS).unwrap();
    let after = db
        .finish_eval(w.num, Outcome::Fired, "", now + DEFER_SECS)
        .unwrap();
    assert_eq!(after.defer_count, 0);
}

#[test]
fn a_one_shot_whose_condition_is_gone_stops() {
    let mut db = db();
    let w = register(&mut db, wd(Kind::Context));
    let now = T0 + 60;
    let due = db.due_watchdogs(now).unwrap().pop().unwrap();
    db.claim_due(&due, now).unwrap();
    let after = db
        .finish_eval(w.num, Outcome::Skipped, "context at 10%", now)
        .unwrap();
    assert_eq!(after.state, "stopped");
    assert_eq!(after.stop_reason.as_deref(), Some("context at 10%"));
}

#[test]
fn expired_and_stopped_watchdogs_never_come_due() {
    let mut db = db();
    let exp = register(
        &mut db,
        NewWatchdog {
            expires_in_secs: Some(30),
            ..wd(Kind::Reminder)
        },
    );
    let stop = register(&mut db, wd(Kind::Reminder));
    db.stop_watchdog(&FixedClock::at(T0), stop.num, "not needed")
        .unwrap();
    assert!(db.due_watchdogs(T0 + 60).unwrap().is_empty());
    assert_eq!(db.watchdog(exp.num).unwrap().state, "expired");
    assert_eq!(db.watchdogs(false, None, 10).unwrap().len(), 0);
    assert_eq!(db.watchdogs(true, Some("bn:y"), 10).unwrap().len(), 2);
}

#[test]
fn intervals_under_a_minute_and_bad_seats_are_refused() {
    let mut db = db();
    let fast = NewWatchdog {
        every_secs: Some(5),
        ..wd(Kind::Reminder)
    };
    assert!(db.register_watchdog(&FixedClock::at(T0), &fast).is_err());
    let bad = NewWatchdog {
        target: "nope".into(),
        ..wd(Kind::Reminder)
    };
    assert!(db.register_watchdog(&FixedClock::at(T0), &bad).is_err());
}

#[test]
fn a_v1_db_migrates_forward() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("coord.db");
    {
        // Hand-build a v1 DB: no watchdog tables, version 1.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_meta VALUES ('version', '1');",
        )
        .unwrap();
    }
    let mut db = CoordDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), 2);
    register(&mut db, wd(Kind::Reminder));
}
