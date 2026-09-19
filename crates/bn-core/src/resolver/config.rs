//! The resolved [`Config`] model: layering (compiled defaults → the config file layers), the
//! [`Resolved`] context key, [`ContextError`], and the classification predicates.

use super::config_file::ConfigFile;
pub use super::config_sections::MachineProfile;
use super::dirs::{machine_name, notes_roots};
use super::util::{home, non_empty_env, parse_weekday};
use crate::pipeline::stage::Stage;
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// TCP port `bn serve` listens on. From `[web].port`; default 7770.
    pub port: u16,
    /// Bind address. From `[web].bind`; default `127.0.0.1` (loopback). `bn serve --lan` overrides
    /// this to `0.0.0.0` for the run.
    pub bind: String,
    /// Fleet-snapshot poll cadence (seconds) for the SSE stream. From `[web].fleet_poll_secs`;
    /// default 2.
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
/// action, for after-the-fact debugging). Resolved from `[log]`, which `BN_VERBOSE`/`BN_LOG` can
/// switch on for one invocation. The global `--verbose/--debug` flag is applied on top at CLI init.
/// Off by default; see [`crate::log`] for the writer.
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

/// One `[pipeline.stages.<stage>]` entry — and the shape of every override layer above it (task
/// meta, note decision, CLI flag). Any field may be omitted so a narrower entry can override only
/// what it cares about while inheriting the rest; see [`crate::pipeline::profile::layer`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct StageConfig {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Agent harness (`claude` | `copilot`) for a dispatch launched at this stage. `None` inherits
    /// the global/`--agent` default. The value is stored raw here; it's normalized/validated at the
    /// dispatch call site (this resolver is a hot, non-`Result` path).
    pub harness: Option<String>,
    /// Workflow skill this stage runs, by `bn skills` name. `None` inherits, ultimately falling
    /// back to the launcher's built-in default for the stage.
    pub skill: Option<String>,
    /// Reference tokens attached to this stage's seed: `["*"]` for every reference in the store,
    /// `[]` for none, or explicit names. `None` inherits. Interpreted into a
    /// [`crate::pipeline::profile::RefSelection`] by the resolver.
    pub references: Option<Vec<String>>,
    /// `auto` | `hold`. Raw string so this stays a non-`Result` path; parsed by the resolver, which
    /// treats an unparseable value as absent.
    pub gate: Option<String>,
}

/// `[pipeline]` per-stage policy, also accepted below `[repo.<name>.pipeline]` for repo overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PipelineConfig {
    /// Keyed by [`Stage`].
    #[serde(default)]
    pub stages: HashMap<Stage, StageConfig>,
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

