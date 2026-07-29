//! The resolved [`Config`] model: layering (defaults → `config.toml` → legacy plaintext → env
//! overrides), the [`Resolved`] context key, [`ContextError`], and the classification predicates.

use super::config_file::ConfigFile;
use super::dirs::config_dir;
use super::util::{
    home, non_empty_env, parse_weekday, parse_work_repos, read_hostname, split_list,
};
use crate::pipeline::stage::Stage;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolved git context for the current directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub repo: String,
    pub branch: String,
}

/// A lightweight borrowed `(repo, branch)` view — the two identifiers that together key a note.
/// Passing this one aggregate instead of two adjacent `&str` params makes a silent argument swap
/// impossible and lets `(repo, branch)` travel as a unit. Borrow it from a [`Resolved`] via
/// [`Resolved::rb`], or build one directly with [`RepoBranch::new`] (e.g. in tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoBranch<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
}

impl<'a> RepoBranch<'a> {
    /// A borrowed `(repo, branch)` pair.
    pub fn new(repo: &'a str, branch: &'a str) -> RepoBranch<'a> {
        RepoBranch { repo, branch }
    }
}

impl Resolved {
    /// Borrow this context's `(repo, branch)` as a [`RepoBranch`] aggregate.
    pub fn rb(&self) -> RepoBranch<'_> {
        RepoBranch {
            repo: &self.repo,
            branch: &self.branch,
        }
    }
}

/// Why context resolution failed. Mirrors the oracle's single "Not in a git repo" exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    /// Not in a git repo, detached HEAD, or repo/branch otherwise underivable.
    NotInGitRepo,
    /// Caller supplied only one half of an explicit target. Falling back to cwd in that case
    /// silently writes to/reads from the wrong note, so explicit targets are all-or-nothing.
    PartialTarget,
}

/// `[web]` block: the `bn-web` dashboard server (#386/#388). `bn serve` binds `bind:port`; the SSE
/// watch task re-samples the fleet every `fleet_poll_secs`. Loopback + 7770 by default; `bn serve
/// --lan` overrides the bind to `0.0.0.0` at launch.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// TCP port `bn serve` listens on. From `$BN_WEB_PORT` / config.toml `[web].port`; default 7770.
    pub port: u16,
    /// Bind address. From `$BN_WEB_BIND` / config.toml `[web].bind`; default `127.0.0.1` (loopback).
    /// `bn serve --lan` overrides this to `0.0.0.0` for the run.
    pub bind: String,
    /// Fleet-snapshot poll cadence (seconds) for the SSE stream. From `$BN_WEB_FLEET_POLL_SECS` /
    /// config.toml `[web].fleet_poll_secs`; default 2.
    pub fleet_poll_secs: u64,
    /// Whether tmux lifecycle hooks ensure the managed bn-web service is running. Default true.
    pub tmux_mcp_autostart: bool,
    /// Whether tmux renders the bnweb:<port> service-health badge. Default true.
    pub tmux_mcp_badge: bool,
}

impl Default for WebConfig {
    fn default() -> WebConfig {
        WebConfig {
            port: 7770,
            bind: "127.0.0.1".to_string(),
            fleet_poll_secs: 2,
            tmux_mcp_autostart: true,
            tmux_mcp_badge: true,
        }
    }
}

/// `[log]` block: the opt-in verbose/debug **action log** (structured JSONL of every high-value
/// action, for after-the-fact debugging). Resolved from env (`BN_VERBOSE`/`BN_LOG`, `BN_LOG_DIR`,
/// `BN_LOG_RETAIN_DAYS`) layered over `config.toml [log]`. The global `--verbose/--debug` flag is
/// applied on top at CLI init (it is not part of the file/env config). Off by default; see
/// [`crate::log`] for the writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// Whether the action log is on (before the CLI `--verbose/--debug` override is applied).
    pub enabled: bool,
    /// Minimum level written, as a lowercase name (`debug`|`info`|`warn`|`error`).
    pub level: String,
    /// Explicit log directory override; `None` ⇒ `<state_dir>/logs`.
    pub dir: Option<PathBuf>,
    /// Daily files retained before pruning; `0` keeps all.
    pub retain_days: i64,
}

impl Default for LogConfig {
    fn default() -> LogConfig {
        LogConfig {
            enabled: false,
            level: "debug".to_string(),
            dir: None,
            retain_days: 14,
        }
    }
}

/// A `[repo.<name>]` entry (#454): per-repo overrides keyed by the repo's bn/local name. `verify`
/// is the repo's NATIVE verify command (e.g. `npm run validate`, `nx affected -t lint test`) that
/// `bn verify`/`bn gate` run in place of resolving the bn build/test/lint jobs — how a repo-agnostic
/// orchestrator learns each repo's own gate. Unset ⇒ fall back to the bn job, so the bn repo
/// itself (which configures no `[repo]` entry) runs its available conventional jobs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct RepoConfig {
    /// The repo's native verify command line, run verbatim via `sh -c`. Blank/absent ⇒ no override.
    pub verify: Option<String>,
    /// Per-stage model overrides for this repo. Missing fields inherit the global pipeline policy.
    pub pipeline: Option<PipelineConfig>,
    /// Node capabilities every execution target must advertise before this repo may be placed.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Node capabilities that make an execution target ineligible for this repo.
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// The resolved repo→node capability policy used by scheduler placement (#728).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoPlacementPolicy {
    pub requires: Vec<String>,
    pub excludes: Vec<String>,
}

/// One `[pipeline.models.<stage>]` entry. Any field may be omitted so repo-specific entries can
/// override only the model, effort, or harness while inheriting the other global/default values.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PipelineModelConfig {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Agent harness (`claude` | `copilot`) for a dispatch launched at this stage. `None` inherits
    /// the global/`--agent` default. The value is stored raw here; it's normalized/validated at the
    /// dispatch call site (this resolver is a hot, non-`Result` path).
    #[serde(alias = "agent")]
    pub harness: Option<String>,
}

/// `[pipeline]` model policy, also accepted below `[repo.<name>.pipeline]` for repo overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PipelineConfig {
    #[serde(default)]
    pub models: HashMap<Stage, PipelineModelConfig>,
}

