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
    eye: Option<EyeSection>,
    /// `[routine]` + `[[routine.blocks]]` (#801): the recurring weekly personal-schedule config.
    /// Deserialized directly into the domain's own raw shape (mirrors `pipeline`) since the
    /// block list is structural, not scalar — no per-field windows overlay for the array.
    routine: Option<crate::routine::RoutineFileConfig>,
    staff: Option<StaffSection>,
    mcp: Option<McpSection>,
    managed_repos: Option<ManagedReposSection>,
    tools: Option<ToolsSection>,
    web: Option<WebSection>,
    cockpit: Option<CockpitSection>,
    log: Option<LogSection>,
    scout: Option<ScoutSection>,
    dispatch: Option<DispatchSection>,
    pipeline: Option<super::PipelineConfig>,
    orchestrate: Option<OrchestrateSection>,
    /// `[hosts.<alias>]`: ssh-host enrichment for `bn connect` / `bn dispatch --host`. Keyed by
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

/// `[mcp]`: per-repo MCP wiring options (#323). `extras` is the opt-in list of curated
/// planning/productivity MCP servers (`crate::mcp::EXTRAS` names) that `bn mcp init` — and the
/// dispatch setup path — wire alongside bn's own server. Empty/absent = bn only.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct McpSection {
    extras: Option<Vec<String>>,
}

/// `[managed_repos]`: repos bn keeps cloned + refreshed under the bn home (`~/.bn/repos`). Each
/// entry is a clone URL; the refresh-all job clones it bare+worktree (via `bn wt`, WT_ROOT pinned
/// to the managed root) if absent, then fetches/fast-forwards it. This is how dotfiles and other
/// always-checked-out repos travel onto a machine without manual `git clone`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ManagedReposSection {
    repos: Option<Vec<String>>,
}

/// `[tools]`: managed docker-backed services. `builtins` is the opt-in allowlist of bn-shipped
/// specs (currently `n8n`). Empty/absent means no built-ins are active; user entries in
/// `<config_dir>/tools.toml` remain active.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ToolsSection {
    builtins: Option<Vec<String>>,
}

/// `[web]`: the `bn-web` dashboard server (#386/#388). All optional — absent falls back to the
/// loopback / 7770 / 2s defaults resolved in [`super::WebConfig`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct WebSection {
    /// TCP port `bn serve` binds. Default 7770.
    port: Option<u16>,
    /// Bind address. Default `127.0.0.1` (loopback); `bn serve --lan` overrides to `0.0.0.0`.
    bind: Option<String>,
    /// Fleet-snapshot poll cadence (seconds) for the SSE stream. Default 2.
    fleet_poll_secs: Option<u64>,
    /// Start the managed bn-web service from tmux lifecycle hooks. Default true.
    tmux_mcp_autostart: Option<bool>,
    /// Show the bnweb:<port> health segment in the tmux status bar. Default true.
    tmux_mcp_badge: Option<bool>,
}

/// `[cockpit]`: the `bn orchestrate cockpit` grounding view. `ground_secs` is the redraw cadence
/// (seconds) of the self-refreshing grounding loop — absent falls back to the
/// [`DEFAULT_COCKPIT_GROUND_SECS`](super::DEFAULT_COCKPIT_GROUND_SECS) default resolved in
/// [`super::Config`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct CockpitSection {
    /// Grounding-loop redraw cadence (seconds). Default 30; clamped `>= 1` at resolve.
    ground_secs: Option<u64>,
}

/// `[log]`: the opt-in verbose/debug **action log** (structured JSONL of every high-value action
/// bn performs, for after-the-fact debugging). All optional — absent/false ⇒ off. Env
/// `BN_VERBOSE`/`BN_LOG` and the global `--verbose/--debug` flag override this at higher
/// precedence. `dir` overrides the default `<state_dir>/logs` location; `retain_days` bounds how
/// many daily files are kept. See [`super::LogConfig`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct LogSection {
    /// Turn the action log on. Default false.
    enabled: Option<bool>,
    /// Minimum level written (`debug`|`info`|`warn`|`error`). Default `debug` when enabled.
    level: Option<String>,
    /// Override the log directory. Default `<state_dir>/logs`.
    dir: Option<String>,
    /// Daily files to retain before pruning. Default 14; `0` keeps all.
    retain_days: Option<i64>,
}