/// Storage + classification config, resolved once from the compiled defaults and the config file
/// layers. The only environment it reads is the set of path anchors test sandboxes pin
/// (`BN_NOTES_DIR`, `BN_WORKTREE_DIR`, `BN_HOSTNAME`, …) and the per-invocation `BN_VERBOSE` /
/// `BN_LOG` switch: no setting has a second, invisible home in the environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub personal_root: PathBuf,
    pub work_root: PathBuf,
    /// Optional personal overlay root (`~/projects/personal-notes/bn` by default) layered on top
    /// of `work_root`/`personal_root`/shipped for `agents/` and `skills/` discovery — personal
    /// overrides win. From `$BN_OVERLAY_DIR`, else `$BN_PERSONAL_NOTES_DIR/bn`, else the default.
    /// The tier is harmless when the directory is absent (discovery skips unreadable dirs).
    pub overlay_root: Option<PathBuf>,
    pub worktree_dir: PathBuf,
    /// `[work].work_repos`: the repositories whose notes go to the work notes tree.
    pub work_repos: Vec<String>,
    /// This machine's name: `$BN_HOSTNAME`, else the pinned identity.
    pub hostname: String,
    /// The `[machine]` table, if a config layer has one. `None` means no table: the machine may
    /// work on every repository and runs every service. A table only narrows that.
    pub machine_profile: Option<MachineProfile>,
    /// Per-job machine allowlist from `[jobs.only_on]`: job → machines it may run on. A job
    /// absent from the map runs everywhere. Backs `bn run`'s per-host gating.
    pub job_only_on: HashMap<String, Vec<String>>,
    /// Weekday the weekly 1-1 review window anchors to (`0=Sun .. 6=Sat`). `bn weekly`
    /// reports since the most recent occurrence of this day. From `[review].weekly_anchor`, else
    /// Monday.
    pub weekly_anchor: u32,
    /// Review SLA threshold (hours) for time-to-first-review. A PR awaiting your review
    /// nears/breaches it as its wait time climbs; `bn next` bumps its priority accordingly. From
    /// `[review].sla_first_review_hours`; default 4h, the healthy-review-latency reference.
    pub sla_first_review_hours: f64,
    /// Harness the auto-review agent launches (`claude` / `copilot`). From `[review].harness`;
    /// `None` falls back to the harness currently driving bn.
    pub review_harness: Option<String>,
    /// Harnesses turned off on this machine (`claude` when the account is org-disabled —
    /// *"organization has disabled Claude subscription access"*). From `[harness].disabled`.
    /// Copilot is never disable-able (the guaranteed fallback), so a stray `copilot` entry is
    /// ignored. Backs [`crate::harness::availability`], which the dashboard's per-agent harness
    /// switcher (#501) reads to render an unusable harness disabled with a reason rather than
    /// failing a relaunch silently.
    pub disabled_harnesses: Vec<String>,
    /// `bn next` recency cutoff (hours): time-aged actions older than this drop off the attention
    /// list — it shows only what's recent enough to act on now. The effective window widens to
    /// cover the weekend on Mondays (see [`crate::next`]). From `[attention].max_age_hours`;
    /// default 24h.
    pub attention_max_age_hours: f64,
    /// Folder the OS / capture tool drops new screenshots into (`bn shot` moves the freshest
    /// into the branch note). From `[capture].screenshot_dir`; `None` if unconfigured (no portable
    /// default — the path is machine-specific).
    pub screenshot_dir: Option<PathBuf>,
    /// Folder the GIF recorder (e.g. ScreenToGif) saves into; backs `bn shot --gif`. From
    /// `[capture].gif_dir`.
    pub gif_dir: Option<PathBuf>,
    /// Whether this is a work machine: it has the work notes tree and no personal one. Cross-repo
    /// views (branches, next, summary, metrics) then hide everything outside [`work_repos`], so
    /// personal work never surfaces on a work screen. Derived, not configured. Backs
    /// [`is_visible_repo`](Config::is_visible_repo).
    pub work_only: bool,
    /// `[managed_repos].repos`: clone URLs bn keeps checked out under the managed root
    /// ([`managed_repos_dir`]). The refresh-all job clones any missing one and fetches/fast-
    /// forwards the rest. Empty by default. Backs the dotfiles-on-a-new-machine flow.
    pub managed_repos: Vec<String>,
    /// `[hosts.<alias>]`: per-host enrichment of `~/.ssh/config` for `bn connect` — what ssh config
    /// can't express (remote worktree root, default agent harness, whether bn is installed there).
    /// Empty by default.
    pub hosts: HashMap<String, crate::hosts::HostExtra>,
    /// `[repo.<name>]`: per-repo verify, pipeline, and node-capability placement policy. Empty by
    /// default; see [`repo_verify`](Config::repo_verify) and
    /// `repo_placement_policy`.
    pub repos: HashMap<String, RepoConfig>,
    /// `[pipeline.stages.<stage>]`: global per-stage agent model/effort overrides. Missing entries
    /// use the built-in stage-specific defaults.
    pub pipeline: PipelineConfig,
    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker registry — each node's
    /// [`crate::backlog::ExecutionTarget`], capability labels, and RAM budget, resolved to
    /// [`crate::hosts::NodeRecord`]s (sorted by `node_id` for deterministic first-fit placement).
    /// Empty by default ⇒ the orchestrate scheduler places every lane `Local` (single-node
    /// behavior).
    pub nodes: Vec<crate::hosts::NodeRecord>,
    /// `[calib] s|m|l`: the coarse t-shirt-size estimate durations (`--est S|M|L`), so sizing —
    /// and `bn next --fit` — works before you have real per-item duration estimates. Defaults
    /// `s=30m m=90m l=3h`; see [`crate::calib::CoarseSizes`].
    pub coarse_sizes: crate::calib::CoarseSizes,
    /// `[web]`: the `bn-web` dashboard server settings (port / bind / fleet poll cadence). See
    /// [`WebConfig`]. Backs `bn serve` and the `bn-web` binary.
    pub web: WebConfig,
    /// `[scout]`: the Scout radar agent (#262) — the public `owner/repo` watchlist, the tool-stack
    /// repos, and the findings cap. Empty watch/tools = the collectors idle. See
    /// [`crate::scout::ScoutConfig`].
    pub scout: crate::scout::ScoutConfig,
    /// `[log]`: the opt-in verbose/debug action log (structured JSONL of every high-value action,
    /// for after-the-fact debugging). The CLI `--verbose/--debug` flag layers on top at init. See
    /// [`LogConfig`] and [`crate::log`].
    pub log: LogConfig,
    /// `[dispatch].plan_review`: make the plan-review gate (#349) the default for `bn dispatch` — a
    /// dispatched ship agent proposes a plan, registers a plan-review, and pauses until reviewer
    /// consensus or required human approval. `bn dispatch --plan` opts in per-dispatch regardless.
    /// Default false.
    pub dispatch_plan_review: bool,
    /// `[dispatch].plan_reviewers`: fresh independent plan reviewers launched by the plan-gate
    /// workflow. Default 3.
    pub dispatch_plan_reviewers: usize,
    /// `[dispatch].plan_review_quorum`: approvals required. Defaults to a strict majority of
    /// `dispatch_plan_reviewers`.
    pub dispatch_plan_review_quorum: usize,
    /// `[review].diff_gate`: enable the adversarial diff-only review gate (#607) on the ship-lane
    /// land path. When on, a dispatched ship PR is skipped by `bn wt merge --all` until
    /// [`review_diff_gate_reviews`](Self::review_diff_gate_reviews) independent diff-only reviewers
    /// have approved its current head and every blocking finding is resolved/waived. Default false.
    pub review_diff_gate: bool,
    /// `[review].diff_gate_reviews`: how many independent approving diff-only reviews the gate
    /// requires. Default [`crate::review_gate::DEFAULT_REQUIRED_REVIEWS`] (2).
    pub review_diff_gate_reviews: usize,
    /// `[harness].default`: the preferred harness every launcher uses when no `--agent`/`--harness`
    /// is requested — `claude` | `copilot` (aliases normalized at use). `None` ⇒ `copilot`, the
    /// guaranteed fallback. A configured harness that's org-disabled on this machine
    /// ([`Config::disabled_harnesses`]) also falls back to copilot. Backs
    /// [`crate::harness::preferred_harness`].
    pub default_harness: Option<String>,
    /// `[orchestrate].stuck_idle_secs`.
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
    /// `[orchestrate]` resource/budget caps (#377) — the guards the orchestrate daemon pauses local
    /// dispatch on. All `None`/off by default, so an
    /// unconfigured run behaves exactly as the pre-#377 fixed-concurrency daemon.
    pub orchestrate_min_free_ram_mb: Option<u64>,
    pub orchestrate_max_loadavg: Option<f64>,
    /// Pause local dispatch (and hold the backlog pull) while swap usage percent is above this —
    /// the #668 swap-pressure guard. Off by default.
    pub orchestrate_max_swap_pct: Option<f64>,
    pub orchestrate_tokens_per_hour: Option<u64>,
    pub orchestrate_daily_cost_usd: Option<f64>,
    /// Per-work-class daily API-equiv `$` spend caps (#381 GAP 5) — pause just the coding /
    /// research lane once its own today's spend reaches the cap.
    pub orchestrate_daily_cost_usd_coding: Option<f64>,
    pub orchestrate_daily_cost_usd_research: Option<f64>,
    /// Whether `auto` items overflow to a GitHub-hosted runner when local is full/paused (#377).
    /// Off by default; an explicit `substrate: remote` item always fans out regardless.
    pub orchestrate_remote_overflow: bool,
    /// `[staff].home`: the single machine the agent staff runs on (#270), so two laptops never
    /// duplicate-review the same PRs into the synced feed. `None` (unset) = the staff runs anywhere
    /// (a single-machine setup needs no homing). A per-agent `only_on` in `config/agents/*.md`
    /// overrides this for that one agent.
    pub staff_home: Option<String>,
    /// `[notify]`: the desktop-toast policy gate — which classes may interrupt, the per-key
    /// cooldown, and the daily/per-source budgets. Infallible: a bad value clamps or falls back to
    /// the default, because a notification preference must never break an unrelated command. See
    /// [`crate::notify::policy`].
    pub notify: crate::notify::policy::NotifyConfig,
}

