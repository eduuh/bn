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
    /// Times of day when some work is preferred — see [`crate::queue::window`].
    pub windows: Vec<crate::queue::window::PriorityWindow>,
}

/// Every producer that can be routed through the queue — and, absent config, is.
pub const ALL_PRODUCERS: [&str; 5] = ["review", "attn", "staff", "cron", "dispatch"];

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
        let (express_per_tick, agent_lease_ttl_secs, express_lease_ttl_secs) =
            file.map(|f| f.queue_nums()).unwrap_or_default();
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
            kind_caps: file.map(|f| f.queue_kind_caps()).unwrap_or_default(),
            windows: file.map(|f| f.queue_windows()).unwrap_or_default(),
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
    /// So capture requires evidence: a tick recorded a decision within [`DISPATCHER_TTL_SECS`].
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

/// Whether a queue tick has run recently enough to trust it will drain what producers enqueue.
///
/// Reads the decision the tick writes on **every** pass, including the ones that start nothing —
/// which is exactly why it writes them: a dispatcher with nothing to do still has to prove it is
/// there.
pub fn dispatcher_alive(now: i64) -> bool {
    crate::queue::decision::QueueDecision::read().is_some_and(|d| {
        // A `--dry-run` pass writes a decision too, and it proves nothing: it deliberately starts
        // nothing and takes no lease. Counting it as evidence would let someone inspecting the
        // queue accidentally convince every producer that work was being drained when it was not.
        !d.dry_run && now.saturating_sub(d.at) <= DISPATCHER_TTL_SECS
    })
}