/// `[dispatch]`: defaults for `bn dispatch`. `plan_review` (#349) makes the plan-review gate the
/// default — a dispatched ship agent proposes a plan, registers a plan-review, and pauses until
/// consensus approves before implementing / opening a PR. Off by default; `--plan` opts in
/// per-dispatch. `--yolo` uses agent consensus rather than bypassing sensitive-plan review.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DispatchSection {
    plan_review: Option<bool>,
    plan_reviewers: Option<usize>,
    plan_review_quorum: Option<usize>,
}

/// `[orchestrate]`: defaults for `bn orchestrate` (#382). `agent` is the harness the daemon
/// dispatches new agents under when `--agent` isn't passed (`claude` | `copilot`, plus the
/// `bn dispatch` aliases); absent falls back to `copilot`. The usage-limit auto-fallback (switch a
/// stalled lane to the other harness) is always on regardless of this default.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct OrchestrateSection {
    agent: Option<String>,
    /// Seconds without pane or transcript activity before `working` is treated as stuck.
    stuck_idle_secs: Option<i64>,
    /// Keep resident supervisors supplied from open GitHub issues.
    keep_warm: Option<bool>,
    low_water: Option<usize>,
    own: Option<Vec<String>>,
    exclude_titles: Option<Vec<String>>,
    exclude_labels: Option<Vec<String>>,
    brainstorm_count: Option<usize>,
    brainstorm_interval_secs: Option<i64>,
    /// Pause local dispatch while free RAM (MB) is below this — the #377 anti-thrash guard.
    min_free_ram_mb: Option<u64>,
    /// Pause local dispatch while the load average is above this (#377).
    max_loadavg: Option<f64>,
    /// Pause local dispatch (and hold the backlog pull) while swap usage percent is above this
    /// (#668) — swap saturation, not agent count, is what chokes a RAM-bound box.
    max_swap_pct: Option<f64>,
    /// Pause local dispatch once this-hour token consumption reaches this rate (#377).
    tokens_per_hour: Option<u64>,
    /// Pause local dispatch once today's API-equiv `$` spend reaches this (#377).
    daily_cost_usd: Option<f64>,
    /// Per-work-class daily API-equiv `$` spend caps (#381 GAP 5) — pause just the coding /
    /// research lane once its own today's spend reaches the cap, leaving the other lane running.
    daily_cost_usd_coding: Option<f64>,
    daily_cost_usd_research: Option<f64>,
    /// Opt in to overflowing `auto` items to a GitHub-hosted runner when local is full/paused
    /// (#377). Off by default — an explicit `substrate: remote` item always fans out regardless.
    remote_overflow: Option<bool>,
}

/// `[wrap]`: the end-of-day digest job (#98). Disabled by default; `time` is the local
/// `HH:MM` the systemd timer fires at (informative — the timer carries the real schedule).
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct WrapSection {
    enabled: Option<bool>,
    time: Option<String>,
}

/// `[standup]`: the standup-autopilot job (#99). Disabled by default; `fetch_delta` gates the
/// optional one-shot PR delta fetch.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct StandupSection {
    enabled: Option<bool>,
    time: Option<String>,
    fetch_delta: Option<bool>,
}

/// `[today]`: Today's agent (#577), the `today-agent` cron. Disabled by default — a scheduled run
/// writes suggestions the owner reviews — so it stays opt-in. A manual `bn today` runs regardless.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct TodaySection {
    enabled: Option<bool>,
}

/// `[attention_agent]`: the periodic LLM cron that reviews recent cron findings + cron health and
/// writes a ranked attention list. Disabled by default (it spends a model call on a schedule) so
/// it stays opt-in, like the other agentic crons.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct AttentionAgentSection {
    enabled: Option<bool>,
}

/// `[dxi]`: the personal DXI micro-prompt (#110). Disabled by default; opt-in surfaces a
/// <10s daily self-rating beside the daily files.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DxiSection {
    enabled: Option<bool>,
}

/// `[night]`: the nightly fleet planner (the `night-shift` job / `bn night run`). Disabled by
/// default and opt-in per repo — nothing runs unless `enabled` is set AND repos are allowlisted.
/// `max_dispatch` hard-caps how many promoted todos a single shift dispatches; `window` is the
/// local `HH:MM-HH:MM` the shift may run in (a manual `bn night run` outside it declines unless
/// `--force`); `repos` is the allowlist of bare repo names the shift may touch.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct NightSection {
    enabled: Option<bool>,
    max_dispatch: Option<usize>,
    window: Option<String>,
    repos: Option<Vec<String>>,
}

