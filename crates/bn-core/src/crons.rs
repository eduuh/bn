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
    /// Canonical schedule = systemd `OnCalendar` spec (e.g. `*:0/15`), or a list of them for a job
    /// that fires at unrelated times (a monorepo's pool builds at 02:00 and 07:30). Mapped to cron /
    /// launchd at generation time.
    pub on_calendar: Calendar,
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
    /// rather than a fixed clock time — a standup-style morning view — so a variable start time still
    /// gets a fresh result without you matching a wall-clock slot. Default false.
    #[serde(default)]
    pub catch_up: bool,
    /// Repository required by this cron. Entry-level values override the manifest's top-level
    /// `repo`; absent means the cron is genuinely machine-global. `repo = ""` on an entry opts it
    /// out of the manifest's repo: machine-global even in a repo-owned manifest (bn's own tunnel
    /// keepalive must run on build machines that do not list the `bn` repo).
    #[serde(default)]
    pub repo: Option<String>,
}

fn default_true() -> bool {
    true
}

/// One or more systemd `OnCalendar` specs. A timer takes any number of `OnCalendar=` lines and
/// fires on each; one string stays the common case, so a manifest may write either
/// `on_calendar = "*:0/15"` or `on_calendar = ["*-*-* 02:00", "*-*-* 07:30"]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar(Vec<String>);

impl Calendar {
    pub fn entries(&self) -> &[String] {
        &self.0
    }
}

impl From<&str> for Calendar {
    fn from(spec: &str) -> Self {
        Calendar(vec![spec.to_string()])
    }
}

impl std::fmt::Display for Calendar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join(", "))
    }
}

impl<'de> serde::Deserialize<'de> for Calendar {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        let specs = match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        };
        if specs.is_empty() || specs.iter().any(|s| s.trim().is_empty()) {
            return Err(serde::de::Error::custom(
                "on_calendar needs at least one non-empty spec",
            ));
        }
        Ok(Calendar(specs))
    }
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
    /// Default repository requirement inherited by every entry that does not declare its own.
    repo: Option<String>,
    #[serde(default, rename = "cron")]
    crons: Vec<CronEntry>,
}

/// Parse one `crons.toml`. `Err(message)` on malformed TOML (the message embeds the path so the
/// caller can report it), mirroring the degrade-loudly contract of config parsing.
pub fn parse_manifest(path: &Path) -> Result<Vec<CronEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut manifest: CronManifest =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for entry in &mut manifest.crons {
        match entry.repo.as_deref() {
            None => entry.repo.clone_from(&manifest.repo),
            Some("") => entry.repo = None,
            Some(_) => {}
        }
    }
    Ok(manifest.crons)
}

/// All declared crons for `ctx`, folded across the job tiers (branch → repo → global) with
/// first-tier-wins shadowing by `job` — the same rule as [`crate::resolver::job_script`], so a
/// repo's `jobs/crons.toml` can override a global entry's schedule. Missing files are skipped;
/// a malformed one is skipped with a stderr warning rather than aborting discovery.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Vec<ResolvedCron> {
    let mut out: Vec<ResolvedCron> = Vec::new();
    // The shipped manifest: `config/crons.toml` in the checkout bn ships from
    // (`resolver::shipped_root`), else `<config_dir>/crons.toml`. Appended as the lowest-precedence
    // Global tier so a private note tier (`<note>/jobs/crons.toml`) still shadows it,
    // first-tier-wins as before.
    let mut tiers = job_dirs(cfg, ctx);
    let shipped_dir = crate::resolver::shipped_config("crons.toml")
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(config_dir);
    let shipped = (JobScope::Global, shipped_dir);
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
                    let repo = entry.repo.clone().or_else(|| match scope {
                        JobScope::Global => None,
                        _ => ctx.map(|c| c.repo.clone()),
                    });
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

/// Whether the resolved cron manifest currently declares `job`.
pub fn declares_job(declared: &[ResolvedCron], job: &str) -> bool {
    declared.iter().any(|cron| cron.entry.job == job)
}

/// The job names marked `catch_up = true` in the resolved manifest — the jobs the day's first bn
/// event should kick off (see [`CronEntry::catch_up`]). Deduped, in discovery order.
pub fn catch_up_jobs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<String> {
    discover(cfg, ctx)
        .into_iter()
        .filter(|rc| {
            rc.entry.catch_up
                && rc
                    .repo
                    .as_deref()
                    .is_none_or(|repo| cfg.repo_available_here(repo))
        })
        .map(|rc| rc.entry.job)
        .collect()
}

/// Repository required by a scheduled job, if its effective cron declaration names one.
pub fn job_repo(cfg: &Config, ctx: Option<&Resolved>, job: &str) -> Option<String> {
    discover(cfg, ctx)
        .into_iter()
        .find(|cron| cron.entry.job == job)
        .and_then(|cron| cron.repo)
}

