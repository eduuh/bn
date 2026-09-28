use super::*;
use crate::clock::FixedClock;

const T0: i64 = 1_790_000_000;

fn db() -> CoordDb {
    CoordDb::open_in_memory().expect("in-memory db")
}

fn new(dest: &str, body: &str) -> NewItem {
    NewItem {
        dest: dest.into(),
        source: "bn:main".into(),
        body: body.into(),
        ..Default::default()
    }
}

fn clock(offset: i64) -> FixedClock {
    FixedClock::at(T0 + offset)
}

#[test]
fn seats_normalize_like_resolver_targets() {
    assert_eq!(normalize_seat("BN:feat/x").unwrap(), "bn:feat-x");
    assert_eq!(normalize_seat("Human:Eduuh").unwrap(), "human:eduuh");
    for bad in ["bn", ":x", "bn:", "a:b:c", ""] {
        assert!(normalize_seat(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn qids_round_trip() {
    assert_eq!(qid(12), "q12");
    assert_eq!(parse_qid("q12"), Some(12));
    assert_eq!(parse_qid("12"), Some(12));
    assert_eq!(parse_qid("q0"), None);
    assert_eq!(parse_qid("t3"), None);
}

#[test]
fn create_is_idempotent_by_key() {
    let mut db = db();
    let item = NewItem {
        idem_key: Some("k1".into()),
        ..new("bn:feat-y", "try it")
    };
    let (a, created_a) = db.create(&clock(0), &item).unwrap();
    let (b, created_b) = db.create(&clock(5), &item).unwrap();
    assert!(created_a && !created_b);
    assert_eq!(a, b);
    assert_eq!(a.state, "pending");
    assert_eq!(db.transitions(a.num).unwrap().len(), 1);
}

#[test]
fn create_refuses_an_empty_body_or_a_bad_seat() {
    let mut db = db();
    assert!(db.create(&clock(0), &new("bn:y", "  ")).is_err());
    assert!(db.create(&clock(0), &new("not-a-seat", "x")).is_err());
}

#[test]
fn claim_sets_owner_and_a_deadline_from_the_tier() {
    let mut db = db();
    let (item, _) = db
        .create(
            &clock(0),
            &NewItem {
                tier: Tier::Fast,
                ..new("bn:y", "x")
            },
        )
        .unwrap();
    let claimed = db.claim(&clock(10), item.num, "bn:y").unwrap();
    assert_eq!(claimed.state, "in-progress");
    assert_eq!(claimed.owner.as_deref(), Some("bn:y"));
    assert_eq!(claimed.closure_required_at, Some(T0 + 10 + 30 * 60));
    // A retried claim by the owner is a no-op, not an error.
    assert_eq!(db.claim(&clock(20), item.num, "bn:y").unwrap(), claimed);
    assert!(!claimed.is_overdue(T0 + 10 + 30 * 60));
    assert!(claimed.is_overdue(T0 + 11 + 30 * 60));
}

#[test]
fn only_the_destination_or_a_person_may_claim() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    assert!(matches!(
        db.claim(&clock(1), item.num, "bn:z"),
        Err(CoordError::Forbidden(_))
    ));
    assert!(db.claim(&clock(1), item.num, "human:eduuh").is_ok());
}

#[test]
fn a_close_must_say_where_the_work_went() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    let err = db
        .close(&clock(1), item.num, "bn:y", CloseOutcome::Done, " ", None)
        .unwrap_err();
    assert!(matches!(err, CoordError::Invalid(_)));
    let done = db
        .close(
            &clock(1),
            item.num,
            "bn:y",
            CloseOutcome::Done,
            "merged in #12",
            Some("https://example/pr/12"),
        )
        .unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.closure_note.as_deref(), Some("merged in #12"));
    // Terminal is terminal.
    assert!(matches!(
        db.close(&clock(2), item.num, "bn:y", CloseOutcome::Failed, "x", None),
        Err(CoordError::Illegal { .. })
    ));
    assert!(matches!(
        db.claim(&clock(2), item.num, "bn:y"),
        Err(CoordError::Illegal { .. })
    ));
}

#[test]
fn handoff_closes_the_source_and_opens_the_destination_together() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "build it")).unwrap();
    db.claim(&clock(1), item.num, "bn:y").unwrap();
    let h = Handoff {
        to: "bn:review".into(),
        note: "ready for review".into(),
        ..Default::default()
    };
    let (src, dst) = db.handoff(&clock(2), item.num, "bn:y", &h).unwrap();
    assert_eq!(src.state, "handed-off");
    assert_eq!(src.next.as_deref(), Some(dst.id.as_str()));
    assert_eq!(dst.parent.as_deref(), Some(src.id.as_str()));
    assert_eq!(
        (dst.dest.as_str(), dst.state.as_str()),
        ("bn:review", "pending")
    );
    assert_eq!(dst.source, "bn:y");
    assert_eq!(dst.body, "build it");
    let verbs: Vec<String> = db
        .transitions(src.num)
        .unwrap()
        .into_iter()
        .map(|t| t.verb)
        .collect();
    assert_eq!(verbs, ["create", "claim", "handoff"]);
    assert!(src.closure_note.unwrap().contains(&dst.id));
}

