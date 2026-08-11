//! Whose work a task is — yours, or the machine's own bookkeeping.
//!
//! Every enqueue must name a task (#1001). That invariant is what turns "work only ever starts from
//! a task" from a convention into something a test can assert, and it is worth keeping. But it was
//! read as though every enqueue were a person typing one, and almost none of them are: a cron
//! firing, a worktree being reclaimed, a pool being warmed, a repo setup agent whose prompt the
//! dashboard rendered from a template. Each minted a line indistinguishable from work you chose.
//!
//! The day's actionable list came back 96 rows deep — 55 crons, 15 reclaims, 12 generated setup
//! prompts (each carrying its entire multi-paragraph prompt as its title), 11 queue jobs — and 3
//! tasks that were actually a person's. A list that answers "what could I do today" that way has
//! stopped answering.
//!
//! So the marker, and one predicate for it, read in three places that would otherwise each spell
//! out the same string comparison:
//!
//! - `bn task`'s day view folds machine tasks out (`--all` still shows them);
//! - [`crate::queue::classify`] calls the entry autonomous, so it stops competing for the watched
//!   coding lane;
//! - [`crate::pipeline::gate`] admits it without SMART judgement — a generated prompt has no author
//!   to hand a refusal back to.
//!
//! **Marked at minting, never inferred.** The one previous attempt to infer this class of thing read
//! the word "warm" out of a repo setup prompt's own instructions and froze priority-100 work a
//! person had just clicked (see `queue::classify::is_warming`). A task is the machine's because the
//! producer said so when it created it, and for no other reason.

use crate::todo::Task;

/// Meta key marking a task the machine minted for itself — `auto:1`.
pub const AUTO_KEY: &str = "auto";

/// The value [`AUTO_KEY`] carries. A key with any other value is not a marker: `auto:0` reads as
/// "explicitly not", and anything else as a hand-edit nobody should have to guess the meaning of.
pub const AUTO_VALUE: &str = "1";

/// Whether the machine minted this task for its own bookkeeping.
///
/// It stays in the store — the enqueue named it and the trail is the point — but it is not work a
/// person chose, so the day view folds it out and the SMART gate does not judge it.
pub fn is_machine(task: &Task) -> bool {
    task.meta(AUTO_KEY).is_some_and(|v| v.trim() == AUTO_VALUE)
}

/// Stamp `task` as the machine's, in place.
///
/// `set_meta_raw` rather than `set_meta`: ownership is decided once, at minting, and is bookkeeping
/// rather than content. Stamping `mod:` for it would make a freshly minted machine task look
/// newer than a human copy of the same line under a union merge, for a write nobody made.
pub fn mark_machine(task: &mut Task) {
    task.set_meta_raw(AUTO_KEY, AUTO_VALUE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::TodoFile;

    #[test]
    fn only_an_explicit_marker_counts() {
        let file = TodoFile::parse(concat!(
            "refactor the goal write side id:t1 repo:bn mod:10\n",
            "run keep-warm id:t2 repo:machine auto:1 mod:20\n",
            "reclaim worktree byte_s/repo-setup id:t3 repo:byte_s auto:1 mod:30\n",
        ));
        let marked: Vec<&str> = file
            .tasks()
            .filter(|t| is_machine(t))
            .filter_map(|t| t.id())
            .collect();
        assert_eq!(marked, vec!["t2", "t3"]);
    }

    #[test]
    fn the_marker_is_read_never_guessed_from_the_words() {
        // A task is the machine's because it was *minted* that way. Inferring it from text is the
        // mistake `queue::classify::is_warming` already made once, when the word "warm" inside a
        // repo setup prompt's own instructions froze a person's priority-100 click for an away
        // window. "run keep-warm by hand" is a person's task and must stay on their list.
        let file = TodoFile::parse(concat!(
            "run keep-warm by hand because the cron is broken id:t9 repo:machine mod:10\n",
            "reclaim worktree bn/main id:t10 repo:bn auto:0 mod:20\n",
        ));
        assert!(!file.tasks().any(is_machine));
    }

    #[test]
    fn marking_is_idempotent_and_does_not_count_as_an_edit() {
        let file = TodoFile::parse("run keep-warm id:t2 repo:machine mod:20\n");
        let mut task = file.tasks().next().expect("one task").clone();
        assert!(!is_machine(&task));

        mark_machine(&mut task);
        assert!(is_machine(&task));
        assert_eq!(
            task.meta(crate::todo::MOD_KEY),
            Some("20"),
            "ownership is decided at minting, so it must not stamp `mod:` and win a union merge \
             against a copy someone actually edited"
        );

        mark_machine(&mut task);
        assert!(is_machine(&task));
    }
}
