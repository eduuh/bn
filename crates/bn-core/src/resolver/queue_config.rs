//! `[queue]` — the machine-wide work queue's policy.
//!
//! Its own module because `config.rs` is at the 1000-line cap and this table will keep growing:
//! every producer the queue absorbs is a name in `capture`.

use super::Config;

impl Config {
    /// The repo scheduled, repo-less work is booked against.
    ///
    /// A queue entry is keyed `(repo, id)`, but a machine-wide cron belongs to no repo in
    /// particular. Rather than invent a null repo — which every downstream join would then have to
    /// handle — such work is booked against a stable sentinel, so it sorts, dedups and displays
    /// like everything else.
    pub fn queue_home_repo(&self) -> String {
        "machine".to_string()
    }

    /// The `[staff].home` pin as a value, so it can travel as an ordinary `home:<machine>`
    /// requirement rather than being re-checked by every scheduled job. Same pin
    /// `staff::home_gate` enforces.
    pub fn staff_home_pin(&self) -> Option<String> {
        self.staff_home.clone()
    }
}

/// Every field is off/absent by default, so a machine that has not configured the queue behaves
/// exactly as it did before the queue existed: nothing is captured, and the lane caps fall back to
/// the dispatcher's own conservative defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueConfig {
    /// Producers that enqueue instead of launching immediately.
    ///
    /// `None` (the key absent) means **every** producer — the queue is the default path. `Some([])`
    /// is the explicit opt-out, which is why this is an `Option` and not a bare `Vec`: "I never
    /// configured this" and "I turned it off" must not collapse into the same value.
    pub capture: Option<Vec<String>>,
    /// Concurrent coding-lane agents. `None` — the normal case — derives the cap from this
    /// machine's RAM and cores; see [`crate::queue::capacity`]. Set it only to override that.
    pub coding: Option<usize>,
    /// Concurrent research-lane agents. `None` derives it, as above.
    pub research: Option<usize>,
    /// Seconds a node may go without a heartbeat before it stops being placeable.
    pub heartbeat_ttl_secs: Option<i64>,
    /// Per-kind ceilings by kebab-case kind label, e.g. `review-pr = 1`.
    pub kind_caps: std::collections::HashMap<String, usize>,
    /// Inline express items one pass may run. `None` uses the built-in budget.
    pub express_per_tick: Option<usize>,
    /// Seconds a worktree agent may hold a lease before it is presumed dead. `None` uses the
    /// built-in TTL. Raise it if you legitimately run agents longer than an hour — reclaiming a
    /// live one double-dispatches it.
    pub agent_lease_ttl_secs: Option<i64>,
    /// Seconds headless work may hold a lease. `None` uses the built-in TTL.
    pub express_lease_ttl_secs: Option<i64>,
    /// Idle minutes after which opportunistic work may run outside an explicit away window.
    pub opportunistic_idle_mins: i64,
    /// Times of day when some work is preferred — see [`crate::queue::window`].
    pub windows: Vec<crate::queue::window::PriorityWindow>,
}

/// Every producer that can be routed through the queue — and, absent config, is.
pub const ALL_PRODUCERS: [&str; 6] = ["review", "attn", "staff", "cron", "dispatch", "today"];

/// How long after a tick the queue still counts as having a live dispatcher.
///
/// The tick runs every minute, so ten is generous. It has to be: the consequence of judging the
/// dispatcher dead is that work launches directly, which is merely the old behaviour — whereas
/// judging it alive when it is not means work is enqueued into something that never drains.
pub const DISPATCHER_TTL_SECS: i64 = 600;

impl QueueConfig {
    /// Read `[queue]` from config, with env overrides for the two lane caps.
    ///
    /// Assembled here rather than inline in `Config::load` so the table's growth — every producer
    /// the queue absorbs adds a key — costs lines in the file that owns it.
    pub(super) fn load(file: Option<&super::config_file::ConfigFile>) -> QueueConfig {
        let (
            express_per_tick,
            agent_lease_ttl_secs,
            express_lease_ttl_secs,
            opportunistic_idle_mins,
        ) = file.map(|f| f.queue_nums()).unwrap_or_default();
        QueueConfig {
            capture: file.and_then(|f| f.queue_capture()),
            coding: super::util::non_empty_env("BN_QUEUE_CODING")
                .and_then(|v| v.trim().parse().ok())
                .or_else(|| file.and_then(|f| f.queue_coding())),
            research: super::util::non_empty_env("BN_QUEUE_RESEARCH")
                .and_then(|v| v.trim().parse().ok())
                .or_else(|| file.and_then(|f| f.queue_research())),
            heartbeat_ttl_secs: file.and_then(|f| f.queue_heartbeat_ttl_secs()),
            express_per_tick,
            agent_lease_ttl_secs,
            express_lease_ttl_secs,
            opportunistic_idle_mins: opportunistic_idle_mins.filter(|n| *n >= 0).unwrap_or(10),
            kind_caps: file.map(|f| f.queue_kind_caps()).unwrap_or_default(),
            windows: file
                .map(|f| f.queue_windows())
                .filter(|windows| !windows.is_empty())
                .unwrap_or_else(crate::queue::window::default_pool_warm_priority_windows),
        }
    }

