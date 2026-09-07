//! `[queue]` — the machine-wide work queue's policy.
//!
//! Its own module because `config.rs` is at the 1000-line cap and this table will keep growing:
//! every producer the queue absorbs is a name in `capture`.

use super::Config;

impl Config {
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
    /// How many heavy jobs may run at once. `None` uses [`DEFAULT_HEAVY_CAP`].
    pub heavy: Option<usize>,
    /// Jobs this machine calls heavy, overriding [`crate::queue::jobs::HEAVY_JOBS`].
    pub heavy_jobs: Option<Vec<String>>,
    /// Per-job priority, overriding [`crate::queue::jobs::priority_for`].
    pub job_priority: std::collections::HashMap<String, i64>,
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
    /// What a person's direct command — a picker verb pressed by hand — does. See [`DirectMode`].
    pub direct: DirectMode,
}

/// What happens when a person triggers a command by hand, as opposed to a cron firing.
///
/// The queue exists to keep the *machine's* work from over-committing the box: crons, warms,
/// reclaims, agents. A verb someone just pressed in the picker is a different thing — they are
/// watching the row, and every admission gate between them and the work (the heavy cap, a warm
/// that owns the machine, a pause, the dispatcher's next pass) reads as the keypress having failed.
/// So a direct command starts on the spot, and the queue is only the fallback for a machine that
/// cannot start it right then. The entry is still filed, which is what keeps the row honest: it
/// carries the lease, the start time and the finish callback exactly as a dispatched one does.
///
/// `Queued` is the opt-out for a box where "start it now" is the wrong default — a laptop with one
/// heavy slot that must not run two builds at once. Scheduled work is unaffected either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DirectMode {
    /// Lease and start the entry from the process that filed it; fall back to the queue only when
    /// that cannot be done.
    #[default]
    Immediate,
    /// File it for the dispatcher and let admission decide, like every other producer.
    Queued,
}

impl DirectMode {
    /// The config word. `Immediate` is the default, so an absent key reads as it.
    pub fn parse(word: Option<&str>) -> DirectMode {
        match word.map(|w| w.trim().to_ascii_lowercase()).as_deref() {
            Some("queued") | Some("queue") => DirectMode::Queued,
            _ => DirectMode::Immediate,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DirectMode::Immediate => "immediate",
            DirectMode::Queued => "queued",
        }
    }

    pub fn is_immediate(self) -> bool {
        matches!(self, DirectMode::Immediate)
    }
}

/// Heavy jobs allowed to run at once when `[queue].heavy` says nothing.
///
/// One: the value it was hardcoded to, so an unconfigured machine behaves exactly as before.
pub const DEFAULT_HEAVY_CAP: usize = 1;

/// Every producer that can be routed through the queue — and, absent config, is.
pub const ALL_PRODUCERS: [&str; 6] = ["review", "attn", "staff", "cron", "dispatch", "today"];

