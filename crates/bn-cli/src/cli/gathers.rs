//! The gather subcommands — one per collector.
//!
//! Each names what it collects, never the agent that reads it. They are grouped here because they
//! are the same shape and the same idea: a deterministic read of some part of the world, printed
//! as JSON, judging none of it. An agent's manifest names one in its `gather:` line, and that line
//! is the only place the two meet.
use super::*;

#[derive(Subcommand, Debug)]
pub(crate) enum RadarCmd {
    /// Print the packet as JSON.
    Gather,
}

#[derive(Subcommand, Debug)]
pub(crate) enum GoalsAlignCmd {
    /// Print the packet as JSON.
    Gather,
}

#[derive(Subcommand, Debug)]
pub(crate) enum EstateCmd {
    /// Print the estate sweep as JSON. Offline: no network, and every section degrades on its own.
    Gather {
        /// Also include the currently-open attention items of this source, so a reader can tell a
        /// finding it has been carrying from one that appeared tonight.
        #[arg(long, value_name = "SOURCE")]
        carried_from: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum WeekCmd {
    /// Print the packet as JSON.
    Gather,
}

#[derive(Subcommand, Debug)]
pub(crate) enum WeaknessCmd {
    /// Print the packet as JSON.
    Gather {
        /// Whose past cards form the continuity signal — the agent that will read this packet.
        /// Passed from its own `gather:` line so a rename cannot silently blind the carry-or-drop
        /// decision.
        #[arg(long, default_value = "staff-curriculum")]
        for_agent: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum MergedPrsCmd {
    /// Print the packet as JSON. Offline discovery; only the diff fetch touches the network.
    Gather,
}

#[derive(Subcommand, Debug)]
pub(crate) enum BriefingCmd {
    /// Print the packet as JSON.
    Gather {
        /// Which run to gather for: `am` looks back at the night and yesterday, `pm` looks at the
        /// day just spent. Defaults to the slot the local clock is in.
        #[arg(long, value_enum)]
        slot: Option<BriefingSlot>,
    },
}

/// The two daily runs, as the CLI spells them.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum BriefingSlot {
    Am,
    Pm,
}

impl From<BriefingSlot> for bn_core::briefing::Slot {
    fn from(s: BriefingSlot) -> Self {
        match s {
            BriefingSlot::Am => bn_core::briefing::Slot::Am,
            BriefingSlot::Pm => bn_core::briefing::Slot::Pm,
        }
    }
}
