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
    /// Put a machine on the mesh over plain ssh, both ways: bn machine join <name> <user@host[:port]>.
    ///
    /// Writes a `bn mesh` block into ~/.ssh/config, trusts the machine's key here, and holds a
    /// reverse forward open (tmux window bn-tunnels:mesh-<name>) so it can ssh back even when this
    /// machine is behind NAT. Then `bn agent msg` reaches agents on either side. Runs the `mesh`
    /// job (config/jobs/mesh.sh).
    Join {
        /// The name to reach it by (`ssh <name>`), letters, digits and dashes.
        name: String,
        /// Where it is: user@host[:port]. This machine's key must already log in there.
        target: String,
        /// This machine is reachable directly at user@host[:port]; no reverse forward.
        #[arg(long, conflicts_with = "no_back")]
        back: Option<String>,
        /// One way only: the machine never reaches back here.
        #[arg(long)]
        no_back: bool,
    },
    /// Take a machine off the mesh: its ssh block here, its key, the reverse forward, and this
    /// machine's block there.
    Leave {
        /// The name it joined under.
        name: String,
    },
    /// Every mesh node joined from here: whether ssh works each way, its bn, its live agents.
    Mesh,
}
