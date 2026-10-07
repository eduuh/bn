use super::*;

#[test]
fn seats_normalize_like_resolver_targets() {
    assert_eq!(normalize_seat("BN:feat/x").unwrap(), "bn:feat-x");
    assert_eq!(normalize_seat("Human:Eduuh").unwrap(), "human:eduuh");
    for bad in ["bn", ":x", "bn:", "a:b:c", ""] {
        assert!(normalize_seat(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn reopening_a_db_keeps_its_schema() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("nested").join("coord.db");
    drop(CoordDb::open(&path).unwrap());
    let db = CoordDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
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
