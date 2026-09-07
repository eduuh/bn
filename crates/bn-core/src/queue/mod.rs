//! The unified work queue: where every dispatchable agent lands before it runs.
//!
//! The durable store itself is [`crate::backlog`] — this module owns the *policy* around it:
//! where an entry can run ([`place`]), and (as the queue lands) what kind of work it is, what it
//! needs from a node, and what the dispatcher decided.
//!
//! Split from [`crate::scheduler`], which keeps the admission arithmetic (does the machine have
//! room at all?) while this module answers placement (which box, and is it capable?).
pub mod build;
pub mod classify;
pub mod config_edit;
pub mod daemon;
pub mod decision;
pub mod durations;
pub mod enqueue;
pub mod extent;
pub mod flock;
pub mod jobs;
pub mod kind;
pub mod log;
pub mod nodes;
pub mod pause;
pub mod place;
pub mod snapshot;
pub mod tick;
pub mod timeline;
pub mod window;

/// Whether the per-repo orchestrate tick may claim `kind` from the backlog.
///
/// The migration partition: while both dispatchers exist, each is handed a disjoint set of kinds so
/// they never contend for an entry. Orchestrate keeps the two kinds it already understood; the
/// queue tick takes everything the widened taxonomy added. Stage 6 removes the split, at which
/// point this returns `false` for everything and the orchestrate run consumes what the tick leased.
///
/// The backlog lease is still the real interlock — this only means the two never even reach it.
pub fn orchestrate_claimable(_kind: kind::BacklogKind) -> bool {
    // Nothing, since stage 6: the tick is the single admission authority. Kept as a named function
    // rather than deleted because it is where the rule is stated, and a caller asking "may I claim
    // this?" should get an answer from the same place it always did.
    false
}

/// The pending entries the per-repo orchestrate tick may pull for `repo` — [`orchestrate_claimable`]
/// applied to [`crate::backlog::Backlog::pending_for`], in the same priority order.
///
/// A named function rather than an inline filter so the partition is stated in one place, next to
/// the rule it enforces, instead of being a predicate somebody has to notice in a refill loop.
pub fn orchestrate_pending<'a>(
    backlog: &'a crate::backlog::Backlog,
    repo: &str,
) -> Vec<&'a crate::backlog::BacklogEntry> {
    backlog
        .pending_for(repo)
        .into_iter()
        .filter(|e| orchestrate_claimable(e.kind))
        .collect()
}

/// The kinds [`tick::plan_tick`] may claim — the complement of [`orchestrate_claimable`].
pub fn tick_claimable_kinds() -> Vec<kind::BacklogKind> {
    kind::BacklogKind::ALL
        .into_iter()
        .filter(|k| !orchestrate_claimable(*k))
        .collect()
}

/// Lane capacity derived from the machine, rather than a number somebody guessed.
///
/// A fixed `coding = 2` is wrong on both ends: it wastes a 64 GB workstation and thrashes an 8 GB
/// laptop. The two things that actually bound concurrent agents are RAM (each harness is a ~2 GB
/// Node process) and cores (an agent that cannot get CPU is just a slower agent holding a slot).
/// So take the lower of the two answers.
pub mod capacity {
    /// RAM one agent is assumed to occupy, MB. A Claude/Copilot harness sits around 1–2 GB; taking
    /// the top of that range means the estimate errs toward leaving the machine usable.
    pub const AGENT_RAM_MB: u64 = 2048;

    /// RAM held back for the OS, the editor, the browser — everything that is not an agent.
    ///
    /// Without it a 16 GB box computes 8 concurrent agents and then swaps, which is precisely the
    /// thrash the caps exist to prevent.
    pub const RESERVED_RAM_MB: u64 = 4096;

    /// Cores per agent. Half a core sounds thin, but agents are overwhelmingly I/O-bound — waiting
    /// on the model, on git, on the network — so cores bound throughput far less than RAM does.
    /// This is the sanity bound for the case where RAM is plentiful and CPU is not.
    pub const CORES_PER_AGENT: usize = 2;

    /// Ceilings, so a very large machine does not decide to run twenty agents at once. Beyond this
    /// the limit stops being the machine and starts being the human's ability to review the output.
    pub const MAX_CODING: usize = 8;
    pub const MAX_RESEARCH: usize = 4;

    /// How many concurrent **coding** agents this machine can hold.
    ///
    /// `min(usable RAM ÷ per-agent RAM, cores ÷ per-agent cores)`, floored at 1 — a machine always
    /// gets to run one thing, however small, because a queue that can start nothing is broken
    /// rather than careful. Pure over its two inputs so the arithmetic is inspectable in
    /// `bn queue status` rather than being folklore.
    pub fn coding(ram_total_mb: u64, cores: usize) -> usize {
        let by_ram = ram_total_mb.saturating_sub(RESERVED_RAM_MB) / AGENT_RAM_MB;
        let by_cpu = cores / CORES_PER_AGENT;
        (by_ram as usize).min(by_cpu).clamp(1, MAX_CODING)
    }

