//! The `[section]` shapes of `config.toml`.
//!
//! Split out of [`super::config_file`] so the file that *reads* config can keep growing an
//! accessor per key without the file that *declares* the shapes growing with it — they change for
//! different reasons and at different rates.
#![allow(dead_code)]

use std::collections::HashMap;

/// `[machine]`: what a machine should do, for a machine that should do less than everything.
///
/// Usually set in that machine's own `<notes_root>/<machine>/config.toml`; set in the shared file
/// it applies to every machine. With no `[machine]` table a machine works on every repository it
/// has cloned and runs the cron and web services. The table only narrows that: `repos` limits the
/// repositories, and a runtime flag set to `false` switches that service off. An omitted field
/// keeps its default, so `web = false` alone still runs cron.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MachineProfile {
    #[serde(default = "every_repo")]
    pub repos: Vec<String>,
    #[serde(default = "on")]
    pub cron: bool,
    #[serde(default = "on")]
    pub web: bool,
}

impl Default for MachineProfile {
    fn default() -> Self {
        MachineProfile {
            repos: every_repo(),
            cron: true,
            web: true,
        }
    }
}

fn every_repo() -> Vec<String> {
    vec!["*".to_string()]
}

fn on() -> bool {
    true
}

/// `[mcp]`: MCP wiring options (#323). `extras` is the opt-in list of curated
/// planning/productivity MCP servers (`crate::mcp::EXTRAS` names) that `bn mcp init` — and the
/// dispatch setup path — wire alongside bn's own server into a *repo*. Empty/absent = bn only.
///
/// `servers` is the separate, *user*-scoped list: personal MCP servers `bn mcp sync-user` writes
/// into every MCP client on this machine. Declared here rather than in the curated catalog because
/// they are yours, not vetted-for-everyone, and never land in a committed file — see
/// [`crate::mcp_user`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct McpSection {
    pub(super) extras: Option<Vec<String>>,
    pub(super) servers: Option<HashMap<String, crate::mcp_user::UserServer>>,
}

/// `[managed_repos]`: repos bn keeps cloned + refreshed under the bn home (`~/.bn/repos`). Each
/// entry is a clone URL; the refresh-all job clones it bare+worktree (via `bn wt`, WT_ROOT pinned
/// to the managed root) if absent, then fetches/fast-forwards it. This is how dotfiles and other
/// always-checked-out repos travel onto a machine without manual `git clone`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ManagedReposSection {
    pub(super) repos: Option<Vec<String>>,
}

/// `[tools]`: managed docker-backed services. `builtins` is the opt-in allowlist of bn-shipped
/// specs (currently `n8n`). Empty/absent means no built-ins are active; user entries in
/// `<config_dir>/tools.toml` remain active.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ToolsSection {
    pub(super) builtins: Option<Vec<String>>,
}

/// `[links]`: the tag vocabulary for `bn links`. A small, curated set of terms you actually file
/// links under. Absent/empty means no vocabulary, which makes every tag "unlisted" — allowed, just
/// unremarked. An unlisted tag warns and still saves; this is a habit, not a schema.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct LinksSection {
    pub(super) tags: Option<Vec<String>>,
}

/// `[tasks]`: where the task list lives. `provider` is the raw word, parsed by
/// [`crate::task_store::TaskProviderKind::parse`]: `"file"` (the default, `tasks.txt`) or
/// `"forge"`. `forge_url` is the Forge MCP endpoint (default
/// [`crate::forge_app::DEFAULT_FORGE_URL`]).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct TasksSection {
    pub(super) provider: Option<String>,
    pub(super) forge_url: Option<String>,
}

/// `[web]`: the `bn-web` dashboard server (#386/#388). All optional — absent falls back to the
/// loopback / 7770 / 2s defaults resolved in [`super::WebConfig`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WebSection {
    /// TCP port `bn serve` binds. Default 7770.
    pub(super) port: Option<u16>,
    /// Bind address. Default `127.0.0.1` (loopback); `bn serve --lan` overrides to `0.0.0.0`.
    pub(super) bind: Option<String>,
    /// Fleet-snapshot poll cadence (seconds) for the SSE stream. Default 2.
    pub(super) fleet_poll_secs: Option<u64>,
    /// Start the managed bn-web service from tmux lifecycle hooks. Default true.
    pub(super) tmux_mcp_autostart: Option<bool>,
    /// Show the bnweb:<port> health segment in the tmux status bar. Default true.
    pub(super) tmux_mcp_badge: Option<bool>,
}

