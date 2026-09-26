use super::*;

// --- prune_decision ---------------------------------------------------

fn slot(over_size: bool, orphan: bool) -> SlotClass {
    SlotClass {
        leased: false,
        dirty: false,
        in_use: false,
        over_size,
        orphan,
    }
}

#[test]
fn prune_removes_over_size_and_orphan_only() {
    assert_eq!(prune_decision(slot(true, false)), Action::Remove);
    assert_eq!(prune_decision(slot(false, true)), Action::Remove);
    assert_eq!(
        prune_decision(slot(false, false)),
        Action::Skip("in-size warm slot")
    );
}

#[test]
fn prune_never_touches_protected_slots() {
    let mut c = slot(true, false);
    c.leased = true;
    assert_eq!(prune_decision(c), Action::Skip("leased"));
    let mut c = slot(true, false);
    c.in_use = true;
    assert_eq!(prune_decision(c), Action::Skip("in use"));
    let mut c = slot(true, false);
    c.dirty = true;
    assert_eq!(prune_decision(c), Action::Skip("uncommitted changes"));
}

#[test]
fn prune_renders_in_size_warm_slots() {
    let leases = Leases::default();
    let line = action_line(
        &leases,
        3,
        &slot(false, false),
        &Action::Skip("in-size warm slot"),
        false,
    );
    assert!(line.contains("[3] keep"));
    assert!(line.contains("in-size warm slot"));
}

// --- destroy_decision -------------------------------------------------

#[test]
fn destroy_skips_protected_without_flags() {
    let mut c = slot(false, false);
    c.leased = true;
    assert_eq!(
        destroy_decision(c, DestroyFlags::default()),
        Action::Skip("leased (use --include-leased)")
    );
}

#[test]
fn destroy_opts_in_with_flags() {
    let mut c = slot(false, false);
    c.leased = true;
    c.dirty = true;
    c.in_use = true;
    let f = DestroyFlags {
        include_leased: true,
        include_dirty: true,
        include_in_use: true,
    };
    assert_eq!(destroy_decision(c, f), Action::Remove);
}

#[test]
fn destroy_removes_plain_idle_slot() {
    assert_eq!(
        destroy_decision(slot(false, false), DestroyFlags::default()),
        Action::Remove
    );
}

// --- fmt_age ----------------------------------------------------------

#[test]
fn fmt_age_units() {
    assert_eq!(fmt_age(42), "42s");
    assert_eq!(fmt_age(5 * 60), "5m");
    assert_eq!(fmt_age(3 * 3600), "3h");
    assert_eq!(fmt_age(2 * 86_400), "2d");
    assert_eq!(fmt_age(-5), "0s");
}
