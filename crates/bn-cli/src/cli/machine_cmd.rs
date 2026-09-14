//! `bn machine` — the hosts this notes tree holds state for, and repairing a split between two.
//!
//! Machine-scoped state (the event log, metrics, dojo decks) lives under
//! `<personal_root>/<machine>/`. That name is pinned now, but a host whose identity drifted before
//! the pin left two directories behind, each with a partial history.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum MachineCmd {
    /// Every host this notes tree holds state for, newest activity first — the answer to "did
    /// this laptop write history under more than one name?": bn machine list.
    List,
    /// Fold another host's append-only history into this machine's: bn machine merge <from>.
    ///
    /// Merges only the JSONL histories (event log, metrics, dojo decks), interleaved by timestamp
    /// and deduped against what is already here. Live stores and point-in-time snapshots are
    /// left alone — there is no correct way to combine two of those — and the source directory is
    /// never modified or deleted.
    Merge {
        /// The machine name to merge *from* (as shown by `bn machine list`).
        from: String,
        /// Print what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}
