//! Declarative cron manifests (`crons.toml`).
//!
//! Job *scripts* already resolve PATH-style across branch → repo → global tiers
//! ([`crate::resolver::job_script`]); this is the matching data-driven layer for *schedules*. A
//! `crons.toml` in any job tier declares `[[cron]]` entries; [`discover`] folds them (first tier
//! wins, like scripts) and `bn cron register` turns them into systemd timers.

use crate::events::{cron_status, CronJobStatus};
use crate::resolver::{config_dir, job_dirs, list_jobs, Config, JobRef, JobScope, Resolved};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What a cron *is for* — a descriptive label, not a behaviour switch. `Info` crons gather
/// information a person reads; `Housekeeping` crons (the default) do background estate work. Every
/// cron runs headlessly either way; the class only tags the `bn cron` dashboard row so you can see
/// at a glance which crons surface information. This is the explicit knob the user classifies each
/// cron with; there is no inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CronClass {
    /// Gathers information a person reads.
    Info,
    /// Background estate work (the default).
    #[default]
    Housekeeping,
}

impl CronClass {
    /// The lowercase label used in TOML, the dashboard, and `CronView`.
    pub fn label(self) -> &'static str {
        match self {
            CronClass::Info => "info",
            CronClass::Housekeeping => "housekeeping",
        }
    }
}

/// One declared cron entry — a `[[cron]]` table in a `crons.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronEntry {
    /// Job script name — resolves to `<job>.sh` via [`crate::resolver::job_script`].
    pub job: String,
    /// What the cron is for — `info` (gathers something a person reads) or `housekeeping` (silent
    /// estate work, the default). A dashboard label only. See [`CronClass`].
    #[serde(default)]
    pub class: CronClass,
    /// Canonical schedule = systemd `OnCalendar` spec (e.g. `*:0/15`). Mapped to cron / launchd
    /// at generation time.
    pub on_calendar: String,
    /// Machines this cron should register on; empty = everywhere. (The per-run host gate is the
    /// existing `[jobs.only_on]`; this drives generation only.)
    #[serde(default)]
    pub only_on: Vec<String>,
    /// `Description=` for the generated unit.
    pub description: Option<String>,
    /// Catch up a run missed while the host was down (systemd `Persistent=`). Default true.
    #[serde(default = "default_true")]
    pub persistent: bool,
    /// `RandomizedDelaySec` (e.g. `2m`) to de-sync fleets hitting the same API. Optional.
    pub randomized_delay: Option<String>,
    /// The job hits the network (gh/az/git remote) → the generated unit waits for
    /// `network-online.target`. Default false (offline jobs need no ordering).
    #[serde(default)]
    pub network: bool,
    /// Also fire this job on the **first bn event of the day** (via `bn run <job> --if-stale`), not
    /// only on its `on_calendar` timer. For refreshes whose value is tied to *when you start work*
    /// rather than a fixed clock time — meetings, the standup view — so a variable start time still
    /// gets a fresh result without you matching a wall-clock slot. Default false.
    #[serde(default)]
    pub catch_up: bool,
}

fn default_true() -> bool {
    true
}

/// A [`CronEntry`] tagged with the tier it came from — drives unit naming and precedence.
#[derive(Debug, Clone)]
pub struct ResolvedCron {
    pub entry: CronEntry,
    pub scope: JobScope,
    /// The repo a non-global cron belongs to, so its generated unit is named `bn-<repo>-<job>`
    /// and never collides with another repo's same-named cron. `None` for global crons.
    pub repo: Option<String>,
    pub source: PathBuf,
}

#[derive(Debug, Default, serde::Deserialize)]
struct CronManifest {
    #[serde(default, rename = "cron")]
    crons: Vec<CronEntry>,
}

/// Parse one `crons.toml`. `Err(message)` on malformed TOML (the message embeds the path so the
/// caller can report it), mirroring the degrade-loudly contract of config parsing.
pub fn parse_manifest(path: &Path) -> Result<Vec<CronEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest: CronManifest =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(manifest.crons)
}

