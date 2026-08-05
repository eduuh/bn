//! tmux subprocess wrappers for bn-cli — now a thin re-export of the shared
//! [`bn_core::tmux`] module (issue #428).
//!
//! The wrappers moved to `bn-core` so bn-web / bn-mcp can reuse them in-process instead of
//! re-implementing the tmux/process bridge. bn-cli's ~20 call sites keep using `crate::proc::…`
//! unchanged via this re-export.

pub use bn_core::tmux::{tmux, tmux_command, tmux_ok, tmux_raw, tmux_run};