impl Default for Config {
    /// The hardcoded-defaults tier (lowest precedence in [`Config::resolve`]): empty roots +
    /// hostname, and the same documented fallbacks `resolve` lands on when nothing is configured
    /// — Monday weekly anchor (`1`), a 4h review SLA, a 24h attention window, no machine profile.
    /// Callers (and tests) spell only the fields they care about via `Config { personal_root: ..,
    /// ..Default::default() }`.
    fn default() -> Config {
        Config {
            personal_root: PathBuf::new(),
            work_root: PathBuf::new(),
            overlay_root: None,
            worktree_dir: PathBuf::new(),
            work_repos: Vec::new(),
            hostname: String::new(),
            machine_profile: None,
            job_only_on: HashMap::new(),
            weekly_anchor: 1,
            sla_first_review_hours: 4.0,
            review_harness: None,
            disabled_harnesses: Vec::new(),
            attention_max_age_hours: 24.0,
            screenshot_dir: None,
            gif_dir: None,
            work_only: false,
            managed_repos: Vec::new(),
            hosts: HashMap::new(),
            repos: HashMap::new(),
            pipeline: PipelineConfig::default(),
            nodes: Vec::new(),
            coarse_sizes: crate::calib::CoarseSizes::default(),
            web: WebConfig::default(),
            scout: crate::scout::ScoutConfig::default(),
            log: LogConfig::default(),
            dispatch_plan_review: false,
            dispatch_plan_reviewers: crate::plan_review::DEFAULT_REVIEWER_COUNT,
            dispatch_plan_review_quorum: crate::plan_review::majority_quorum(
                crate::plan_review::DEFAULT_REVIEWER_COUNT,
            ),
            review_diff_gate: false,
            review_diff_gate_reviews: crate::review_gate::DEFAULT_REQUIRED_REVIEWS,
            default_harness: None,
            orchestrate_stuck_idle_secs: crate::agent::DEFAULT_STUCK_LANE_IDLE_SECS,
            orchestrate_keep_warm: false,
            orchestrate_low_water: crate::keep_warm::DEFAULT_KEEP_WARM_LOW_WATER,
            orchestrate_owned_repos: Vec::new(),
            orchestrate_exclude_titles: vec!["gap(".to_string()],
            orchestrate_exclude_labels: vec!["blocked".to_string()],
            orchestrate_brainstorm_count: crate::keep_warm::DEFAULT_KEEP_WARM_BRAINSTORM_COUNT,
            orchestrate_brainstorm_interval_secs:
                crate::keep_warm::DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS,
            orchestrate_min_free_ram_mb: None,
            orchestrate_max_loadavg: None,
            orchestrate_max_swap_pct: None,
            orchestrate_tokens_per_hour: None,
            orchestrate_daily_cost_usd: None,
            orchestrate_daily_cost_usd_coding: None,
            orchestrate_daily_cost_usd_research: None,
            orchestrate_remote_overflow: false,
            staff_home: None,
            notify: crate::notify::policy::NotifyConfig::default(),
        }
    }
}