    /// Whether `producer` is configured to enqueue rather than launch.
    ///
    /// **Configuration only.** Whether it *actually* enqueues also depends on a dispatcher being
    /// alive — see [`capture_active`](QueueConfig::capture_active), which is what producers call.
    pub fn captures(&self, producer: &str) -> bool {
        match &self.capture {
            // Unconfigured: the queue is the default path for everything.
            None => ALL_PRODUCERS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(producer)),
            Some(list) => list
                .iter()
                .any(|c| c.eq_ignore_ascii_case(producer) || c.eq_ignore_ascii_case("all")),
        }
    }

    /// Whether `producer` should enqueue **right now**.
    ///
    /// The guard that makes an on-by-default queue safe. Enqueueing is only correct if something
    /// will later dequeue; on a machine where `queue-tick` was never registered, or where it has
    /// stopped, capturing every producer would quietly halt all scheduled work — the worst possible
    /// failure, because everything looks fine and nothing runs.
    ///
    /// So capture requires evidence: the resident dispatcher heartbeat is fresh within
    /// [`DISPATCHER_TTL_SECS`].
    /// No evidence means the producer launches inline, exactly as it did before the queue existed.
    /// This also gives clean progressive enablement — a fresh machine behaves classically until the
    /// tick is registered, then starts queueing on its own.
    pub fn capture_active(&self, producer: &str, now: i64) -> bool {
        self.captures(producer) && dispatcher_alive(now)
    }

    /// The lane caps: an explicit override, else derived from this machine's RAM and cores.
    pub fn lanes(&self) -> (usize, usize) {
        let (coding, research, _) = crate::queue::capacity::observe();
        (
            self.coding.filter(|n| *n > 0).unwrap_or(coding),
            self.research.filter(|n| *n > 0).unwrap_or(research),
        )
    }

    /// Inline express items one pass may run.
    pub fn express_per_tick(&self) -> usize {
        self.express_per_tick
            .filter(|n| *n > 0)
            .unwrap_or(crate::queue::tick::MAX_EXPRESS_PER_TICK)
    }

    /// The lease TTL for `worktree`-shaped work versus headless work.
    pub fn lease_ttl(&self, needs_worktree: bool) -> i64 {
        let (configured, default) = if needs_worktree {
            (
                self.agent_lease_ttl_secs,
                crate::queue::reap::AGENT_LEASE_TTL_SECS,
            )
        } else {
            (
                self.express_lease_ttl_secs,
                crate::queue::reap::EXPRESS_LEASE_TTL_SECS,
            )
        };
        configured.filter(|n| *n > 0).unwrap_or(default)
    }

    /// The heartbeat TTL, or the default.
    pub fn heartbeat_ttl(&self) -> i64 {
        self.heartbeat_ttl_secs
            .filter(|v| *v > 0)
            .unwrap_or(crate::hosts::DEFAULT_HEARTBEAT_TTL_SECS)
    }
}

/// Whether the resident dispatcher is alive enough to trust it will drain what producers enqueue.
///
/// This intentionally reads the daemon heartbeat, not the latest decision. The decision is written
/// by every tick, including inline fallback ticks; treating it as liveness lets the fallback mask a
/// dead resident daemon.
pub fn dispatcher_alive(now: i64) -> bool {
    dispatcher_heartbeat_age(now).is_some_and(|age| age <= DISPATCHER_TTL_SECS)
}

/// The resident dispatcher's liveness file — written by a timer thread inside `bn queue run`.
pub fn dispatcher_heartbeat_path() -> std::path::PathBuf {
    crate::resolver::state_dir().join("queue-daemon.heartbeat")
}

/// The stamp written when a dispatcher pass *completes*, as opposed to when the process last drew
/// breath. The two are separate files because they answer different questions, and conflating them
/// is what made a wedged dispatcher indistinguishable from a dead one.
pub fn dispatcher_pass_path() -> std::path::PathBuf {
    crate::resolver::state_dir().join("queue-daemon.pass")
}

/// Seconds since the resident dispatcher last said it was alive, or `None` if it never has.
pub fn dispatcher_heartbeat_age(now: i64) -> Option<i64> {
    read_stamp(&dispatcher_heartbeat_path()).map(|beat| now.saturating_sub(beat))
}

/// Seconds since a dispatcher pass last ran to completion, or `None` if none ever has.
///
/// The companion to [`dispatcher_alive`], and the reason the heartbeat could move to its own
/// thread without creating a new blind spot. A heartbeat proves the process exists; only this
/// proves it is still *getting anywhere*. A dispatcher whose pass is blocked on a hung child now
/// reports `alive` with a stale pass — which is the truth, and points at the stuck work rather than
/// at a daemon that is running fine.
pub fn dispatcher_pass_age(now: i64) -> Option<i64> {
    read_stamp(&dispatcher_pass_path()).map(|at| now.saturating_sub(at))
}

/// How long a pass may go unfinished before the dispatcher counts as wedged rather than merely
/// quiet. Generously above the idle interval (15s) and the busy floor (3s): a healthy loop stamps
/// this several times a minute even with nothing to do.
pub const DISPATCHER_STALL_SECS: i64 = 300;

fn read_stamp(path: &std::path::Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
}