/// `[log]`: the opt-in verbose/debug **action log** (structured JSONL of every high-value action
/// bn performs, for after-the-fact debugging). All optional — absent/false ⇒ off. Env
/// `BN_VERBOSE`/`BN_LOG` and the global `--verbose/--debug` flag override this at higher
/// precedence. `dir` overrides the default `<state_dir>/logs` location; `retain_days` bounds how
/// many daily files are kept. See [`super::LogConfig`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct LogSection {
    /// Turn the action log on. Default false.
    pub(super) enabled: Option<bool>,
    /// Minimum level written (`debug`|`info`|`warn`|`error`). Default `debug` when enabled.
    pub(super) level: Option<String>,
    /// Override the log directory. Default `<state_dir>/logs`.
    pub(super) dir: Option<String>,
    /// Daily files to retain before pruning. Default 14; `0` keeps all.
    pub(super) retain_days: Option<i64>,
}

/// `[dispatch]`: defaults for `bn dispatch`. `plan_review` (#349) makes the plan-review gate the
/// default — a dispatched ship agent proposes a plan, registers a plan-review, and pauses until
/// consensus approves before implementing / opening a PR. Off by default; `--plan` opts in
/// per-dispatch. `--yolo` uses agent consensus rather than bypassing sensitive-plan review.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct DispatchSection {
    pub(super) plan_review: Option<bool>,
    pub(super) plan_reviewers: Option<usize>,
    pub(super) plan_review_quorum: Option<usize>,
}

/// `[orchestrate]`: the keep-warm supply of resident supervisors, and the resource/budget caps the
/// scheduler pauses local dispatch on (#377).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct OrchestrateSection {
    /// Seconds without pane or transcript activity before `working` is treated as stuck.
    pub(super) stuck_idle_secs: Option<i64>,
    /// Keep resident supervisors supplied from open GitHub issues.
    pub(super) keep_warm: Option<bool>,
    pub(super) low_water: Option<usize>,
    pub(super) own: Option<Vec<String>>,
    pub(super) exclude_titles: Option<Vec<String>>,
    pub(super) exclude_labels: Option<Vec<String>>,
    pub(super) brainstorm_count: Option<usize>,
    pub(super) brainstorm_interval_secs: Option<i64>,
    /// Pause local dispatch while free RAM (MB) is below this — the #377 anti-thrash guard.
    pub(super) min_free_ram_mb: Option<u64>,
    /// Pause local dispatch while the load average is above this (#377).
    pub(super) max_loadavg: Option<f64>,
    /// Pause local dispatch (and hold the backlog pull) while swap usage percent is above this
    /// (#668) — swap saturation, not agent count, is what chokes a RAM-bound box.
    pub(super) max_swap_pct: Option<f64>,
    /// Pause local dispatch once this-hour token consumption reaches this rate (#377).
    pub(super) tokens_per_hour: Option<u64>,
    /// Pause local dispatch once today's API-equiv `$` spend reaches this (#377).
    pub(super) daily_cost_usd: Option<f64>,
    /// Per-work-class daily API-equiv `$` spend caps (#381 GAP 5) — pause just the coding /
    /// research lane once its own today's spend reaches the cap, leaving the other lane running.
    pub(super) daily_cost_usd_coding: Option<f64>,
    pub(super) daily_cost_usd_research: Option<f64>,
    /// Opt in to overflowing `auto` items to a GitHub-hosted runner when local is full/paused
    /// (#377). Off by default — an explicit `substrate: remote` item always fans out regardless.
    pub(super) remote_overflow: Option<bool>,
}

/// `[wrap]`: the end-of-day digest job (#98). Disabled by default.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WrapSection {
    pub(super) enabled: Option<bool>,
}

/// `[standup]`: the standup-autopilot job (#99). Disabled by default; `fetch_delta` gates the
/// optional one-shot PR delta fetch.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct StandupSection {
    pub(super) enabled: Option<bool>,
    pub(super) fetch_delta: Option<bool>,
}