/// The fully resolved model selection passed to an agent harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineModel {
    pub model: String,
    pub effort: String,
    /// The stage's harness override (`claude` | `copilot`), or `None` when the stage sets no
    /// harness (the dispatch then uses the `--agent`/global default). Raw string — validated at the
    /// dispatch call site.
    pub harness: Option<String>,
}

/// Default cockpit grounding-loop redraw cadence (seconds). Raised from the original 15s to cut
/// the flicker of a full clear+redraw; overridable via `[cockpit].ground_secs` /
/// `$BN_COCKPIT_GROUND_SECS`.
pub const DEFAULT_COCKPIT_GROUND_SECS: u64 = 30;

/// Storage + classification config, resolved from env + `config.toml` once (mirrors oracle
/// startup). Layering, low → high precedence: hardcoded defaults → config.toml → legacy
/// plaintext → env overrides. Env always wins, preserving the test contract.
#[derive(Debug, Clone)]
pub struct Config {
    pub personal_root: PathBuf,
    pub work_root: PathBuf,
    /// Optional personal overlay root (`~/projects/personal-notes/bn` by default) layered on top
    /// of `work_root`/`personal_root`/shipped for `agents/` and `skills/` discovery — personal
    /// overrides win. From `$BN_OVERLAY_DIR` or config.toml `[personal].overlay_root`. The tier is
    /// harmless when the directory is absent (discovery skips unreadable dirs), so machines without
    /// the overlay behave exactly as before. Backs the "move config into personal-notes" migration
    /// (a layered overlay, not a wholesale move of the notes/references/state home).
    pub overlay_root: Option<PathBuf>,
    pub worktree_dir: PathBuf,
    pub work_repos: Vec<String>,
    pub hostname: String,
    /// Per-job machine allowlist from `[jobs.only_on]`: job → machines it may run on. A job
    /// absent from the map runs everywhere. Backs `bn run`'s per-host gating.
    pub job_only_on: HashMap<String, Vec<String>>,
    /// Weekday the weekly 1-1 review window anchors to (`0=Sun .. 6=Sat`). `bn weekly`
    /// reports since the most recent occurrence of this day. From `$BN_WEEKLY_ANCHOR`,
    /// config.toml `[review].weekly_anchor`, else Monday.
    pub weekly_anchor: u32,
    /// Review SLA thresholds (hours): time-to-first-review and open→merge. A PR awaiting your
    /// review nears/breaches the first-review SLA as its wait time climbs; `bn next` bumps its
    /// priority accordingly. From config.toml `[review].sla_first_review_hours` /
    /// `sla_merge_hours` (or `$BN_SLA_FIRST_REVIEW_HOURS` / `$BN_SLA_MERGE_HOURS`); defaults 4h /
    /// 24h, the healthy-review-latency reference.
    pub sla_first_review_hours: f64,
    pub sla_merge_hours: f64,
    /// Harness the auto-review agent launches (`claude` / `copilot`). From `$BN_REVIEW_HARNESS`
    /// or config.toml `[review].harness`; `None` falls back to the harness currently driving bn.
    pub review_harness: Option<String>,
    /// Harnesses turned off on this machine (`claude` when the account is org-disabled —
    /// *"organization has disabled Claude subscription access"*). From `[harness].disabled` or the
    /// comma-separated `$BN_DISABLED_HARNESSES`. Copilot is never disable-able (the guaranteed
    /// fallback), so a stray `copilot` entry is ignored. Backs [`crate::harness::availability`],
    /// which the dashboard's per-agent harness switcher (#501) reads to render an unusable harness
    /// disabled with a reason rather than failing a relaunch silently.
    pub disabled_harnesses: Vec<String>,
    /// `bn next` recency cutoff (hours): time-aged actions older than this drop off the attention
    /// list — it shows only what's recent enough to act on now. The effective window widens to
    /// cover the weekend on Mondays (see [`crate::next`]). From config.toml
    /// `[attention].max_age_hours` or `$BN_ATTENTION_MAX_AGE_HOURS`; default 24h.
    pub attention_max_age_hours: f64,
    /// Folder the OS / capture tool drops new screenshots into (`bn shot` moves the freshest
    /// into the branch note). From `$BN_SCREENSHOT_DIR` or config.toml `[capture].screenshot_dir`;
    /// `None` if unconfigured (no portable default — the path is machine-specific).
    pub screenshot_dir: Option<PathBuf>,
    /// Folder the GIF recorder (e.g. ScreenToGif) saves into; backs `bn shot --gif`. From
    /// `$BN_GIF_DIR` or config.toml `[capture].gif_dir`.
    pub gif_dir: Option<PathBuf>,
    /// `[jobs].tmux_window`: run each cron job in a tmux window named after it (visible cron)
    /// instead of headlessly. Default false. Backs `bn run`'s windowed dispatch.
    pub jobs_tmux_window: bool,
    /// `[jobs].cron_global_layout` = `"pane"` (or `$BN_CRON_GLOBAL_LAYOUT=pane`): global visible-crons
    /// share one `cron` window (a pane per job) instead of one window per job. Default false (window
    /// per job). Repo crons are always paned, independent of this.
    pub jobs_cron_global_pane: bool,
    /// `[jobs].session` (or `$BN_CRON_SESSION`): the tmux session that hosts global crons *and* the
    /// fleet/cockpit agents (controller, attention lanes, dispatch orchestrator, scheduled
    /// `agents/*.md`). One knob to re-home the whole automation fleet; default `"cron"`. A per-agent
    /// `window: <session>:<window>` frontmatter still wins for that one agent. Backs
    /// `wt::orchestrator_session()`.
    pub cockpit_session: String,
    /// `[work].only` (or `$BN_WORK_ONLY`): this is a work machine — cross-repo views (branches,
    /// next, summary, metrics) hide everything outside [`work_repos`] so personal work never
    /// surfaces on a work screen. Default false. Backs [`is_visible_repo`](Config::is_visible_repo).
    pub work_only: bool,
    /// `[managed_repos].repos` (or `$BN_MANAGED_REPOS`): clone URLs bn keeps checked out under the
    /// managed root ([`managed_repos_dir`]). The refresh-all job clones any missing one and
    /// fetches/fast-forwards the rest. Empty by default. Backs the dotfiles-on-a-new-machine flow.
    pub managed_repos: Vec<String>,
    /// `[hosts.<alias>]`: per-host enrichment of `~/.ssh/config` for `bn connect` /
    /// `bn dispatch --host` — what ssh config can't express (remote worktree root, default agent
    /// harness, whether bn is installed there). Registration here is the trust opt-in remote
    /// dispatch requires. Empty by default.
    pub hosts: HashMap<String, crate::hosts::HostExtra>,
    /// `[repo.<name>]`: per-repo verify, pipeline, and node-capability placement policy. Empty by
    /// default; see [`repo_verify`](Config::repo_verify) and
    /// [`repo_placement_policy`](Config::repo_placement_policy).
    pub repos: HashMap<String, RepoConfig>,
    /// `[pipeline.models.<stage>]`: global per-stage agent model/effort overrides. Missing entries
    /// use the built-in stage-specific defaults.
    pub pipeline: PipelineConfig,
    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker registry — each node's
    /// [`crate::backlog::ExecutionTarget`], capability labels, and RAM budget, resolved to
    /// [`crate::hosts::NodeRecord`]s (sorted by `node_id` for deterministic first-fit placement).
    /// Empty by default ⇒ the orchestrate scheduler places every lane `Local` (single-node
    /// behavior). Config-file only; there is no env override for a whole table.
    pub nodes: Vec<crate::hosts::NodeRecord>,
    /// `[calib] s|m|l`: the coarse t-shirt-size estimate durations (`--est S|M|L`), so sizing —
    /// and `bn next --fit` — works before you have real per-item duration estimates. Defaults
    /// `s=30m m=90m l=3h`; see [`crate::calib::CoarseSizes`].
    pub coarse_sizes: crate::calib::CoarseSizes,
    /// `[web]`: the `bn-web` dashboard server settings (port / bind / fleet poll cadence). See
    /// [`WebConfig`]. Backs `bn serve` and the `bn-web` binary.
    pub web: WebConfig,
    /// `[cockpit].ground_secs` (or `$BN_COCKPIT_GROUND_SECS`): how often the `bn orchestrate
    /// cockpit` grounding window redraws its snapshot, in seconds. From
    /// [`DEFAULT_COCKPIT_GROUND_SECS`] (30) by default; clamped `>= 1` so it can never become a
    /// `sleep 0` busy loop. Backs the `bn orchestrate cockpit` grounding loop (`ground_loop_cmd`
    /// in `bn-cli`).
    pub cockpit_ground_secs: u64,
    /// `[scout]`: the Scout radar agent (#262) — the public `owner/repo` watchlist, the tool-stack
    /// repos, and the findings cap. Empty watch/tools = the collectors idle. See
    /// [`crate::scout::ScoutConfig`].
    pub scout: crate::scout::ScoutConfig,
    /// `[log]`: the opt-in verbose/debug action log (structured JSONL of every high-value action,
    /// for after-the-fact debugging). Resolved from env + config.toml; the CLI `--verbose/--debug`
    /// flag layers on top at init. See [`LogConfig`] and [`crate::log`].
    pub log: LogConfig,
    /// `[dispatch].plan_review` (or `$BN_DISPATCH_PLAN_REVIEW`): make the plan-review gate (#349)
    /// the default for `bn dispatch` — a dispatched ship agent proposes a plan, registers a
    /// plan-review, and pauses until reviewer consensus or required human approval. `bn dispatch
    /// --plan` opts in per-dispatch regardless. Default false.
    pub dispatch_plan_review: bool,
    /// `[dispatch].plan_reviewers` / `$BN_DISPATCH_PLAN_REVIEWERS`: fresh independent plan
    /// reviewers launched by the plan-gate workflow. Default 3.
    pub dispatch_plan_reviewers: usize,
    /// `[dispatch].plan_review_quorum` / `$BN_DISPATCH_PLAN_REVIEW_QUORUM`: approvals required.
    /// Defaults to a strict majority of `dispatch_plan_reviewers`.
    pub dispatch_plan_review_quorum: usize,
    /// `[review].diff_gate` (or `$BN_REVIEW_DIFF_GATE`): enable the adversarial diff-only review
    /// gate (#607) on the ship-lane land path. When on, a dispatched ship PR is skipped by
    /// `bn wt merge --all` until [`review_diff_gate_reviews`](Self::review_diff_gate_reviews)
    /// independent diff-only reviewers have approved its current head and every blocking finding is
    /// resolved/waived. Default false.
    pub review_diff_gate: bool,
    /// `[review].diff_gate_reviews` (or `$BN_REVIEW_DIFF_GATE_REVIEWS`): how many independent
    /// approving diff-only reviews the gate requires. Default
    /// [`crate::review_gate::DEFAULT_REQUIRED_REVIEWS`] (2).
    pub review_diff_gate_reviews: usize,
    /// `[orchestrate].agent` (or `$BN_ORCHESTRATE_AGENT`): the harness `bn orchestrate` dispatches
    /// new agents under when `--agent` isn't passed (#382) — `claude` | `copilot` (aliases
    /// `claudecode` / `cc` / `gh`, normalized at use). `None` ⇒ `copilot`, the prior hardcoded
    /// default. The daemon still auto-switches a lane to the other harness on a usage-limit stall.
    pub orchestrate_agent: Option<String>,
    /// `[orchestrate].stuck_idle_secs` / `$BN_ORCH_STUCK_IDLE_SECS`.
    pub orchestrate_stuck_idle_secs: i64,
    /// `[orchestrate]` keep-warm defaults. Disabled unless config or `supervise --keep-warm`
    /// opts in; ownership limits which repos a box is allowed to feed.
    pub orchestrate_keep_warm: bool,
    pub orchestrate_low_water: usize,
    pub orchestrate_owned_repos: Vec<String>,
    pub orchestrate_exclude_titles: Vec<String>,
    pub orchestrate_exclude_labels: Vec<String>,
    pub orchestrate_brainstorm_count: usize,
    pub orchestrate_brainstorm_interval_secs: i64,
    /// `[orchestrate]` resource/budget caps (#377) — the guards the resource-aware scheduler
    /// ([`crate::scheduler::Caps`]) pauses local dispatch on. All `None`/off by default, so an
    /// unconfigured run behaves exactly as the pre-#377 fixed-concurrency daemon. Env overrides:
    /// `$BN_ORCH_MIN_FREE_RAM_MB`, `$BN_ORCH_MAX_LOADAVG`, `$BN_ORCH_MAX_SWAP_PCT`,
    /// `$BN_ORCH_TOKENS_PER_HOUR`,
    /// `$BN_ORCH_DAILY_COST_USD`, `$BN_ORCH_REMOTE_OVERFLOW`.
    pub orchestrate_min_free_ram_mb: Option<u64>,
    pub orchestrate_max_loadavg: Option<f64>,
    /// Pause local dispatch (and hold the backlog pull) while swap usage percent is above this —
    /// the #668 swap-pressure guard. `$BN_ORCH_MAX_SWAP_PCT`. Off by default.
    pub orchestrate_max_swap_pct: Option<f64>,
    pub orchestrate_tokens_per_hour: Option<u64>,
    pub orchestrate_daily_cost_usd: Option<f64>,
    /// Per-work-class daily API-equiv `$` spend caps (#381 GAP 5) — pause just the coding /
    /// research lane once its own today's spend reaches the cap.
    /// `$BN_ORCH_DAILY_COST_USD_CODING`, `$BN_ORCH_DAILY_COST_USD_RESEARCH`.
    pub orchestrate_daily_cost_usd_coding: Option<f64>,
    pub orchestrate_daily_cost_usd_research: Option<f64>,
    /// Whether `auto` items overflow to a GitHub-hosted runner when local is full/paused (#377).
    /// Off by default; an explicit `substrate: remote` item always fans out regardless.
    pub orchestrate_remote_overflow: bool,
    /// `[staff].home` (or `$BN_STAFF_HOME`): the single machine the agent staff runs on (#270).
    /// `bn staff run` — and every `staff-<agent>` cron — no-ops loudly on any other box so two
    /// laptops never duplicate-review the same PRs into the synced feed. `None` (unset) = the
    /// staff runs anywhere (a single-machine setup needs no homing). A per-agent `only_on` in
    /// `config/agents/*.md` overrides this for that one agent. Backs [`crate::staff::home_gate`].
    pub staff_home: Option<String>,
    /// `[routine]` + `[[routine.blocks]]` (#801): the resolved, validated recurring weekly
    /// personal-schedule config — timezone-aware, evaluated against an authoritative IANA zone
    /// (never `chrono::Local`). `Err` holds a clear message (bad IANA name, undiscoverable host
    /// zone, invalid day/time, zero-length block, or an overlap) rather than panicking or
    /// silently degrading; only `bn routine …` and the `bn next`/`bn daily` integration read
    /// this, so a broken `[routine]` section never breaks unrelated commands. See
    /// [`crate::routine::RoutineConfig`].
    pub routine: Result<crate::routine::RoutineConfig, String>,
}

