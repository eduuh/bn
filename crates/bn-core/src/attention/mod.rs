//! Attention items — entries on the attention list pushed by hand, by a cron script, or by an
//! agent. Stored as append-only event kinds in the same `events.jsonl` as everything else, so
//! there's no new store and no migration: an item is an `attn` event; later events that
//! reference its id resolve or edit it — `attn_ack` (resolved/done), `attn_remove` (cancelled or
//! raised in error), `attn_planned` (promoted to a branch todo), `attn_snooze` (hidden until a
//! date), or `attn_update` (edit its fields in place). Folded on read exactly like the PR
//! lifecycle (pr_opened → pr_merged) — an item is *open* unless a later event resolved it or
//! snoozed it into the future, with the latest `attn_update` fields overlaid. `bn doctor` folds
//! the open set into the aggregated attention list; the `notify` job reads it to surface work.
//! Resolution is *typed* (ack vs remove vs planned) so metrics can tell a finished item from a
//! cancelled one from one that became tracked work.
//!
//! Split into focused submodules; the public types and functions are re-exported
//! so callers keep using `attention::add`, `attention::open_items`, … as before.

mod dispatch;
mod ingest;
mod lifecycle;
mod model;
mod query;
mod status;
mod store;
mod trust;

pub use dispatch::*;
pub use ingest::*;
pub use lifecycle::*;
pub use model::*;
pub use query::*;
pub use status::*;
pub use store::*;
pub use trust::*;




