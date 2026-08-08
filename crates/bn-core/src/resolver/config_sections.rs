//! The `[section]` shapes of `config.toml`.
//!
//! Split out of [`super::config_file`] so the file that *reads* config can keep growing an
//! accessor per key without the file that *declares* the shapes growing with it — they change for
//! different reasons and at different rates.
#![allow(dead_code)]

use std::collections::HashMap;

/// `[mcp]`: per-repo MCP wiring options (#323). `extras` is the opt-in list of curated
/// planning/productivity MCP servers (`crate::mcp::EXTRAS` names) that `bn mcp init` — and the
/// dispatch setup path — wire alongside bn's own server. Empty/absent = bn only.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct McpSection {
    pub(super) extras: Option<Vec<String>>,
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

/// `[orchestrate]`: defaults for `bn orchestrate` (#382). `agent` is the harness the daemon
/// dispatches new agents under when `--agent` isn't passed (`claude` | `copilot`, plus the
/// `bn dispatch` aliases); absent falls back to `copilot`. The usage-limit auto-fallback (switch a
/// stalled lane to the other harness) is always on regardless of this default.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct OrchestrateSection {
    pub(super) agent: Option<String>,
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

/// `[wrap]`: the end-of-day digest job (#98). Disabled by default; `time` is the local
/// `HH:MM` the systemd timer fires at (informative — the timer carries the real schedule).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WrapSection {
    pub(super) enabled: Option<bool>,
    pub(super) time: Option<String>,
}

/// `[standup]`: the standup-autopilot job (#99). Disabled by default; `fetch_delta` gates the
/// optional one-shot PR delta fetch.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct StandupSection {
    pub(super) enabled: Option<bool>,
    pub(super) time: Option<String>,
    pub(super) fetch_delta: Option<bool>,
}

/// `[today]`: Today's agent (#577), the `today-agent` cron. Disabled by default — a scheduled run
/// writes suggestions the owner reviews — so it stays opt-in. A manual `bn today` runs regardless.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct TodaySection {
    pub(super) enabled: Option<bool>,
}

/// `[attention_agent]`: the periodic LLM cron that reviews recent cron findings + cron health and
/// writes a ranked attention list. Disabled by default (it spends a model call on a schedule) so
/// it stays opt-in, like the other agentic crons.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct AttentionAgentSection {
    pub(super) enabled: Option<bool>,
}

/// `[dxi]`: the personal DXI micro-prompt (#110). Disabled by default; opt-in surfaces a
/// <10s daily self-rating beside the daily files.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct DxiSection {
    pub(super) enabled: Option<bool>,
}

/// `[night]`: the nightly fleet planner (the `night-shift` job / `bn night run`). Disabled by
/// default and opt-in per repo — nothing runs unless `enabled` is set AND repos are allowlisted.
/// `max_dispatch` hard-caps how many promoted todos a single shift dispatches; `window` is the
/// local `HH:MM-HH:MM` the shift may run in (a manual `bn night run` outside it declines unless
/// `--force`); `repos` is the allowlist of bare repo names the shift may touch.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct NightSection {
    pub(super) enabled: Option<bool>,
    pub(super) max_dispatch: Option<usize>,
    pub(super) window: Option<String>,
    pub(super) repos: Option<Vec<String>>,
}

/// `[sentinel]`: the Debt Sentinel — the 22:30 estate patrol (#259). `repos` is the allowlist of
/// bare repo names it scans and may promote from; it defaults to the night-shift allowlist
/// (`[night] repos`) so the two stay in lockstep unless overridden, and an empty allowlist makes
/// the patrol a no-op. `todo_age_days` is how old a TODO/FIXME marker must be before it's flagged.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct SentinelSection {
    pub(super) repos: Option<Vec<String>>,
    pub(super) todo_age_days: Option<i64>,
}

/// `[quarter]`: the recurring impact-agent review (#252). Disabled by default — it dispatches an
/// LLM harness, so it stays opt-in. `review_cron` is retained only to emit a migration error;
/// schedules now live in `agents/quarter-review.md`. `review_window_days` bounds the ground-truth
/// window when there's no prior review; `harness` picks the CLI (copilot/claude).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct QuarterSection {
    pub(super) enabled: Option<bool>,
    pub(super) review_cron: Option<String>,
    pub(super) review_window_days: Option<i64>,
    pub(super) harness: Option<String>,
}

/// `[dojo]`: the gamified learning engine (#253, `bn dojo`). `repos` is the rotation allowlist of
/// bare repo names the session and card-miner draw from (empty = every repo with cards).
/// `daily_target` caps how many cards one session presents. `enabled` gates the nightly
/// `dojo-cards` mining cron (off by default — mining is opt-in), but `bn dojo` itself always runs.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct DojoSection {
    pub(super) enabled: Option<bool>,
    pub(super) repos: Option<Vec<String>>,
    pub(super) daily_target: Option<usize>,
}

/// `[chief]`: the Chief of Staff briefing agent (#257) — the twice-daily `staff-chief-{am,pm}`
/// jobs. Disabled by default (it writes daily goals), so it stays opt-in. `am_time`/`pm_time` are
/// the local `HH:MM` the AM brief / PM shutdown fire (informative — the real schedule lives in
/// `config/crons.toml`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ChiefSection {
    pub(super) enabled: Option<bool>,
    pub(super) am_time: Option<String>,
    pub(super) pm_time: Option<String>,
}