impl Config {
    /// Production entry: the compiled defaults, then the config file layers.
    pub fn load() -> Config {
        Config::resolve(ConfigFile::load().as_ref())
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> Config {
        let home = home();

        // The notes roots come from the path anchors and defaults alone: the config lives inside
        // one of them, so it cannot also be what chooses it.
        let (personal_root, work_root) = notes_roots();
        // A machine that has the work notes tree and no personal one is a work machine — there is
        // nothing personal on it to surface.
        let work_only = !personal_root.is_dir() && work_root.is_dir();

        // Optional personal overlay (`~/projects/personal-notes/bn` by default) layered on top of
        // the notes roots for `agents/`+`skills/` discovery: `$BN_OVERLAY_DIR` wins, else
        // `$BN_PERSONAL_NOTES_DIR/bn`, else the default anchor. `off` / `-` disables it. The tier is
        // inert when the directory is absent.
        let overlay_root = match non_empty_env("BN_OVERLAY_DIR") {
            Some(d) if d.eq_ignore_ascii_case("off") || d == "-" => None,
            Some(d) => Some(PathBuf::from(d)),
            None => Some(
                non_empty_env("BN_PERSONAL_NOTES_DIR")
                    .map(|d| PathBuf::from(d).join("bn"))
                    .unwrap_or_else(|| PathBuf::from(format!("{home}/projects/personal-notes/bn"))),
            ),
        };
        let worktree_dir = super::dirs::worktree_dir();

        let work_repos = file.and_then(|f| f.work_repos()).unwrap_or_default();

        // The pinned identity rather than the live hostname, which on macOS is not stable across
        // networks; `$BN_HOSTNAME` for sandboxes.
        let hostname = machine_name();
        let machine_profile = file
            .and_then(|f| f.machine_profile())
            .map(Self::normalize_machine_profile);

        let job_only_on = file.map(|f| f.job_only_on()).unwrap_or_default();

        let weekly_anchor = file
            .and_then(|f| f.weekly_anchor())
            .and_then(|s| parse_weekday(&s))
            .unwrap_or(1); // Monday

        let screenshot_dir = file.and_then(|f| f.screenshot_dir());
        let gif_dir = file.and_then(|f| f.gif_dir());

        let sla_first_review_hours = file
            .and_then(|f| f.sla_first_review_hours())
            .filter(|h| *h > 0.0)
            .unwrap_or(4.0);
        let review_harness = file
            .and_then(|f| f.review_harness())
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        // `[harness].disabled` (#501): harnesses turned off on this box (org-disabled claude). Each
        // entry is trimmed + lowercased and blanks dropped. `copilot` is never disable-able, so it's
        // filtered out here — the availability policy treats it as the guaranteed fallback.
        let disabled_harnesses = file
            .and_then(|f| f.disabled_harnesses())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty() && s != "copilot")
            .collect::<Vec<_>>();
        let attention_max_age_hours = file
            .and_then(|f| f.attention_max_age_hours())
            .filter(|h| *h > 0.0)
            .unwrap_or(24.0);

        let managed_repos = file.and_then(|f| f.managed_repos()).unwrap_or_default();

        let hosts = file.and_then(|f| f.hosts.clone()).unwrap_or_default();

        // `[nodes.<node_id>]` (#729): the declarative multi-node worker registry. Empty ⇒
        // single-node behavior (placement always lands `Local`).
        let nodes = crate::hosts::resolve_nodes(&file.and_then(|f| f.nodes()).unwrap_or_default());

        // `[repo.<name>]` (#454): per-repo overrides (currently the native verify command). Empty ⇒
        // every repo falls back to its bn `test`/`build` job (the bn repo's unchanged behavior).
        let repos = file.and_then(|f| f.repos()).unwrap_or_default();
        let pipeline = file.and_then(|f| f.pipeline()).unwrap_or_default();

        let coarse_sizes = file.map(|f| f.coarse_sizes()).unwrap_or_default();

        // `[web]` (#388/#879): config, then defaults.
        let web = WebConfig {
            port: file
                .and_then(|f| f.web_port())
                .filter(|p| *p > 0)
                .unwrap_or(7770),
            bind: file
                .and_then(|f| f.web_bind())
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            fleet_poll_secs: file
                .and_then(|f| f.web_fleet_poll_secs())
                .filter(|s| *s > 0)
                .unwrap_or(2),
            tmux_mcp_autostart: file
                .and_then(|f| f.web_tmux_mcp_autostart())
                .unwrap_or(true),
            tmux_mcp_badge: file.and_then(|f| f.web_tmux_mcp_badge()).unwrap_or(true),
        };

        // `[log]`: the opt-in verbose/debug action log, off by default. `BN_VERBOSE` / `BN_LOG`
        // switch it on for one invocation — a cron or a job cannot pass `--verbose` — and `BN_LOG`
        // may be a level name (`debug`/`info`/…) that both enables and sets the threshold.
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
                .or_else(|| file.and_then(|f| f.log_level()))
                .and_then(|s| crate::log::LogLevel::parse(&s).map(|l| l.as_str().to_string()))
                .unwrap_or_else(|| "debug".to_string());
            LogConfig {
                enabled,
                level,
                dir: file.and_then(|f| f.log_dir()).map(PathBuf::from),
                retain_days: file.and_then(|f| f.log_retain_days()).unwrap_or(14),
            }
        };

