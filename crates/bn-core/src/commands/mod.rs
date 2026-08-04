//! Mutating note commands: `add` / `done` and the note-lifecycle verbs. Each engine
//! function reads, mutates, and persists the note, returning a typed outcome; the CLI
//! renders text or JSON. No human printing here.
//!
//! Split into focused submodules; the public verbs and outcome types are re-exported so
//! callers keep using `commands::add`, `commands::close`, `commands::CmdError`, … as
//! before. Screenshot capture (`crate::capture`) and note-store migration
//! (`crate::migrate`) live outside this façade.

mod body;
mod error;
mod note;
mod todo;

pub use body::*;
pub use error::*;
pub use note::*;
pub use todo::*;