impl QueueConfig {
    /// Read `[queue]` from config, with env overrides for the two lane caps.
    ///
    /// Assembled here rather than inline in `Config::load` so the table's growth — every producer
    /// the queue absorbs adds a key — costs lines in the file that owns it.
    pub(super) fn load(file: Option<&super::config_file::ConfigFile>) -> QueueConfig {
        let nums = file.map(|f| f.queue_nums()).unwrap_or_default();
        let job_classes = file.and_then(|f| f.queue_job_classes());
        QueueConfig {
            capture: file.and_then(|f| f.queue_capture()),
            coding: super::util::non_empty_env("BN_QUEUE_CODING")
                .and_then(|v| v.trim().parse().ok())
                .or_else(|| file.and_then(|f| f.queue_coding())),
            research: super::util::non_empty_env("BN_QUEUE_RESEARCH")
                .and_then(|v| v.trim().parse().ok())
                .or_else(|| file.and_then(|f| f.queue_research())),
            heartbeat_ttl_secs: file.and_then(|f| f.queue_heartbeat_ttl_secs()),
            express_per_tick: nums.express_per_tick,
            heavy: nums.heavy,
            heavy_jobs: job_classes.as_ref().and_then(|j| j.heavy.clone()),
            job_priority: job_classes.map(|j| j.priority).unwrap_or_default(),
            agent_lease_ttl_secs: nums.agent_lease_ttl_secs,
            express_lease_ttl_secs: nums.express_lease_ttl_secs,
            opportunistic_idle_mins: nums
                .opportunistic_idle_mins
                .filter(|n| *n >= 0)
                .unwrap_or(10),
            kind_caps: file.map(|f| f.queue_kind_caps()).unwrap_or_default(),
            // Configured windows are *added to* the built-in pool-warm pair, not swapped for it.
            //
            // They used to replace it outright, which made defining any window at all silently
            // delete the lunch and after-work schedules — `wt_pool_refresh` already carried a
            // workaround re-deriving the after-work deadline when it went missing. Now that
            // `bn queue schedule add` exists, that trap would fire the first time anyone used it.
            windows: crate::queue::window::normalize_pool_warm_priority_windows(
                crate::queue::window::with_defaults(
                    file.map(|f| f.queue_windows()).unwrap_or_default(),
                ),
            ),
            direct: DirectMode::parse(file.and_then(|f| f.queue_direct()).as_deref()),
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
    /// will later dequeue; on a machine whose dispatcher never started, or has stopped, capturing
    /// every producer would quietly halt all scheduled work — the worst possible failure, because
    /// everything looks fine and nothing runs.
    ///
    /// So capture requires evidence, and the evidence is a live one: a dispatcher is *holding the
    /// daemon lock right now*. No evidence means the producer launches inline, exactly as it did
    /// before the queue existed. This also gives clean progressive enablement — a fresh machine
    /// behaves classically until a dispatcher starts, then begins queueing on its own.
    ///
    /// Every producer must come through here. The two that did not — the `bn tat` picker verbs and
    /// the worktree-close pool reclaim — spent three and a half days filing work into a queue with
    /// no dispatcher, reporting each one as `✓ queued`.
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

    /// How many heavy jobs (pool warms, builds, releases) may run at once.
    ///
    /// One by default, which is what it was hardcoded to. A machine with cores to spare can raise
    /// it without also raising the agent lanes — heavy work is sized by compile cost, agents by
    /// memory, and conflating them is what made `[queue].coding` the only dial anyone had.
    pub fn heavy_cap(&self) -> usize {
        self.heavy.filter(|n| *n > 0).unwrap_or(DEFAULT_HEAVY_CAP)
    }

    /// Whether `job` costs like a build on this machine.
    ///
    /// `[queue.jobs].heavy` replaces the built-in list outright rather than adding to it, because
    /// the interesting case is subtractive: a machine whose `build.sh` takes thirty seconds has a
    /// reason to say `build` is not heavy, and a list that could only grow could not express it.
    ///
    /// Consulted where the answer is *stamped* onto an entry, never where one is read back — an
    /// entry carries its own `heavy` bit from the moment it was queued, so changing this affects new
    /// work and leaves what is already waiting classified as it was filed.
    pub fn is_heavy(&self, job: &str) -> bool {
        match &self.heavy_jobs {
            Some(list) => list.iter().any(|j| j == job),
            None => crate::queue::jobs::is_heavy(job),
        }
    }

    /// How urgent `job` is on this machine.
    ///
    /// An explicit `[queue.jobs].priority` entry wins; otherwise the derived recovery/heavy/cron
    /// banding. The bands are also spelled as literals in [`crate::rank`]'s table, so overriding a
    /// priority across a band boundary changes which band the work reports in — the honest outcome,
    /// since the machine has just said it is that urgent.
    pub fn priority_for(&self, job: &str) -> i64 {
        match self.job_priority.get(job) {
            Some(p) => *p,
            None => crate::queue::jobs::priority_for(job),
        }
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

/// Whether a resident dispatcher is running, and will therefore drain what producers enqueue.
///
/// Asks the kernel — [`crate::queue::daemon::is_running`] — rather than reading a timestamp. A
/// heartbeat had to be generous enough to survive a slow pass, which left a window in which a dead
/// dispatcher still read as alive; work filed in that window went nowhere. A held `flock` has no
/// such window: it exists for exactly as long as the process does.
///
/// `now` is unused and kept so every caller keeps passing the clock it already has, rather than
/// this becoming the one liveness question that reads the wall clock for itself.
pub fn dispatcher_alive(_now: i64) -> bool {
    crate::queue::daemon::is_running()
}

/// The stamp written when a dispatcher pass *completes*, as opposed to when the process last drew
/// breath. The two are separate files because they answer different questions, and conflating them
/// is what made a wedged dispatcher indistinguishable from a dead one.
pub fn dispatcher_pass_path() -> std::path::PathBuf {
    crate::resolver::state_dir().join("queue-daemon.pass")
}

/// Seconds since a dispatcher pass last ran to completion, or `None` if none ever has.
///
/// The companion to [`dispatcher_alive`], and the half it deliberately does not answer. Holding the
/// daemon lock proves the process exists; only this proves it is still *getting anywhere*. A
/// dispatcher blocked inside a hung child holds its lock perfectly well, so it reports `alive` with
/// a stale pass — which is the truth, and points at the stuck work rather than at a daemon that is
/// running fine.
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

#[cfg(test)]
mod direct_mode_tests {
    use super::DirectMode;

    /// Absent, empty or unrecognised all read as the default: a typo in the opt-out must not
    /// silently opt out of something else.
    #[test]
    fn the_default_is_immediate_and_only_queued_opts_out() {
        assert_eq!(DirectMode::parse(None), DirectMode::Immediate);
        assert_eq!(DirectMode::parse(Some("")), DirectMode::Immediate);
        assert_eq!(DirectMode::parse(Some("immediate")), DirectMode::Immediate);
        assert_eq!(DirectMode::parse(Some("later")), DirectMode::Immediate);
        assert_eq!(DirectMode::parse(Some("queued")), DirectMode::Queued);
        assert_eq!(DirectMode::parse(Some(" Queue ")), DirectMode::Queued);
    }

    #[test]
    fn labels_round_trip_through_parse() {
        for mode in [DirectMode::Immediate, DirectMode::Queued] {
            assert_eq!(DirectMode::parse(Some(mode.label())), mode);
        }
    }
}