/// `[sentinel]`: the Debt Sentinel — the 22:30 estate patrol (#259). `repos` is the allowlist of
/// bare repo names it scans and may promote from; it defaults to the night-shift allowlist
/// (`[night] repos`) so the two stay in lockstep unless overridden, and an empty allowlist makes
/// the patrol a no-op. `todo_age_days` is how old a TODO/FIXME marker must be before it's flagged.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct SentinelSection {
    repos: Option<Vec<String>>,
    todo_age_days: Option<i64>,
}

/// `[quarter]`: the recurring impact-agent review (#252). Disabled by default — it dispatches an
/// LLM harness, so it stays opt-in. `review_cron` overrides the `quarter-review` job's systemd
/// `OnCalendar` (a sane weekly default lives in `config/crons.toml`); `review_window_days` bounds
/// the ground-truth window when there's no prior review; `harness` picks the CLI (copilot/claude).
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct QuarterSection {
    enabled: Option<bool>,
    review_cron: Option<String>,
    review_window_days: Option<i64>,
    harness: Option<String>,
}

/// `[dojo]`: the gamified learning engine (#253, `bn dojo`). `repos` is the rotation allowlist of
/// bare repo names the session and card-miner draw from (empty = every repo with cards).
/// `daily_target` caps how many cards one session presents. `enabled` gates the nightly
/// `dojo-cards` mining cron (off by default — mining is opt-in), but `bn dojo` itself always runs.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DojoSection {
    enabled: Option<bool>,
    repos: Option<Vec<String>>,
    daily_target: Option<usize>,
}

/// `[chief]`: the Chief of Staff briefing agent (#257) — the twice-daily `staff-chief-{am,pm}`
/// jobs. Disabled by default (it writes daily goals), so it stays opt-in. `am_time`/`pm_time` are
/// the local `HH:MM` the AM brief / PM shutdown fire (informative — the real schedule lives in
/// `config/crons.toml`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ChiefSection {
    enabled: Option<bool>,
    am_time: Option<String>,
    pm_time: Option<String>,
}

/// `[archivist]`: the Archivist (#263) — the Friday private weekly chronicle (`staff-archivist`
/// job). Disabled by default (it writes a file into the private notes repo), so it stays opt-in.
/// `time` is the local `HH:MM` the chronicle fires (informative — the real schedule lives in
/// `config/crons.toml`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ArchivistSection {
    enabled: Option<bool>,
    time: Option<String>,
}

/// `[staff]`: the agent-staff framework's multi-machine settings (#270). `home` names the single
/// machine the staff runs on — `bn staff run` (and every `staff-<agent>` cron) no-ops loudly on
/// any other box, so two laptops never duplicate-review the same PRs into the synced feed. Unset =
/// the staff runs anywhere (a single-machine setup needs no homing). A per-agent `only_on` in
/// `config/agents/*.md` overrides this for that one agent (e.g. a work-box-only agent). Env: `$BN_STAFF_HOME`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct StaffSection {
    home: Option<String>,
}

/// `[scout]`: the Scout radar agent (#262). All optional. `watch` is the public `owner/repo`
/// upstream watchlist the Scout scans for help-wanted / good-first issues; `tools` the public tool
/// repos in your stack whose changelogs it tracks; `max_findings` overrides the ≤ 5 cap. All three
/// are the *only* outbound tokens, so they're public by definition.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ScoutSection {
    watch: Option<Vec<String>>,
    tools: Option<Vec<String>>,
    max_findings: Option<usize>,
}

/// `[retro]`: the Interviewer (#261) — the weekly subjective-retro pull agent. Disabled by
/// default (it raises a weekly attention item), so it stays opt-in. Enabling registers the
/// `staff-interviewer` cron, which raises the Friday item and escalates it once on Monday; the
/// session itself is always run by hand (`bn retro chat`), never dispatched.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RetroSection {
    enabled: Option<bool>,
}

/// `[mentor]`: the Mentor (#258) — the Mon..Fri `staff-mentor` post-merge review-to-teach agent.
/// Disabled by default (it dispatches an LLM harness and appends craft notes to your branch
/// notes), so it stays opt-in. A manual `bn mentor` runs regardless.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct MentorSection {
    enabled: Option<bool>,
}

