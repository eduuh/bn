use super::*;
use bn_core::clock::FixedClock;

const T0: i64 = 1_790_000_000;

/// `bn agent checkin 15 "…"` is a one-shot reminder to the target, due 15 minutes out, with an
/// exact-time timer armed to fire that very watchdog.
#[test]
fn a_checkin_registers_a_one_shot_reminder_and_arms_its_timer() {
    let mut db = CoordDb::open_in_memory().unwrap();
    let mut armed: Vec<(i64, Vec<String>)> = Vec::new();
    let wd = checkin_in(
        &mut db,
        &FixedClock::at(T0),
        "bn:feat",
        15,
        "did the tests pass?",
        "bn:main",
        &mut |mins, args| {
            armed.push((mins, args.iter().map(|a| a.to_string()).collect()));
            true
        },
    )
    .unwrap();

    assert_eq!(wd.kind(), Some(Kind::Reminder));
    assert_eq!(wd.target, "bn:feat");
    assert_eq!(wd.text.as_deref(), Some("did the tests pass?"));
    assert_eq!(wd.created_by, "bn:main");
    assert_eq!(wd.next_due_at, T0 + 15 * 60);
    assert_eq!(wd.every_secs, None);
    assert_eq!(wd.max_fires, Some(1));
    assert_eq!(
        armed,
        vec![(15, vec!["watchdog".into(), "fire".into(), wd.id.clone()])]
    );

    // The row is the durable record: it reads back from the DB unchanged.
    let stored = db.watchdog(wd.num).unwrap();
    assert_eq!(stored.next_due_at, wd.next_due_at);
    assert!(stored.is_active());
}

/// A repeating watchdog rides the tick; only one-shots get an exact-time timer.
#[test]
fn a_repeating_watchdog_arms_no_timer() {
    let mut db = CoordDb::open_in_memory().unwrap();
    let mut armed = 0;
    register_with(
        &mut db,
        &FixedClock::at(T0),
        NewWatchdog {
            kind: Kind::Reminder,
            target: "bn:feat".into(),
            text: Some("ping".into()),
            spec: Spec::default(),
            first_in_secs: 600,
            every_secs: Some(600),
            expires_in_secs: None,
            max_fires: None,
            created_by: "bn:main".into(),
        },
        &mut |_, _| {
            armed += 1;
            true
        },
    )
    .unwrap();
    assert_eq!(armed, 0);
}

/// The `watch` job's watchdog pass: a DB that will not open is skipped, not an error, and a
/// check-in that is not due yet is left alone.
#[test]
fn the_watch_job_ticks_an_open_db_and_skips_a_missing_one() {
    let cfg = Config::default();
    let failed = tick_opened(&cfg, Err(CoordError::Invalid("no db".into())), T0);
    assert!(failed.is_none());

    let mut db = CoordDb::open_in_memory().unwrap();
    let wd = checkin_in(&mut db, &FixedClock::at(T0), "bn:feat", 15, "later", "bn:main", &mut |_, _| true)
        .unwrap();
    let report = tick_opened(&cfg, Ok(db), T0 + 60).expect("an open DB is ticked");
    assert!(report.fired.is_empty() && report.errors.is_empty(), "{}", report.summary());
    assert!(report.deferred.is_empty() && report.skipped.is_empty(), "{}", wd.id);
}
