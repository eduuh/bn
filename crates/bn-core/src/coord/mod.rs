//! Agent coordination: durable work rows handed between seats (the OpenRig queue model).
//!
//! A **seat** is who holds work: a live agent addressed `repo:branch` (the same address
//! `bn agent msg` uses) or a person, `human:<name>`. A **row** (`q<N>`) is one piece of work with
//! a destination seat, an owner once claimed, a state, and an append-only transition history.
//!
//! The rules the verbs enforce, because they are what make the queue worth having over a message:
//! * a seat cannot close work without saying where it went — every close carries a note;
//! * a handoff closes the source row and opens the destination's in ONE transaction, so nothing
//!   strands between seats;
//! * closing records delivery, not acceptance — acceptance is the next seat's verdict on its row;
//! * a send is not a claim — nudging a seat never changes a row's state.
//!
//! Storage is SQLite at [`db_path`], modelled on [`crate::worker::registry`]: machine-local like
//! the agents it coordinates, and never in the synced notes tree.
use std::path::PathBuf;

mod db;
pub mod mirror;
mod park;
pub mod policy;
mod qstate;
mod queue;
mod queue_read;
mod seat;
pub mod sweep;
mod tier;
pub mod watchdog;
pub mod watchdog_eval;

pub use db::{CoordDb, CoordError, Result, SCHEMA_VERSION};
pub use mirror::{close_mirror, open_mirror, MirrorOutcome};
pub use park::{Block, Trigger};
pub use qstate::QState;
pub use queue::{CloseOutcome, Handoff, NewItem};
pub use queue_read::{ListFilter, QItem, Transition};
pub use seat::{acting as acting_seat, agent_target, is_human, normalize as normalize_seat};
pub use tier::Tier;

/// Path to the coordination DB. `$BN_COORD_DB` wins (tests, explicit override); otherwise
/// `<bn_home>/coord/coord.db` — machine-local, because a SQLite file in the git-synced notes tree
/// would churn and conflict on every write.
pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BN_COORD_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::resolver::bn_home().join("coord").join("coord.db")
}

/// `q12` for row 12 — how a row id is shown and typed everywhere.
pub fn qid(id: i64) -> String {
    format!("q{id}")
}

/// Parse `q12` or `12` into a row id.
pub fn parse_qid(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits = s
        .strip_prefix('q')
        .or_else(|| s.strip_prefix('Q'))
        .unwrap_or(s);
    digits.parse::<i64>().ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod watchdog_tests;
