//! tmux subprocess wrappers for bn-cli — now a thin re-export of the shared
//! [`bn_core::tmux`] module (issue #428).
//!
//! The wrappers moved to `bn-core` so CLI and MCP can reuse them in-process instead of
//! re-implementing the tmux/process bridge. bn-cli's ~20 call sites keep using `crate::proc::…`
//! unchanged via this re-export.

//! `exact_target` is re-exported alongside them deliberately: every `-t` argument built from a
//! session/window NAME must go through it, and the one-token reach (`proc::exact_target`) is what
//! makes that the path of least resistance rather than `format!("={a}:{b}")`, which anchors only
//! the session half and lets a write fnmatch into a neighbouring window.

pub use bn_core::tmux::{
    exact_target, tmux, tmux_command, tmux_ok, tmux_raw, tmux_run, tmux_run_try, tmux_status,
    warn_unreachable,
};
