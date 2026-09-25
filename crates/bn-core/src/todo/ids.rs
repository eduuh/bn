//! The `t<N>` handle side of [`TodoFile`]: minting the next id, healing collisions, and the id
//! floor a store that loads only part of the list uses to keep old ids from coming back.

use std::collections::HashSet;

use super::{Line, TodoFile};

fn parse(id: &str) -> Option<u64> {
    id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok())
}

impl TodoFile {
    /// The next `t<N>` id: one past the highest ever assigned, so ids of done/cancelled tasks are
    /// never reused (matching the note-section id discipline in [`crate::id`]). The highest is
    /// the larger of the tasks in the file and [`TodoFile::id_floor`].
    pub fn next_id(&self) -> String {
        format!("t{}", self.max_id() + 1)
    }

    /// The highest `t<N>` in the file, or the floor when that is higher.
    fn max_id(&self) -> u64 {
        self.tasks()
            .filter_map(|t| t.id().and_then(parse))
            .max()
            .unwrap_or(0)
            .max(self.id_floor)
    }

    /// The highest id known to have been assigned beyond the tasks in this file. Zero for a file
    /// that holds the whole list (`tasks.txt`).
    pub fn id_floor(&self) -> u64 {
        self.id_floor
    }

    /// Raise [`TodoFile::id_floor`] to `n` (never lowers it). A store whose load leaves tasks out
    /// (Forge lists only recently completed ones) sets it, so [`TodoFile::next_id`] never mints an
    /// id a left-out task still carries.
    pub fn raise_id_floor(&mut self, n: u64) {
        self.id_floor = self.id_floor.max(n);
    }

    /// Make every task's `id:` unique and present. A union-merge of the shared file (see
    /// `.gitattributes`) can concatenate two branches that each minted `t6`, and hand-added lines
    /// may carry no `id:` at all. In file order, the first valid unique `t<N>` is kept; any
    /// duplicate, malformed, or missing id is reassigned to a fresh `t<max+1>`. Deterministic, so
    /// repeated read-only `list`s show stable ids; a later mutation persists the healed numbering.
    pub fn heal_ids(&mut self) {
        let mut max = self.max_id();
        let mut seen: HashSet<String> = HashSet::new();
        for line in &mut self.lines {
            let Line::Task(t) = line else { continue };
            let keep = t
                .id()
                .filter(|id| parse(id).is_some() && !seen.contains(*id))
                .map(str::to_string);
            match keep {
                Some(id) => {
                    seen.insert(id);
                }
                None => {
                    max += 1;
                    let nid = format!("t{max}");
                    // Raw: healing runs on every `load`, and stamping `mod:` here would make a
                    // merely-listed stale copy outrank the real one in `dedupe_by_uid`.
                    t.set_meta_raw("id", &nid);
                    seen.insert(nid);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::Task;

    #[test]
    fn the_floor_keeps_next_id_past_tasks_the_file_does_not_hold() {
        let mut file = TodoFile::default();
        file.push_task(Task::new_todo("t3", "x", None, "", "", "2026-09-14"));
        assert_eq!(file.next_id(), "t4");
        file.raise_id_floor(40);
        file.raise_id_floor(12);
        assert_eq!(file.id_floor(), 40, "the floor never drops");
        assert_eq!(file.next_id(), "t41");

        file.push_task(Task::new_todo("t3", "dup", None, "", "", "2026-09-14"));
        file.heal_ids();
        let ids: Vec<&str> = file.tasks().filter_map(|t| t.id()).collect();
        assert_eq!(ids, ["t3", "t41"]);
    }
}