#[test]
fn handoff_and_complete_ends_the_source_done() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    let h = Handoff {
        to: "bn:z".into(),
        note: "my part is done".into(),
        complete: true,
        ..Default::default()
    };
    let (src, _) = db.handoff(&clock(1), item.num, "bn:y", &h).unwrap();
    assert_eq!(src.state, "done");
}

#[test]
fn a_refused_handoff_leaves_no_half_written_row() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    // Refused inside the transaction (after it began): same destination.
    let same = Handoff {
        to: "bn:y".into(),
        note: "loop".into(),
        ..Default::default()
    };
    assert!(db.handoff(&clock(1), item.num, "bn:y", &same).is_err());
    // Refused inside the transaction: not this seat's row.
    let other = Handoff {
        to: "bn:z".into(),
        note: "steal".into(),
        ..Default::default()
    };
    assert!(matches!(
        db.handoff(&clock(1), item.num, "bn:q", &other),
        Err(CoordError::Forbidden(_))
    ));
    let all = db
        .list(&ListFilter {
            states: QState::ALL.to_vec(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 1, "no destination row was created");
    assert_eq!(all[0].state, "pending");
    assert_eq!(db.transitions(item.num).unwrap().len(), 1);
}

#[test]
fn unclaim_needs_the_owner_and_a_reason() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    db.claim(&clock(1), item.num, "bn:y").unwrap();
    assert!(db.unclaim(&clock(2), item.num, "bn:y", "").is_err());
    let back = db
        .unclaim(&clock(2), item.num, "bn:y", "wrong seat for this")
        .unwrap();
    assert_eq!(back.state, "pending");
    assert_eq!(back.owner, None);
    assert_eq!(back.closure_required_at, None);
}

#[test]
fn update_appends_history_without_changing_state() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    let after = db
        .update(&clock(3), item.num, "bn:y", "half way", None)
        .unwrap();
    assert_eq!(after.state, "pending");
    let last = db.transitions(item.num).unwrap().pop().unwrap();
    assert_eq!(
        (last.verb.as_str(), last.note.as_deref()),
        ("note", Some("half way"))
    );
}

