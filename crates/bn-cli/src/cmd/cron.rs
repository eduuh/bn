//! `bn cron` — job scheduling: run/register/dashboard.
//!
//! Crons are **headless**: `bn run <job>` executes the job in this process and records the
//! outcome. There is no tmux session, window, or pane involved — a cron never creates a session,
//! never steals focus, and never leaves a window behind. Read a run's result from the event log
//! (`bn cron`) rather than from a parked terminal.
use crate::*;

fn run_builtin_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    agent: Option<&bn_core::agents::AgentDef>,
    staff_json: bool,
) -> Option<i32> {
    if agent.is_some_and(bn_core::agents::AgentDef::is_staff) {
        return Some(run_native_job(cfg, ctx, job, |cfg, ctx| {
            staff_run_code(cfg, ctx, job, staff_json)
        }));
    }
    if let Some(f) = native_job(job) {
        return Some(run_native_job(cfg, ctx, job, f));
    }
    None
}

fn cron_event_target(
    ctx: Option<&Resolved>,
    scope: resolver::JobScope,
) -> (Option<&str>, Option<&str>) {
    match scope {
        resolver::JobScope::Global => (None, None),
        _ => ctx
            .map(|c| (Some(c.repo.as_str()), Some(c.branch.as_str())))
            .unwrap_or((None, None)),
    }
}

/// Apply the common host gate, running marker, timer, output tail, and `cron_run` ledger around one
/// resolved execution. Every native, staff, script, and custom agent command passes through here
/// exactly once.
pub(crate) fn run_recorded_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    scope: resolver::JobScope,
    execute: impl FnOnce() -> (i32, String),
) -> i32 {
    use std::time::Instant;

    if !cfg.job_allowed_here(job) {
        println!(
            "bn run: skipping '{job}' — not configured to run on '{}'",
            cfg.machine()
        );
        return 0;
    }

    let start = today_epoch();
    let t0 = Instant::now();
    let _running = bn_core::events::running_marker(job);
    let (code, tail) = execute();
    let duration_ms = t0.elapsed().as_millis() as i64;
    let (repo, branch) = cron_event_target(ctx, scope);
    let _ = bn_core::events::log_cron_run(
        job,
        scope.label(),
        repo,
        branch,
        start,
        duration_ms,
        code,
        &tail,
    );
    code
}

/// The refusal for a `bn run <name>` that resolves to nothing. Shared by the pre-capture guard and
/// the post-builtin fallback so the two wordings cannot drift apart.
fn no_such_job(job: &str) -> String {
    format!("bn run: no job or agent '{job}' (looked branch → repo → global)")
}

/// Run a named agent or ordinary job **in this process**. Agent manifests are resolved first:
/// specialists are refused, explicit `run:` commands use the common wrapper, and absent `run:`
/// resolves the same-name staff/native/script implementation without recursive `bn` spawning.
pub(crate) fn run_job(cfg: &Config, ctx: Option<&Resolved>, job: &str) -> i32 {
    run_job_with_staff_output(cfg, ctx, job, false)
}

