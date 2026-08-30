//! The `config.toml` deserialization layer: [`ConfigFile`] + its `*Section` tables and
//! windows-overlay accessors. Internal to the resolver — [`super::Config`] / [`super::JobConfig`]
//! consume it and expose the resolved values.

use super::config_file_path;
use std::collections::HashMap;
use std::path::PathBuf;

/// Parsed `config.toml`. All fields optional. A `[windows]` table overlays the unix-named
/// tables field-by-field when running on native Windows, so one file serves WSL (unix paths)
/// and native Windows (`C:\…`) — see the `win()`/accessor pattern below.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ConfigFile {
    worktree_dir: Option<String>,
    hostname: Option<String>,
    personal: Option<PersonalSection>,
    work: Option<WorkSection>,
    jobs: Option<JobsSection>,
    review: Option<ReviewSection>,
    harness: Option<HarnessSection>,
    attention: Option<AttentionSection>,
    calib: Option<CalibSection>,
    capture: Option<CaptureSection>,
    wrap: Option<WrapSection>,
    standup: Option<StandupSection>,
    dxi: Option<DxiSection>,
    night: Option<NightSection>,
    sentinel: Option<SentinelSection>,
    quarter: Option<QuarterSection>,
    dojo: Option<DojoSection>,
    chief: Option<ChiefSection>,
    today: Option<TodaySection>,
    attention_agent: Option<AttentionAgentSection>,
    archivist: Option<ArchivistSection>,
    retro: Option<RetroSection>,
    mentor: Option<MentorSection>,
    /// `[routine]` + `[[routine.blocks]]` (#801): the recurring weekly personal-schedule config.
    /// Deserialized directly into the domain's own raw shape (mirrors `pipeline`) since the
    /// block list is structural, not scalar — no per-field windows overlay for the array.
    routine: Option<crate::routine::RoutineFileConfig>,
    /// `[notify]` — the desktop-toast policy gate. Deserialized into the domain's own raw shape
    /// for the same reason `routine` is: the clamping and the `bar` vocabulary belong next to the
    /// policy that reads them. See [`crate::notify::policy`].
    notify: Option<crate::notify::policy::NotifyFileConfig>,
    staff: Option<StaffSection>,
    mcp: Option<McpSection>,
    managed_repos: Option<ManagedReposSection>,
    tools: Option<ToolsSection>,
    web: Option<WebSection>,
    log: Option<LogSection>,
    scout: Option<ScoutSection>,
    dispatch: Option<DispatchSection>,
    pipeline: Option<super::PipelineConfig>,
    orchestrate: Option<OrchestrateSection>,
    /// `[queue]`: the machine-wide work queue — which producers enqueue, and its ceilings.
    queue: Option<QueueSection>,
    /// `[machines.<name>]`: synced machine registrations. Missing current profile is deny-all.
    machines: Option<HashMap<String, MachineProfile>>,
    /// `[hosts.<alias>]`: ssh-host enrichment for `bn connect`. Keyed by
    /// the ssh-config `Host` alias; connection details stay in `~/.ssh/config`.
    pub(super) hosts: Option<HashMap<String, crate::hosts::HostExtra>>,
    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker list — each node's execution
    /// target, capability labels, and RAM budget. Resolved to a [`crate::hosts::NodeRecord`]
    /// registry the orchestrate scheduler places work across. Empty/absent ⇒ single-node behavior.
    pub(super) nodes: Option<HashMap<String, crate::hosts::NodeConfig>>,
    /// `[repo.<name>]`: per-repo verify, pipeline, and node-capability placement policy. Keyed by
    /// the repo's bn/local name; matched case-insensitively on the trailing path segment at use.
    pub(super) repo: Option<HashMap<String, super::RepoConfig>>,
    windows: Option<Box<ConfigFile>>,
}

use super::config_sections::*;

impl ConfigFile {
    pub(super) fn machine_profile(&self, machine: &str) -> Option<MachineProfile> {
        self.machines
            .as_ref()
            .and_then(|profiles| profiles.get(machine).cloned())
    }

