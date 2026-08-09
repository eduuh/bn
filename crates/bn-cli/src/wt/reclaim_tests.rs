//! What the deferred half hands to the queue. Pure over strings — no queue, no tmux, no git.

use super::*;


#[test]
fn the_queued_command_reenters_this_binary_on_its_deferred_half() {
    let cmd = reclaim_cmd(Path::new("/wt/bn/_pool/4"), false);
    assert!(cmd.contains("wt close --reclaim"), "{cmd}");
    assert!(cmd.contains("'/wt/bn/_pool/4'"), "the path is quoted: {cmd}");
    assert!(
        !cmd.contains("--confirmed"),
        "an unconfirmed close must not authorise a discard later: {cmd}"
    );
}

#[test]
fn a_confirmed_close_carries_the_confirmation_into_the_queued_command() {
    // The reclaim re-derives its plan when it runs, so the answer the human gave has to travel with
    // it — otherwise a slot they agreed to discard comes back as "has uncommitted work" and sits
    // leased forever.
    let cmd = reclaim_cmd(Path::new("/wt/bn/_pool/4"), true);
    assert!(cmd.ends_with("--confirmed"), "{cmd}");
}

#[test]
fn a_path_with_a_quote_in_it_cannot_break_out_of_the_queued_command() {
    let cmd = reclaim_cmd(Path::new("/wt/bn/it's; rm -rf /"), false);
    assert!(cmd.contains(r"'/wt/bn/it'\''s; rm -rf /'"), "{cmd}");
}

#[test]
fn the_reclaim_entry_is_express_so_it_never_queues_behind_an_agent() {
    // Reclaiming a checkout hands a resource *back*; making it wait for a coding slot would be
    // exactly backwards. A shell executor needs no worktree, so the tick runs it inline.
    let spec = reclaim_spec("task".to_string(), "bn wt close --reclaim".to_string());
    assert_eq!(spec.kind, BacklogKind::Cleanup);
    assert!(matches!(spec.exec, ExecSpec::Shell { .. }));
    let entry = bn_core::backlog::BacklogEntry::new("bn", "reclaim worktree bn/feat", &spec, 0);
    assert!(entry.is_express(), "a reclaim must not take an agent lease");
    assert_eq!(
        bn_core::queue::classify::classify(&entry).subtype,
        bn_core::queue::classify::QueueSubtype::Maintenance,
        "it is housekeeping, and should read as housekeeping"
    );
}