pub(crate) fn run_job_with_staff_output(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    staff_json: bool,
) -> i32 {
    // The scheduler must be able to record its own catalog-load failure. Resolving it through the
    // catalog first would return before the common wrapper could write the failing scheduler row.
    if job == "agent-scheduler" {
        if let Some(queued) = crate::cmd::queue_producers::maybe_capture_job(cfg, ctx, job) {
            return i32::from(!queued);
        }
        return run_builtin_job(cfg, ctx, job, None, staff_json).unwrap_or(2);
    }

    let catalog = match bn_core::agents::discover(cfg, ctx) {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("bn run: invalid agent catalog: {error}");
            return 2;
        }
    };
    let agent = catalog.find(job).cloned();
    if let Some(agent) = agent.as_ref() {
        if let Err(error) = agent.ensure_runnable() {
            eprintln!("bn run: {error}");
            return 2;
        }
    }

    // Resolve *before* capturing. A name nothing backs used to be enqueued regardless, and only
    // died exit 2 later inside the queue executor where the diagnostic reached nobody — so
    // `bn run task-improve` printed "queued machine/run-task-improve" and exited 0 for a job that
    // had been renamed away. Refuse here, while a person is still watching the terminal.
    let script = resolver::job_script(cfg, ctx, job);
    if agent.is_none() && native_job(job).is_none() && script.is_none() {
        eprintln!("{}", no_such_job(job));
        return 2;
    }

    // Captured agents retain their identity and re-enter this exact resolver when dequeued.
    let queued = match agent.as_ref() {
        Some(agent) => crate::cmd::queue_producers::maybe_capture_agent(cfg, ctx, agent),
        None => crate::cmd::queue_producers::maybe_capture_job(cfg, ctx, job),
    };
    if let Some(queued) = queued {
        return i32::from(!queued);
    }

    if let Some((agent, command)) = agent
        .as_ref()
        .and_then(|agent| agent.run.as_deref().map(|command| (agent, command)))
    {
        return run_recorded_job(cfg, ctx, job, agent.scope, || {
            run_custom_command(command)
        });
    }
    if let Some(code) = run_builtin_job(cfg, ctx, job, agent.as_ref(), staff_json) {
        return code;
    }
    let Some((scope, script)) = script else {
        if agent.is_some() {
            eprintln!(
                "bn run: agent '{job}' has no custom `run:` and no same-name staff, native, or script job"
            );
        } else {
            eprintln!("{}", no_such_job(job));
        }
        return 2;
    };
    run_recorded_job(cfg, ctx, job, scope, || run_script(&script))
}

/// Single-quote a string for safe embedding inside a shell command.
pub(crate) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[path = "cron/retire.rs"]
mod retire;
pub(crate) use retire::{emit_cron_retire, emit_cron_unregister};

#[path = "cron/exec.rs"]
mod exec;
pub(crate) use exec::job_path;
use exec::{run_custom_command, run_script};

#[path = "cron/scheduler.rs"]
mod scheduler;
pub(crate) use scheduler::*;
#[path = "cron/drift.rs"]
mod drift;
pub(crate) use drift::cron_drift;

/// Health of the WSL keepalive Windows task (the one running `ensure-tmux`). `None` when it's
/// fine — or when we're not on WSL, where the warning is irrelevant. `Some(reason)` when it's
/// missing or disabled, which silently halts every systemd timer that needs WSL held up.
pub(crate) fn keepalive_status(wintasks: &[winsched::WinTask]) -> Option<&'static str> {
    if !winsched::on_wsl() {
        return None;
    }
    match wintasks.iter().find(|w| w.job == "ensure-tmux") {
        Some(w) if w.disabled() => Some("disabled"),
        Some(_) => None,
        None => Some("missing"),
    }
}

/// `bn cron running`: a compact badge of jobs running right now, for the tmux status bar.
/// Prints nothing when idle so the bar stays clean; one job → `⟳ <job>`; more → `⟳ <job>+N`.
pub(crate) fn emit_cron_running() {
    let jobs = bn_core::events::running_jobs();
    match jobs.split_first() {
        None => {}
        Some((first, [])) => println!("⟳ {first}"),
        Some((first, rest)) => println!("⟳ {first}+{}", rest.len()),
    }
}

