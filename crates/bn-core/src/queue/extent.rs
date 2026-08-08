//! The dispatcher's *dynamic extent* — the marker that tells a `bn` child "this work has already
//! been admitted, do not enqueue it again."
//!
//! The extent has to cross process boundaries: the dispatcher runs `bn dispatch` and `bn run <job>`
//! as children, and each of those re-decides whether to queue. An environment variable is the only
//! thing a child inherits, so that is what carries it.
//!
//! But inheritance does not stop at the child bn asked for. It reaches **everything descended from
//! it, forever** — the agent harness a dispatch launches and outlives the tick by hours, and worst
//! of all a `tmux` server started during a dispatch, which then hands the marker to every pane of
//! every window it will ever create. A plain "is the variable set?" test therefore stops meaning
//! "we are inside a dispatch" within minutes of the first dispatch, and starts meaning "this
//! machine once ran one". Every producer downstream then silently takes the pre-queue inline path:
//! work launching direct while the operator believes it is queued, which is precisely the failure
//! the extent exists to prevent, arriving through the mechanism meant to prevent it.
//!
//! So the marker names the dispatch rather than merely asserting one: it carries the dispatcher's
//! pid, and an inherited copy is honoured only while that process is still alive. A tick exits in
//! seconds, so a leaked copy in a tmux server or a long-lived agent is inert by the time anything
//! reads it — including copies already sitting in environments written by older versions.

/// The environment variable carrying the dispatcher's pid for the extent of one pass.
///
/// Named in `bn-core` rather than in the dispatcher because the tmux bridge has to strip it (see
/// [`crate::tmux::tmux_command`]) and both sides must agree on the spelling.
pub const DISPATCH_EXTENT_ENV: &str = "BN_IN_QUEUE_TICK";

/// Whether an inherited [`DISPATCH_EXTENT_ENV`] value still names a running dispatch.
///
/// Pure in its liveness check so the rule is testable without spawning anything. A value that is
/// absent, unparseable, or names pid 0/1 is not a dispatch: pid 1 is init, and `1` is exactly what
/// older bn versions wrote as a bare "yes" flag — so those stale markers read as dead, which is the
/// behaviour that heals an already-poisoned tmux server without restarting it.
pub fn extent_is_live(value: Option<&str>, pid_alive: impl Fn(u32) -> bool) -> bool {
    value
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid > 1 && pid_alive(pid))
}

/// Whether this process is running inside a live dispatch, read from the real environment and
/// process table — the I/O counterpart of [`extent_is_live`].
pub fn in_dispatch_extent() -> bool {
    let value = std::env::var(DISPATCH_EXTENT_ENV).ok();
    extent_is_live(value.as_deref(), crate::queue::build::pid_alive)
}

/// The value to publish for the current process's dispatch extent.
pub fn extent_marker() -> String {
    std::process::id().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_pid_is_inside_the_extent() {
        assert!(extent_is_live(Some("4242"), |pid| pid == 4242));
    }

    #[test]
    fn a_dead_pid_is_a_leaked_marker() {
        // The agent that outlived its tick, or a tmux server holding a copy from hours ago.
        assert!(!extent_is_live(Some("4242"), |_| false));
    }

    #[test]
    fn the_legacy_bare_flag_reads_as_dead() {
        // Older bn wrote "1". Honouring it would keep every already-poisoned tmux server broken.
        assert!(!extent_is_live(Some("1"), |_| true));
        assert!(!extent_is_live(Some("0"), |_| true));
    }

    #[test]
    fn absent_or_junk_is_not_an_extent() {
        assert!(!extent_is_live(None, |_| true));
        assert!(!extent_is_live(Some(""), |_| true));
        assert!(!extent_is_live(Some("yes"), |_| true));
    }
}