impl Default for Config {
    /// The hardcoded-defaults tier (lowest precedence in [`Config::resolve`]): empty roots +
    /// hostname, and the same documented fallbacks `resolve` lands on when nothing is configured
    /// — Monday weekly anchor (`1`), 4h/24h review SLAs, a 24h attention window. Callers (and
    /// tests) spell only the fields they care about via `Config { personal_root: ..,
    /// ..Default::default() }`.
    fn default() -> Config {
        Config {
            personal_root: PathBuf::new(),
            work_root: PathBuf::new(),
            overlay_root: None,
            worktree_dir: PathBuf::new(),
            work_repos: Vec::new(),
            hostname: String::new(),
            job_only_on: HashMap::new(),
            weekly_anchor: 1,
            sla_first_review_hours: 4.0,
            sla_merge_hours: 24.0,
            review_harness: None,
            disabled_harnesses: Vec::new(),
            attention_max_age_hours: 24.0,
            screenshot_dir: None,
            gif_dir: None,
            jobs_tmux_window: false,
            jobs_cron_global_pane: false,
            cockpit_session: "cron".to_string(),
            work_only: false,
            managed_repos: Vec::new(),
            hosts: HashMap::new(),
            repos: HashMap::new(),
            pipeline: PipelineConfig::default(),
            nodes: Vec::new(),
            coarse_sizes: crate::calib::CoarseSizes::default(),
            web: WebConfig::default(),
            cockpit_ground_secs: DEFAULT_COCKPIT_GROUND_SECS,
            scout: crate::scout::ScoutConfig::default(),
            log: LogConfig::default(),
            dispatch_plan_review: false,
            dispatch_plan_reviewers: crate::plan_review::DEFAULT_REVIEWER_COUNT,
            dispatch_plan_review_quorum: crate::plan_review::majority_quorum(
                crate::plan_review::DEFAULT_REVIEWER_COUNT,
            ),
            review_diff_gate: false,
            review_diff_gate_reviews: crate::review_gate::DEFAULT_REQUIRED_REVIEWS,
            orchestrate_agent: None,
            orchestrate_stuck_idle_secs: crate::agent::DEFAULT_STUCK_LANE_IDLE_SECS,
            orchestrate_keep_warm: false,
            orchestrate_low_water: crate::orchestrate::DEFAULT_KEEP_WARM_LOW_WATER,
            orchestrate_owned_repos: Vec::new(),
            orchestrate_exclude_titles: vec!["gap(".to_string()],
            orchestrate_exclude_labels: vec!["blocked".to_string()],
            orchestrate_brainstorm_count: crate::orchestrate::DEFAULT_KEEP_WARM_BRAINSTORM_COUNT,
            orchestrate_brainstorm_interval_secs:
                crate::orchestrate::DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS,
            orchestrate_min_free_ram_mb: None,
            orchestrate_max_loadavg: None,
            orchestrate_max_swap_pct: None,
            orchestrate_tokens_per_hour: None,
            orchestrate_daily_cost_usd: None,
            orchestrate_daily_cost_usd_coding: None,
            orchestrate_daily_cost_usd_research: None,
            orchestrate_remote_overflow: false,
            staff_home: None,
            routine: Ok(crate::routine::RoutineConfig::disabled()),
        }
    }
}

