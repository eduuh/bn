//! Value enums for `bn dispatch`'s arguments.
//!
//! bn-core is clap-free, so anything that has to be a `clap::ValueEnum` lives on this side of
//! the boundary.

/// What `bn dispatch` does when every warm pool slot is already leased.
///
/// The old behaviour was `Cold`, unconditionally and silently: a full pool meant a brand-new
/// worktree directory, every time, forever, with nothing to reclaim them. Queueing is the honest
/// default — capacity is finite, so work waits for a slot rather than growing the disk.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum OnPoolFull {
    /// Schedule the item on the repo's orchestrate run and start nothing now.
    #[default]
    Queue,
    /// Cut a cold worktree anyway.
    Cold,
    /// Report `pool_full` and do nothing.
    Fail,
}
