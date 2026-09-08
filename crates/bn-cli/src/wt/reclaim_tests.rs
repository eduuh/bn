//! The command the keystroke re-enters itself with. Pure over strings — no tmux, no git.

use super::*;


#[test]
fn the_reclaim_command_reenters_this_binary_on_its_reclaim_half() {
    let cmd = reclaim_cmd(Path::new("/wt/bn/_pool/4"), false);
    assert!(cmd.contains("wt close --reclaim"), "{cmd}");
    assert!(cmd.contains("'/wt/bn/_pool/4'"), "the path is quoted: {cmd}");
    assert!(
        !cmd.contains("--confirmed"),
        "an unconfirmed close must not authorise a discard later: {cmd}"
    );
}

#[test]
fn a_confirmed_close_carries_the_confirmation_into_the_reclaim_command() {
    // The reclaim re-derives its plan when it runs, so the answer the human gave has to travel with
    // it — otherwise a slot they agreed to discard comes back as "has uncommitted work" and sits
    // leased forever.
    let cmd = reclaim_cmd(Path::new("/wt/bn/_pool/4"), true);
    assert!(cmd.ends_with("--confirmed"), "{cmd}");
}

#[test]
fn a_path_with_a_quote_in_it_cannot_break_out_of_the_reclaim_command() {
    let cmd = reclaim_cmd(Path::new("/wt/bn/it's; rm -rf /"), false);
    assert!(cmd.contains(r"'/wt/bn/it'\''s; rm -rf /'"), "{cmd}");
}
