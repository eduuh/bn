//! One sampling of the world, shared by everything that reasons about the queue.
//!
//! The tick, the timeline's forward projection, `bn queue status` and `GET /api/queue` all need the
//! same picture: what is pending, what is in flight, which nodes exist, what the caps are. Each had
//! grown its own copy of that assembly — three versions of "what counts as in-flight", two of the
//! cap arithmetic, three of the node list. Copies of a *question* are worse than copies of an
//! answer: they drift silently, and then two surfaces disagree about the same machine and both look
//! right.
//!
//! So there is one capture, and [`Snapshot::inputs`] hands it to the pure policy. Anything that
//! wants to know about the queue asks this.
use crate::advise::{AttnSignal, GoalContext};
use crate::backlog::{self, BacklogEntry, BacklogStatus};
use crate::hosts::NodeRecord;
use crate::queue::kind::{BacklogKind, WorkClass};
use crate::queue::tick::{InFlight, KindCaps, Presence, TickInputs};
use crate::resolver::Config;
use crate::scheduler::{Caps, Observed};
use std::collections::BTreeMap;

/// Everything one pass of queue reasoning needs, owned.
pub struct Snapshot {
    pub now: i64,
    pub machine: String,
    /// Every backlog entry, in any state — the terminal ones back the timeline's `Done` band.
    pub entries: Vec<BacklogEntry>,
    pub pending: Vec<BacklogEntry>,
    pub in_flight: Vec<InFlight>,
    pub nodes: Vec<NodeRecord>,
    pub caps: Caps,
    pub observed: Observed,
    pub kind_caps: KindCaps,
    pub kinds: Vec<BacklogKind>,
    pub goals: Vec<GoalContext>,
    pub attention: Vec<AttnSignal>,
    /// Inline express items one pass may run — configurable, so a slow box can lower it.
    pub max_express: usize,
    pub windows: Vec<crate::queue::window::PriorityWindow>,
    /// The pauses in force at capture — what makes a drained repo's work stay pending.
    pub pauses: crate::queue::pause::PauseState,
    pub presence: Presence,
}

impl Snapshot {
    /// Sample the machine.
    ///
    /// Reads the **persisted** node registry rather than probing hosts, so a web request or a
    /// status command never blocks on SSH to a box that may be down. The tick refreshes that
    /// registry before capturing; everything else reads what it last wrote.
    pub fn capture(cfg: &Config, now: i64) -> Snapshot {
        let bl = backlog::load(&backlog::backlog_path());
        let entries = bl.entries().to_vec();
        let pending: Vec<BacklogEntry> = entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Pending)
            .cloned()
            .collect();
        let (coding, research) = cfg.queue.lanes();
        let mut kind_caps = BTreeMap::new();
        for (label, n) in &cfg.queue.kind_caps {
            if let Some(kind) = BacklogKind::parse(label) {
                kind_caps.insert(kind, *n);
            }
        }
        let windows = cfg.queue.windows.clone();
        Snapshot {
            now,
            machine: cfg.machine().to_string(),
            in_flight: in_flight(cfg, &entries, now),
            nodes: crate::queue::nodes::NodeRegistry::load().live(now, cfg.queue.heartbeat_ttl()),
            caps: Caps {
                max_local: coding,
                research: Some(research),
                min_free_ram_mb: cfg.orchestrate_min_free_ram_mb,
                max_loadavg: cfg.orchestrate_max_loadavg,
                max_swap_pct: cfg.orchestrate_max_swap_pct,
                tokens_per_hour: cfg.orchestrate_tokens_per_hour,
                daily_cost_usd: cfg.orchestrate_daily_cost_usd,
                daily_cost_usd_coding: cfg.orchestrate_daily_cost_usd_coding,
                daily_cost_usd_research: cfg.orchestrate_daily_cost_usd_research,
                plan_gated: cfg.dispatch_plan_review,
                remote_overflow: cfg.orchestrate_remote_overflow,
                // Machine-scoped: the local box runs whatever it is asked to. Per-repo ownership is
                // answered by each node's `owned_repos` during placement instead.
                local_owned: true,
            },
            observed: observed(cfg),
            kind_caps: KindCaps(kind_caps),
            kinds: crate::queue::tick_claimable_kinds(),
            goals: crate::advise::open_quarter_goals(cfg, &crate::util::local_date_of(now)),
            attention: crate::attention::open_items(&crate::util::local_date_of(now))
                .into_iter()
                .map(|item| AttnSignal {
                    repo: item.repo,
                    severity: item.severity,
                })
                .collect(),
            max_express: cfg.queue.express_per_tick(),
            presence: Presence {
                idle: crate::idle::is_idle(cfg, now, cfg.queue.opportunistic_idle_mins),
                away_window: windows
                    .iter()
                    .any(|w| w.mode == crate::queue::window::WindowMode::Away && w.is_open(now)),
            },
            windows,
            // Folded once per capture, alongside every other machine signal, so the whole gate
            // stays decidable from a captured snapshot.
            pauses: crate::queue::pause::PauseState::load(now),
            pending,
            entries,
        }
    }

    /// Borrow it as the pure policy's inputs.
    pub fn inputs(&self) -> TickInputs<'_> {
        TickInputs {
            now: self.now,
            machine: &self.machine,
            pending: &self.pending,
            in_flight: &self.in_flight,
            nodes: &self.nodes,
            caps: &self.caps,
            observed: &self.observed,
            kind_caps: &self.kind_caps,
            kinds: Some(&self.kinds),
            goals: &self.goals,
            attention: &self.attention,
            max_express: self.max_express,
            windows: &self.windows,
            pauses: &self.pauses,
            presence: self.presence,
        }
    }

    /// Terminal entries whose completion stamp falls on `date` — the timeline's `Done` band.
    pub fn done_on(&self, date: &str) -> Vec<BacklogEntry> {
        self.entries
            .iter()
            .filter(|e| e.status.is_terminal())
            .filter(|e| crate::util::local_date_of(e.updated_at) == date)
            .cloned()
            .collect()
    }
}