    /// `[queue].capture` — the producers that enqueue instead of launching.
    pub(super) fn queue_capture(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.queue.as_ref())
            .and_then(|q| q.capture.clone())
            .or_else(|| self.queue.as_ref().and_then(|q| q.capture.clone()))
    }

    /// `[queue].coding` / `.research` / `.heartbeat_ttl_secs`.
    pub(super) fn queue_coding(&self) -> Option<usize> {
        self.queue.as_ref().and_then(|q| q.coding)
    }

    pub(super) fn queue_research(&self) -> Option<usize> {
        self.queue.as_ref().and_then(|q| q.research)
    }

    pub(super) fn queue_heartbeat_ttl_secs(&self) -> Option<i64> {
        self.queue.as_ref().and_then(|q| q.heartbeat_ttl_secs)
    }

    /// `[queue]` numeric knobs: the express budget and the two lease TTLs.
    pub(super) fn queue_nums(&self) -> (Option<usize>, Option<i64>, Option<i64>, Option<i64>) {
        let q = self.queue.as_ref();
        (
            q.and_then(|q| q.express_per_tick),
            q.and_then(|q| q.agent_lease_ttl_secs),
            q.and_then(|q| q.express_lease_ttl_secs),
            q.and_then(|q| q.opportunistic_idle_mins),
        )
    }

    /// `[[queue.windows]]` — resolved into [`crate::queue::window::PriorityWindow`] by the caller,
    /// which is where the duration grammar and the defaults live.
    pub(super) fn queue_windows(&self) -> Vec<crate::queue::window::PriorityWindow> {
        self.queue
            .as_ref()
            .and_then(|q| q.windows.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|w| {
                let at = w.at?;
                // A window with no clock or no span is not a window; dropping it beats silently
                // treating it as always-open.
                let secs = w
                    .fore
                    .as_deref()
                    .and_then(crate::focus::parse_duration)
                    .filter(|s| *s > 0)?;
                Some(crate::queue::window::PriorityWindow {
                    name: w.name.unwrap_or_else(|| at.clone()),
                    at,
                    secs,
                    boost: w.boost.unwrap_or(100),
                    matches: w.matches.unwrap_or_else(|| vec!["*".to_string()]),
                    mode: w
                        .mode
                        .as_deref()
                        .and_then(crate::queue::window::WindowMode::parse)
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    /// `[queue.kind_caps]` as raw `label -> cap` pairs; the kind labels are validated by the caller.
    pub(super) fn queue_kind_caps(&self) -> HashMap<String, usize> {
        self.queue
            .as_ref()
            .and_then(|q| q.kind_caps.clone())
            .unwrap_or_default()
    }

    /// Read + parse `config.toml`. `None` when the file is absent or invalid; errors are reported
    /// while ordinary config resolution remains fail-closed.
    pub(super) fn load() -> Option<ConfigFile> {
        Self::load_with_status().0
    }

    /// Read + parse `config.toml`, retaining whether a real file parsed successfully.
    ///
    /// Callers that reconcile managed services need this distinction: an absent or malformed file
    /// must deny execution, but it must not be treated as an explicit instruction to uninstall
    /// services.
    pub(super) fn load_with_status() -> (Option<ConfigFile>, bool) {
        let path = config_file_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, false),
            Err(error) => {
                eprintln!("bn: could not read {}: {error}", path.display());
                return (None, false);
            }
        };
        match toml::from_str::<ConfigFile>(&text) {
            Ok(cf) => (Some(cf), true),
            Err(e) => {
                eprintln!("bn: ignoring malformed {}: {e}", path.display());
                (None, false)
            }
        }
    }

    /// The `[windows]` overlay, but only on native Windows; `None` elsewhere. Accessors read
    /// the overlay first, then fall back to the base table — so a `[windows]` table that sets
    /// only some keys still inherits the rest.
    fn win(&self) -> Option<&ConfigFile> {
        if cfg!(windows) {
            self.windows.as_deref()
        } else {
            None
        }
    }

    pub(super) fn worktree_dir(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.worktree_dir.clone())
            .or_else(|| self.worktree_dir.clone())
            .map(PathBuf::from)
    }

    pub(super) fn hostname(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.hostname.clone())
            .or_else(|| self.hostname.clone())
    }

    pub(super) fn personal_notes_root(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.personal.as_ref())
            .and_then(|p| p.notes_root.clone())
            .or_else(|| self.personal.as_ref().and_then(|p| p.notes_root.clone()))
            .map(PathBuf::from)
    }

    pub(super) fn work_notes_root(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.work.as_ref())
            .and_then(|p| p.notes_root.clone())
            .or_else(|| self.work.as_ref().and_then(|p| p.notes_root.clone()))
            .map(PathBuf::from)
    }

    pub(super) fn overlay_root(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.personal.as_ref())
            .and_then(|p| p.overlay_root.clone())
            .or_else(|| self.personal.as_ref().and_then(|p| p.overlay_root.clone()))
            .map(PathBuf::from)
    }

    pub(super) fn work_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.work.as_ref())
            .and_then(|p| p.work_repos.clone())
            .or_else(|| self.work.as_ref().and_then(|p| p.work_repos.clone()))
    }

    /// `[managed_repos].repos`, windows-overlaid like the rest. The clone URLs bn keeps
    /// checked out under the managed root.
    pub(super) fn managed_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.managed_repos.as_ref())
            .and_then(|m| m.repos.clone())
            .or_else(|| self.managed_repos.as_ref().and_then(|m| m.repos.clone()))
    }

    pub(super) fn tools_builtins(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.tools.as_ref())
            .and_then(|t| t.builtins.clone())
            .or_else(|| self.tools.as_ref().and_then(|t| t.builtins.clone()))
    }

    /// `[repo.<name>]` (#454): per-repo overrides, windows-overlaid like the rest (a
    /// `[windows.repo]` table replaces `[repo]` on native Windows). `None` when unset.
    pub(super) fn repos(&self) -> Option<HashMap<String, super::RepoConfig>> {
        self.win()
            .and_then(|w| w.repo.clone())
            .or_else(|| self.repo.clone())
    }

    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker list, windows-overlaid like
    /// the rest. `None` when unset.
    pub(super) fn nodes(&self) -> Option<HashMap<String, crate::hosts::NodeConfig>> {
        self.win()
            .and_then(|w| w.nodes.clone())
            .or_else(|| self.nodes.clone())
    }

    /// `[jobs.only_on]`, windows-overlaid like the rest. Empty map when unset.
    pub(super) fn job_only_on(&self) -> HashMap<String, Vec<String>> {
        self.win()
            .and_then(|w| w.jobs.as_ref())
            .and_then(|j| j.only_on.clone())
            .or_else(|| self.jobs.as_ref().and_then(|j| j.only_on.clone()))
            .unwrap_or_default()
    }

    pub(super) fn work_only(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.work.as_ref())
            .and_then(|p| p.only)
            .or_else(|| self.work.as_ref().and_then(|p| p.only))
    }

    pub(super) fn weekly_anchor(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|p| p.weekly_anchor.clone())
            .or_else(|| self.review.as_ref().and_then(|p| p.weekly_anchor.clone()))
    }

    pub(super) fn sla_first_review_hours(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|p| p.sla_first_review_hours)
            .or_else(|| self.review.as_ref().and_then(|p| p.sla_first_review_hours))
    }

    pub(super) fn sla_merge_hours(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|p| p.sla_merge_hours)
            .or_else(|| self.review.as_ref().and_then(|p| p.sla_merge_hours))
    }

    pub(super) fn review_harness(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|p| p.harness.clone())
            .or_else(|| self.review.as_ref().and_then(|p| p.harness.clone()))
    }

    pub(super) fn disabled_harnesses(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.harness.as_ref())
            .and_then(|h| h.disabled.clone())
            .or_else(|| self.harness.as_ref().and_then(|h| h.disabled.clone()))
    }

    pub(super) fn default_harness(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.harness.as_ref())
            .and_then(|h| h.default.clone())
            .or_else(|| self.harness.as_ref().and_then(|h| h.default.clone()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub(super) fn attention_max_age_hours(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.attention.as_ref())
            .and_then(|a| a.max_age_hours)
            .or_else(|| self.attention.as_ref().and_then(|a| a.max_age_hours))
    }

    /// The coarse `--est S|M|L` durations. A per-key override (windows section first, then the
    /// top-level `[calib]`) that parses to a positive duration wins; otherwise the built-in
    /// default for that size stands, so a partial or malformed `[calib]` degrades per-key rather
    /// than dropping the whole table.
    pub(super) fn coarse_sizes(&self) -> crate::calib::CoarseSizes {
        let d = crate::calib::CoarseSizes::default();
        let pick = |get: &dyn Fn(&CalibSection) -> Option<String>, fallback: i64| -> i64 {
            self.win()
                .and_then(|w| w.calib.as_ref())
                .and_then(get)
                .or_else(|| self.calib.as_ref().and_then(get))
                .and_then(|s| crate::focus::parse_duration(&s))
                .filter(|&n| n > 0)
                .unwrap_or(fallback)
        };
        crate::calib::CoarseSizes {
            s: pick(&|c| c.s.clone(), d.s),
            m: pick(&|c| c.m.clone(), d.m),
            l: pick(&|c| c.l.clone(), d.l),
        }
    }

    pub(super) fn screenshot_dir(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.capture.as_ref())
            .and_then(|c| c.screenshot_dir.clone())
            .or_else(|| self.capture.as_ref().and_then(|c| c.screenshot_dir.clone()))
            .map(PathBuf::from)
    }

    pub(super) fn gif_dir(&self) -> Option<PathBuf> {
        self.win()
            .and_then(|w| w.capture.as_ref())
            .and_then(|c| c.gif_dir.clone())
            .or_else(|| self.capture.as_ref().and_then(|c| c.gif_dir.clone()))
            .map(PathBuf::from)
    }

    pub(super) fn wrap_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.wrap.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.wrap.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn wrap_time(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.wrap.as_ref())
            .and_then(|s| s.time.clone())
            .or_else(|| self.wrap.as_ref().and_then(|s| s.time.clone()))
    }

    pub(super) fn standup_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.standup.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.standup.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn standup_time(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.standup.as_ref())
            .and_then(|s| s.time.clone())
            .or_else(|| self.standup.as_ref().and_then(|s| s.time.clone()))
    }

    pub(super) fn standup_fetch_delta(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.standup.as_ref())
            .and_then(|s| s.fetch_delta)
            .or_else(|| self.standup.as_ref().and_then(|s| s.fetch_delta))
    }

    pub(super) fn dxi_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.dxi.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.dxi.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn night_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.night.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.night.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn night_max_dispatch(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.night.as_ref())
            .and_then(|s| s.max_dispatch)
            .or_else(|| self.night.as_ref().and_then(|s| s.max_dispatch))
    }

    pub(super) fn dispatch_plan_review(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.dispatch.as_ref())
            .and_then(|s| s.plan_review)
            .or_else(|| self.dispatch.as_ref().and_then(|s| s.plan_review))
    }

    pub(super) fn dispatch_plan_reviewers(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.dispatch.as_ref())
            .and_then(|s| s.plan_reviewers)
            .or_else(|| self.dispatch.as_ref().and_then(|s| s.plan_reviewers))
    }

    pub(super) fn dispatch_plan_review_quorum(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.dispatch.as_ref())
            .and_then(|s| s.plan_review_quorum)
            .or_else(|| self.dispatch.as_ref().and_then(|s| s.plan_review_quorum))
    }

    pub(super) fn pipeline(&self) -> Option<super::PipelineConfig> {
        self.win()
            .and_then(|w| w.pipeline.clone())
            .or_else(|| self.pipeline.clone())
    }

    pub(super) fn review_diff_gate(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|s| s.diff_gate)
            .or_else(|| self.review.as_ref().and_then(|s| s.diff_gate))
    }

    pub(super) fn review_diff_gate_reviews(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.review.as_ref())
            .and_then(|s| s.diff_gate_reviews)
            .or_else(|| self.review.as_ref().and_then(|s| s.diff_gate_reviews))
    }

    pub(super) fn orchestrate_agent(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.agent.clone())
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.agent.clone()))
    }

    pub(super) fn orchestrate_stuck_idle_secs(&self) -> Option<i64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.stuck_idle_secs)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.stuck_idle_secs))
    }

    pub(super) fn orchestrate_keep_warm(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.keep_warm)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.keep_warm))
    }

    pub(super) fn orchestrate_low_water(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.low_water)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.low_water))
    }

    pub(super) fn orchestrate_owned_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.own.clone())
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.own.clone()))
    }

    pub(super) fn orchestrate_exclude_titles(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.exclude_titles.clone())
            .or_else(|| {
                self.orchestrate
                    .as_ref()
                    .and_then(|s| s.exclude_titles.clone())
            })
    }

    pub(super) fn orchestrate_exclude_labels(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.exclude_labels.clone())
            .or_else(|| {
                self.orchestrate
                    .as_ref()
                    .and_then(|s| s.exclude_labels.clone())
            })
    }

    pub(super) fn orchestrate_brainstorm_count(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.brainstorm_count)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.brainstorm_count))
    }

    pub(super) fn orchestrate_brainstorm_interval_secs(&self) -> Option<i64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.brainstorm_interval_secs)
            .or_else(|| {
                self.orchestrate
                    .as_ref()
                    .and_then(|s| s.brainstorm_interval_secs)
            })
    }

    pub(super) fn orchestrate_min_free_ram_mb(&self) -> Option<u64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.min_free_ram_mb)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.min_free_ram_mb))
    }

    pub(super) fn orchestrate_max_loadavg(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.max_loadavg)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.max_loadavg))
    }

    pub(super) fn orchestrate_max_swap_pct(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.max_swap_pct)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.max_swap_pct))
    }

    pub(super) fn orchestrate_tokens_per_hour(&self) -> Option<u64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.tokens_per_hour)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.tokens_per_hour))
    }

    pub(super) fn orchestrate_daily_cost_usd(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.daily_cost_usd)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.daily_cost_usd))
    }

    pub(super) fn orchestrate_daily_cost_usd_coding(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.daily_cost_usd_coding)
            .or_else(|| {
                self.orchestrate
                    .as_ref()
                    .and_then(|s| s.daily_cost_usd_coding)
            })
    }

    pub(super) fn orchestrate_daily_cost_usd_research(&self) -> Option<f64> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.daily_cost_usd_research)
            .or_else(|| {
                self.orchestrate
                    .as_ref()
                    .and_then(|s| s.daily_cost_usd_research)
            })
    }

    pub(super) fn orchestrate_remote_overflow(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.orchestrate.as_ref())
            .and_then(|s| s.remote_overflow)
            .or_else(|| self.orchestrate.as_ref().and_then(|s| s.remote_overflow))
    }

    pub(super) fn night_window(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.night.as_ref())
            .and_then(|s| s.window.clone())
            .or_else(|| self.night.as_ref().and_then(|s| s.window.clone()))
    }

    pub(super) fn night_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.night.as_ref())
            .and_then(|s| s.repos.clone())
            .or_else(|| self.night.as_ref().and_then(|s| s.repos.clone()))
    }

    pub(super) fn sentinel_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.sentinel.as_ref())
            .and_then(|s| s.repos.clone())
            .or_else(|| self.sentinel.as_ref().and_then(|s| s.repos.clone()))
    }

    pub(super) fn sentinel_todo_age_days(&self) -> Option<i64> {
        self.win()
            .and_then(|w| w.sentinel.as_ref())
            .and_then(|s| s.todo_age_days)
            .or_else(|| self.sentinel.as_ref().and_then(|s| s.todo_age_days))
    }

    pub(super) fn quarter_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.quarter.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.quarter.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn quarter_review_cron(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.quarter.as_ref())
            .and_then(|s| s.review_cron.clone())
            .or_else(|| self.quarter.as_ref().and_then(|s| s.review_cron.clone()))
    }

    pub(super) fn quarter_review_window_days(&self) -> Option<i64> {
        self.win()
            .and_then(|w| w.quarter.as_ref())
            .and_then(|s| s.review_window_days)
            .or_else(|| self.quarter.as_ref().and_then(|s| s.review_window_days))
    }

    pub(super) fn quarter_harness(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.quarter.as_ref())
            .and_then(|s| s.harness.clone())
            .or_else(|| self.quarter.as_ref().and_then(|s| s.harness.clone()))
    }

    pub(super) fn dojo_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.dojo.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.dojo.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn dojo_repos(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.dojo.as_ref())
            .and_then(|s| s.repos.clone())
            .or_else(|| self.dojo.as_ref().and_then(|s| s.repos.clone()))
    }

    pub(super) fn dojo_daily_target(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.dojo.as_ref())
            .and_then(|s| s.daily_target)
            .or_else(|| self.dojo.as_ref().and_then(|s| s.daily_target))
    }

    pub(super) fn chief_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.chief.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.chief.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn today_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.today.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.today.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn attention_agent_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.attention_agent.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.attention_agent.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn chief_am_time(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.chief.as_ref())
            .and_then(|s| s.am_time.clone())
            .or_else(|| self.chief.as_ref().and_then(|s| s.am_time.clone()))
    }

    pub(super) fn chief_pm_time(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.chief.as_ref())
            .and_then(|s| s.pm_time.clone())
            .or_else(|| self.chief.as_ref().and_then(|s| s.pm_time.clone()))
    }

    pub(super) fn archivist_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.archivist.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.archivist.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn archivist_time(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.archivist.as_ref())
            .and_then(|s| s.time.clone())
            .or_else(|| self.archivist.as_ref().and_then(|s| s.time.clone()))
    }

    pub(super) fn retro_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.retro.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.retro.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn mentor_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.mentor.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.mentor.as_ref().and_then(|s| s.enabled))
    }

    /// `[routine]` + `[[routine.blocks]]` (#801), windows-overlaid whole-table like `pipeline`
    /// (the block list is structural, so a `[windows.routine]` table replaces the whole thing
    /// rather than merging field-by-field).
    pub(super) fn routine(&self) -> Option<crate::routine::RoutineFileConfig> {
        self.win()
            .and_then(|w| w.routine.clone())
            .or_else(|| self.routine.clone())
    }

    /// `[notify]` — the toast policy gate, windows-overlaid like the rest (a Windows box may want
    /// a different bar than a Linux one).
    pub(super) fn notify(&self) -> Option<crate::notify::policy::NotifyFileConfig> {
        self.win()
            .and_then(|w| w.notify.clone())
            .or_else(|| self.notify.clone())
    }

    /// `[staff].home` — the machine the agent staff runs on (#270), windows-overlaid like the
    /// rest. `None` (unset) leaves the staff running anywhere.
    pub(super) fn staff_home(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.staff.as_ref())
            .and_then(|s| s.home.clone())
            .or_else(|| self.staff.as_ref().and_then(|s| s.home.clone()))
    }

    /// `[staff].mail` — the agents whose card is drafted to email as soon as it lands. Empty by
    /// default: leaving the machine is opt-in per agent.
    pub(super) fn staff_mail(&self) -> Vec<String> {
        self.win()
            .and_then(|w| w.staff.as_ref())
            .map(|s| s.mail.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| self.staff.as_ref().map(|s| s.mail.clone()))
            .unwrap_or_default()
    }

    pub(super) fn mcp_extras(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.mcp.as_ref())
            .and_then(|s| s.extras.clone())
            .or_else(|| self.mcp.as_ref().and_then(|s| s.extras.clone()))
    }

    pub(super) fn web_port(&self) -> Option<u16> {
        self.win()
            .and_then(|w| w.web.as_ref())
            .and_then(|s| s.port)
            .or_else(|| self.web.as_ref().and_then(|s| s.port))
    }

    pub(super) fn web_bind(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.web.as_ref())
            .and_then(|s| s.bind.clone())
            .or_else(|| self.web.as_ref().and_then(|s| s.bind.clone()))
    }

    pub(super) fn web_fleet_poll_secs(&self) -> Option<u64> {
        self.win()
            .and_then(|w| w.web.as_ref())
            .and_then(|s| s.fleet_poll_secs)
            .or_else(|| self.web.as_ref().and_then(|s| s.fleet_poll_secs))
    }

    pub(super) fn web_tmux_mcp_autostart(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.web.as_ref())
            .and_then(|s| s.tmux_mcp_autostart)
            .or_else(|| self.web.as_ref().and_then(|s| s.tmux_mcp_autostart))
    }

    pub(super) fn web_tmux_mcp_badge(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.web.as_ref())
            .and_then(|s| s.tmux_mcp_badge)
            .or_else(|| self.web.as_ref().and_then(|s| s.tmux_mcp_badge))
    }

    pub(super) fn log_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.log.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.log.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn log_level(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.log.as_ref())
            .and_then(|s| s.level.clone())
            .or_else(|| self.log.as_ref().and_then(|s| s.level.clone()))
    }

    pub(super) fn log_dir(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.log.as_ref())
            .and_then(|s| s.dir.clone())
            .or_else(|| self.log.as_ref().and_then(|s| s.dir.clone()))
    }

    pub(super) fn log_retain_days(&self) -> Option<i64> {
        self.win()
            .and_then(|w| w.log.as_ref())
            .and_then(|s| s.retain_days)
            .or_else(|| self.log.as_ref().and_then(|s| s.retain_days))
    }

    pub(super) fn scout_watch(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.scout.as_ref())
            .and_then(|s| s.watch.clone())
            .or_else(|| self.scout.as_ref().and_then(|s| s.watch.clone()))
    }

    pub(super) fn scout_tools(&self) -> Option<Vec<String>> {
        self.win()
            .and_then(|w| w.scout.as_ref())
            .and_then(|s| s.tools.clone())
            .or_else(|| self.scout.as_ref().and_then(|s| s.tools.clone()))
    }

    pub(super) fn scout_max_findings(&self) -> Option<usize> {
        self.win()
            .and_then(|w| w.scout.as_ref())
            .and_then(|s| s.max_findings)
            .or_else(|| self.scout.as_ref().and_then(|s| s.max_findings))
    }
}