#[test]
fn list_filters_by_seat_state_and_overdue() {
    let mut db = db();
    let (a, _) = db.create(&clock(0), &new("bn:y", "a")).unwrap();
    let (b, _) = db
        .create(
            &clock(0),
            &NewItem {
                tier: Tier::Fast,
                ..new("bn:y", "b")
            },
        )
        .unwrap();
    let (_c, _) = db.create(&clock(0), &new("bn:z", "c")).unwrap();
    db.claim(&clock(0), b.num, "bn:y").unwrap();

    let mine = |seat: &str| ListFilter {
        seat: Some(seat.into()),
        ..Default::default()
    };
    assert_eq!(db.list(&mine("bn:y")).unwrap().len(), 2);
    assert_eq!(db.list(&mine("bn:z")).unwrap().len(), 1);
    let pending = db
        .list(&ListFilter {
            states: vec![QState::Pending],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pending.len(), 2);
    let overdue = |now| ListFilter {
        overdue_at: Some(now),
        ..Default::default()
    };
    assert!(db.list(&overdue(T0 + 60)).unwrap().is_empty());
    let late = db.list(&overdue(T0 + 31 * 60)).unwrap();
    assert_eq!(late.iter().map(|q| q.num).collect::<Vec<_>>(), [b.num]);
    // Closed rows drop out of the default (open) listing.
    db.close(
        &clock(1),
        a.num,
        "bn:y",
        CloseOutcome::Canceled,
        "dup",
        None,
    )
    .unwrap();
    assert_eq!(db.list(&mine("bn:y")).unwrap().len(), 1);
}

#[test]
fn two_connections_racing_to_claim_get_exactly_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("coord.db");
    let id = {
        let mut db = CoordDb::open(&path).unwrap();
        db.create(&clock(0), &new("human:a", "x")).unwrap().0.num
    };
    // Collect first: both threads must be spawned before either is joined, or it isn't a race.
    #[allow(clippy::needless_collect)]
    let racers: Vec<_> = ["human:a", "human:b"]
        .into_iter()
        .map(|who| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut db = CoordDb::open(&path).unwrap();
                db.claim(&clock(1), id, who).is_ok()
            })
        })
        .collect();
    let wins = racers
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
}

#[test]
fn reopening_a_db_keeps_its_rows_and_schema() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("nested").join("coord.db");
    let id = {
        let mut db = CoordDb::open(&path).unwrap();
        db.create(&clock(0), &new("bn:y", "x")).unwrap().0.num
    };
    let db = CoordDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(db.get(id).unwrap().body, "x");
}

#[test]
fn db_path_honours_the_override() {
    let _lock = crate::test_support::env_lock();
    let _g = crate::test_support::EnvGuard::set("BN_COORD_DB", "/tmp/x/coord.db");
    assert_eq!(db_path(), std::path::PathBuf::from("/tmp/x/coord.db"));
}

#[test]
fn the_acting_seat_prefers_flag_then_env_then_worktree_then_user() {
    assert_eq!(
        acting_seat(Some("bn:x"), Some("bn:y"), Some("bn:z"), "me").unwrap(),
        "bn:x"
    );
    assert_eq!(
        acting_seat(None, Some("bn:y"), Some("bn:z"), "me").unwrap(),
        "bn:y"
    );
    assert_eq!(
        acting_seat(Some(" "), None, Some("bn:z"), "me").unwrap(),
        "bn:z"
    );
    assert_eq!(
        acting_seat(None, None, None, "Eduuh").unwrap(),
        "human:eduuh"
    );
    // A typo'd explicit seat is an error, never a silent fall-through to someone else.
    assert!(acting_seat(Some("oops"), None, Some("bn:z"), "me").is_err());
}

mod mirror {
    use super::*;
    use crate::model::Note;
    use crate::resolver::{Config, RepoBranch, Resolved};