impl Config {
    /// Production entry: layer defaults → `config.toml` → legacy plaintext → env overrides.
    /// Reads the real config file (`$BN_CONFIG` or `<config_dir>/config.toml`).
    pub fn load() -> Config {
        Config::resolve(ConfigFile::load().as_ref())
    }

    /// Build config from the environment only (no config file). Kept for the test contract
    /// and callers that must not read on-disk config; `load()` is the production path.
    pub fn from_env() -> Config {
        Config::resolve(None)
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> Config {
        let home = home();

        // BN_NOTES_DIR collapses personal+work to one root for tests; BN_NOTES_DIR_WORK
        // optionally re-splits. Otherwise config.toml roots, else the oracle defaults (bn:61-68).
        let (personal_root, work_root) = match non_empty_env("BN_NOTES_DIR") {
            Some(d) => {
                let work = non_empty_env("BN_NOTES_DIR_WORK")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(&d));
                (PathBuf::from(d), work)
            }
            None => {
                let personal = file
                    .and_then(|f| f.personal_notes_root())
                    .unwrap_or_else(|| PathBuf::from(format!("{home}/projects/branch-notes")));
                let work = file.and_then(|f| f.work_notes_root()).unwrap_or_else(|| {
                    let work_primary = PathBuf::from(format!("{home}/projects/notes/bn"));
                    if work_primary.is_dir() {
                        work_primary
                    } else {
                        PathBuf::from(format!("{home}/projects/branch-notes-work/bn"))
                    }
                });
                (personal, work)
            }
        };

        // Optional personal overlay (`~/projects/personal-notes/bn` by default) layered on top of
        // the notes roots for `agents/`+`skills/` discovery. Resolution mirrors the anchor
        // `install.sh` uses for the private `config.toml`: `$BN_OVERLAY_DIR` wins, else config.toml
        // `[personal].overlay_root`, else `$BN_PERSONAL_NOTES_DIR/bn` (the same personal-notes repo
        // that holds config.toml), else the default anchor. Empty string / `off` / `-` disables it.
        // The tier is inert when the directory is absent, so it's fully back-compat.
        let overlay_root = match non_empty_env("BN_OVERLAY_DIR") {
            Some(d) if d.eq_ignore_ascii_case("off") || d == "-" => None,
            Some(d) => Some(PathBuf::from(d)),
            None => file.and_then(|f| f.overlay_root()).or_else(|| {
                let anchor = non_empty_env("BN_PERSONAL_NOTES_DIR")
                    .map(|d| PathBuf::from(d).join("bn"))
                    .unwrap_or_else(|| PathBuf::from(format!("{home}/projects/personal-notes/bn")));
                Some(anchor)
            }),
        };
        let worktree_dir = non_empty_env("BN_WORKTREE_DIR")
            .map(PathBuf::from)
            .or_else(|| file.and_then(|f| f.worktree_dir()))
            .unwrap_or_else(|| PathBuf::from(format!("{home}/projects/worktree")));

        // BN_WORK_REPOS_FILE (tests) wins; else config.toml's inline list; else the legacy file.
        let work_repos = if let Some(f) = non_empty_env("BN_WORK_REPOS_FILE") {
            parse_work_repos(Path::new(&f))
        } else if let Some(list) = file.and_then(|f| f.work_repos()) {
            list
        } else {
            parse_work_repos(&config_dir().join("work-repos"))
        };

        let hostname = non_empty_env("BN_HOSTNAME")
            .or_else(|| file.and_then(|f| f.hostname()))
            .unwrap_or_else(read_hostname);

        let job_only_on = file.map(|f| f.job_only_on()).unwrap_or_default();

        let jobs_tmux_window = non_empty_env("BN_JOBS_TMUX_WINDOW")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .or_else(|| file.and_then(|f| f.jobs_tmux_window()))
            .unwrap_or(false);

        let jobs_cron_global_pane = non_empty_env("BN_CRON_GLOBAL_LAYOUT")
            .map(|v| v.eq_ignore_ascii_case("pane"))
            .or_else(|| {
                file.and_then(|f| f.cron_global_layout())
                    .map(|v| v.eq_ignore_ascii_case("pane"))
            })
            .unwrap_or(false);

        // The single session that hosts global crons + the fleet/cockpit agents. `$BN_CRON_SESSION`
        // (the long-standing cron override) wins so it re-homes agents too; else `[jobs].session`;
        // else `"cron"`. `wt::orchestrator_session()` caches this so every launch/protection path
        // resolves the same name.
        let cockpit_session = non_empty_env("BN_CRON_SESSION")
            .or_else(|| file.and_then(|f| f.jobs_session()))
            .unwrap_or_else(|| "cron".to_string());

        let weekly_anchor = non_empty_env("BN_WEEKLY_ANCHOR")
            .and_then(|s| parse_weekday(&s))
            .or_else(|| {
                file.and_then(|f| f.weekly_anchor())
                    .and_then(|s| parse_weekday(&s))
            })
            .unwrap_or(1); // Monday

        let screenshot_dir = non_empty_env("BN_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .or_else(|| file.and_then(|f| f.screenshot_dir()));
        let gif_dir = non_empty_env("BN_GIF_DIR")
            .map(PathBuf::from)
            .or_else(|| file.and_then(|f| f.gif_dir()));

        let work_only = non_empty_env("BN_WORK_ONLY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .or_else(|| file.and_then(|f| f.work_only()))
            .unwrap_or(false);

        let sla_first_review_hours = non_empty_env("BN_SLA_FIRST_REVIEW_HOURS")
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.sla_first_review_hours()))
            .filter(|h| *h > 0.0)
            .unwrap_or(4.0);
        let sla_merge_hours = non_empty_env("BN_SLA_MERGE_HOURS")
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.sla_merge_hours()))
            .filter(|h| *h > 0.0)
            .unwrap_or(24.0);
        let review_harness = non_empty_env("BN_REVIEW_HARNESS")
            .or_else(|| file.and_then(|f| f.review_harness()))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        // `[harness].disabled` (#501): harnesses turned off on this box (org-disabled claude). Env
        // `$BN_DISABLED_HARNESSES` is comma-separated and wins over the file list; each entry is
        // trimmed + lowercased and blanks dropped. `copilot` is never disable-able, so it's filtered
        // out here — the availability policy treats it as the guaranteed fallback regardless.
        let disabled_harnesses = non_empty_env("BN_DISABLED_HARNESSES")
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .or_else(|| file.and_then(|f| f.disabled_harnesses()))
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty() && s != "copilot")
            .collect::<Vec<_>>();
        let attention_max_age_hours = non_empty_env("BN_ATTENTION_MAX_AGE_HOURS")
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.attention_max_age_hours()))
            .filter(|h| *h > 0.0)
            .unwrap_or(24.0);

        // BN_MANAGED_REPOS (comma/space/newline-separated) wins, else config.toml's list, else none.
        let managed_repos = match non_empty_env("BN_MANAGED_REPOS") {
            Some(s) => split_list(&s),
            None => file.and_then(|f| f.managed_repos()).unwrap_or_default(),
        };

        let hosts = file.and_then(|f| f.hosts.clone()).unwrap_or_default();

        // `[nodes.<node_id>]` (#729): the declarative multi-node worker registry. Config file only.
        // Empty ⇒ single-node behavior (placement always lands `Local`).
        let nodes = crate::hosts::resolve_nodes(&file.and_then(|f| f.nodes()).unwrap_or_default());

        // `[repo.<name>]` (#454): per-repo overrides (currently the native verify command). Config
        // file only — there's no env override for a whole table. Empty ⇒ every repo falls back to
        // its bn `test`/`build` job (the bn repo's unchanged behavior).
        let repos = file.and_then(|f| f.repos()).unwrap_or_default();
        let pipeline = file.and_then(|f| f.pipeline()).unwrap_or_default();

        let coarse_sizes = file.map(|f| f.coarse_sizes()).unwrap_or_default();

        // `[web]` (#388/#879): env overrides win, then config.toml, then defaults.
        let web = {
            let env_bool = |key: &str| {
                non_empty_env(key).map(|value| {
                    value == "1"
                        || value.eq_ignore_ascii_case("true")
                        || value.eq_ignore_ascii_case("on")
                })
            };
            let port = non_empty_env("BN_WEB_PORT")
                .and_then(|s| s.parse::<u16>().ok())
                .or_else(|| file.and_then(|f| f.web_port()))
                .filter(|p| *p > 0)
                .unwrap_or(7770);
            let bind = non_empty_env("BN_WEB_BIND")
                .or_else(|| file.and_then(|f| f.web_bind()))
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let fleet_poll_secs = non_empty_env("BN_WEB_FLEET_POLL_SECS")
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| file.and_then(|f| f.web_fleet_poll_secs()))
                .filter(|s| *s > 0)
                .unwrap_or(2);
            let tmux_mcp_autostart = env_bool("BN_WEB_TMUX_MCP_AUTOSTART")
                .or_else(|| file.and_then(|f| f.web_tmux_mcp_autostart()))
                .unwrap_or(true);
            let tmux_mcp_badge = env_bool("BN_WEB_TMUX_MCP_BADGE")
                .or_else(|| file.and_then(|f| f.web_tmux_mcp_badge()))
                .unwrap_or(true);
            WebConfig {
                port,
                bind,
                fleet_poll_secs,
                tmux_mcp_autostart,
                tmux_mcp_badge,
            }
        };

        // `[cockpit].ground_secs` / `$BN_COCKPIT_GROUND_SECS`: cockpit grounding-loop redraw
        // cadence (seconds). Env wins over config.toml over the default; clamped `>= 1` so a
        // stray `0` can never spin the loop with `sleep 0`.
        let cockpit_ground_secs = non_empty_env("BN_COCKPIT_GROUND_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .or_else(|| file.and_then(|f| f.cockpit_ground_secs()))
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_COCKPIT_GROUND_SECS)
            .max(1);

        // `[log]`: the opt-in verbose/debug action log. Env wins over config.toml over the
        // off-by-default. `BN_VERBOSE` is a bare on switch; `BN_LOG` may be `1`/`true` or a level
        // name (`debug`/`info`/…) that both enables and sets the threshold.
        let log = {
            let env_verbose = non_empty_env("BN_VERBOSE").map(|v| {
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
            });
            let bn_log = non_empty_env("BN_LOG");
            let bn_log_level = bn_log
                .as_deref()
                .and_then(crate::log::LogLevel::parse)
                .map(|l| l.as_str().to_string());
            let env_bn_log_on = bn_log.as_deref().map(|v| {
                v == "1"
                    || v.eq_ignore_ascii_case("true")
                    || v.eq_ignore_ascii_case("on")
                    || crate::log::LogLevel::parse(v).is_some()
            });
            let enabled = env_verbose
                .or(env_bn_log_on)
                .or_else(|| file.and_then(|f| f.log_enabled()))
                .unwrap_or(false);
            let level = bn_log_level
                .or_else(|| non_empty_env("BN_LOG_LEVEL"))
                .or_else(|| file.and_then(|f| f.log_level()))
                .and_then(|s| crate::log::LogLevel::parse(&s).map(|l| l.as_str().to_string()))
                .unwrap_or_else(|| "debug".to_string());
            let dir = non_empty_env("BN_LOG_DIR")
                .or_else(|| file.and_then(|f| f.log_dir()))
                .map(PathBuf::from);
            let retain_days = non_empty_env("BN_LOG_RETAIN_DAYS")
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| file.and_then(|f| f.log_retain_days()))
                .unwrap_or(14);
            LogConfig {
                enabled,
                level,
                dir,
                retain_days,
            }
        };

        // `[scout]` (#262): env overrides win, then config.toml, then empty lists / the ≤ 5 cap.
        let scout = {
            let watch = match non_empty_env("BN_SCOUT_WATCH") {
                Some(s) => split_list(&s),
                None => file.and_then(|f| f.scout_watch()).unwrap_or_default(),
            };
            let tools = match non_empty_env("BN_SCOUT_TOOLS") {
                Some(s) => split_list(&s),
                None => file.and_then(|f| f.scout_tools()).unwrap_or_default(),
            };
            let max_findings = non_empty_env("BN_SCOUT_MAX_FINDINGS")
                .and_then(|s| s.parse::<usize>().ok())
                .or_else(|| file.and_then(|f| f.scout_max_findings()))
                .filter(|n| *n > 0)
                .unwrap_or(crate::scout::DEFAULT_MAX_FINDINGS);
            crate::scout::ScoutConfig {
                watch,
                tools,
                max_findings,
            }
        };

        // `[dispatch].plan_review` (#349): env override wins, then config.toml, then off.
        let dispatch_plan_review = non_empty_env("BN_DISPATCH_PLAN_REVIEW")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .or_else(|| file.and_then(|f| f.dispatch_plan_review()))
            .unwrap_or(false);
        let dispatch_plan_reviewers = non_empty_env("BN_DISPATCH_PLAN_REVIEWERS")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .or_else(|| file.and_then(|f| f.dispatch_plan_reviewers()))
            .filter(|n| *n > 0)
            .unwrap_or(crate::plan_review::DEFAULT_REVIEWER_COUNT);
        let dispatch_plan_review_quorum = non_empty_env("BN_DISPATCH_PLAN_REVIEW_QUORUM")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .or_else(|| file.and_then(|f| f.dispatch_plan_review_quorum()))
            .unwrap_or_else(|| crate::plan_review::majority_quorum(dispatch_plan_reviewers))
            .clamp(1, dispatch_plan_reviewers);

        // `[review].diff_gate` (#607): env override wins, then config.toml, then off. The required
        // review count clamps to at least 1 (a gate requiring zero reviews would be a no-op).
        let review_diff_gate = non_empty_env("BN_REVIEW_DIFF_GATE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .or_else(|| file.and_then(|f| f.review_diff_gate()))
            .unwrap_or(false);
        let review_diff_gate_reviews = non_empty_env("BN_REVIEW_DIFF_GATE_REVIEWS")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .or_else(|| file.and_then(|f| f.review_diff_gate_reviews()))
            .filter(|n| *n > 0)
            .unwrap_or(crate::review_gate::DEFAULT_REQUIRED_REVIEWS);

        // `[staff].home` (#270): env override wins, then config.toml; trimmed, empty → None so a
        // blank value never gates the staff off. `None` leaves the staff running anywhere.
        let staff_home = non_empty_env("BN_STAFF_HOME")
            .or_else(|| file.and_then(|f| f.staff_home()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // `[routine]` + `[[routine.blocks]]` (#801): parsed/validated once here so every reader
        // (CLI, `bn next`, `bn daily`) shares the same resolved config; `Err` carries a clear
        // message rather than panicking the whole `Config::resolve` on a bad `[routine]` section.
        let routine = crate::routine::RoutineConfig::resolve(file.and_then(|f| f.routine()));

        // `[orchestrate].agent` (#382): env override wins, then config.toml; trimmed, empty → None
        // so a blank value falls back to the `copilot` default. Normalized to a canonical harness at
        // use (it accepts the `bn dispatch` aliases), so an unparseable value degrades to the default.
        let orchestrate_agent = non_empty_env("BN_ORCHESTRATE_AGENT")
            .or_else(|| file.and_then(|f| f.orchestrate_agent()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let orchestrate_stuck_idle_secs = non_empty_env("BN_ORCH_STUCK_IDLE_SECS")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_stuck_idle_secs()))
            .unwrap_or(crate::agent::DEFAULT_STUCK_LANE_IDLE_SECS)
            .max(1);
        let orchestrate_keep_warm = file
            .and_then(|f| f.orchestrate_keep_warm())
            .unwrap_or(false);
        let orchestrate_low_water = file
            .and_then(|f| f.orchestrate_low_water())
            .unwrap_or(crate::orchestrate::DEFAULT_KEEP_WARM_LOW_WATER)
            .max(1);
        let orchestrate_owned_repos = file
            .and_then(|f| f.orchestrate_owned_repos())
            .unwrap_or_default();
        let orchestrate_exclude_titles = file
            .and_then(|f| f.orchestrate_exclude_titles())
            .unwrap_or_else(|| vec!["gap(".to_string()]);
        let orchestrate_exclude_labels = file
            .and_then(|f| f.orchestrate_exclude_labels())
            .unwrap_or_else(|| vec!["blocked".to_string()]);
        let orchestrate_brainstorm_count = file
            .and_then(|f| f.orchestrate_brainstorm_count())
            .unwrap_or(crate::orchestrate::DEFAULT_KEEP_WARM_BRAINSTORM_COUNT);
        let orchestrate_brainstorm_interval_secs = file
            .and_then(|f| f.orchestrate_brainstorm_interval_secs())
            .unwrap_or(crate::orchestrate::DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS)
            .max(1);

        // `[orchestrate]` resource/budget caps (#377): env override wins, then config.toml; an
        // unparseable / blank env is ignored (falls through to file, then off), so a stray value
        // never silently disables a guard. All default off — an unconfigured run is unchanged.
        let orchestrate_min_free_ram_mb = non_empty_env("BN_ORCH_MIN_FREE_RAM_MB")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_min_free_ram_mb()));
        let orchestrate_max_loadavg = non_empty_env("BN_ORCH_MAX_LOADAVG")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_max_loadavg()));
        let orchestrate_max_swap_pct = non_empty_env("BN_ORCH_MAX_SWAP_PCT")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_max_swap_pct()));
        let orchestrate_tokens_per_hour = non_empty_env("BN_ORCH_TOKENS_PER_HOUR")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_tokens_per_hour()));
        let orchestrate_daily_cost_usd = non_empty_env("BN_ORCH_DAILY_COST_USD")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_daily_cost_usd()));
        let orchestrate_daily_cost_usd_coding = non_empty_env("BN_ORCH_DAILY_COST_USD_CODING")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_daily_cost_usd_coding()));
        let orchestrate_daily_cost_usd_research = non_empty_env("BN_ORCH_DAILY_COST_USD_RESEARCH")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| file.and_then(|f| f.orchestrate_daily_cost_usd_research()));
        let orchestrate_remote_overflow = non_empty_env("BN_ORCH_REMOTE_OVERFLOW")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .or_else(|| file.and_then(|f| f.orchestrate_remote_overflow()))
            .unwrap_or(false);

        Config {
            personal_root,
            work_root,
            overlay_root,
            worktree_dir,
            work_repos,
            hostname,
            job_only_on,
            weekly_anchor,
            screenshot_dir,
            gif_dir,
            jobs_tmux_window,
            jobs_cron_global_pane,
            cockpit_session,
            work_only,
            sla_first_review_hours,
            sla_merge_hours,
            review_harness,
            disabled_harnesses,
            attention_max_age_hours,
            managed_repos,
            hosts,
            repos,
            pipeline,
            nodes,
            coarse_sizes,
            web,
            cockpit_ground_secs,
            scout,
            log,
            dispatch_plan_review,
            dispatch_plan_reviewers,
            dispatch_plan_review_quorum,
            review_diff_gate,
            review_diff_gate_reviews,
            orchestrate_agent,
            orchestrate_stuck_idle_secs,
            orchestrate_keep_warm,
            orchestrate_low_water,
            orchestrate_owned_repos,
            orchestrate_exclude_titles,
            orchestrate_exclude_labels,
            orchestrate_brainstorm_count,
            orchestrate_brainstorm_interval_secs,
            orchestrate_min_free_ram_mb,
            orchestrate_max_loadavg,
            orchestrate_max_swap_pct,
            orchestrate_tokens_per_hour,
            orchestrate_daily_cost_usd,
            orchestrate_daily_cost_usd_coding,
            orchestrate_daily_cost_usd_research,
            orchestrate_remote_overflow,
            staff_home,
            routine,
        }
    }

    /// Whether `job` is allowed to run on this host. A job listed in `[jobs.only_on]` runs only
    /// on its named machines (matched against [`machine`]); any job not listed runs everywhere.
    /// This is how `bn run` makes a cron laptop-specific without per-machine installers.
    pub fn job_allowed_here(&self, job: &str) -> bool {
        match self.job_only_on.get(job) {
            Some(machines) => machines.iter().any(|m| m == self.machine()),
            None => true,
        }
    }
    /// Whether `repo` is in the work allowlist (`[work].work_repos`). Matched on the trailing
    /// path segment so a gh `owner/repo` identifier (`m365-core/arena`) and a bare bn repo name
    /// (`arena`) classify the same.
    pub fn is_work_repo(&self, repo: &str) -> bool {
        let base = repo.rsplit('/').next().unwrap_or(repo);
        self.work_repos.iter().any(|w| w == base || w == repo)
    }

    /// The repo's configured NATIVE verify command (`[repo.<name>].verify`, #454), if any. The
    /// resolver lowercases `repo`, so keys are matched case-insensitively — a `[repo.MyRepo]`
    /// heading still resolves — and on the trailing path segment like [`is_work_repo`], so a gh
    /// `owner/repo` slug and a bare bn repo name resolve the same (a full-slug key wins over the
    /// base segment). `bn verify`/`bn gate` run this instead of the bn `test`/`build` job; `None`
    /// (unconfigured, blank, or whitespace-only) ⇒ fall back to the job — the bn repo's behavior.
    pub fn repo_verify(&self, repo: &str) -> Option<&str> {
        let base = repo.rsplit('/').next().unwrap_or(repo);
        let by = |want: &str| {
            self.repos
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(want))
                .map(|(_, v)| v)
        };
        by(repo)
            .or_else(|| by(base))
            .and_then(|r| r.verify.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Resolve `[repo.<name>].requires` / `excludes` for scheduler placement (#728). Matching is
    /// case-insensitive and tries the full slug before its trailing repo name, like
    /// [`repo_verify`](Self::repo_verify). `mt5-data-api` always requires `mt5-bridge` as a
    /// fail-closed built-in safety rule, even when no machine-local config declares it.
    pub fn repo_placement_policy(&self, repo: &str) -> RepoPlacementPolicy {
        let base = repo.rsplit(['/', '\\']).next().unwrap_or(repo);
        let by = |want: &str| {
            self.repos
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(want))
                .map(|(_, value)| value)
        };
        let configured = by(repo).or_else(|| by(base));
        let mut policy = RepoPlacementPolicy {
            requires: configured
                .map(|entry| Self::normalized_capabilities(&entry.requires))
                .unwrap_or_default(),
            excludes: configured
                .map(|entry| Self::normalized_capabilities(&entry.excludes))
                .unwrap_or_default(),
        };
        if base.eq_ignore_ascii_case("mt5-data-api")
            && !policy.requires.iter().any(|cap| cap == "mt5-bridge")
        {
            policy.requires.push("mt5-bridge".to_string());
        }
        policy
    }

    /// Resolve the agent model and reasoning effort for `stage`, layering built-in defaults →
    /// `[pipeline.models.<stage>]` → `[repo.<name>.pipeline.models.<stage>]`. Blank override values
    /// are ignored. Repo matching follows [`repo_verify`](Self::repo_verify): full slug first,
    /// then trailing repo name, case-insensitively.
    pub fn pipeline_model(&self, repo: &str, stage: Stage) -> PipelineModel {
        let (default_model, default_effort) = match stage {
            Stage::Plan | Stage::Critique => ("claude-opus-4.8", "max"),
            Stage::Investigate => ("gpt-5.4", "high"),
            _ => ("gpt-5.4-mini", "medium"),
        };
        let mut resolved = PipelineModel {
            model: default_model.to_string(),
            effort: default_effort.to_string(),
            harness: None,
        };
        Self::apply_pipeline_model(&mut resolved, self.pipeline.models.get(&stage));

        let base = repo.rsplit('/').next().unwrap_or(repo);
        let by = |want: &str| {
            self.repos
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(want))
                .map(|(_, value)| value)
        };
        if let Some(overrides) = by(repo)
            .or_else(|| by(base))
            .and_then(|repo| repo.pipeline.as_ref())
            .and_then(|pipeline| pipeline.models.get(&stage))
        {
            Self::apply_pipeline_model(&mut resolved, Some(overrides));
        }
        resolved
    }

    /// Whether a repo should appear in cross-repo views on this machine. Always true on a
    /// personal machine; on a [`work_only`](Config::work_only) machine, only work repos pass —
    /// the single predicate that keeps personal work off a work screen.
    pub fn is_visible_repo(&self, repo: &str) -> bool {
        !self.work_only || self.is_work_repo(repo)
    }

    fn apply_pipeline_model(resolved: &mut PipelineModel, override_: Option<&PipelineModelConfig>) {
        let Some(override_) = override_ else {
            return;
        };
        if let Some(model) = override_
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            resolved.model = model.to_string();
        }
        if let Some(effort) = override_
            .effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            resolved.effort = effort.to_string();
        }
        if let Some(harness) = override_
            .harness
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            resolved.harness = Some(harness.to_string());
        }
    }

    fn normalized_capabilities(values: &[String]) -> Vec<String> {
        let mut normalized = Vec::new();
        for value in values {
            let value = value.trim().to_ascii_lowercase();
            if !value.is_empty() && !normalized.contains(&value) {
                normalized.push(value);
            }
        }
        normalized
    }

    /// This host's path segment for the machine-subtree layout. A blank hostname falls back
    /// to `"unknown-host"` so a misconfigured machine never aliases the old (machine-less)
    /// layout and silently collides with another host's notes.
    pub fn machine(&self) -> &str {
        if self.hostname.is_empty() {
            "unknown-host"
        } else {
            &self.hostname
        }
    }
}