        // `[scout]` (#262): config, then empty lists / the ≤ 5 cap.
        let scout = crate::scout::ScoutConfig {
            watch: file.and_then(|f| f.scout_watch()).unwrap_or_default(),
            tools: file.and_then(|f| f.scout_tools()).unwrap_or_default(),
            max_findings: file
                .and_then(|f| f.scout_max_findings())
                .filter(|n| *n > 0)
                .unwrap_or(crate::scout::DEFAULT_MAX_FINDINGS),
        };

        // `[dispatch].plan_review` (#349): off unless configured.
        let dispatch_plan_review = file.and_then(|f| f.dispatch_plan_review()).unwrap_or(false);
        let dispatch_plan_reviewers = file
            .and_then(|f| f.dispatch_plan_reviewers())
            .filter(|n| *n > 0)
            .unwrap_or(crate::plan_review::DEFAULT_REVIEWER_COUNT);
        let dispatch_plan_review_quorum = file
            .and_then(|f| f.dispatch_plan_review_quorum())
            .unwrap_or_else(|| crate::plan_review::majority_quorum(dispatch_plan_reviewers))
            .clamp(1, dispatch_plan_reviewers);

        // `[review].diff_gate` (#607): off unless configured. The required review count clamps to
        // at least 1 (a gate requiring zero reviews would be a no-op).
        let review_diff_gate = file.and_then(|f| f.review_diff_gate()).unwrap_or(false);
        let review_diff_gate_reviews = file
            .and_then(|f| f.review_diff_gate_reviews())
            .filter(|n| *n > 0)
            .unwrap_or(crate::review_gate::DEFAULT_REQUIRED_REVIEWS);