/// `[archivist]`: the Archivist (#263) — the Friday private weekly chronicle (`staff-archivist`
/// job). Disabled by default (it writes a file into the private notes repo), so it stays opt-in.
/// `time` is the local `HH:MM` the chronicle fires (informative — the real schedule lives in
/// `config/crons.toml`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ArchivistSection {
    pub(super) enabled: Option<bool>,
    pub(super) time: Option<String>,
}

/// `[staff]`: the agent-staff framework's multi-machine settings (#270). `home` names the single
/// machine the staff runs on — `bn staff run` (and every `staff-<agent>` cron) no-ops loudly on
/// any other box, so two laptops never duplicate-review the same PRs into the synced feed. Unset =
/// the staff runs anywhere (a single-machine setup needs no homing). A per-agent `only_on` in
/// `config/agents/*.md` overrides this for that one agent (e.g. a work-box-only agent). Env: `$BN_STAFF_HOME`.
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

/// `[retro]`: the Interviewer (#261) — the weekly subjective-retro pull agent. Disabled by
/// default (it raises a weekly attention item), so it stays opt-in. Enabling registers the
/// `staff-interviewer` cron, which raises the Friday item and escalates it once on Monday; the
/// session itself is always run by hand (`bn retro chat`), never dispatched.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct RetroSection {
    pub(super) enabled: Option<bool>,
}

/// `[mentor]`: the Mentor (#258) — the Mon..Fri `staff-mentor` post-merge review-to-teach agent.
/// Disabled by default (it dispatches an LLM harness and appends craft notes to your branch
/// notes), so it stays opt-in. A manual `bn mentor` runs regardless.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct MentorSection {
    pub(super) enabled: Option<bool>,
}

/// `[eye]`: the opt-in keratoconus protective-behaviour coach. `last_review` seeds monitoring
/// state until `bn eye review` records a newer date; the interval is clamped to 6-12 months.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct EyeSection {
    pub(super) enabled: Option<bool>,
    pub(super) last_review: Option<String>,
    pub(super) review_interval_months: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct JobsSection {
    /// `[jobs.only_on]`: job name → machines it may run on. Unlisted jobs run everywhere.
    pub(super) only_on: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct PersonalSection {
    pub(super) notes_root: Option<String>,
    /// `[personal].overlay_root`: the personal overlay layered on top for `agents/`+`skills/`
    /// discovery (default `~/projects/personal-notes/bn`). Overridden by `$BN_OVERLAY_DIR`.
    pub(super) overlay_root: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WorkSection {
    pub(super) notes_root: Option<String>,
    pub(super) work_repos: Option<Vec<String>>,
    /// `[work].only`: this is a work machine — hide non-work repos from cross-repo views.
    pub(super) only: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ReviewSection {
    /// Weekday the `bn weekly` 1-1 window anchors to (name or `0..6`).
    pub(super) weekly_anchor: Option<String>,
    /// Review SLA: hours-to-first-review target (default 4).
    pub(super) sla_first_review_hours: Option<f64>,
    /// Review SLA: open→merge target hours (default 24).
    pub(super) sla_merge_hours: Option<f64>,
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

/// `[queue]`: the machine-wide work queue (#queue).
///
/// `capture` is the list of producers that **enqueue instead of launching**. It is a list rather
/// than a bool so each one can be turned on — and rolled back — independently: switching every
/// dispatch path at once is not a change anybody can safely evaluate.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct QueueSection {
    /// Producers that enqueue instead of launching: `review`, `attn`, `staff`, `cron`.
    pub(super) capture: Option<Vec<String>>,
    /// Concurrent coding-lane agents this machine will start.
    pub(super) coding: Option<usize>,
    /// Concurrent research-lane agents this machine will start.
    pub(super) research: Option<usize>,
    /// Seconds a node may go without a heartbeat before it stops being placeable.
    pub(super) heartbeat_ttl_secs: Option<i64>,
    /// `[queue.kind_caps]` — a nested table, so it belongs to this section rather than being a
    /// top-level key with a dot in its name.
    pub(super) kind_caps: Option<HashMap<String, usize>>,
    /// Inline express items one pass may run.
    pub(super) express_per_tick: Option<usize>,
    /// Seconds a worktree agent may hold a lease before it is presumed dead.
    pub(super) agent_lease_ttl_secs: Option<i64>,
    /// Seconds headless work may hold a lease. Express work resolves in-tick, so this is short.
    pub(super) express_lease_ttl_secs: Option<i64>,
    /// Idle minutes after which opportunistic work may run outside an explicit away window.
    pub(super) opportunistic_idle_mins: Option<i64>,
    /// `[[queue.windows]]` — times of day when some work is preferred.
    pub(super) windows: Option<Vec<WindowSection>>,
}

/// One `[[queue.windows]]` entry: a recurring span during which matching work ranks higher.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct WindowSection {
    pub(super) name: Option<String>,
    /// `OnCalendar` for when it opens — `Mon..Fri *-*-* 12:30:00`.
    pub(super) at: Option<String>,
    /// How long it stays open, in the `bn focus` duration grammar (`45m`, `2h`).
    pub(super) fore: Option<String>,
    pub(super) boost: Option<i64>,
    pub(super) matches: Option<Vec<String>>,
    pub(super) mode: Option<String>,
}
