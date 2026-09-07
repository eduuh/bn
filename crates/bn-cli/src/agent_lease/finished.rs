//! "Is everyone in this slot finished?" — the one question [`super::any_live_pane_in`] cannot
//! answer, split out of `agent_lease.rs` for the 1000-line ratchet.
use std::path::Path;

/// Whether everyone in `worktree` has finished: at least one live pane sits there, and every one
/// of them is in a window whose agent stamped `@bn_agent=done`.
///
/// The question `bn dispatch` asks before refusing a same-branch pool slot as occupied. A Copilot
/// that reported done keeps its process and its pane, so by [`any_live_pane_in`]'s test the slot
/// is taken — and the queue's launch for that branch was refused, backed off and refused again for
/// hours, while the only thing in the slot was a finished agent waiting for someone to close its
/// window. `done` is the agent's own word that the slot is free to take over.
///
/// `false` for an empty directory: nothing there is a different answer, and the lease ledger's.
pub(crate) fn only_finished_agents_in(worktree: &Path) -> bool {
    let format = "#{pane_current_path}\t#{pane_dead}\t#{@bn_agent}";
    crate::proc::tmux_raw(&["list-panes", "-a", "-F", format])
        .is_some_and(|output| only_finished(&output, worktree))
}

/// The pure half of [`only_finished_agents_in`], over `list-panes` output.
fn only_finished(output: &str, worktree: &Path) -> bool {
    let mut any = false;
    for line in output.lines() {
        let mut f = line.split('\t');
        let (Some(cwd), Some(dead)) = (f.next(), f.next()) else {
            continue;
        };
        if dead.trim() != "0" || !Path::new(cwd).starts_with(worktree) {
            continue;
        }
        any = true;
        if f.next().unwrap_or_default().trim() != "done" {
            return false;
        }
    }
    any
}

#[cfg(test)]
mod finished_tests {
    use super::*;

    #[test]
    fn a_slot_whose_only_occupant_reported_done_is_finished() {
        let out = "/wt/bn/_pool/6\t0\tdone\n/wt/bn/_pool/5\t0\tworking\n";
        assert!(only_finished(out, Path::new("/wt/bn/_pool/6")));
        assert!(!only_finished(out, Path::new("/wt/bn/_pool/5")));
    }

    /// One live agent among the finished ones keeps the slot occupied; a dead pane does not count
    /// either way.
    #[test]
    fn one_working_pane_keeps_the_slot_and_a_dead_one_says_nothing() {
        let out = "/wt/bn/_pool/6\t0\tdone\n/wt/bn/_pool/6/sub\t0\tworking\n";
        assert!(!only_finished(out, Path::new("/wt/bn/_pool/6")));
        let out = "/wt/bn/_pool/6\t1\tworking\n/wt/bn/_pool/6\t0\tdone\n";
        assert!(only_finished(out, Path::new("/wt/bn/_pool/6")));
    }

    /// An empty slot is not "finished" — nobody was there. And a pane with no stamp at all is a
    /// shell someone has open, which is exactly the occupant this must not evict.
    #[test]
    fn an_empty_or_unstamped_slot_is_not_finished() {
        assert!(!only_finished("", Path::new("/wt/bn/_pool/6")));
        let out = "/wt/bn/_pool/6\t0\t\n";
        assert!(!only_finished(out, Path::new("/wt/bn/_pool/6")));
    }
}