    fn with_note(temp: &std::path::Path) -> (Config, Resolved) {
        let cfg = crate::test_support::config(temp);
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-y".into(),
        };
        std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
        let note = Note::new(
            RepoBranch {
                repo: "bn",
                branch: "feat-y",
            },
            "2026-09-24",
            "testhost",
        );
        crate::store::save(&note, &cfg.note_file(&ctx)).unwrap();
        (cfg, ctx)
    }

    fn todos(cfg: &Config, ctx: &Resolved) -> Vec<crate::model::Item> {
        crate::store::read(&cfg.note_file(ctx)).unwrap().front.todos
    }

    #[test]
    fn a_row_for_an_agent_becomes_one_todo_and_closing_ticks_it() {
        let temp = tempfile::tempdir().unwrap();
        let (cfg, ctx) = with_note(temp.path());
        let mut db = db();
        let (item, _) = db
            .create(&clock(0), &new("bn:feat/y", "review the diff"))
            .unwrap();

        let out = open_mirror(&cfg, &item, "2026-09-24");
        let MirrorOutcome::Ok(todo) = out.clone() else {
            panic!("expected a todo, got {out:?}");
        };
        assert_eq!(
            todos(&cfg, &ctx)[0].text,
            "[q1] review the diff (from bn:main)"
        );
        // Mirroring again adds nothing.
        assert_eq!(
            open_mirror(&cfg, &item, "2026-09-24"),
            MirrorOutcome::Ok(todo.clone())
        );
        assert_eq!(todos(&cfg, &ctx).len(), 1);

        db.set_mirror(item.num, &out).unwrap();
        let closed = db
            .close(
                &clock(1),
                item.num,
                "bn:feat-y",
                CloseOutcome::Done,
                "shipped",
                None,
            )
            .unwrap();
        assert_eq!(closed.mirror_todo.as_deref(), Some(todo.as_str()));
        assert_eq!(
            close_mirror(&cfg, &closed, "2026-09-24"),
            MirrorOutcome::Ok(todo.clone())
        );
        assert_eq!(todos(&cfg, &ctx)[0].done, Some(true));
        // Ticking twice is fine.
        assert_eq!(
            close_mirror(&cfg, &closed, "2026-09-24"),
            MirrorOutcome::Ok(todo)
        );
    }

    #[test]
    fn no_note_or_a_human_seat_is_skipped_and_nothing_is_created() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(temp.path());
        let mut db = db();
        let (agent, _) = db.create(&clock(0), &new("bn:nobody", "x")).unwrap();
        let (person, _) = db.create(&clock(0), &new("human:eduuh", "x")).unwrap();
        assert_eq!(
            open_mirror(&cfg, &agent, "2026-09-24"),
            MirrorOutcome::Skipped
        );
        assert_eq!(
            open_mirror(&cfg, &person, "2026-09-24"),
            MirrorOutcome::Skipped
        );
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "nobody".into(),
        };
        assert!(
            !cfg.note_file(&ctx).exists(),
            "a mirror never creates a note"
        );
    }

    #[test]
    fn long_bodies_are_cut_in_the_todo_but_kept_on_the_row() {
        let mut db = db();
        let body = format!("{}\nsecond line", "x".repeat(200));
        let (item, _) = db.create(&clock(0), &new("bn:y", &body)).unwrap();
        let text = crate::coord::mirror::todo_text(&item);
        assert!(text.starts_with("[q1] xxx"));
        assert!(text.contains("…") && !text.contains("second line"));
        assert_eq!(db.get(item.num).unwrap().body, body);
    }
}

mod sweeping {
    use super::*;
    use crate::coord::sweep::{self, plan, KEY_PREFIX, NUDGE_EVERY_SECS, NUDGE_MAX};

    fn open_rows(db: &CoordDb) -> Vec<QItem> {
        db.list(&ListFilter::default()).unwrap()
    }

    fn run(db: &CoordDb, live: &[&str], now: i64) -> sweep::Plan {
        let live: Vec<String> = live.iter().map(|s| s.to_string()).collect();
        plan(
            &open_rows(db),
            &|id| db.nudged_at(id).unwrap(),
            &|id| db.has_wake(id).unwrap(),
            &live,
            now,
        )
    }

    #[test]
    fn a_new_agent_row_is_nudged_then_backed_off_then_left_alone() {
        let mut db = db();
        let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
        assert_eq!(run(&db, &[], T0).nudge, [item.num]);
        db.record_nudge(item.num, T0).unwrap();
        assert!(run(&db, &[], T0 + 60).nudge.is_empty(), "backing off");
        assert_eq!(run(&db, &[], T0 + NUDGE_EVERY_SECS).nudge, [item.num]);
        for i in 1..NUDGE_MAX {
            db.record_nudge(item.num, T0 + i * NUDGE_EVERY_SECS)
                .unwrap();
        }
        assert!(run(&db, &[], T0 + 10 * NUDGE_EVERY_SECS).nudge.is_empty());
        // A nudge is history, not a state change.
        assert_eq!(db.get(item.num).unwrap().state, "pending");
        assert_eq!(
            db.transitions(item.num).unwrap().len() as i64,
            1 + NUDGE_MAX
        );
    }