    /// How many concurrent **research** agents. Read-only scouts are lighter than a coding agent
    /// but not free, so the research lane gets half the coding lane, floored at 1.
    pub fn research(ram_total_mb: u64, cores: usize) -> usize {
        (coding(ram_total_mb, cores) / 2).clamp(1, MAX_RESEARCH)
    }

    /// Both lanes, plus the sentence explaining how they were reached — so a surprising number is
    /// answerable rather than mysterious.
    ///
    /// The sentence states the operations in the order they happen: the reserve comes off the RAM
    /// *before* the division, and the lower of the two bounds is the answer. Saying "16 GB ÷ 2
    /// GB/agent (less 4 GB reserved) = 6" instead — as this did — reads as arithmetic that does not
    /// compute, which makes the explanation less trustworthy than the number it was explaining.
    pub fn derive(ram_total_mb: u64, cores: usize) -> (usize, usize, String) {
        let by_ram = ram_total_mb.saturating_sub(RESERVED_RAM_MB) / AGENT_RAM_MB;
        let by_cpu = cores / CORES_PER_AGENT;
        let why = format!(
            "({} GB RAM − {} GB reserved) ÷ {} GB/agent = {by_ram}; {cores} cores ÷ {} = {by_cpu}; the lower wins",
            ram_total_mb / 1024,
            RESERVED_RAM_MB / 1024,
            AGENT_RAM_MB / 1024,
            CORES_PER_AGENT,
        );
        (
            coding(ram_total_mb, cores),
            research(ram_total_mb, cores),
            why,
        )
    }

    /// Sample this machine and derive its lanes.
    pub fn observe() -> (usize, usize, String) {
        let mem = crate::machine::read_meminfo();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        derive(mem.total_mb.unwrap_or(0), cores)
    }
}

/// Reclaiming leases whose executor died.
///
/// A lease says "someone is running this". If that someone is killed — SIGKILL, a reboot, a laptop
/// lid — nothing puts the entry back: it is not `pending`, so it is never retried, and not
/// terminal, so it is never reported. The work is simply gone, silently, which is the worst failure
/// a queue can have. Nothing reclaimed these before; a killed tick lost its entry permanently.
pub mod reap {
    use crate::backlog::{BacklogEntry, BacklogStatus};

    /// How long a **worktree agent** may hold a lease before it is presumed dead.
    ///
    /// Generous, because a legitimate coding agent runs 10–40 minutes and reclaiming a live one
    /// would double-dispatch it. An hour is well past any honest run and well short of "nobody
    /// noticed for a day".
    pub const AGENT_LEASE_TTL_SECS: i64 = 3600;

    /// How long **headless** work may hold a lease. Express work runs inline inside a tick and is
    /// resolved by the same pass, so a lease older than a few minutes means its tick died.
    pub const EXPRESS_LEASE_TTL_SECS: i64 = 300;

    /// Whether a live unit corresponds to this entry.
    ///
    /// A dispatched agent's branch is derived from the item, so it *contains* the entry id rather
    /// than equalling it — matching on equality (as the first version did) never matched anything,
    /// which quietly reduced the reaper to a pure timeout and would have reclaimed healthy agents.
    pub fn is_live(entry: &BacklogEntry, live: &[(String, String)]) -> bool {
        live.iter().any(|(repo, branch)| {
            repo.eq_ignore_ascii_case(&entry.repo)
                && (branch == &entry.id || branch.contains(&entry.id))
        })
    }

    /// The leases to return to pending.
    ///
    /// Two independent grounds, and the order matters:
    ///
    ///   1. **Provably gone** — the entry needs a worktree agent, and the machine's live agent set
    ///      contains nothing matching it. That is a fact, so it is reclaimed at once rather than
    ///      after a timeout; waiting an hour to retry work whose executor is demonstrably absent
    ///      helps nobody. A short grace covers the window between leasing and the agent appearing.
    ///   2. **Past its TTL** — the backstop, for everything the liveness check cannot see (heavy
    ///      headless work, ordinary headless work, an agent on another node).
    ///
    /// Pure, so the rule is inspectable rather than a background behaviour people discover.
    pub fn stale<'a>(
        entries: &'a [BacklogEntry],
        live: &[(String, String)],
        now: i64,
        ttl: impl Fn(bool) -> i64,
    ) -> Vec<&'a BacklogEntry> {
        entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Leased)
            .filter(|e| {
                let age = now.saturating_sub(e.updated_at);
                let worktree = e.exec.needs_worktree();
                let long_lived = worktree || e.heavy;
                let provably_gone = worktree && age > START_GRACE_SECS && !is_live(e, live);
                provably_gone || age > ttl(long_lived)
            })
            .collect()
    }

    /// How long a freshly-leased agent has to appear in the live set before its absence counts as
    /// evidence. Launching a worktree agent involves a checkout and a tmux window; reclaiming
    /// inside that window would fight the launcher it is waiting for.
    pub const START_GRACE_SECS: i64 = 180;
}
