//! Compaction discipline. A compaction is lossy, and what comes back believes it knows everything
//! and does not. So before one, the seat writes a **recap** (its own words plus a state block bn
//! composes from the note) and a handoff; after one, an **audit** puts the note, the recap and
//! the proof in front of it before it acts. Both ends are logged in `compactions.jsonl` beside the
//! note, and the log is where the honest outcome of a restore comes from.

mod log;
mod recap;

pub use log::{append, entries, outcome_for_restore, Entry, Outcome, Phase, PREP_FRESH_SECS};
pub use recap::{compose, recap_path, write_recap};

#[cfg(test)]
mod tests;