pub(crate) fn emit_cron_dashboard(cfg: &Config, ctx: Option<&Resolved>, as_json: bool) {
    let scripts = resolver::list_jobs(cfg, ctx);
    let runs = bn_core::events::cron_status();
    let registered = timers::registered();
    let wintasks = winsched::registered();
    let now = bn_core::util::now_epoch_secs();
    let catalog = bn_core::agents::discover(cfg, ctx)
        .unwrap_or_else(|error| fail(&format!("bn cron: {error}")));
    let agent_views = bn_core::crons::agent_schedule_views(&catalog, &runs, now);
    let scheduled_agents: std::collections::HashSet<&str> =
        agent_views.iter().map(|agent| agent.job.as_str()).collect();
    let declared = bn_core::crons::discover(cfg, ctx);

    // Job-name universe, scheduled first → scripted → run-only orphans (insertion order kept).
    let mut names: Vec<String> = Vec::new();
    let add = |n: &str, names: &mut Vec<String>| {
        if !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for cron in &declared {
        add(&cron.entry.job, &mut names);
    }
    for t in &registered {
        add(&t.job, &mut names);
    }
    for w in &wintasks {
        add(&w.job, &mut names);
    }
    for s in &scripts {
        add(&s.name, &mut names);
    }
    for r in &runs {
        add(&r.job, &mut names);
    }
    names.retain(|name| !scheduled_agents.contains(name.as_str()));

    let timer = |name: &str| registered.iter().find(|t| t.job == name);
    let wintask = |name: &str| wintasks.iter().find(|w| w.job == name);
    let script = |name: &str| scripts.iter().find(|s| s.name == name);
    // Declared crons drive the class tag (info vs housekeeping) — a job with no `[[cron]]` entry
    // (script-only or run-only orphan) carries no class.
    let class = |name: &str| {
        declared
            .iter()
            .find(|rc| rc.entry.job == name)
            .map(|rc| rc.entry.class)
    };
    // cron_status is most-recent-first, so the first match is the latest run for this job.
    let run = |name: &str| runs.iter().find(|r| r.job == name);
    let scope = |name: &str| {
        declared
            .iter()
            .find(|cron| cron.entry.job == name)
            .map(|cron| cron.scope.label())
            .or_else(|| script(name)
            .map(|s| s.scope.label())
            .or_else(|| run(name).map(|r| r.scope.as_str())))
            .unwrap_or("?")
    };

    if as_json {
        let rows: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                let t = timer(name);
                let w = wintask(name);
                let r = run(name);
                let agents: &[bn_core::crons::AgentScheduleView] = if name == "agent-scheduler" {
                    &agent_views
                } else {
                    &[]
                };
                // systemd timer wins over a Windows task if somehow both schedule a job.
                let (scheduler, next_run) = match (t, w) {
                    (Some(t), _) => (Some("systemd"), t.next_epoch),
                    (None, Some(w)) => (Some("windows"), w.next_epoch),
                    (None, None) => (None, None),
                };
                serde_json::json!({
                    "job": name,
                    "scope": scope(name),
                    "class": class(name).map(|c| c.label()),
                    "native": native_job(name).is_some() || catalog.find(name).is_some(),
                    "has_script": script(name).is_some(),
                    "script_path": script(name).map(|s| s.path.clone()),
                    "scheduled": scheduler.is_some(),
                    "scheduler": scheduler,
                    "next_run": next_run,
                    "schedule": declared.iter()
                        .find(|cron| cron.entry.job == *name)
                        .map(|cron| cron.entry.on_calendar.clone())
                        .or_else(|| t.and_then(|timer| timer.schedule.clone())),
                    "timer_unit": t.map(|t| t.timer_unit.clone()),
                    "task_name": w.map(|w| w.task_name.clone()),
                    "win_state": w.map(|w| w.state.clone()),
                    "win_last_result": w.and_then(|w| w.last_result),
                    "last_run": r.map(|r| r.last_run),
                    "last_exit": r.map(|r| r.last_exit),
                    "ok": r.map(|r| r.ok),
                    "runs": r.map(|r| r.runs),
                    "failures": r.map(|r| r.failures),
                    "agents": agents,
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success(
                "cron",
                serde_json::json!({
                    "jobs": rows,
                    "on_wsl": winsched::on_wsl(),
                    "keepalive_ok": keepalive_status(&wintasks).is_none(),
                })
            )
            .to_json()
        );
        return;
    }

    // Drift: declared crons with no timer here — the silent "fresh box" case. Surface before the
    // empty/keepalive checks so a machine that registered nothing still gets the nudge.
    let drift = cron_drift(cfg, ctx);
    if !drift.is_empty() {
        println!(
            "⚠ {} cron(s) declared but not registered on this machine — run `bn cron register`:",
            drift.len()
        );
        println!("    {}\n", drift.join(", "));
    }

    if names.is_empty() {
        if drift.is_empty() {
            println!("No cron jobs registered or scripted");
        }
        return;
    }
    // On WSL every systemd timer depends on the keepalive Windows task holding WSL up; if it's
    // missing or disabled, those timers silently never fire — surface that first.
    if let Some(why) = keepalive_status(&wintasks) {
        println!(
            "⚠ WSL keepalive task (ensure-tmux) is {why} — systemd timers may not fire. Re-run jobs/windows/install.ps1."
        );
    }
    for name in &names {
        // systemd timer wins over a Windows task if somehow both schedule a job.
        let schedule = if let Some(t) = timer(name) {
            match t.next_epoch {
                Some(next) => format!("systemd · next {}", until(next - now)),
                None => "systemd · scheduled".to_string(),
            }
        } else if let Some(w) = wintask(name) {
            if w.disabled() {
                "windows · DISABLED".to_string()
            } else {
                let base = match w.next_epoch {
                    Some(next) => format!("windows · next {}", until(next - now)),
                    None => "windows · scheduled".to_string(),
                };
                if w.last_failed() {
                    format!("{base} · last FAIL ({})", w.last_result.unwrap_or(-1))
                } else {
                    base
                }
            }
        } else {
            "not scheduled".to_string()
        };
        let orphan = if (timer(name).is_some() || wintask(name).is_some()) && script(name).is_none()
        {
            if native_job(name).is_some() || catalog.find(name).is_some() {
                "  (native handler)"
            } else {
                "  (no script — `bn run` would fail)"
            }
        } else {
            ""
        };
        // `info` crons gather something a person reads, `housekeeping` ones do silent estate work.
        // Both run headlessly; the tag is a descriptive label for the dashboard, nothing more.
        let class_tag = match class(name) {
            Some(bn_core::crons::CronClass::Info) => "  👁 info",
            _ => "",
        };
        println!(
            "{name:<16} {:<7} {schedule}{orphan}{class_tag}",
            scope(name)
        );

        let last = match run(name) {
            Some(r) => format!(
                "last: {} {} ago · {}ms · {} runs, {} fail",
                if r.ok { "ok" } else { "FAIL" },
                ago(now - r.last_run),
                r.last_duration_ms,
                r.runs,
                r.failures
            ),
            None => "last: never run".to_string(),
        };
        println!("{:<24} {last}", "");

        if name == "agent-scheduler" && !agent_views.is_empty() {
            for a in &agent_views {
                let state = a
                    .retry_at
                    .map(|retry_at| format!("retry {}", until(retry_at - now)))
                    .unwrap_or_else(|| a.state.clone());
                println!(
                    "  ├ {:<20} {:<8} {} · {}",
                    a.job,
                    a.kind.label(),
                    a.schedule,
                    state
                );
            }
        }
    }
}

/// `bn cron unregister <job>`: stop and unregister the systemd `--user` timer that drives a
/// job (`bn run <job>`). The job script stays — only its schedule is removed, so it still shows
/// in `bn cron` as not-scheduled. Re-register by re-running `jobs/systemd/install.sh`.
/// Show a job's most recent run: a status line plus its captured output tail (the error output
/// for a failed run; for a prompt job, what the prompt produced).
pub(crate) fn emit_cron_show(job: &str, as_json: bool) {
    let Some(d) = bn_core::events::last_run(job) else {
        if as_json {
            println!(
                "{}",
                Envelope::success("cron_show", serde_json::json!({ "job": job, "run": null }))
                    .to_json()
            );
        } else {
            println!("'{job}' has no recorded runs — `bn cron` lists scheduled jobs");
        }
        return;
    };
    if as_json {
        println!(
            "{}",
            Envelope::success("cron_show", serde_json::json!({ "job": job, "run": d })).to_json()
        );
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    let status = if d.exit_code == 0 { "ok" } else { "FAILED" };
    println!(
        "{job}: {status} (exit {}) · {} ago · {}ms",
        d.exit_code,
        ago(now - d.ts),
        d.duration_ms
    );
    let tail = d.output_tail.trim_end();
    println!(
        "{}",
        if tail.is_empty() {
            "(no output captured)"
        } else {
            tail
        }
    );
}

/// List recent failed runs, newest first, each with its error tail — the detail behind the
/// failure counts in `bn cron status`.
pub(crate) fn emit_cron_failures(limit: usize, as_json: bool) {
    let fails = bn_core::events::recent_failures(limit);
    if as_json {
        println!(
            "{}",
            Envelope::success("cron_failures", serde_json::json!({ "failures": fails })).to_json()
        );
        return;
    }
    if fails.is_empty() {
        println!("No failed runs recorded");
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    for d in &fails {
        println!(
            "✗ {} · exit {} · {} ago",
            d.job,
            d.exit_code,
            ago(now - d.ts)
        );
        for line in d.output_tail.trim_end().lines() {
            println!("    {line}");
        }
    }
}

/// `~/.config/systemd/user` (honoring `$XDG_CONFIG_HOME`) — where user timer units live.
pub(crate) fn systemd_user_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(format!(
                "{}/.config",
                std::env::var("HOME").unwrap_or_default()
            ))
        });
    base.join("systemd").join("user")
}

pub(crate) fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Whether systemd's per-user manager is actually usable here — the `systemctl` binary exists AND
/// its `--user` bus answers. Probes with `show-environment`, a side-effect-free read that requires
/// the bus, so this is `false` both where systemd is absent (macOS / stock WSL — the spawn fails)
/// and on a systemd host whose per-user manager isn't reachable (no login session, lingering
/// disabled, many containers). The old `--version` probe never touched the bus, so it reported
/// `true` on a bus-less host and every subsequent `daemon-reload`/`enable` spewed "Failed to
/// connect to bus"; gating on the bus here degrades cleanly to the report-only plan instead.
pub(crate) fn have_systemctl() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether the `systemctl` binary merely *exists* (its `--version` answers), independent of the
/// `--user` bus. Lets `bn cron register` tell "no systemd at all" (macOS / stock WSL) apart from
/// "systemd present but the user bus is unreachable" and point at `loginctl enable-linger`.
pub(crate) fn systemctl_present() -> bool {
    std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `bn cron register`: generate + install platform timers from the cron manifests, diffing against
/// what's registered so the run is idempotent. systemd hosts get real units written + enabled;
/// elsewhere we print the plan (and the crontab lines for the supported subset) without touching
/// the system — honest about the v1 reporting-only path off systemd.
pub(crate) fn emit_cron_register(
    cfg: &Config,
    ctx: Option<&Resolved>,
    dry_run: bool,
    as_json: bool,
) {
    bn_core::agents::discover(cfg, ctx)
        .unwrap_or_else(|error| fail(&format!("bn cron register: {error}")));

    // Prefer the bindir-independent stable anchor (`<config_dir>/bn`, which install.sh keeps
    // pointed at the live binary) so the systemd units survive a bindir move; fall back to the
    // resolved bin dir. Both are absolute, as ExecStart requires.
    let anchor = resolver::config_dir().join("bn");
    let bn_path = if anchor.exists() {
        anchor.to_string_lossy().into_owned()
    } else {
        resolver::bin_dir()
            .join("bn")
            .to_string_lossy()
            .into_owned()
    };
    let want = bn_core::crons::discover(cfg, ctx);

    // Backend: systemd on Linux; launchd user agents on macOS. `have` and the on-disk artifact
    // dir are backend-shaped, but the diff/prune logic below is shared (launchd agents are mapped
    // into the same RegisteredTimer shape, `timer_unit` carrying the label).
    let systemd = have_systemctl();
    let launchd_on = !systemd && launchd::have_launchctl();
    let have: Vec<timers::RegisteredTimer> = if systemd {
        timers::registered()
    } else if launchd_on {
        launchd::registered()
            .into_iter()
            .map(|a| timers::RegisteredTimer {
                job: a.job,
                timer_unit: a.label,
                schedule: None,
                next_epoch: None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let dir = if launchd_on {
        launchd::agents_dir()
    } else {
        systemd_user_dir()
    };
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Diff by comparing the rendered artifact text against what's on disk (systemd normalizes
    // OnCalendar, so a schedule-string compare would report spurious updates forever). A job is
    // "unchanged" only when its rendered file(s) match AND it's registered. A schedule launchd
    // can't express is warned about once here and dropped from the plan.
    let mut adds: Vec<&bn_core::crons::ResolvedCron> = Vec::new();
    let mut updates: Vec<&bn_core::crons::ResolvedCron> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    let on_disk = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    for rc in &want {
        let files_match = if launchd_on {
            match cron_gen::render_launchd(rc, &bn_path) {
                Ok(plist) => {
                    let label = cron_gen::launchd_label(rc);
                    on_disk(dir.join(format!("{label}.plist"))) == plist
                }
                Err(e) => {
                    // A schedule launchd can't express isn't an install failure — it's still
                    // scheduled on Linux via systemd. Name the timer and skip it so `--core`
                    // install output stays clean and actionable instead of spewing a bare
                    // `error:` that makes a healthy install look broken. #421
                    warnings.push(format!("skipping timer '{}' — {e}", rc.entry.job));
                    continue;
                }
            }
        } else {
            let name = cron_gen::unit_name(rc);
            let (service, timer) = cron_gen::render_systemd(rc, &bn_path);
            on_disk(dir.join(format!("{name}.service"))) == service
                && on_disk(dir.join(format!("{name}.timer"))) == timer
        };
        let registered = have.iter().any(|t| t.job == rc.entry.job);
        if files_match && registered {
            unchanged.push(rc.entry.job.clone());
        } else if registered {
            updates.push(rc);
        } else {
            adds.push(rc);
        }
    }
    let want_jobs: std::collections::HashSet<&str> =
        want.iter().map(|rc| rc.entry.job.as_str()).collect();
    let orphans: Vec<&timers::RegisteredTimer> = have
        .iter()
        .filter(|t| !want_jobs.contains(t.job.as_str()))
        .collect();

    // Legacy units to supersede: a timer for a job we're (re)generating but under a DIFFERENT unit
    // name / label (e.g. an old `bn-refresh.timer` when we now generate `bn-refresh-all.timer`).
    // Left in place they'd double-fire the job — a same-job rename, always safe to replace.
    let superseded: Vec<&timers::RegisteredTimer> = want
        .iter()
        .flat_map(|rc| {
            let current = if launchd_on {
                cron_gen::launchd_label(rc)
            } else {
                format!("{}.timer", cron_gen::unit_name(rc))
            };
            have.iter()
                .filter(move |t| t.job == rc.entry.job && t.timer_unit != current)
        })
        .collect();

    let mut applied: Vec<String> = Vec::new();

    if !dry_run && systemd {
        // Write/refresh every add + update, then one daemon-reload, then enable each.
        let mut to_enable: Vec<String> = Vec::new();
        for rc in adds.iter().chain(updates.iter()) {
            let name = cron_gen::unit_name(rc);
            let (service, timer) = cron_gen::render_systemd(rc, &bn_path);
            let write = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(format!("{name}.service")), service)?;
                std::fs::write(dir.join(format!("{name}.timer")), timer)?;
                Ok(())
            })();
            match write {
                Ok(()) => to_enable.push(format!("{name}.timer")),
                Err(e) => errors.push(format!("{name}: write failed: {e}")),
            }
        }
        if !to_enable.is_empty() {
            if let Err(e) = systemctl(&["daemon-reload"]) {
                errors.push(format!("daemon-reload: {e}"));
            }
            for unit in &to_enable {
                match systemctl(&["enable", "--now", unit]) {
                    Ok(()) => applied.push(unit.clone()),
                    Err(e) => errors.push(format!("{unit}: enable failed: {e}")),
                }
            }
        }
        // Replace differently-named legacy units for jobs we just (re)generated, so the job runs
        // once, not twice. Always done (a rename, not an orphan prune).
        for t in &superseded {
            match timers::unregister(&t.timer_unit) {
                Ok(()) => {
                    if t.timer_unit.starts_with("bn-") {
                        let stem = t.timer_unit.trim_end_matches(".timer");
                        let _ = std::fs::remove_file(dir.join(format!("{stem}.timer")));
                        let _ = std::fs::remove_file(dir.join(format!("{stem}.service")));
                    }
                    applied.push(format!("superseded {}", t.timer_unit));
                }
                Err(e) => errors.push(format!("{}: supersede failed: {e}", t.timer_unit)),
            }
        }
        // Remove bn timers no longer in any manifest. Registration is the source of truth, so a
        // dropped manifest entry means a dropped timer — no flag to opt in.
        for t in &orphans {
            match timers::unregister(&t.timer_unit) {
                Ok(()) => {
                    // Only delete files we own (bn-*), leave anything else in place.
                    if t.timer_unit.starts_with("bn-") {
                        let stem = t.timer_unit.trim_end_matches(".timer");
                        let _ = std::fs::remove_file(dir.join(format!("{stem}.timer")));
                        let _ = std::fs::remove_file(dir.join(format!("{stem}.service")));
                    }
                    applied.push(format!("pruned {}", t.timer_unit));
                }
                Err(e) => errors.push(format!("{}: prune failed: {e}", t.timer_unit)),
            }
        }
    }

    if !dry_run && launchd_on {
        // Write + (re)bootstrap every add + update — launchd needs no daemon-reload/enable split;
        // bootstrap both loads and starts the schedule.
        for rc in adds.iter().chain(updates.iter()) {
            let Ok(plist) = cron_gen::render_launchd(rc, &bn_path) else {
                continue; // unrenderable schedules were already reported in the diff
            };
            let label = cron_gen::launchd_label(rc);
            let path = dir.join(format!("{label}.plist"));
            let write = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, plist));
            match write {
                Ok(()) => match launchd::bootstrap(&path, &label) {
                    Ok(()) => applied.push(label.clone()),
                    Err(e) => errors.push(format!("{label}: bootstrap failed: {e}")),
                },
                Err(e) => errors.push(format!("{label}: write failed: {e}")),
            }
        }
        // Superseded legacy labels (same job, older label — e.g. the hand-written com.bn.refresh
        // vs the generated com.bn.refresh-all) and orphans (job left the manifest): unload, and
        // delete only files we own (com.bn.*).
        for t in superseded.iter().chain(orphans.iter()) {
            match launchd::bootout(&t.timer_unit) {
                Ok(()) => {
                    if t.timer_unit.starts_with("com.bn.") {
                        let _ = std::fs::remove_file(dir.join(format!("{}.plist", t.timer_unit)));
                    }
                    applied.push(format!("removed {}", t.timer_unit));
                }
                Err(e) => errors.push(format!("{}: bootout failed: {e}", t.timer_unit)),
            }
        }
    }

    if as_json {
        let plan = serde_json::json!({
            "add": adds.iter().map(|rc| rc.entry.job.clone()).collect::<Vec<_>>(),
            "update": updates.iter().map(|rc| rc.entry.job.clone()).collect::<Vec<_>>(),
            "unchanged": unchanged,
            "orphans": orphans.iter().map(|t| t.job.clone()).collect::<Vec<_>>(),
            "superseded": superseded.iter().map(|t| t.timer_unit.clone()).collect::<Vec<_>>(),
            "applied": applied,
            "errors": errors,
            "warnings": warnings,
            "dry_run": dry_run,
            "systemd": systemd,
            "launchd": launchd_on,
        });
        println!("{}", Envelope::success("cron_register", plan).to_json());
        return;
    }

    println!(
        "cron register — {} declared ({} add, {} update, {} unchanged){}",
        want.len(),
        adds.len(),
        updates.len(),
        unchanged.len(),
        if dry_run { " [dry-run]" } else { "" }
    );
    for rc in &adds {
        println!("  + {:<16} {}", rc.entry.job, rc.entry.on_calendar);
    }
    for rc in &updates {
        let old = have
            .iter()
            .find(|t| t.job == rc.entry.job)
            .and_then(|t| t.schedule.clone())
            .unwrap_or_default();
        println!(
            "  ~ {:<16} {} → {}",
            rc.entry.job, old, rc.entry.on_calendar
        );
    }
    if !orphans.is_empty() {
        let verb = if dry_run {
            "orphaned (will remove)"
        } else {
            "pruned"
        };
        for t in &orphans {
            println!("  - {:<16} {verb}", t.job);
        }
    }
    for t in &superseded {
        println!("  ⇄ {:<16} supersedes legacy unit {}", t.job, t.timer_unit);
    }
    if !systemd && !launchd_on {
        if systemctl_present() {
            // systemd exists but its per-user bus isn't reachable (no login session, lingering
            // off, container) — the exact case that used to spew one "Failed to connect to bus"
            // per timer. Point at the one-time fix instead of a wall of `error:` lines.
            let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
            println!(
                "\nsystemd is present but its per-user bus isn't reachable — no timers written.\n  \
                 Start the user manager so it runs without an active login session, then re-run:\n    \
                 loginctl enable-linger {user}\n    bn cron register"
            );
        } else {
            println!("\nno scheduler backend (systemd or launchd) — no timers written.");
        }
    } else if !dry_run {
        if !applied.is_empty() {
            println!("\napplied: {}", applied.join(", "));
        }
        if applied.is_empty() && adds.is_empty() && updates.is_empty() {
            println!("\nall timers already match the manifest — nothing to do");
        }
    }
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    for e in &errors {
        eprintln!("error: {e}");
    }
}