/// `[eye]`: the opt-in keratoconus protective-behaviour coach. `last_review` seeds monitoring
/// state until `bn eye review` records a newer date; the interval is clamped to 6-12 months.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct EyeSection {
    enabled: Option<bool>,
    last_review: Option<String>,
    review_interval_months: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct JobsSection {
    /// `[jobs.only_on]`: job name → machines it may run on. Unlisted jobs run everywhere.
    only_on: Option<HashMap<String, Vec<String>>>,
    /// `[jobs].tmux_window`: when true, `bn run` runs each job in a tmux window named after the
    /// job (visible cron) instead of headlessly. Default false.
    tmux_window: Option<bool>,
    /// `[jobs].cron_global_layout`: how global visible-crons are laid out in the `cron` session —
    /// `"window"` (default, one window per job) or `"pane"` (one shared `cron` window, a pane per
    /// job, like repo crons). Default `"window"`.
    cron_global_layout: Option<String>,
    /// `[jobs].session`: the tmux session that hosts global crons *and* the fleet/cockpit agents
    /// (controller, attention lanes, dispatch orchestrator, scheduled `agents/*.md`). One knob to
    /// re-home the whole automation fleet. Default `"cron"`; `$BN_CRON_SESSION` overrides it. A
    /// per-agent `window: <session>:<window>` frontmatter still wins for that one agent.
    session: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct PersonalSection {
    notes_root: Option<String>,
    /// `[personal].overlay_root`: the personal overlay layered on top for `agents/`+`skills/`
    /// discovery (default `~/projects/personal-notes/bn`). Overridden by `$BN_OVERLAY_DIR`.
    overlay_root: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct WorkSection {
    notes_root: Option<String>,
    work_repos: Option<Vec<String>>,
    /// `[work].only`: this is a work machine — hide non-work repos from cross-repo views.
    only: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ReviewSection {
    /// Weekday the `bn weekly` 1-1 window anchors to (name or `0..6`).
    weekly_anchor: Option<String>,
    /// Review SLA: hours-to-first-review target (default 4).
    sla_first_review_hours: Option<f64>,
    /// Review SLA: open→merge target hours (default 24).
    sla_merge_hours: Option<f64>,
    /// Harness the auto-review agent launches: `claude` or `copilot`.
    harness: Option<String>,
    /// `[review].diff_gate` (#607): enable the adversarial diff-only review gate on the ship-lane
    /// land path. Off by default.
    diff_gate: Option<bool>,
    /// `[review].diff_gate_reviews` (#607): independent approving diff-only reviews the gate
    /// requires. Default 2.
    diff_gate_reviews: Option<usize>,
}

/// `[harness]` (#501): per-machine harness availability. `disabled` lists harnesses turned off on
/// this box — currently only `claude`, when the account is org-disabled ("organization has disabled
/// Claude subscription access"). Copilot is the guaranteed fallback and can't be disabled. Backs the
/// dashboard's harness switcher, which renders a disabled harness greyed-out with a reason.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct HarnessSection {
    disabled: Option<Vec<String>>,
}

/// `[attention]`: the `bn next` attention list. `max_age_hours` is the recency cutoff —
/// time-aged actions older than it drop off (widened over the weekend on Mondays). Default 24.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct AttentionSection {
    max_age_hours: Option<f64>,
}

/// `[calib]`: coarse t-shirt-size estimate durations for `--est S|M|L` (see
/// [`crate::calib::CoarseSizes`]). Each is a human duration in the `bn focus start` grammar
/// (`30m`, `1h30m`, `3h`); a missing/garbage value falls back to the built-in default.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct CalibSection {
    s: Option<String>,
    m: Option<String>,
    l: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct CaptureSection {
    /// Folder new screenshots land in (WSL: a `/mnt/c/...` path). `bn shot` grabs from here.
    screenshot_dir: Option<String>,
    /// Folder the GIF recorder saves into. `bn shot --gif` grabs from here.
    gif_dir: Option<String>,
}

impl ConfigFile {
    /// Read + parse `config.toml`. `None` when the file is absent; a parse error is reported
    /// to stderr and treated as absent so a malformed file degrades to legacy/defaults
    /// rather than bricking every command.
    pub(super) fn load() -> Option<ConfigFile> {
        let path = config_file_path();
        let text = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<ConfigFile>(&text) {
            Ok(cf) => Some(cf),
            Err(e) => {
                eprintln!("bn: ignoring malformed {}: {e}", path.display());
                None
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

    pub(super) fn jobs_tmux_window(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.jobs.as_ref())
            .and_then(|j| j.tmux_window)
            .or_else(|| self.jobs.as_ref().and_then(|j| j.tmux_window))
    }

    pub(super) fn cron_global_layout(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.jobs.as_ref())
            .and_then(|j| j.cron_global_layout.clone())
            .or_else(|| {
                self.jobs
                    .as_ref()
                    .and_then(|j| j.cron_global_layout.clone())
            })
    }

    pub(super) fn jobs_session(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.jobs.as_ref())
            .and_then(|j| j.session.clone())
            .or_else(|| self.jobs.as_ref().and_then(|j| j.session.clone()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
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

    pub(super) fn eye_enabled(&self) -> Option<bool> {
        self.win()
            .and_then(|w| w.eye.as_ref())
            .and_then(|s| s.enabled)
            .or_else(|| self.eye.as_ref().and_then(|s| s.enabled))
    }

    pub(super) fn eye_last_review(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.eye.as_ref())
            .and_then(|s| s.last_review.clone())
            .or_else(|| self.eye.as_ref().and_then(|s| s.last_review.clone()))
    }

    pub(super) fn eye_review_interval_months(&self) -> Option<u32> {
        self.win()
            .and_then(|w| w.eye.as_ref())
            .and_then(|s| s.review_interval_months)
            .or_else(|| self.eye.as_ref().and_then(|s| s.review_interval_months))
    }

    /// `[routine]` + `[[routine.blocks]]` (#801), windows-overlaid whole-table like `pipeline`
    /// (the block list is structural, so a `[windows.routine]` table replaces the whole thing
    /// rather than merging field-by-field).
    pub(super) fn routine(&self) -> Option<crate::routine::RoutineFileConfig> {
        self.win()
            .and_then(|w| w.routine.clone())
            .or_else(|| self.routine.clone())
    }

    /// `[staff].home` — the machine the agent staff runs on (#270), windows-overlaid like the
    /// rest. `None` (unset) leaves the staff running anywhere.
    pub(super) fn staff_home(&self) -> Option<String> {
        self.win()
            .and_then(|w| w.staff.as_ref())
            .and_then(|s| s.home.clone())
            .or_else(|| self.staff.as_ref().and_then(|s| s.home.clone()))
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

    pub(super) fn cockpit_ground_secs(&self) -> Option<u64> {
        self.win()
            .and_then(|w| w.cockpit.as_ref())
            .and_then(|s| s.ground_secs)
            .or_else(|| self.cockpit.as_ref().and_then(|s| s.ground_secs))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_section_accessors() {
        let toml = r#"
            [log]
            enabled = true
            level = "info"
            dir = "/tmp/bn-logs"
            retain_days = 30
        "#;
        let cf: ConfigFile = toml::from_str(toml).expect("valid [log] toml");
        assert_eq!(cf.log_enabled(), Some(true));
        assert_eq!(cf.log_level().as_deref(), Some("info"));
        assert_eq!(cf.log_dir().as_deref(), Some("/tmp/bn-logs"));
        assert_eq!(cf.log_retain_days(), Some(30));
    }

    #[test]
    fn absent_log_section_yields_none() {
        let cf: ConfigFile = toml::from_str("").expect("empty toml");
        assert_eq!(cf.log_enabled(), None);
        assert_eq!(cf.log_level(), None);
        assert_eq!(cf.log_dir(), None);
        assert_eq!(cf.log_retain_days(), None);
    }

    #[test]
    fn parses_cockpit_section_accessor() {
        let toml = r#"
            [cockpit]
            ground_secs = 45
        "#;
        let cf: ConfigFile = toml::from_str(toml).expect("valid [cockpit] toml");
        assert_eq!(cf.cockpit_ground_secs(), Some(45));
    }

    #[test]
    fn absent_cockpit_section_yields_none() {
        let cf: ConfigFile = toml::from_str("").expect("empty toml");
        assert_eq!(cf.cockpit_ground_secs(), None);
    }

    #[test]
    fn jobs_session_accessor_trims_and_filters_empty() {
        let cf: ConfigFile =
            toml::from_str("[jobs]\nsession = \"fleet\"\n").expect("valid [jobs] toml");
        assert_eq!(cf.jobs_session().as_deref(), Some("fleet"));

        // Whitespace is trimmed.
        let cf: ConfigFile =
            toml::from_str("[jobs]\nsession = \"  cron  \"\n").expect("valid [jobs] toml");
        assert_eq!(cf.jobs_session().as_deref(), Some("cron"));

        // Blank string is treated as unset.
        let cf: ConfigFile =
            toml::from_str("[jobs]\nsession = \"   \"\n").expect("valid [jobs] toml");
        assert_eq!(cf.jobs_session(), None);

        // Absent [jobs] / absent key → None.
        let cf: ConfigFile = toml::from_str("[jobs]\ntmux_window = true\n").expect("valid toml");
        assert_eq!(cf.jobs_session(), None);
        let cf: ConfigFile = toml::from_str("").expect("empty toml");
        assert_eq!(cf.jobs_session(), None);
    }
}
