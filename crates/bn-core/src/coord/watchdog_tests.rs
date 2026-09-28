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
        qitem_id: None,
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
    let (rem, keep, iq, ctx, wake) = (
        at(Kind::Reminder),
        at(Kind::Keepalive),
        at(Kind::IdleQueue),
        at(Kind::Context),
        at(Kind::WakeRow),
    );
    let idle = Some(DEFAULT_IDLE_SECS);
    let busy = Some(10);
    let f = |idle_secs, pending_rows, context_pct, row_blocked| Facts {
        idle_secs,
        pending_rows,
        context_pct,
        row_blocked,
    };
    assert!(matches!(decide(&rem, &Facts::default()), Decision::Fire(_)));
    assert!(matches!(
        decide(&keep, &f(idle, 0, None, None)),
        Decision::Fire(_)
    ));
    assert!(matches!(
        decide(&keep, &f(busy, 0, None, None)),
        Decision::Skip(_)
    ));
    assert!(matches!(
        decide(&keep, &f(None, 0, None, None)),
        Decision::Unavailable(_)
    ));
    assert!(matches!(
        decide(&iq, &f(idle, 0, None, None)),
        Decision::Skip(_)
    ));
    assert!(matches!(
        decide(&iq, &f(idle, 2, None, None)),
        Decision::Fire(_)
    ));
    assert!(matches!(
        decide(&iq, &f(busy, 2, None, None)),
        Decision::Skip(_)
    ));
    // Unknown context never fires — it is not 0%.
    assert!(matches!(
        decide(&ctx, &f(None, 0, None, None)),
        Decision::Unavailable(_)
    ));
    assert!(matches!(
        decide(&ctx, &f(None, 0, Some(79), None)),
        Decision::Skip(_)
    ));
    assert!(matches!(
        decide(&ctx, &f(None, 0, Some(80), None)),
        Decision::Fire(_)
    ));
    assert!(matches!(
        decide(&wake, &f(None, 0, None, Some(true))),
        Decision::Fire(_)
    ));
    assert!(matches!(
        decide(&wake, &f(None, 0, None, Some(false))),
        Decision::Skip(_)
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
    let w = register(&mut db, wd(Kind::WakeRow));
    let now = T0 + 60;
    let due = db.due_watchdogs(now).unwrap().pop().unwrap();
    db.claim_due(&due, now).unwrap();
    let after = db
        .finish_eval(w.num, Outcome::Skipped, "row is no longer blocked", now)
        .unwrap();
    assert_eq!(after.state, "stopped");
    assert_eq!(
        after.stop_reason.as_deref(),
        Some("row is no longer blocked")
    );
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
fn a_v1_db_migrates_forward_and_keeps_its_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("coord.db");
    {
        // Hand-build a v1 DB: queue tables only, version 1.
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
    let (row, _) = db
        .create(
            &FixedClock::at(T0),
            &NewItem {
                dest: "bn:y".into(),
                source: "bn:x".into(),
                body: "x".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(row.id, "q1");
}

mod parks {
    use super::*;

    fn row(db: &mut CoordDb) -> QItem {
        let (item, _) = db
            .create(
                &FixedClock::at(T0),
                &NewItem {
                    dest: "bn:y".into(),
                    source: "bn:x".into(),
                    body: "build it".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        db.claim(&FixedClock::at(T0), item.num, "bn:y").unwrap()
    }

    fn block(on: &str, trigger: Trigger) -> Block {
        Block {
            on: on.into(),
            trigger,
            note: None,
        }
    }

    #[test]
    fn wake_after_parks_the_row_and_creates_its_timer_together() {
        let mut db = db();
        let item = row(&mut db);
        let parked = db
            .block(
                &FixedClock::at(T0),
                item.num,
                "bn:y",
                &block("bn:review", Trigger::WakeAfter(600)),
            )
            .unwrap();
        assert_eq!(parked.state, "blocked");
        let wd = parked.wake_watchdog.expect("a wake watchdog");
        let w = db.watchdog(parse_wdid(&wd).unwrap()).unwrap();
        assert_eq!(
            (w.kind.as_str(), w.row.as_deref()),
            ("wake-row", Some("q1"))
        );
        assert_eq!(w.next_due_at, T0 + 600);
        assert_eq!(w.target, "bn:y");
        assert!(db.has_wake(item.num).unwrap());
    }

    #[test]
    fn waking_unparks_with_a_fresh_deadline_and_retires_the_timer() {
        let mut db = db();
        let item = row(&mut db);
        let parked = db
            .block(
                &FixedClock::at(T0),
                item.num,
                "bn:y",
                &block("ci", Trigger::WakeAfter(600)),
            )
            .unwrap();
        let wd = parse_wdid(parked.wake_watchdog.as_deref().unwrap()).unwrap();
        let woke = db
            .wake(&FixedClock::at(T0 + 600), item.num, "watchdog:wd1")
            .unwrap();
        assert_eq!(woke.state, "in-progress");
        assert_eq!(woke.wake_watchdog, None);
        assert_eq!(
            woke.closure_required_at,
            Some(T0 + 600 + Tier::Routine.window_secs())
        );
        assert_eq!(db.watchdog(wd).unwrap().state, "fired");
        let last = db.transitions(item.num).unwrap().pop().unwrap();
        assert_eq!(
            (last.verb.as_str(), last.actor.as_str()),
            ("wake", "watchdog:wd1")
        );
        // A second wake finds nothing to do.
        assert!(matches!(
            db.wake(&FixedClock::at(T0 + 601), item.num, "watchdog:wd1"),
            Err(CoordError::Illegal { .. })
        ));
    }

    #[test]
    fn resolve_stops_its_own_timer() {
        let mut db = db();
        let a = row(&mut db);
        let own = db
            .block(
                &FixedClock::at(T0),
                a.num,
                "bn:y",
                &block("ci", Trigger::WakeAfter(600)),
            )
            .unwrap();
        db.resolve(&FixedClock::at(T0 + 5), a.num, "bn:y", "ci went green")
            .unwrap();
        let own_wd = parse_wdid(own.wake_watchdog.as_deref().unwrap()).unwrap();
        let own_wd = db.watchdog(own_wd).unwrap();
        assert_eq!(own_wd.state, "stopped");
        assert_eq!(own_wd.stop_reason.as_deref(), Some("resolved q1"));
    }

    #[test]
    fn only_a_wake_row_watchdog_can_guard_a_park() {
        let mut db = db();
        let a = row(&mut db);
        // A reminder would fire, paste its text and leave the row blocked forever.
        let reminder = register(&mut db, wd(Kind::Reminder));
        let err = db
            .block(
                &FixedClock::at(T0),
                a.num,
                "bn:y",
                &block("ci", Trigger::Watchdog(reminder.num)),
            )
            .unwrap_err();
        assert!(err.to_string().contains("needs a wake-row"), "{err}");
        // An unattached wake-row watchdog is adopted for the row.
        let spare = register(&mut db, wd(Kind::WakeRow));
        db.block(
            &FixedClock::at(T0),
            a.num,
            "bn:y",
            &block("ci", Trigger::Watchdog(spare.num)),
        )
        .unwrap();
        assert_eq!(db.watchdog(spare.num).unwrap().row.as_deref(), Some("q1"));
        // …and can't then be borrowed by another row.
        let b = row(&mut db);
        assert!(db
            .block(
                &FixedClock::at(T0),
                b.num,
                "bn:y",
                &block("ci", Trigger::Watchdog(spare.num))
            )
            .is_err());
    }

    #[test]
    fn closing_a_parked_row_retires_its_timer_and_unclaimed_rows_cannot_park() {
        let mut db = db();
        let a = row(&mut db);
        let parked = db
            .block(
                &FixedClock::at(T0),
                a.num,
                "bn:y",
                &block("ci", Trigger::WakeAfter(600)),
            )
            .unwrap();
        let wd = parse_wdid(parked.wake_watchdog.as_deref().unwrap()).unwrap();
        let closed = db
            .close(
                &FixedClock::at(T0 + 5),
                a.num,
                "bn:y",
                CloseOutcome::Canceled,
                "moot",
                None,
            )
            .unwrap();
        assert_eq!(closed.wake_watchdog, None);
        assert_eq!(db.watchdog(wd).unwrap().state, "stopped");

        let (pending, _) = db
            .create(
                &FixedClock::at(T0),
                &NewItem {
                    dest: "bn:y".into(),
                    source: "bn:x".into(),
                    body: "x".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(matches!(
            db.block(
                &FixedClock::at(T0),
                pending.num,
                "bn:y",
                &block("x", Trigger::WakeAfter(600))
            ),
            Err(CoordError::Illegal { .. })
        ));
    }

    #[test]
    fn a_person_park_needs_a_summary_and_evidence_and_raises_an_alert() {
        let mut db = db();
        let item = row(&mut db);
        let human = |summary: &str, evidence: &str| {
            block(
                "human:eduuh",
                Trigger::Human {
                    summary: summary.into(),
                    evidence: evidence.into(),
                },
            )
        };
        assert!(db
            .block(&FixedClock::at(T0), item.num, "bn:y", &human("", "pr/1"))
            .is_err());
        assert!(db
            .block(
                &FixedClock::at(T0),
                item.num,
                "bn:y",
                &human("ship it?", " ")
            )
            .is_err());
        let not_a_person = block(
            "bn:z",
            Trigger::Human {
                summary: "s".into(),
                evidence: "e".into(),
            },
        );
        assert!(db
            .block(&FixedClock::at(T0), item.num, "bn:y", &not_a_person)
            .is_err());

        let parked = db
            .block(
                &FixedClock::at(T0),
                item.num,
                "bn:y",
                &human("Merge the schema change?", "https://example/pr/9"),
            )
            .unwrap();
        let plan = sweep::plan(
            std::slice::from_ref(&parked),
            &|_| None,
            &|_| false,
            &[],
            T0 + 60,
        );
        assert_eq!(plan.raise.len(), 1);
        let alert = &plan.raise[0];
        assert_eq!(alert.seat, "human:eduuh");
        assert_eq!(alert.link.as_deref(), Some("https://example/pr/9"));
        assert!(alert.detail.contains("Merge the schema change?"));

        // The person resolves it; the next sweep clears the alert.
        let resolved = db
            .resolve(
                &FixedClock::at(T0 + 90),
                item.num,
                "human:eduuh",
                "yes, merge",
            )
            .unwrap();
        let plan = sweep::plan(
            &[resolved],
            &|_| None,
            &|_| false,
            std::slice::from_ref(&alert.key),
            T0 + 120,
        );
        assert_eq!(plan.ack, std::slice::from_ref(&alert.key));
    }

    #[test]
    fn a_closed_row_cannot_be_parked() {
        let mut db = db();
        let item = row(&mut db);
        db.close(
            &FixedClock::at(T0),
            item.num,
            "bn:y",
            CloseOutcome::Done,
            "done",
            None,
        )
        .unwrap();
        assert!(matches!(
            db.block(
                &FixedClock::at(T0),
                item.num,
                "bn:y",
                &block("x", Trigger::WakeAfter(600))
            ),
            Err(CoordError::Illegal { .. })
        ));
    }
}