        // `[staff].home` (#270): trimmed, empty → None so a blank value never gates the staff off.
        let staff_home = file
            .and_then(|f| f.staff_home())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // `[notify]`: the desktop-toast policy gate. Infallible by design (a bad value clamps or
        // falls back), so a notification preference can never break an unrelated command.
        let notify = crate::notify::policy::NotifyConfig::resolve(file.and_then(|f| f.notify()));

        // `[harness].default`: trimmed, empty → None so a blank value falls back to the `copilot`
        // default. Normalized + availability-gated at use by `crate::harness::preferred_harness`.
        let default_harness = file
            .and_then(|f| f.default_harness())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let orchestrate_stuck_idle_secs = file
            .and_then(|f| f.orchestrate_stuck_idle_secs())
            .unwrap_or(crate::agent::DEFAULT_STUCK_LANE_IDLE_SECS)
            .max(1);
        let orchestrate_keep_warm = file
            .and_then(|f| f.orchestrate_keep_warm())
            .unwrap_or(false);
        let orchestrate_low_water = file
            .and_then(|f| f.orchestrate_low_water())
            .unwrap_or(crate::keep_warm::DEFAULT_KEEP_WARM_LOW_WATER)
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
            .unwrap_or(crate::keep_warm::DEFAULT_KEEP_WARM_BRAINSTORM_COUNT);
        let orchestrate_brainstorm_interval_secs = file
            .and_then(|f| f.orchestrate_brainstorm_interval_secs())
            .unwrap_or(crate::keep_warm::DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS)
            .max(1);