#[cfg(test)]
mod repo_tests {
    use super::*;

    #[test]
    fn on_calendar_takes_one_spec_or_a_list() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("crons.toml");
        std::fs::write(
            &path,
            r#"
[[cron]]
job = "one"
on_calendar = "daily"

[[cron]]
job = "two"
on_calendar = ["*-*-* 02:00:00", "*-*-* 07:30:00"]
"#,
        )
        .expect("write manifest");
        let entries = parse_manifest(&path).expect("parses");
        assert_eq!(entries[0].on_calendar.entries(), ["daily"]);
        assert_eq!(
            entries[1].on_calendar.entries(),
            ["*-*-* 02:00:00", "*-*-* 07:30:00"]
        );
        assert_eq!(
            entries[1].on_calendar.to_string(),
            "*-*-* 02:00:00, *-*-* 07:30:00"
        );

        std::fs::write(&path, "[[cron]]\njob = \"none\"\non_calendar = []\n").expect("write");
        assert!(parse_manifest(&path).is_err(), "an empty list is refused");
    }

    #[test]
    fn manifest_repo_is_inherited_unless_an_entry_overrides_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("crons.toml");
        std::fs::write(
            &path,
            r#"
repo = "bn"

[[cron]]
job = "defaulted"
on_calendar = "daily"

[[cron]]
job = "overridden"
repo = "arena"
on_calendar = "hourly"
"#,
        )
        .expect("write manifest");

        let entries = parse_manifest(&path).expect("parse manifest");
        assert_eq!(entries[0].repo.as_deref(), Some("bn"));
        assert_eq!(entries[1].repo.as_deref(), Some("arena"));
    }

    #[test]
    fn an_empty_entry_repo_opts_out_of_the_manifest_repo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("crons.toml");
        std::fs::write(
            &path,
            r#"
repo = "bn"

[[cron]]
job = "everywhere"
repo = ""
on_calendar = "*:0/5"
"#,
        )
        .expect("write manifest");

        let entries = parse_manifest(&path).expect("parse manifest");
        assert_eq!(entries[0].repo, None);
    }
}

/// The minimum recorded failures before a still-failing cron is worth alerting on — a couple of
/// transient failures stay quiet, a job that keeps failing does not.
pub const ALERT_MIN_FAILURES: usize = 3;

/// How long a job that nothing declares may stay silent before the watchdog treats it as retired
/// rather than broken.
///
/// Deliberately longer than the slowest cadence bn ships (weekly), because the recency half of
/// [`LiveJobs::is_retired`] is what covers natively-implemented jobs: those live in the binary's
/// dispatch table, not in any manifest [`LiveJobs`] can read, so the only evidence that one is
/// still scheduled is that it keeps running. A two-week silence clears every real cadence.
pub const RETIREMENT_HORIZON_SECS: i64 = 14 * 24 * 60 * 60;

const _: () = assert!(
    RETIREMENT_HORIZON_SECS > 7 * 24 * 60 * 60,
    "the horizon must clear the slowest shipped cadence (weekly), or a failing weekly cron would \
     be mistaken for a deleted one and silently stop alerting"
);

/// The job names this machine can still resolve — every source `bn run` looks in that is written
/// down somewhere: declared `[[cron]]` entries, agent manifests, and `<job>.sh` scripts — plus any
/// explicit tombstones ([`crate::events::retire_cron`]).
///
/// Native jobs are absent from the live set by construction (their names are a table in the CLI
/// binary, not data), which is why membership alone never decides retirement — see
/// [`LiveJobs::is_retired`].
pub struct LiveJobs {
    live: std::collections::HashSet<String>,
    /// Job → when it was tombstoned. Compared against the run timestamp, never used as a final
    /// verdict, so a revived name needs no un-retire step.
    retired_at: std::collections::HashMap<String, i64>,
}

impl LiveJobs {
    /// Fold the three manifest-backed job sources across the same tiers `bn run` resolves through,
    /// and read the tombstone log. An unreadable agent catalog is not fatal here: a watchdog that
    /// went silent because one manifest failed to parse would hide exactly the failures it exists
    /// to surface, so a broken catalog yields the other two sources and leaves the recency clause
    /// to do the rest.
    pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Self {
        let declared = discover(cfg, ctx).into_iter().map(|rc| rc.entry.job);
        let scripts = list_jobs(cfg, ctx).into_iter().map(|j| j.name);
        let agents = crate::agents::discover(cfg, ctx)
            .map(|catalog| catalog.iter().map(|a| a.name.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        Self {
            live: declared.chain(scripts).chain(agents).collect(),
            retired_at: crate::events::retired_crons(),
        }
    }

    /// Build from an explicit name set — the unit-test seam, so retirement logic is testable
    /// without a filesystem tier layout or an event log.
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            live: names.into_iter().map(Into::into).collect(),
            retired_at: std::collections::HashMap::new(),
        }
    }