pub(crate) fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub(crate) fn today_epoch() -> i64 {
    bn_core::util::now_epoch_secs()
}

/// First-bn-event catch-up. On the day's first bn command, fire every `catch_up = true` cron job
/// once, detached, via `bn run <job> --if-stale` — so a meetings/standup refresh happens whenever
/// you start work, not at a wall-clock time you may not match. Gated to once per day by a sentinel
/// in the state dir; the spawned children carry `--if-stale` (a no-op if their timer already ran
/// today) and are fully detached, so this never blocks or delays the command you actually ran.
pub(crate) fn maybe_fire_catchup(cfg: &Config) {
    // The spawned children are `bn run …`, already excluded by the caller's match — but guard with
    // an env flag too, so any nested bn call inside a job can't re-trigger the sweep.
    if std::env::var_os("BN_NO_CATCHUP").is_some() {
        return;
    }
    // Cheap hot path: a single stat. The sentinel exists for the rest of the day after the first
    // event, so every later command returns here without parsing the manifest.
    let sentinel = resolver::state_dir().join(format!("catchup-{}", today()));
    if sentinel.exists() {
        return;
    }
    // First event today: claim the day by writing the sentinel (best-effort — a read-only or
    // missing state dir just means no catch-up, never a failure of the command the user ran).
    let _ = std::fs::create_dir_all(resolver::state_dir());
    if std::fs::write(&sentinel, b"").is_err() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    for job in bn_core::crons::catch_up_jobs(cfg, None) {
        let _ = std::process::Command::new(&exe)
            .args(["run", &job, "--if-stale"])
            .env("BN_NO_CATCHUP", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Whether `job`'s most recent run was a success (`exit_code == 0`) that landed on today's
/// local date — the gate behind `bn run --if-stale`. A job that has never run, last failed,
/// or last ran on a prior day is "stale" and should run again.
pub(crate) fn ran_ok_today(job: &str) -> bool {
    let Some(run) = bn_core::events::last_run(job) else {
        return false;
    };
    if run.exit_code != 0 {
        return false;
    }
    bn_core::util::local_date_of(run.ts) == today()
}

/// Compact human age for an elapsed-seconds value.
pub(crate) fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// Human relative future ("3h", "2d"); anything due now or overdue reads as "now".
pub(crate) fn until(secs: i64) -> String {
    if secs <= 0 {
        "now".to_string()
    } else {
        ago(secs)
    }
}

#[cfg(all(test, unix))]
#[path = "cron/tests.rs"]
mod tests;