        Config {
            personal_root,
            work_root,
            overlay_root,
            worktree_dir,
            work_repos,
            hostname,
            machine_profile,
            job_only_on,
            weekly_anchor,
            screenshot_dir,
            gif_dir,
            work_only,
            sla_first_review_hours,
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
            scout,
            log,
            dispatch_plan_review,
            dispatch_plan_reviewers,
            dispatch_plan_review_quorum,
            review_diff_gate,
            review_diff_gate_reviews,
            default_harness,
            orchestrate_stuck_idle_secs,
            orchestrate_keep_warm,
            orchestrate_low_water,
            orchestrate_owned_repos,
            orchestrate_exclude_titles,
            orchestrate_exclude_labels,
            orchestrate_brainstorm_count,
            orchestrate_brainstorm_interval_secs,
            // `[orchestrate]` resource/budget caps (#377): all off unless configured, so an
            // unconfigured run is unchanged.
            orchestrate_min_free_ram_mb: file.and_then(|f| f.orchestrate_min_free_ram_mb()),
            orchestrate_max_loadavg: file.and_then(|f| f.orchestrate_max_loadavg()),
            orchestrate_max_swap_pct: file.and_then(|f| f.orchestrate_max_swap_pct()),
            orchestrate_tokens_per_hour: file.and_then(|f| f.orchestrate_tokens_per_hour()),
            orchestrate_daily_cost_usd: file.and_then(|f| f.orchestrate_daily_cost_usd()),
            orchestrate_daily_cost_usd_coding: file
                .and_then(|f| f.orchestrate_daily_cost_usd_coding()),
            orchestrate_daily_cost_usd_research: file
                .and_then(|f| f.orchestrate_daily_cost_usd_research()),
            orchestrate_remote_overflow: file
                .and_then(|f| f.orchestrate_remote_overflow())
                .unwrap_or(false),
            staff_home,
            notify,
        }
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

    /// Resolve the agent model and reasoning effort for `stage`, layering built-in defaults →
    /// `[pipeline.stages.<stage>]` → `[repo.<name>.pipeline.stages.<stage>]`. Blank override values
    /// are ignored. Repo matching follows [`repo_verify`](Self::repo_verify): full slug first,
    /// then trailing repo name, case-insensitively.
    pub fn pipeline_model(&self, repo: &str, stage: Stage) -> PipelineModel {
        // Config-only view of the full per-stage profile: one layering implementation, so this and
        // `bn pipeline preview` can never disagree about which model a stage runs.
        let profile = crate::pipeline::profile::layer(
            self,
            repo,
            stage,
            &crate::pipeline::profile::Overrides::default(),
        );
        PipelineModel {
            model: profile.model,
            effort: profile.effort,
            harness: profile.harness,
        }
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
