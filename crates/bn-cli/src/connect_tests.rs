//! `bn connect --window` attaches an existing `host:<alias>` session when one is listed and only
//! creates a new one otherwise; the lookup that decides between the two is pure.
//!
//! The not-inside-tmux branch itself is not covered: it execs `tmux attach-session` /
//! `tmux new-session` directly, so it needs a real tmux server and an ssh alias.

use super::session_id_from_listing;

const LISTING: &str = "main\t$0\nhost:devbox\t$3\nhost:devbox-2\t$5\n";

#[test]
fn an_existing_session_resolves_to_its_id() {
    assert_eq!(
        session_id_from_listing(LISTING, "host:devbox"),
        Some("$3".to_string())
    );
    assert_eq!(
        session_id_from_listing(LISTING, "host:devbox-2"),
        Some("$5".to_string())
    );
}

#[test]
fn a_missing_session_means_create_a_new_one() {
    assert_eq!(session_id_from_listing(LISTING, "host:other"), None);
    assert_eq!(session_id_from_listing("", "host:devbox"), None);
}

#[test]
fn the_name_must_match_exactly_and_malformed_rows_are_skipped() {
    let listing = "host:devbox-old\t$1\nno-tab-here\nHOST:DEVBOX\t$2\n";
    assert_eq!(session_id_from_listing(listing, "host:devbox"), None);
}