/// All declared crons for `ctx`, folded across the job tiers (branch → repo → global) with
/// first-tier-wins shadowing by `job` — the same rule as [`crate::resolver::job_script`], so a
/// repo's `jobs/crons.toml` can override a global entry's schedule. Missing files are skipped;
/// a malformed one is skipped with a stderr warning rather than aborting discovery.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Vec<ResolvedCron> {
    let mut out: Vec<ResolvedCron> = Vec::new();
    // The shipped/public manifest now lives at `config/crons.toml` (→ `config_dir()/crons.toml`
    // at runtime, symlinked by install.sh). Append it as the lowest-precedence Global tier so a
    // private note tier (`<note>/jobs/crons.toml`) still shadows it, first-tier-wins as before.
    let mut tiers = job_dirs(cfg, ctx);
    let shipped = (JobScope::Global, config_dir());
    if !tiers.iter().any(|(_, d)| *d == shipped.1) {
        tiers.push(shipped);
    }
    for (scope, dir) in tiers {
        let path = dir.join("crons.toml");
        if !path.is_file() {
            continue;
        }
        match parse_manifest(&path) {
            Ok(entries) => {
                for entry in entries {
                    // Earlier (higher-precedence) tier already declared this job → shadow.
                    if out.iter().any(|rc| rc.entry.job == entry.job) {
                        continue;
                    }
                    let repo = match scope {
                        JobScope::Global => None,
                        _ => ctx.map(|c| c.repo.clone()),
                    };
                    out.push(ResolvedCron {
                        entry,
                        scope,
                        repo,
                        source: path.clone(),
                    });
                }
            }
            Err(e) => eprintln!("bn: skipping malformed cron manifest {e}"),
        }
    }
    out
}

/// The job names marked `catch_up = true` in the resolved manifest — the jobs the day's first bn
/// event should kick off (see [`CronEntry::catch_up`]). Deduped, in discovery order.
pub fn catch_up_jobs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<String> {
    discover(cfg, ctx)
        .into_iter()
        .filter(|rc| rc.entry.catch_up)
        .map(|rc| rc.entry.job)
        .collect()
}

/// The declared [`CronClass`] for `job`, folded across the job tiers ([`discover`]). Jobs no
/// `[[cron]]` entry declares default to [`CronClass::Housekeeping`] — an undeclared/ad-hoc `bn run`
/// carries no special meaning beyond the dashboard label.
pub fn job_class(cfg: &Config, ctx: Option<&Resolved>, job: &str) -> CronClass {
    discover(cfg, ctx)
        .into_iter()
        .find(|rc| rc.entry.job == job)
        .map(|rc| rc.entry.class)
        .unwrap_or_default()
}

/// The minimum recorded failures before a still-failing cron is worth alerting on — a couple of
/// transient failures stay quiet, a job that keeps failing does not.
pub const ALERT_MIN_FAILURES: usize = 3;

/// The chronic cron failures a watchdog should raise: jobs whose *latest* run failed AND which have
/// accumulated at least `min_failures` total failures (so a `tools-reconcile` that fails every tick
/// can't rot silently, but a one-off blip stays quiet). Pure over the folded `cron_status` rows so
/// it's unit-testable without the event log. Preserves input order (most-recent-run first).
pub fn alerting_crons(runs: &[CronJobStatus], min_failures: usize) -> Vec<&CronJobStatus> {
    runs.iter()
        .filter(|r| !r.ok && r.failures >= min_failures)
        .collect()
}

/// A single cron job's whole-picture status for a dashboard read: what it's declared to do
/// (`crons.toml` schedule + flags), whether a job *script* backs it, and its last-run health
/// (folded from the `cron_run` event log). One row per distinct job name across all three sources
/// — the machine-wide "registered crons + last run" view the web dashboard surfaces (#483).
#[derive(Debug, Clone, Default, Serialize)]
pub struct CronView {
    /// Job name — the key that ties a declared schedule, a script, and its runs together.
    pub job: String,
    /// Tier label (`branch`/`repo`/`global`), from the declared cron else the script else the run.
    pub scope: String,
    /// The cron's class (`info`/`housekeeping`) when declared; a descriptive dashboard label.
    /// `None` for script-only or run-only jobs that no `[[cron]]` entry declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Whether a `[[cron]]` entry declares this job (it's in the registry, not just run-observed).
    pub declared: bool,
    /// The declared `on_calendar` schedule spec, when declared.
    pub schedule: Option<String>,
    /// `Description=` from the declared entry, when set.
    pub description: Option<String>,
    /// The declared entry hits the network (drives scheduler ordering).
    pub network: bool,
    /// Also fired on the first bn event of the day (`catch_up`).
    pub catch_up: bool,
    /// Whether a `<job>.sh` script backs this job.
    pub has_script: bool,
    /// The backing script's path, when one exists.
    pub script_path: Option<String>,
    /// Epoch-seconds of the most recent recorded run, when the job has ever run.
    pub last_run: Option<i64>,
    /// The most recent run's exit code, when the job has ever run.
    pub last_exit: Option<i32>,
    /// Whether the most recent run exited 0, when the job has ever run.
    pub ok: Option<bool>,
    /// Total recorded runs.
    pub runs: usize,
    /// Recorded runs that exited non-zero.
    pub failures: usize,
    /// The last run's captured output tail (stderr+stdout, bounded to the 15-line tail persisted in
    /// the `cron_run` event), surfaced only when that run *failed* — so a dashboard can show *why*
    /// without a shell. `None` for jobs whose last run succeeded, that have never run, or that
    /// captured no output. Populated by [`status_views`] from the event log, not [`fold_cron_views`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
    /// Scheduled Markdown agents owned by this cron. Populated only on the `agent-scheduler` row
    /// by [`status_views_with_agents`], so agent schedules stay nested beneath their one OS timer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentScheduleView>,
}