/// `[dxi]`: the personal DXI micro-prompt (#110). Disabled by default; opt-in surfaces a
/// <10s daily self-rating beside the daily files.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct DxiSection {
    pub(super) enabled: Option<bool>,
}

/// `[staff]`: the agent-staff framework's multi-machine settings (#270). `home` names the single
/// machine the staff runs on, so two laptops never duplicate-review the same PRs into the synced
/// feed. Unset = the staff runs anywhere (a single-machine setup needs no homing). A per-agent
/// `only_on` in `config/agents/*.md` overrides this for that one agent (e.g. a work-box-only
/// agent).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct StaffSection {
    pub(super) home: Option<String>,
}

/// `[scout]`: the Scout radar agent (#262). All optional. `watch` is the public `owner/repo`
/// upstream watchlist the Scout scans for help-wanted / good-first issues; `tools` the public tool
/// repos in your stack whose changelogs it tracks; `max_findings` overrides the ≤ 5 cap. All three
/// are the *only* outbound tokens, so they're public by definition.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ScoutSection {
    pub(super) watch: Option<Vec<String>>,
    pub(super) tools: Option<Vec<String>>,
    pub(super) max_findings: Option<usize>,
}

/// `[agents]`: which opt-in agents are switched on, by name.
///
/// An agent whose manifest sets `opt_in: true` does something consequential — spends model tokens,
/// writes into your notes, opens a session — so it stays off until it is named here. One list
/// rather than a `[<agent>] enabled` table per agent: an agent is a Markdown manifest, and it
/// should be possible to add one without also teaching `bn` a new config section.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct AgentsSection {
    pub(super) enabled: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct JobsSection {
    /// `[jobs.only_on]`: job name → machines it may run on. Unlisted jobs run everywhere.
    pub(super) only_on: Option<HashMap<String, Vec<String>>>,
}

/// `[work]`: which repositories are work, so their notes go to the work notes tree.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WorkSection {
    pub(super) work_repos: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ReviewSection {
    /// Weekday the `bn weekly` 1-1 window anchors to (name or `0..6`).
    pub(super) weekly_anchor: Option<String>,
    /// Review SLA: hours-to-first-review target (default 4).
    pub(super) sla_first_review_hours: Option<f64>,
    /// Harness the auto-review agent launches: `claude` or `copilot`.
    pub(super) harness: Option<String>,
    /// `[review].diff_gate` (#607): enable the adversarial diff-only review gate on the ship-lane
    /// land path. Off by default.
    pub(super) diff_gate: Option<bool>,
    /// `[review].diff_gate_reviews` (#607): independent approving diff-only reviews the gate
    /// requires. Default 2.
    pub(super) diff_gate_reviews: Option<usize>,
}

/// `[harness]` (#501): per-machine harness availability + preference. `disabled` lists harnesses
/// turned off on this box — currently only `claude`, when the account is org-disabled ("organization
/// has disabled Claude subscription access"). Copilot is the guaranteed fallback and can't be
/// disabled. `default` names the preferred harness every launcher uses when nothing is requested
/// (`copilot` | `claude`); unset/unavailable falls back to copilot. Backs the dashboard's harness
/// switcher, which renders a disabled harness greyed-out with a reason.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct HarnessSection {
    pub(super) disabled: Option<Vec<String>>,
    pub(super) default: Option<String>,
}

/// `[attention]`: the `bn next` attention list. `max_age_hours` is the recency cutoff —
/// time-aged actions older than it drop off (widened over the weekend on Mondays). Default 24.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct AttentionSection {
    pub(super) max_age_hours: Option<f64>,
}

/// `[calib]`: coarse t-shirt-size estimate durations for `--est S|M|L` (see
/// [`crate::calib::CoarseSizes`]). Each is a human duration in the `bn focus start` grammar
/// (`30m`, `1h30m`, `3h`); a missing/garbage value falls back to the built-in default.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct CalibSection {
    pub(super) s: Option<String>,
    pub(super) m: Option<String>,
    pub(super) l: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct CaptureSection {
    /// Folder new screenshots land in (WSL: a `/mnt/c/...` path). `bn shot` grabs from here.
    pub(super) screenshot_dir: Option<String>,
    /// Folder the GIF recorder saves into. `bn shot --gif` grabs from here.
    pub(super) gif_dir: Option<String>,
}