    /// Add an explicit tombstone, as [`crate::events::retire_cron`] would have. Test seam.
    pub fn with_tombstone(mut self, job: impl Into<String>, at: i64) -> Self {
        self.retired_at.insert(job.into(), at);
        self
    }

    pub fn contains(&self, job: &str) -> bool {
        self.live.contains(job)
    }

    /// Whether `status` describes a job that no longer exists rather than one that is broken.
    ///
    /// Two independent routes, because retirement has two sources of truth. The operator can say so
    /// outright — a tombstone retires the name at once, and only a run *after* it was written
    /// revives it, so reviving a job needs nothing but running it. Failing that, the watchdog has
    /// to infer it, and inference needs both clauses: a job bn can still resolve is never retired
    /// no matter how long it has been quiet, and a job that is still running is never retired no
    /// matter what does or does not declare it. Both directions fail toward alerting.
    pub fn is_retired(&self, status: &CronJobStatus, now: i64) -> bool {
        if let Some(&at) = self.retired_at.get(&status.job) {
            return status.last_run <= at;
        }
        !self.contains(&status.job) && now.saturating_sub(status.last_run) > RETIREMENT_HORIZON_SECS
    }
}

/// The chronic cron failures a watchdog should raise: jobs whose *latest* run failed AND which have
/// accumulated at least `min_failures` total failures (so a `tools-reconcile` that fails every tick
/// can't rot silently, but a one-off blip stays quiet), minus the jobs that no longer exist at all
/// ([`LiveJobs::is_retired`]). Pure over the folded `cron_status` rows so it's unit-testable
/// without the event log. Preserves input order (most-recent-run first).
///
/// `live` is a parameter rather than something this resolves itself so the filter cannot be
/// forgotten at a call site: every watchdog that raises on cron health has to answer the
/// does-this-still-exist question to call it.
pub fn alerting_crons<'a>(
    runs: &'a [CronJobStatus],
    min_failures: usize,
    live: &LiveJobs,
    now: i64,
) -> Vec<&'a CronJobStatus> {
    runs.iter()
        .filter(|r| !r.ok && r.failures >= min_failures)
        .filter(|r| !live.is_retired(r, now))
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
                schedule: decl.map(|d| d.entry.on_calendar.to_string()),
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
    let last_runs = crate::events::last_runs();
    catalog
        .iter()
        .filter(|agent| agent.is_cron() && !agent.is_specialist())
        .map(|agent| {
            let last = last_runs.get(&agent.name).cloned();
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
    let declared = discover(cfg, ctx);
    let mut views = fold_cron_views(&declared, &cron_status(), &list_jobs(cfg, ctx));
    if !declares_job(&declared, "agent-scheduler") {
        views.retain(|view| view.job != "agent-scheduler");
    }
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
    let views = status_views(cfg, ctx);
    if !declares_job(&discover(cfg, ctx), "agent-scheduler") {
        return Ok(views);
    }
    let catalog = crate::agents::discover(cfg, ctx)?;
    let runs = cron_status();
    let children = agent_schedule_views(&catalog, &runs, crate::util::now_epoch_secs());
    Ok(nest_agent_schedules(views, children))
}

fn nest_agent_schedules(
    mut views: Vec<CronView>,
    children: Vec<AgentScheduleView>,
) -> Vec<CronView> {
    if views.iter().any(|view| view.job == "agent-scheduler") {
        let child_names: std::collections::HashSet<&str> =
            children.iter().map(|agent| agent.job.as_str()).collect();
        views.retain(|view| !child_names.contains(view.job.as_str()));
        if let Some(scheduler) = views.iter_mut().find(|view| view.job == "agent-scheduler") {
            scheduler.agents = children;
        }
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

    /// A chronically-failing status row: latest run failed, enough failures to alert.
    fn failing(job: &str, last_run: i64) -> CronJobStatus {
        CronJobStatus {
            job: job.to_string(),
            scope: "global".to_string(),
            repo: None,
            branch: None,
            last_run,
            last_exit: 127,
            last_duration_ms: 3,
            ok: false,
            runs: 7,
            failures: 7,
        }
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn a_job_that_still_resolves_is_never_retired_however_long_it_has_been_quiet() {
        let live = LiveJobs::from_names(["quarter-review"]);
        // A year of silence: a declared job is still a job, so this is "broken", not "gone".
        let stale = failing("quarter-review", NOW - 365 * 24 * 60 * 60);
        assert!(!live.is_retired(&stale, NOW));
        assert_eq!(
            alerting_crons(&[stale], ALERT_MIN_FAILURES, &live, NOW).len(),
            1
        );
    }

    #[test]
    fn a_job_nothing_declares_still_alerts_while_it_keeps_running() {
        // The native-job case: nothing on disk names `cron-health`, so only its own recency shows
        // it is still scheduled. Suppressing it here would mute a real, ongoing failure.
        let live = LiveJobs::from_names::<[&str; 0], &str>([]);
        let recent = failing("cron-health", NOW - 15 * 60);
        assert!(!live.is_retired(&recent, NOW));
        assert_eq!(
            alerting_crons(&[recent], ALERT_MIN_FAILURES, &live, NOW).len(),
            1
        );
    }

    #[test]
    fn a_renamed_job_retires_once_it_is_both_unresolvable_and_silent() {
        // `task-improve` after the rename to `task-enhance`: its 7/7 failures are frozen in the
        // event log and it will never run again, so the watchdog must stop raising it.
        let live = LiveJobs::from_names(["task-enhance"]);
        let renamed = failing("task-improve", NOW - RETIREMENT_HORIZON_SECS - 1);
        assert!(live.is_retired(&renamed, NOW));
        assert!(alerting_crons(&[renamed], ALERT_MIN_FAILURES, &live, NOW).is_empty());
    }

    #[test]
    fn retirement_waits_out_the_full_horizon() {
        // Exactly at the boundary the job is not yet retired — the horizon has to be *exceeded*,
        // so a weekly cron that fails is never mistaken for a deleted one.
        let live = LiveJobs::from_names(["task-enhance"]);
        let at_boundary = failing("task-improve", NOW - RETIREMENT_HORIZON_SECS);
        assert!(!live.is_retired(&at_boundary, NOW));
    }

    #[test]
    fn a_tombstone_retires_a_job_at_once_without_waiting_for_the_horizon() {
        // The whole point of the explicit form: `task-improve` failed minutes ago, so the recency
        // clause alone would keep it alerting for a fortnight.
        let live = LiveJobs::from_names(["task-enhance"]).with_tombstone("task-improve", NOW - 60);
        let recent = failing("task-improve", NOW - 120);
        assert!(live.is_retired(&recent, NOW));
        assert!(alerting_crons(&[recent], ALERT_MIN_FAILURES, &live, NOW).is_empty());
    }

    #[test]
    fn running_again_after_a_tombstone_revives_the_job() {
        // A tombstone is a claim about the past, so a later run overrides it — reviving a name
        // costs nothing but running it, and a tombstone aimed at the wrong job self-heals on its
        // next cadence rather than muting it forever.
        let live = LiveJobs::from_names::<[&str; 0], &str>([]).with_tombstone("revived", NOW - 600);
        let ran_since = failing("revived", NOW - 60);
        assert!(!live.is_retired(&ran_since, NOW));
        assert_eq!(
            alerting_crons(&[ran_since], ALERT_MIN_FAILURES, &live, NOW).len(),
            1
        );
    }

    #[test]
    fn a_healthy_or_barely_failing_cron_never_alerts() {
        let live = LiveJobs::from_names(["alpha", "beta"]);
        let mut healthy = failing("alpha", NOW - 60);
        healthy.ok = true;
        let mut blip = failing("beta", NOW - 60);
        blip.failures = ALERT_MIN_FAILURES - 1;
        assert!(alerting_crons(&[healthy, blip], ALERT_MIN_FAILURES, &live, NOW).is_empty());
    }

    fn agent(job: &str) -> AgentScheduleView {
        AgentScheduleView {
            job: job.to_string(),
            kind: crate::agents::AgentKind::Command,
            scope: "global".to_string(),
            schedule: "daily".to_string(),
            description: None,
            network: false,
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

    #[test]
    fn child_rows_are_preserved_without_a_scheduler_row() {
        let views = vec![cron("alpha"), cron("plain-cron"), cron("beta")];
        let nested = nest_agent_schedules(views, vec![agent("alpha"), agent("beta")]);

        assert_eq!(
            nested
                .iter()
                .map(|view| view.job.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "plain-cron", "beta"]
        );
    }

    #[test]
    fn status_views_hide_scheduler_history_when_it_is_undeclared() {
        let _lock = crate::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _cfg_dir =
            crate::test_support::EnvGuard::set("BN_CONFIG_DIR", temp.path().join("config"));
        let _jobs = crate::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
        let _state = crate::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
        let cfg = Config {
            personal_root: temp.path().join("personal"),
            work_root: temp.path().join("work"),
            hostname: "test-host".to_string(),
            ..Default::default()
        };

        crate::events::log_cron_run("agent-scheduler", "global", None, None, 1, 10, 0, "ok")
            .expect("scheduler run");

        assert!(!status_views(&cfg, None)
            .iter()
            .any(|view| view.job == "agent-scheduler"));
    }
}
