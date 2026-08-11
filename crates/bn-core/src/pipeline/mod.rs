//! Pure primitives for the autonomous task pipeline (EPIC #746).
//!
//! Stage-specific modules keep seed construction and transition decisions store-free and
//! unit-tested so the stateful CLI glue stays thin.

pub mod critic;
pub mod gate;
pub mod investigate;
pub mod profile;
pub mod stage;
pub mod status;
pub mod sweep;