/// What the machine is already committed to.
///
/// Two sources, deliberately: leased backlog entries (whose kind and touches are known) **and** live
/// agent sessions no lease explains — the ones a human started with `bn dispatch --on-pool-full
/// fail`. Those consume the same RAM as anything the queue starts, so leaving them out would let
/// the tick cheerfully over-commit a box that is already full.
fn in_flight(cfg: &Config, entries: &[BacklogEntry], now: i64) -> Vec<InFlight> {
    let mut out: Vec<InFlight> = entries
        .iter()
        .filter(|e| e.status == BacklogStatus::Leased)
        .map(|e| InFlight {
            repo: e.repo.clone(),
            id: e.id.clone(),
            kind: e.kind,
            work_class: WorkClass::from_kind(e.kind),
            heavy: e.heavy,
            node: e.assigned_to.clone(),
            touches: e.touches.clone(),
            started_at: e.updated_at,
        })
        .collect();
    for agent in crate::agent::supervise(cfg, 0, 0).agents {
        // A finished session still has a readable transcript for half an hour after it exits, and
        // charging that ghost a lane slot is how a machine with one live agent reported "coding 5/5,
        // at local concurrency cap" and held everything behind it.
        if !agent.health.is_running() {
            continue;
        }
        if out
            .iter()
            .any(|f| f.repo == agent.repo && agent.branch.contains(&f.id))
        {
            continue;
        }
        out.push(InFlight {
            repo: agent.repo.clone(),
            id: agent.branch.clone(),
            // An agent bn did not queue has no declared kind. Charging it to the coding lane is the
            // conservative read: under-counting is what over-commits the machine.
            kind: BacklogKind::Ship,
            work_class: WorkClass::Coding,
            heavy: false,
            node: None,
            touches: Vec::new(),
            started_at: agent.last_activity_ts.min(now),
        });
    }
    out
}

/// The live machine + budget signals. Each is sampled only when its guard is configured, so an
/// unconfigured machine pays nothing to read what could never bind.
fn observed(cfg: &Config) -> Observed {
    let mem = (cfg.orchestrate_min_free_ram_mb.is_some() || cfg.orchestrate_max_swap_pct.is_some())
        .then(crate::machine::read_meminfo)
        .unwrap_or_default();
    Observed {
        loadavg: cfg
            .orchestrate_max_loadavg
            .and_then(|_| crate::machine::read_loadavg1()),
        free_ram_mb: cfg
            .orchestrate_min_free_ram_mb
            .and(mem.total_mb.and(mem.available_mb)),
        swap_pct: cfg
            .orchestrate_max_swap_pct
            .and_then(|_| crate::machine::swap_used_pct(&mem)),
        // Token and cost attribution is per-repo in the cost store; the machine-wide roll-up lands
        // with the executor stage that actually spends them.
        tokens_this_hour: 0,
        cost_today_usd: 0.0,
        cost_today_coding_usd: 0.0,
        cost_today_research_usd: 0.0,
    }
}