/// One scheduled Markdown agent nested beneath the common `agent-scheduler` status row.
#[derive(Debug, Clone, Serialize)]
pub struct AgentScheduleView {
    /// The stable identity accepted by `bn run <job>`.
    pub job: String,
    /// The manifest's explicit execution kind.
    pub kind: crate::agents::AgentKind,
    /// Tier label (`branch`/`repo`/`global`) of the effective manifest.
    pub scope: String,
    /// The manifest-owned `on_calendar` schedule.
    pub schedule: String,
    /// One-line manifest description.
    pub description: Option<String>,
    /// Whether the agent may use the network.
    pub network: bool,
    /// Whether queue capture is enabled for this agent.
    pub queue: bool,
    /// Whether the agent also declares `trigger: startup`.
    pub startup: bool,
    /// Failure retry delay declared by the manifest (scheduled default: 15 minutes).
    pub retry_after_secs: i64,
    /// Whether the scheduler would launch this agent at the supplied status-view time.
    pub due: bool,
    /// Next retry instant after a failed attempt, when still in backoff.
    pub retry_at: Option<i64>,
    /// Compact scheduler state: `due`, `retry`, or `idle`.
    pub state: String,
    /// Most recent recorded attempt.
    pub last_run: Option<i64>,
    pub last_exit: Option<i32>,
    pub ok: Option<bool>,
    pub runs: usize,
    pub failures: usize,
    /// Captured output tail from the latest failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
}

