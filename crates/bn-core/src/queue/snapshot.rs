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
    /// How many heavy jobs may run at once — `[queue].heavy`.
    pub heavy_cap: usize,
    pub windows: Vec<crate::queue::window::PriorityWindow>,
    /// The pauses in force at capture — what makes a drained repo's work stay pending.
    pub pauses: crate::queue::pause::PauseState,
    pub presence: Presence,
    /// A due pool-warm sweep is pending or running and owns new admission until it completes.
    pub mandatory_warming: bool,
    /// What the machine has learned about how long its work takes — read from the cache, so a
    /// three-second tick does not fold a fifty-megabyte event log to find out.
    pub durations: crate::queue::durations::Durations,
    /// When the machine is next claimed by a deadline window, if one is configured.
    pub horizon: Option<crate::queue::tick::Horizon>,
}

impl Snapshot {
    /// Sample the machine.
    ///
    /// Reads the **persisted** node registry rather than probing hosts, so a web request or a
    /// status command never blocks on SSH to a box that may be down. The tick refreshes that
    /// registry before capturing; everything else reads what it last wrote.
    pub fn capture(cfg: &Config, now: i64) -> Snapshot {
        let bl = backlog::load(&backlog::backlog_path());
        let entries = eligible_entries(cfg, &bl);
        let pending: Vec<BacklogEntry> = entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Pending)
            .cloned()
            .collect();
        let mandatory_warming = entries
            .iter()
            .any(|entry| crate::queue::classify::owns_admission(entry, now));
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
            // No goal store since the quarter was retired: ranking falls back to
            // priority plus attention pressure, which is what it already did whenever
            // no quarter file existed.
            goals: Vec::new(),
            attention: crate::attention::open_items(&crate::util::local_date_of(now))
                .into_iter()
                .map(|item| AttnSignal {
                    repo: item.repo,
                    severity: item.severity,
                })
                .collect(),
            max_express: cfg.queue.express_per_tick(),
            heavy_cap: cfg.queue.heavy_cap(),
            presence: Presence {
                idle: crate::idle::is_idle(cfg, now, cfg.queue.opportunistic_idle_mins),
                away_window: windows
                    .iter()
                    .any(|w| w.mode == crate::queue::window::WindowMode::Away && w.is_open(now)),
            },
            mandatory_warming,
            durations: crate::queue::durations::cached_or_compute(&entries, now),
            horizon: crate::queue::window::next_claim(&windows, now)
                .map(|(at, source)| crate::queue::tick::Horizon { at, source }),
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
            heavy_cap: self.heavy_cap,
            durations: &self.durations,
            horizon: self.horizon.as_ref(),
            windows: &self.windows,
            pauses: &self.pauses,
            presence: self.presence,
            mandatory_warming: self.mandatory_warming,
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

fn eligible_entries(cfg: &Config, backlog: &backlog::Backlog) -> Vec<BacklogEntry> {
    backlog
        .entries()
        .iter()
        .filter(|entry| cfg.repo_allowed_here(&entry.repo))
        .cloned()
        .collect()
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
            // Leased by the queue: it started this, and can wait for it.
            queue_owned: true,
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
            // Noticed, not launched. It counts against capacity — it is using the same RAM — but
            // nothing may *wait* for it, because nobody has promised it will ever end.
            queue_owned: false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::{AddSpec, ExecSpec};
    use crate::hosts::{NodeRecord, NodeStatus};
    use crate::queue::pause::PauseState;
    use crate::queue::tick::{plan_tick, KindCaps, Presence, TickInputs};
    use crate::resolver::MachineProfile;

    #[test]
    fn snapshot_excludes_unregistered_repos_before_planning() {
        let cfg = Config {
            machine_profile: Some(MachineProfile {
                repos: vec!["acme/registered".to_string()],
                queue: true,
                cron: false,
                web: false,
            }),
            ..Config::default()
        };
        let mut backlog = backlog::Backlog::new();
        let spec = AddSpec {
            kind: BacklogKind::Cron,
            exec: ExecSpec::Shell {
                cmd: "true".to_string(),
            },
            ..Default::default()
        };
        for (repo, id) in [
            ("acme/unregistered", "blocked"),
            ("acme/registered", "registered"),
        ] {
            backlog.apply(BacklogEntry::new(repo, id, &spec, 900));
        }

        let pending = eligible_entries(&cfg, &backlog);
        assert_eq!(
            pending
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["registered"]
        );

        let caps = Caps {
            max_local: 2,
            research: Some(1),
            min_free_ram_mb: None,
            max_loadavg: None,
            max_swap_pct: None,
            tokens_per_hour: None,
            daily_cost_usd: None,
            daily_cost_usd_coding: None,
            daily_cost_usd_research: None,
            plan_gated: false,
            remote_overflow: false,
            local_owned: true,
        };
        let observed = Observed::default();
        let kind_caps = KindCaps::default();
        let pauses = PauseState::default();
        let durations = crate::queue::durations::Durations::default();
        let nodes = vec![NodeRecord {
            node_id: "testbox".to_string(),
            status: NodeStatus::Ready,
            slots: 2,
            last_heartbeat: 1_000,
            ..Default::default()
        }];
        let plan = plan_tick(&TickInputs {
            now: 1_000,
            machine: "testbox",
            pending: &pending,
            in_flight: &[],
            nodes: &nodes,
            caps: &caps,
            observed: &observed,
            kind_caps: &kind_caps,
            durations: &durations,
            horizon: None,
            kinds: None,
            goals: &[],
            attention: &[],
            max_express: 2,
            heavy_cap: 1,
            windows: &[],
            pauses: &pauses,
            presence: Presence {
                idle: true,
                away_window: false,
            },
            mandatory_warming: false,
        });

        assert_eq!(
            plan.start
                .iter()
                .map(|dispatch| dispatch.id.as_str())
                .collect::<Vec<_>>(),
            vec!["registered"]
        );
    }
}
