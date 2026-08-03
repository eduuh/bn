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
    // Let `[quarter].review_cron` / `$BN_QUARTER_REVIEW_CRON` steer the quarter-review schedule
    // across every backend `bn cron register` generates, keeping config the single knob.
    if let Some(sched) = crate::resolver::quarter_review_cron_override() {
        for rc in out.iter_mut() {
            if rc.entry.job == "quarter-review" {
                rc.entry.on_calendar = sched.clone();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_defaults_housekeeping_and_parses_info() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crons.toml");
        std::fs::write(
            &p,
            r#"
[[cron]]
job = "wt-cleanup"
on_calendar = "*:0/15"

[[cron]]
job = "meetings"
on_calendar = "daily"
class = "info"
"#,
        )
        .unwrap();
        let entries = parse_manifest(&p).unwrap();
        assert_eq!(entries[0].class, CronClass::Housekeeping, "class defaults");
        assert_eq!(entries[1].class, CronClass::Info, "class = info parses");
    }

    #[test]
    fn fold_surfaces_declared_class_only() {
        let mut info = resolved("meetings", "daily", JobScope::Global);
        info.entry.class = CronClass::Info;
        let declared = vec![info];
        let scripts = vec![script("wt-cleanup")]; // undeclared → no class
        let views = fold_cron_views(&declared, &[], &scripts);
        assert_eq!(
            views.iter().find(|v| v.job == "meetings").unwrap().class,
            Some("info".to_string())
        );
        assert_eq!(
            views.iter().find(|v| v.job == "wt-cleanup").unwrap().class,
            None,
            "an undeclared script job carries no class"
        );
    }

    #[test]
    fn alerting_crons_flags_only_chronic_current_failures() {
        let runs = vec![
            run("tools-reconcile", 1, 540, 539, 3000), // failing now, chronic → alert
            run("wt-cleanup", 0, 345, 2, 2000),        // healthy last run → no alert
            run("blip", 1, 5, 1, 1000),                // failed now but below threshold → quiet
        ];
        let alerting = alerting_crons(&runs, ALERT_MIN_FAILURES);
        let jobs: Vec<&str> = alerting.iter().map(|r| r.job.as_str()).collect();
        assert_eq!(jobs, vec!["tools-reconcile"]);
    }

    #[test]
    fn parse_manifest_round_trips_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crons.toml");
        std::fs::write(
            &p,
            r#"
[[cron]]
job = "wt-cleanup"
on_calendar = "*:0/15"

[[cron]]
job = "refresh-all"
on_calendar = "*-*-* 05:00:00"
"#,
        )
        .unwrap();
        let entries = parse_manifest(&p).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].job, "wt-cleanup");
        assert!(entries[0].persistent, "persistent defaults true");
    }

    #[test]
    fn catch_up_defaults_false_and_parses_true() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crons.toml");
        std::fs::write(
            &p,
            r#"
[[cron]]
job = "refresh-all"
on_calendar = "daily"

[[cron]]
job = "meetings"
on_calendar = "*-*-* 14:50:00"
catch_up = true
"#,
        )
        .unwrap();
        let entries = parse_manifest(&p).unwrap();
        assert!(!entries[0].catch_up, "catch_up defaults false");
        assert!(entries[1].catch_up, "catch_up = true parses");
    }

    #[test]
    fn parse_manifest_rejects_missing_required_field() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crons.toml");
        // Missing `on_calendar`.
        std::fs::write(&p, "[[cron]]\njob = \"x\"\n").unwrap();
        let err = parse_manifest(&p).unwrap_err();
        assert!(err.contains("on_calendar"), "error names the field: {err}");
    }

    fn resolved(job: &str, on_calendar: &str, scope: JobScope) -> ResolvedCron {
        ResolvedCron {
            entry: CronEntry {
                job: job.to_string(),
                class: CronClass::Housekeeping,
                on_calendar: on_calendar.to_string(),
                only_on: Vec::new(),
                description: Some(format!("{job} desc")),
                persistent: true,
                randomized_delay: None,
                network: false,
                catch_up: false,
            },
            scope,
            repo: None,
            source: PathBuf::from("/x/crons.toml"),
        }
    }

    fn run(job: &str, exit: i32, runs: usize, failures: usize, ts: i64) -> CronJobStatus {
        CronJobStatus {
            job: job.to_string(),
            scope: "global".into(),
            repo: None,
            branch: None,
            last_run: ts,
            last_exit: exit,
            last_duration_ms: 10,
            ok: exit == 0,
            runs,
            failures,
        }
    }

    fn script(job: &str) -> JobRef {
        JobRef {
            name: job.to_string(),
            scope: JobScope::Global,
            path: format!("/jobs/{job}.sh"),
        }
    }

    #[test]
    fn fold_joins_declared_script_and_runs_by_job() {
        let declared = vec![resolved("wt-cleanup", "*:0/15", JobScope::Global)];
        let runs = vec![run("wt-cleanup", 0, 12, 1, 1000)];
        let scripts = vec![script("wt-cleanup")];
        let views = fold_cron_views(&declared, &runs, &scripts);
        assert_eq!(views.len(), 1);
        let v = &views[0];
        assert_eq!(v.job, "wt-cleanup");
        assert!(v.declared);
        assert_eq!(v.schedule.as_deref(), Some("*:0/15"));
        assert!(v.has_script);
        assert_eq!(v.script_path.as_deref(), Some("/jobs/wt-cleanup.sh"));
        assert_eq!(v.last_run, Some(1000));
        assert_eq!(v.ok, Some(true));
        assert_eq!(v.runs, 12);
        assert_eq!(v.failures, 1);
    }

    #[test]
    fn fold_orders_declared_first_then_scripts_then_run_orphans() {
        let declared = vec![resolved("refresh-all", "daily", JobScope::Global)];
        let scripts = vec![script("meetings"), script("refresh-all")];
        // A run for a job that is neither declared nor scripted — a run-only orphan, listed last.
        let runs = vec![run("self-update", 1, 3, 2, 500)];
        let views = fold_cron_views(&declared, &runs, &scripts);
        let names: Vec<&str> = views.iter().map(|v| v.job.as_str()).collect();
        assert_eq!(names, vec!["refresh-all", "meetings", "self-update"]);
        // The run-only orphan is undeclared, scriptless, but carries its last-run health.
        let orphan = &views[2];
        assert!(!orphan.declared);
        assert!(!orphan.has_script);
        assert_eq!(orphan.ok, Some(false));
        assert_eq!(orphan.failures, 2);
        // The declared-but-never-run job has no run fields.
        let never = &views[0];
        assert_eq!(never.last_run, None);
        assert_eq!(never.runs, 0);
    }

    #[test]
    fn fold_scope_prefers_declared_then_script_then_run() {
        let declared = vec![resolved("a", "daily", JobScope::Repo)];
        let scripts = vec![script("b")]; // JobScope::Global
        let runs = vec![run("c", 0, 1, 0, 1)];
        let views = fold_cron_views(&declared, &runs, &scripts);
        assert_eq!(views.iter().find(|v| v.job == "a").unwrap().scope, "repo");
        assert_eq!(views.iter().find(|v| v.job == "b").unwrap().scope, "global");
        assert_eq!(views.iter().find(|v| v.job == "c").unwrap().scope, "global");
    }

    #[test]
    fn status_views_surfaces_last_output_only_for_a_failing_run() {
        // A failing job's captured output tail is surfaced as `last_output`; a healthy one carries
        // nothing. Isolate the event log + jobs dir so only the seeded run-observed orphan appears.
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::with_testhost_state_and_notes_dir(dir.path(), || {
            let cfg = crate::test_support::config(dir.path());
            crate::events::log_cron_run(
                "failing-job",
                "global",
                None,
                None,
                1000,
                100,
                1,
                "boom stderr\n",
            )
            .unwrap();
            crate::events::log_cron_run("ok-job", "global", None, None, 1000, 100, 0, "all good\n")
                .unwrap();
            let views = status_views(&cfg, None);
            let failing = views
                .iter()
                .find(|v| v.job == "failing-job")
                .expect("run-observed orphan");
            assert_eq!(failing.ok, Some(false));
            assert_eq!(failing.last_output.as_deref(), Some("boom stderr"));
            let ok = views
                .iter()
                .find(|v| v.job == "ok-job")
                .expect("run-observed orphan");
            assert_eq!(ok.ok, Some(true));
            assert!(
                ok.last_output.is_none(),
                "a healthy job carries no output tail"
            );
        });
    }
}