/// Fold the three cron data sources into one row per job — the pure core of [`status_views`], so
/// the union/join/precedence rules are unit-testable without touching the filesystem or event log.
/// Rows are ordered declared-first (registry order), then script-only, then run-only orphans;
/// within that, declared order is preserved. `scope` prefers the declared tier, then the script's,
/// then the last run's.
pub fn fold_cron_views(
    declared: &[ResolvedCron],
    runs: &[CronJobStatus],
    scripts: &[JobRef],
) -> Vec<CronView> {
    let mut names: Vec<String> = Vec::new();
    let push = |n: &str, names: &mut Vec<String>| {
        if !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for rc in declared {
        push(&rc.entry.job, &mut names);
    }
    for s in scripts {
        push(&s.name, &mut names);
    }
    // cron_status is most-recent-first; the first match is the latest run for a job.
    for r in runs {
        push(&r.job, &mut names);
    }

    names
        .into_iter()
        .map(|name| {
            let decl = declared.iter().find(|rc| rc.entry.job == name);
            let script = scripts.iter().find(|s| s.name == name);
            let run = runs.iter().find(|r| r.job == name);
            let scope = decl
                .map(|d| d.scope.label().to_string())
                .or_else(|| script.map(|s| s.scope.label().to_string()))
                .or_else(|| run.map(|r| r.scope.clone()))
                .unwrap_or_else(|| "?".to_string());
            CronView {
                job: name,
                scope,
                class: decl.map(|d| d.entry.class.label().to_string()),
                declared: decl.is_some(),
                schedule: decl.map(|d| d.entry.on_calendar.clone()),
                description: decl.and_then(|d| d.entry.description.clone()),
                network: decl.map(|d| d.entry.network).unwrap_or(false),
                catch_up: decl.map(|d| d.entry.catch_up).unwrap_or(false),
                has_script: script.is_some(),
                script_path: script.map(|s| s.path.clone()),
                last_run: run.map(|r| r.last_run),
                last_exit: run.map(|r| r.last_exit),
                ok: run.map(|r| r.ok),
                runs: run.map(|r| r.runs).unwrap_or(0),
                failures: run.map(|r| r.failures).unwrap_or(0),
                last_output: None,
                agents: Vec::new(),
            }
        })
        .collect()
}

/// Build the nested schedule rows for every scheduled, runnable Markdown agent.
pub fn agent_schedule_views(
    catalog: &crate::agents::AgentCatalog,
    runs: &[CronJobStatus],
    now: i64,
) -> Vec<AgentScheduleView> {
    catalog
        .iter()
        .filter(|agent| agent.is_cron() && !agent.is_specialist())
        .map(|agent| {
            let last = crate::events::last_run(&agent.name);
            let status = runs.iter().find(|run| run.job == agent.name);
            let due = agent.due(last.as_ref(), now);
            let retry_at = agent
                .retry_at(last.as_ref())
                .filter(|retry_at| *retry_at > now);
            let state = if due {
                "due"
            } else if retry_at.is_some() {
                "retry"
            } else {
                "idle"
            };
            AgentScheduleView {
                job: agent.name.clone(),
                kind: agent.kind,
                scope: agent.scope.label().to_string(),
                schedule: agent.on_calendar.clone().unwrap_or_default(),
                description: agent.description.clone(),
                network: agent.network,
                queue: agent.queue,
                startup: agent.is_startup(),
                retry_after_secs: agent
                    .retry_after_secs
                    .unwrap_or(crate::agents::DEFAULT_RETRY_AFTER_SECS),
                due,
                retry_at,
                state: state.to_string(),
                last_run: status.map(|run| run.last_run),
                last_exit: status.map(|run| run.last_exit),
                ok: status.map(|run| run.ok),
                runs: status.map(|run| run.runs).unwrap_or(0),
                failures: status.map(|run| run.failures).unwrap_or(0),
                last_output: last
                    .filter(|run| run.exit_code != 0)
                    .map(|run| run.output_tail.trim().to_string())
                    .filter(|tail| !tail.is_empty()),
            }
        })
        .collect()
}

/// Gather the declared crons ([`discover`]), their run history ([`cron_status`]), and the backing
/// job scripts ([`list_jobs`]) for `ctx`, then [`fold_cron_views`] them into the dashboard rows.
/// Pass `ctx = None` for the machine-wide (global-tier) registry the web dashboard shows.
pub fn status_views(cfg: &Config, ctx: Option<&Resolved>) -> Vec<CronView> {
    let mut views = fold_cron_views(&discover(cfg, ctx), &cron_status(), &list_jobs(cfg, ctx));
    // Enrich a failing job with its last run's captured output tail (the exit code + time are
    // already folded above) so the dashboard can show *why* it broke without a shell. Kept off the
    // pure fold — it's an extra event-log read per failing job — and bounded to the persisted tail.
    for v in views.iter_mut() {
        if v.ok == Some(false) {
            if let Some(tail) = crate::events::last_run(&v.job)
                .map(|d| d.output_tail.trim().to_string())
                .filter(|t| !t.is_empty())
            {
                v.last_output = Some(tail);
            }
        }
    }
    views
}

/// The machine cron registry with every manifest-owned agent schedule nested beneath the single
/// `agent-scheduler` row. Strict agent discovery is part of this view: a malformed manifest fails
/// the caller instead of silently dropping its schedule.
pub fn status_views_with_agents(
    cfg: &Config,
    ctx: Option<&Resolved>,
) -> Result<Vec<CronView>, crate::agents::AgentCatalogError> {
    let catalog = crate::agents::discover(cfg, ctx)?;
    let runs = cron_status();
    let children = agent_schedule_views(&catalog, &runs, crate::util::now_epoch_secs());
    Ok(nest_agent_schedules(status_views(cfg, ctx), children))
}

fn nest_agent_schedules(
    mut views: Vec<CronView>,
    children: Vec<AgentScheduleView>,
) -> Vec<CronView> {
    let child_names: std::collections::HashSet<&str> =
        children.iter().map(|agent| agent.job.as_str()).collect();
    views.retain(|view| !child_names.contains(view.job.as_str()));
    if let Some(scheduler) = views.iter_mut().find(|view| view.job == "agent-scheduler") {
        scheduler.agents = children;
    }
    views
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cron(job: &str) -> CronView {
        CronView {
            job: job.to_string(),
            scope: "global".to_string(),
            class: None,
            declared: true,
            schedule: None,
            description: None,
            network: false,
            catch_up: false,
            has_script: false,
            script_path: None,
            last_run: None,
            last_exit: None,
            ok: None,
            runs: 0,
            failures: 0,
            last_output: None,
            agents: Vec::new(),
        }
    }

    fn agent(job: &str) -> AgentScheduleView {
        AgentScheduleView {
            job: job.to_string(),
            kind: crate::agents::AgentKind::Command,
            scope: "global".to_string(),
            schedule: "daily".to_string(),
            description: None,
            network: false,
            queue: false,
            startup: false,
            retry_after_secs: crate::agents::DEFAULT_RETRY_AFTER_SECS,
            due: false,
            retry_at: None,
            state: "idle".to_string(),
            last_run: None,
            last_exit: None,
            ok: None,
            runs: 0,
            failures: 0,
            last_output: None,
        }
    }

    #[test]
    fn scheduled_agents_are_nested_without_flat_duplicates() {
        let views = vec![
            cron("agent-scheduler"),
            cron("alpha"),
            cron("plain-cron"),
            cron("beta"),
        ];
        let nested = nest_agent_schedules(views, vec![agent("alpha"), agent("beta")]);

        assert_eq!(
            nested
                .iter()
                .map(|view| view.job.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-scheduler", "plain-cron"]
        );
        assert_eq!(
            nested[0]
                .agents
                .iter()
                .map(|agent| agent.job.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }
}
