//! Agent coordination: seats and the watchdogs that wake them.
//!
//! A **seat** is who holds work: a live agent addressed `repo:branch` (the same address
//! `bn agent msg` uses) or a person, `human:<name>`. A **watchdog** is a durable scheduled check
//! that wakes a seat, because a seat cannot wake itself.
//!
//! Storage is SQLite at [`db_path`], modelled on [`crate::worker::registry`]: machine-local like
//! the agents it coordinates, and never in the synced notes tree.
use std::path::PathBuf;

mod db;
pub mod policy;
mod seat;
pub mod watchdog;
pub mod watchdog_eval;

pub use db::{CoordDb, CoordError, Result, SCHEMA_VERSION};
pub use seat::{acting as acting_seat, agent_target, is_human, normalize as normalize_seat};

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

#[cfg(test)]
mod tests;
#[cfg(test)]
mod watchdog_tests;