    #[test]
    fn humans_are_alerted_not_typed_at() {
        let mut db = db();
        let (item, _) = db
            .create(&clock(0), &new("human:eduuh", "approve?"))
            .unwrap();
        let p = run(&db, &[], T0);
        assert!(p.nudge.is_empty());
        assert_eq!(p.raise.len(), 1);
        assert_eq!(p.raise[0].key, sweep::human_key(&item));
    }

    #[test]
    fn overdue_claims_and_unclaimed_rows_raise_and_clear() {
        let mut db = db();
        let fast = |d: &str| NewItem {
            tier: Tier::Fast,
            ..new(d, "x")
        };
        let (claimed, _) = db.create(&clock(0), &fast("bn:y")).unwrap();
        let (ignored, _) = db.create(&clock(0), &fast("bn:z")).unwrap();
        db.claim(&clock(0), claimed.num, "bn:y").unwrap();

        assert!(run(&db, &[], T0 + 60).raise.is_empty());
        let late = run(&db, &[], T0 + 31 * 60);
        let keys: Vec<&str> = late.raise.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["queue:overdue:q1", "queue:overdue:q2"]);
        assert!(late.raise.iter().all(|a| a.severity == "warn"));

        // Once closed, the next sweep acks its alert and leaves others (and non-queue keys) alone.
        db.close(
            &clock(40 * 60),
            claimed.num,
            "bn:y",
            CloseOutcome::Done,
            "done",
            None,
        )
        .unwrap();
        let after = run(
            &db,
            &["queue:overdue:q1", "queue:overdue:q2", "attn:other"],
            T0 + 41 * 60,
        );
        assert_eq!(after.ack, ["queue:overdue:q1"]);
        let _ = ignored;
    }

    #[test]
    fn critical_rows_alert_urgent() {
        let mut db = db();
        let (item, _) = db
            .create(
                &clock(0),
                &NewItem {
                    tier: Tier::Critical,
                    ..new("bn:y", "x")
                },
            )
            .unwrap();
        db.claim(&clock(0), item.num, "bn:y").unwrap();
        let p = run(&db, &[], T0 + 2 * 60 * 60);
        assert_eq!(p.raise[0].severity, "urgent");
    }

    #[test]
    fn failed_mirrors_are_retried() {
        let mut db = db();
        let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
        db.set_mirror(item.num, &MirrorOutcome::Failed("locked".into()))
            .unwrap();
        assert_eq!(run(&db, &[], T0).mirror_retry, [item.num]);
        assert!(KEY_PREFIX.ends_with(':'));
    }

    #[test]
    fn the_nudge_informs_and_says_how_to_claim() {
        let mut db = db();
        let (item, _) = db
            .create(&clock(0), &new("bn:y", "review the diff\nmore"))
            .unwrap();
        assert_eq!(
            sweep::nudge_text(&item),
            "[bn queue q1 from bn:main] review the diff — take it with: bn queue claim q1"
        );
    }
}

#[test]
fn a_failed_close_mirror_is_still_found_for_retry() {
    let mut db = db();
    let (item, _) = db.create(&clock(0), &new("bn:y", "x")).unwrap();
    db.set_mirror(item.num, &MirrorOutcome::Ok("t1".into()))
        .unwrap();
    db.close(
        &clock(1),
        item.num,
        "bn:y",
        CloseOutcome::Done,
        "done",
        None,
    )
    .unwrap();
    db.set_mirror(item.num, &MirrorOutcome::Failed("locked".into()))
        .unwrap();
    let stuck = db.closed_with_pending_mirror().unwrap();
    assert_eq!(stuck.len(), 1);
    assert_eq!(
        stuck[0].mirror_todo.as_deref(),
        Some("t1"),
        "keeps the todo to tick"
    );
    let plan = crate::coord::sweep::plan(&stuck, &|_| None, &|_| false, &[], T0);
    assert_eq!(plan.mirror_retry, [item.num]);
    assert!(plan.raise.is_empty() && plan.nudge.is_empty());
}
