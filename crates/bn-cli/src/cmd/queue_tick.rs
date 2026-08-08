//! `bn queue tick` — one dispatcher pass, and the loop that repeats it.
//!
//! The IO half of the queue dispatcher: sample the machine, call the pure
//! [`bn_core::queue::tick::plan_tick`], persist the decision, and realize the intent. All the
//! *policy* lives in bn-core; what happens here is reading the world and acting on the answer.
//!
//! Two drivers, one code path: the `queue-tick` cron (once a minute, the always-on floor) and
//! `bn queue run` (a resident loop for a supervised box). They interlock on a non-blocking lock
//! file, so whichever wins the race runs and the other cleanly skips — one tick per machine, no
//! matter how it was started.
mod event_fields;

use crate::*;
use event_fields::{dispatch_fields, finish_fields, launch_failure_fields};
use bn_core::backlog::{self, BacklogKind};
use bn_core::queue::decision::QueueDecision;
use bn_core::queue::tick::{plan_tick, Dispatch, TickPlan};
use std::path::PathBuf;

/// The lock that makes "one tick per machine" true regardless of which driver started it.
fn lock_path() -> PathBuf {
    bn_core::resolver::state_dir().join("queue-tick.lock")
}

/// `bn queue tick` — one pass.
pub(crate) fn emit_tick(cfg: &Config, dry_run: bool, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    match tick_once(cfg, now, dry_run) {
        TickOutcome::Skipped => {
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "queue-tick",
                        serde_json::json!({ "skipped": true, "reason": "locked" })
                    )
                    .to_json()
                );
            } else {
                println!("another tick is already running on this machine — skipped");
            }
        }
        TickOutcome::Ran(decision) => {
            if json {
                println!(
                    "{}",
                    Envelope::success("queue-tick", serde_json::to_value(&decision).unwrap_or_default())
                        .to_json()
                );
            } else {
                print_decision(&decision);
            }
        }
    }
}

/// The `queue-tick` cron job — the always-on floor.
///
/// Once a minute is well inside the noise floor of an agent that runs for ten to forty minutes,
/// and it needs no new mechanism: the existing timer generators already turn a `[[cron]]` entry
/// into a systemd unit or launchd job that survives reboot. A box wanting faster pickup runs
/// `bn queue run` alongside; the lock keeps them from overlapping.
pub(crate) fn job_queue_tick(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    match tick_once(cfg, now, false) {
        // A skip is a healthy outcome, not a failure — another pass already has the machine.
        TickOutcome::Skipped => 0,
        TickOutcome::Ran(decision) => {
            if decision.started > 0 {
                println!("queue tick: started {}", decision.started);
            }
            0
        }
    }
}

/// `bn queue run` — the resident dispatcher.
///
/// The loop the machine actually runs. The shared event loop wakes it for backlog, wake-queue,
/// config, or binary changes, with `--interval` as the bounded idle fallback. While there is work
/// it comes straight back, so a burst drains at the speed the caps allow instead of one item per
/// tick; when the queue is empty it backs off, so an idle machine costs nothing.
///
/// **It re-reads config, and it retires itself in favour of a newer binary — but only one that can
/// actually replace it.** The first two follow from the same fact: a process that lives for weeks
/// accumulates a stale view of a world that keeps moving, and a daemon holding the config you edited
/// an hour ago is worse than one that is down, because it looks like it is working. The
/// qualification is the lesson from the one time this went wrong: handing over is irreversible, so
/// the successor is proved before the incumbent leaves.
///
/// `--interval` sets the idle ceiling. The busy floor is fixed and short — the point of a resident
/// loop is that arriving work does not wait for the next round number on the clock.
pub(crate) fn emit_run(cfg: &Config, interval: u64) {
    use bn_core::clock::Clock;
    let idle = interval.clamp(1, 3600);
    let mut cfg = cfg.clone();
    let mut config_seen = config_stamp();
    let mut exe_seen = exe_stamp();
    let mut watches = vec![
        crate::event_loop::Watch::file(backlog::backlog_path()),
        crate::event_loop::Watch::file(bn_core::resolver::config_file_path()),
    ];
    if let Some(path) = executable_path() {
        watches.push(crate::event_loop::Watch::file(path));
    }
    let mut reactor =
        crate::event_loop::EventLoop::with_wake_queue("bn queue", watches, &[]);
    println!(
        "queue dispatcher running — event-driven, {BUSY_SECS}s busy / {idle}s idle fallback · ctrl-c to stop"
    );
    loop {
        // A new binary means this process is the old one. Exit cleanly and let the supervisor
        // (launchd KeepAlive / systemd Restart=always) start the new image — restarting is a
        // solved problem owned by something that already does it well, and re-execing ourselves
        // would be a worse copy of it.
        //
        // But only once the new image is known to work. **This process is the last dispatcher the
        // machine has**, and handing over is irreversible: if the successor cannot run `queue run`
        // it fails instantly, the supervisor throttles it, and the service is dropped — with
        // nothing left to notice. That is not hypothetical. `self-update` rebuilt bn from a
        // revision predating the queue, and the dispatcher dutifully retired itself in favour of a
        // binary that had no `queue` subcommand at all.
        //
        // So the old image keeps serving until a replacement proves it can take over. Each new
        // binary is probed exactly once — `exe_seen` advances either way — so a machine stuck on a
        // bad build costs one probe per build, not one per pass.
        let current = exe_stamp();
        if current != exe_seen {
            exe_seen = current;
            let successor = crate::cmd::queue_install::dispatcher_bin();
            if crate::cmd::queue_install::can_dispatch(&successor) {
                println!("queue dispatcher: binary changed — exiting for the supervisor to restart");
                return;
            }
            eprintln!(
                "queue dispatcher: {successor} changed but cannot run `bn queue run` — staying on \
                 the running image. The queue keeps draining; reinstall bn from a revision with the \
                 queue to hand over."
            );
        }
        // Config is cheap to stat and only re-parsed when it actually moved.
        let stamp = config_stamp();
        if stamp != config_seen {
            config_seen = stamp;
            cfg = Config::load();
            println!("queue dispatcher: config reloaded");
        }

        cap_own_log();
        beat();

        let now = bn_core::clock::RealClock.now_epoch_secs();
        let mut wait = idle;
        if let TickOutcome::Ran(decision) = tick_once(&cfg, now, false) {
            if decision.started > 0 {
                println!("started {} · {} pending", decision.started, decision.pending);
            }
            // Come back immediately while there is anything left to consider. "Started something"
            // is not the test — an entry held on capacity is also a reason to look again soon,
            // because a lane frees the moment an agent finishes.
            if decision.pending > 0 {
                wait = BUSY_SECS;
            }
        }
        if reactor.wait(std::time::Duration::from_secs(wait))
            == crate::event_loop::LoopEvent::Shutdown
        {
            return;
        }
    }
}

/// The resident dispatcher's heartbeat file, touched once per pass.
///
/// Distinct from `queue-decision.json` on purpose, and the distinction is the whole point: a
/// decision is written by *any* tick, including the inline fallback `bn` runs when it notices the
/// dispatcher is missing. Using that as the liveness signal made the fallback mask the very failure
/// it compensates for — run `bn` often enough and the decision file never goes stale, so the repair
/// that would restart the daemon never fires and the machine quietly runs on inline ticks forever.
///
/// Only [`emit_run`] writes this, so "recent" means the daemon itself is alive.
pub(crate) fn heartbeat_path() -> PathBuf {
    bn_core::resolver::state_dir().join("queue-daemon.heartbeat")
}

/// Record that the resident loop is alive. Best-effort: a dispatcher must not stop dispatching
/// because it could not write a heartbeat.
fn beat() {
    let path = heartbeat_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, bn_core::util::now_epoch_secs().to_string());
}

/// A cheap fingerprint of `config.toml` — modified time and length.
///
/// Two fields rather than mtime alone: an editor that writes-and-restores a timestamp still changes
/// the length, and a same-length edit still moves the clock. `None` when unreadable, which compares
/// equal to itself, so a missing config does not cause a reload every pass.
fn config_stamp() -> Option<(std::time::SystemTime, u64)> {
    let path = bn_core::resolver::config_file_path();
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn executable_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// The same fingerprint for the running binary, resolved through any symlink so replacing the
/// target of `~/.bn/config/bn` counts as a change.
fn exe_stamp() -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(executable_path()?).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Keep the supervisor's capture of our stdout from growing without bound.
///
/// Neither launchd nor systemd rotates the file it redirects a daemon into, and the volume is not
/// really ours: express jobs run *inline* in the tick, so every captured cron's output lands here
/// too — duplicating the 15-line tail `run_native_job` already records against each `cron_run`
/// event. The durable record is the event log; this file is a convenience.
///
/// So it is truncated rather than rotated. Losing yesterday's copy of something already stored
/// twice is not a loss, and a rotation scheme would be more machinery than the thing is worth.
fn cap_own_log() {
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    let path = std::path::Path::new("/tmp").join(format!("{}.log", crate::launchd::QUEUE_LABEL));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        // The supervisor holds this fd in append mode, so truncating underneath it is safe: the
        // next write lands at the new end rather than leaving a sparse hole.
        if std::fs::File::create(&path).is_ok() {
            println!("queue dispatcher: truncated its own log (>4MB; the event log keeps the record)");
        }
    }
}

/// How long the resident loop waits between passes while the queue still has work.
const BUSY_SECS: u64 = 3;

/// `bn queue why` — the last decision, verbatim.
pub(crate) fn emit_why(json: bool) {
    let Some(decision) = QueueDecision::read() else {
        if json {
            println!(
                "{}",
                Envelope::success("queue-why", serde_json::json!({ "decision": null })).to_json()
            );
        } else {
            println!("no tick has run on this machine yet — try `bn queue tick --dry-run`");
        }
        return;
    };
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-why",
                serde_json::to_value(&decision).unwrap_or_default()
            )
            .to_json()
        );
        return;
    }
    print_decision(&decision);
}

/// The result of attempting a pass.
enum TickOutcome {
    /// Another tick held the lock. A clean no-op, not an error — the other one is doing the work.
    Skipped,
    Ran(Box<QueueDecision>),
}

/// One pass, under the machine lock.
fn tick_once(cfg: &Config, now: i64, dry_run: bool) -> TickOutcome {
    let _guard = match TickLock::try_acquire() {
        Ok(g) => g,
        Err(denied) => {
            if let LockDenied::Unusable(e) = &denied {
                eprintln!("bn queue tick: cannot take the machine lock — {e}");
            }
            return TickOutcome::Skipped;
        }
    };

    // Refresh the node registry first (the one thing that needs SSH and so cannot live in the pure
    // core), then take the shared snapshot every queue surface reads.
    let _ = observe_nodes(cfg, now);
    reclaim_stale_leases(cfg, now);
    // The same reclaim, one level down: a backlog lease points at an executor, a pool lease points
    // at a warm checkout, and both outlive whatever they were tracking. Pool slots are the capacity
    // `realize` spends further down this pass, so the ledger has to be honest *here* — a stale one
    // reads as "pool full" and every dispatch below spills into a cold worktree.
    crate::wt_pool::reclaim::reconcile_pools(cfg, now);
    if !dry_run {
        maybe_enqueue_spare_work(cfg, now);
    }
    let snapshot = bn_core::queue::snapshot::Snapshot::capture(cfg, now);
    let inputs = snapshot.inputs();
    let plan = plan_tick(&inputs);
    let decision = QueueDecision::build(&inputs, &plan, dry_run);
    let _ = decision.write();

    if !dry_run {
        log_pass(&decision, now);
        // Declare the dispatcher's extent. Inside it no producer captures, so `realize` may call
        // ordinary entry points — `run_job`, `bn dispatch` — without re-queueing what it just
        // dequeued. See `queue_producers::Dispatching`.
        let _dispatching = crate::cmd::queue_producers::Dispatching::begin();
        realize(cfg, &plan, now);
    }
    TickOutcome::Ran(Box::new(decision))
}

/// Queue autonomous maintenance only when it has real work to do.
///
/// The queue already protects the machine once the item exists: `pool-warm` is classified as heavy
/// and priority -50, so it only runs after person-facing and routine work and it consumes a lane
/// slot rather than hiding inside the tick. This hook supplies the missing piece: it is no longer
/// purely calendar-driven. Every dispatcher pass may top up background work, but only when the pool
/// planner says a stale warm pool exists.
fn maybe_enqueue_spare_work(cfg: &Config, now: i64) {
    if !cfg.job_allowed_here("pool-warm") {
        return;
    }
    if !crate::wt_pool::pool_refresh_due(cfg, now) {
        return;
    }
    let repo = cfg.queue_home_repo();
    if bn_core::backlog::load(&bn_core::backlog::backlog_path()).is_active(
        &repo,
        &bn_core::orchestrate::derive_id("run pool-warm"),
    ) {
        return;
    }
    let _ = crate::cmd::queue_producers::capture_job(
        cfg,
        "pool-warm",
        bn_core::queue::kind::ExecSpec::Job {
            job: "pool-warm".to_string(),
            repo: None,
            branch: None,
        },
        bn_core::queue::kind::Requirements::default(),
        None,
    );
}

/// Record a dispatcher pass in the event log — but only a pass worth remembering.
///
/// `queue-decision.json` holds one pass: the last. That answers "why is nothing running *now*" and
/// nothing at all about "what did the dispatcher do while I was at lunch" — the question you
/// actually have when work you expected did not run.
///
/// Deliberately not every pass. The dispatcher is a resident loop, so an event per pass would be
/// thousands of identical `nothing to do` lines a day, burying the handful that matter under their
/// own volume — a log nobody can read is not observability. A pass is recorded when it:
///
///   * **acted** — started something, so the log carries every dispatch this machine made.
///
/// Held, skipped and empty passes stay in `queue-decision.json`, not the event log. The decision
/// file answers "why not now"; history is for work that actually moved.
fn log_pass(decision: &QueueDecision, now: i64) {
    if decision.started == 0 {
        return;
    }
    let mut f = serde_json::Map::new();
    f.insert(
        "outcome".to_string(),
        serde_json::Value::String(decision.outcome.clone()),
    );
    f.insert("started".to_string(), decision.started.into());
    f.insert("pending".to_string(), decision.pending.into());
    f.insert("deferred".to_string(), decision.deferred.len().into());
    if let Some(bind) = &decision.bind {
        f.insert("bind".to_string(), serde_json::Value::String(bind.clone()));
    }
    if let Some(reason) = &decision.reason {
        f.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.clone()),
        );
    }
    let _ = bn_core::events::append_at("queue_tick", now, f);
}

/// Return leases whose executor died to `pending`.
///
/// Runs at the head of every pass, before anything is planned, so a reclaimed entry is a candidate
/// in the same tick that noticed it. Without this a killed tick loses its entry permanently — not
/// pending so never retried, not terminal so never reported.
fn reclaim_stale_leases(cfg: &Config, now: i64) {
    let path = backlog::backlog_path();
    let bl = backlog::load(&path);
    let entries = bl.entries().to_vec();
    let live: Vec<(String, String)> = bn_core::agent::supervise(cfg, 0, 0)
        .agents
        .iter()
        .map(|a| (a.repo.clone(), a.branch.clone()))
        .collect();
    let stale = bn_core::queue::reap::stale(&entries, &live, now, |w| cfg.queue.lease_ttl(w));
    if stale.is_empty() {
        return;
    }
    // Grouped by repo because the release API is per-repo, and reported because a reclaim means
    // something died — silence here would hide a machine that keeps losing its executors.
    let mut by_repo: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for e in stale {
        by_repo.entry(e.repo.clone()).or_default().push(e.id.clone());
    }
    for (repo, ids) in by_repo {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        match backlog::release_many(&path, &repo, &refs, now) {
            Ok(n) if n > 0 => {
                eprintln!("bn queue tick: reclaimed {n} stale lease(s) in {repo} — their executor died");
                let mut f = serde_json::Map::new();
                f.insert("repo".to_string(), serde_json::Value::String(repo));
                f.insert("count".to_string(), serde_json::Value::from(n));
                let _ = bn_core::events::append_at("queue_lease_reclaimed", now, f);
            }
            _ => {}
        }
    }
}

/// Realize the plan: claim each entry, run it, and reconcile the outcome.
///
/// The lease is taken **before** the launch and released if the launch fails, so a crash between
/// the two leaves the entry leased rather than silently double-startable — and a launch that never
/// happened leaves it visibly pending rather than swallowed.
fn realize(cfg: &Config, plan: &TickPlan, now: i64) {
    let path = backlog::backlog_path();
    // Work whose moment has passed is resolved, not retried. Left pending it would sit there
    // getting later, and eventually run at an hour where it does more harm than good.
    for e in &plan.expired {
        let outcome = [(e.id.as_str(), bn_core::backlog::BacklogStatus::Failed)];
        if backlog::resolve_many(&path, &e.repo, &outcome, now).is_ok() {
            println!(
                "queue: dropped {}/{} — {} (its window passed)",
                e.repo, e.id, e.detail
            );
            let mut f = serde_json::Map::new();
            f.insert("repo".to_string(), serde_json::Value::String(e.repo.clone()));
            f.insert("id".to_string(), serde_json::Value::String(e.id.clone()));
            f.insert(
                "work_kind".to_string(),
                serde_json::Value::String(e.kind.label().to_string()),
            );
            f.insert(
                "lane".to_string(),
                serde_json::Value::String(
                    bn_core::queue::kind::WorkClass::from_kind(e.kind)
                        .label()
                        .to_string(),
                ),
            );
            f.insert(
                "reason".to_string(),
                serde_json::Value::String("execution-window-passed".to_string()),
            );
            if !e.detail.is_empty() {
                f.insert(
                    "detail".to_string(),
                    serde_json::Value::String(e.detail.clone()),
                );
            }
            let _ = bn_core::events::append_at("queue_expired", now, f);
        }
    }
    for d in &plan.start {
        let ids = [d.id.clone()];
        if let Err(e) = backlog::lease_selected(&path, &d.repo, &ids, now) {
            eprintln!("bn queue tick: could not lease {}/{}: {e}", d.repo, d.id);
            continue;
        }
        // Record WHICH machine holds it. `status: leased` only says someone took it; without the
        // holder, a crashed box's leases cannot be told apart from a healthy one's.
        let _ = backlog::assign(&path, &d.repo, &d.id, cfg.machine(), now);
        let _ = bn_core::events::append_at(
            "queue_dispatch",
            now,
            dispatch_fields(d, cfg.machine()),
        );
        let outcome = execute(cfg, d);
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        match outcome {
            // An agent launch hands off to a long-running harness; the entry stays leased until the
            // run reconciles its terminal outcome, exactly as an orchestrate-dispatched item does.
            Executed::Launched => {}
            // A headless job is over the moment it returns, so resolve it here rather than leaving
            // it leased forever waiting for a reconciler that has nothing to observe.
            Executed::Finished { code } => {
                let finished_at = bn_core::util::now_epoch_secs();
                let status = if code == 0 {
                    bn_core::backlog::BacklogStatus::Done
                } else {
                    bn_core::backlog::BacklogStatus::Failed
                };
                let _ =
                    backlog::resolve_many(&path, &d.repo, &[(d.id.as_str(), status)], finished_at);
                // Every queued execution gets an ending in the log, not just heavy work's callback.
                // Without this a failed job is a `queue_dispatch` with no sequel — the log shows it
                // started and never says it did not work.
                let _ = bn_core::events::append_at(
                    "queue_finish",
                    finished_at,
                    finish_fields(&d.repo, &d.id, code),
                );
            }
            Executed::Failed(err) => {
                eprintln!("bn queue tick: {}/{} did not start: {err}", d.repo, d.id);
                let finished_at = bn_core::util::now_epoch_secs();
                let _ = backlog::release_many(&path, &d.repo, &refs, finished_at);
                let _ = bn_core::events::append_at(
                    "queue_finish",
                    finished_at,
                    launch_failure_fields(&d.repo, &d.id, &err),
                );
            }
        }
    }
}

/// What happened when the tick tried to run a dispatch.
enum Executed {
    /// A long-running agent was started; its terminal outcome is reconciled elsewhere.
    Launched,
    /// A headless job ran to completion here, with this exit status.
    Finished { code: i32 },
    /// Nothing started — the entry goes back to pending.
    Failed(String),
}

/// Run one planned dispatch according to its executor.
fn execute(cfg: &Config, d: &Dispatch) -> Executed {
    match &d.exec {
        // Heavy work is spawned, never run inline — a minutes-long build inside the tick would
        // hold the dispatcher for minutes and make every pass behind it skip on the lock.
        bn_core::queue::kind::ExecSpec::Job { job, repo, branch } if d.heavy => {
            spawn_heavy(d, &job_command(job, repo.as_deref(), branch.as_deref()))
        }
        bn_core::queue::kind::ExecSpec::Shell { cmd } if d.heavy => spawn_heavy(d, cmd),
        // The ordinary entry point, deliberately: `Dispatching` makes capture inert for this
        // extent, so `run_job` runs the job instead of re-queueing it. Special-casing the
        // implementation here was the old per-call-site fix, and it is exactly what kept getting
        // forgotten.
        bn_core::queue::kind::ExecSpec::Job { job, repo, branch } => {
            let ctx = if repo.is_none() && branch.is_none() {
                None
            } else {
                target_ctx(cfg, repo.clone(), branch.clone())
            };
            Executed::Finished {
                code: crate::cmd::cron::run_job(cfg, ctx.as_ref(), job),
            }
        }
        bn_core::queue::kind::ExecSpec::Shell { cmd } => match run_shell(cmd) {
            Ok(code) => Executed::Finished { code },
            Err(e) => Executed::Failed(e),
        },
        bn_core::queue::kind::ExecSpec::Agent => launch_agent(d, None),
        bn_core::queue::kind::ExecSpec::Skill { skill, arg } => {
            launch_agent(d, Some((skill.as_str(), arg.as_deref())))
        }
    }
}

/// Launch a worktree agent by re-entering `bn dispatch` — the one launcher, rather than a second
/// copy of the worktree/lease/tmux sequence that would drift from it.
fn launch_agent(d: &Dispatch, skill: Option<(&str, Option<&str>)>) -> Executed {
    let item = match skill.and_then(|(_, arg)| arg) {
        Some(arg) => arg.to_string(),
        None => d.item.clone(),
    };
    let harness = d.harness.clone().unwrap_or_else(|| "claude".to_string());
    let mut inv = crate::cmd::dispatch::DispatchInvocation::new(&item, &d.repo, &harness);
    if let Some((name, _)) = skill {
        inv = inv.skill(Some(name));
    }
    if matches!(d.kind, BacklogKind::Scout | BacklogKind::Research) {
        inv = inv.scout(true);
    }
    let Ok(exe) = std::env::current_exe() else {
        return Executed::Failed("cannot resolve the bn executable".to_string());
    };
    // No special flag needed: the child inherits the dispatch extent through the environment, so
    // its own capture check is inert. `BN_DISPATCH_NOW` remains, but as *your* escape hatch rather
    // than the dispatcher's workaround.
    match std::process::Command::new(exe).args(inv.args()).status() {
        Ok(st) if st.success() => Executed::Launched,
        Ok(st) => Executed::Failed(format!("bn dispatch exited {}", st.code().unwrap_or(-1))),
        Err(e) => Executed::Failed(e.to_string()),
    }
}

/// Start heavy work detached, with a wrapper that reports its own completion.
///
/// The tick cannot wait for it — that is the whole reason this shape exists — so the entry stays
/// leased and something has to close it. The same trick the worker path already uses: wrap the
/// command so that whatever its exit status, it calls back to resolve the entry. If the wrapper
/// never runs (machine died mid-build) the lease reaper picks it up on TTL, so the entry is
/// recoverable either way rather than leased forever.
fn spawn_heavy(d: &Dispatch, cmd: &str) -> Executed {
    let wrapped = completion_wrapped(cmd, &d.repo, &d.id);
    match std::process::Command::new("/bin/sh")
        .arg("-lc")
        .arg(&wrapped)
        .env("PATH", crate::cmd::cron::job_path())
        .stdin(std::process::Stdio::null())
        // Detached output: heavy work is minutes of build noise, and the dispatcher's own log is
        // not where it belongs. `bn queue why` and the event log carry the outcome.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => Executed::Launched,
        Err(e) => Executed::Failed(e.to_string()),
    }
}

/// Wrap `cmd` so it resolves its own queue entry on exit, whatever that exit is.
fn completion_wrapped(cmd: &str, repo: &str, id: &str) -> String {
    let script = r#"sh -lc "$1"
code=$?
"$4" queue finish "$2" "$3" --exit-code "$code"
exit "$code""#;
    ["sh", "-c", script, "bn-queue-heavy", cmd, repo, id]
        .into_iter()
        .map(shq)
        .chain(std::iter::once(shq(&exe_str())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The absolute path of the running bn, for a wrapper that will outlive this process.
fn exe_str() -> String {
    std::env::current_exe()
        .unwrap_or_else(|_| "bn".into())
        .to_string_lossy()
        .into_owned()
}

fn job_command(job: &str, repo: Option<&str>, branch: Option<&str>) -> String {
    let mut command = format!("{} run {}", shq(&exe_str()), shq(job));
    if let Some(repo) = repo {
        command.push_str(&format!(" --repo {}", shq(repo)));
    }
    if let Some(branch) = branch {
        command.push_str(&format!(" --branch {}", shq(branch)));
    }
    command
}

/// Single-quote for `sh`.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `bn queue finish` — the callback heavy work makes when it is done.
pub(crate) fn emit_finish(repo: &str, id: &str, exit_code: i32, json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let status = if exit_code == 0 {
        bn_core::backlog::BacklogStatus::Done
    } else {
        bn_core::backlog::BacklogStatus::Failed
    };
    let path = backlog::backlog_path();
    let resolved = backlog::resolve_many(&path, repo, &[(id, status)], now).unwrap_or(0);
    let _ = bn_core::events::append_at("queue_finish", now, finish_fields(repo, id, exit_code));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-finish",
                serde_json::json!({ "repo": repo, "id": id, "resolved": resolved,
                                    "status": status.label() })
            )
            .to_json()
        );
    } else if resolved > 0 {
        println!("{repo}/{id} → {}", status.label());
    }
}

/// Run a headless shell command — a `.md` cron agent's `run:` contract, verbatim.
fn run_shell(cmd: &str) -> Result<i32, String> {
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .env("PATH", crate::cmd::cron::job_path())
        .status()
        // The real code, not just success/failure: `exit 3` should read as `exit 3` in the log.
        // A signal death has no code; 1 is the conventional stand-in and is still non-zero.
        .map(|st| st.code().unwrap_or(1))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// observation
// ---------------------------------------------------------------------------------------------

/// Refresh and read the machine-scoped node registry.
///
/// Polls each declared SSH node's `bn worker status`, folds the answer in through
/// `hosts::apply_snapshot` (which stamps the heartbeat and carries the node's advertised skills
/// across), persists the result, and returns it with staleness applied.
///
/// The registry has to be machine-scoped: the pre-existing one lived inside a per-repo orchestrate
/// run, so a box with no run up had no live registry at all and every declared node sat at
/// `Unknown` forever — unplaceable no matter how healthy it was.
pub(crate) fn observe_nodes(cfg: &Config, now: i64) -> Vec<bn_core::hosts::NodeRecord> {
    let base = std::env::current_dir().unwrap_or_default();
    let labels = bn_core::worker::advertise::advertised_labels(cfg, &base);
    let worktree_root = PathBuf::from(&cfg.worktree_dir);

    let mut registry = bn_core::queue::nodes::NodeRegistry::load();
    registry.upsert(bn_core::hosts::local_record(
        cfg,
        labels,
        &worktree_root,
        now,
    ));
    let declared: Vec<String> = cfg.nodes.iter().map(|n| n.node_id.clone()).collect();
    for node in &cfg.nodes {
        if node.node_id.eq_ignore_ascii_case(cfg.machine()) {
            continue;
        }
        // Start from what the operator declared, so `owned_repos`/`slots`/extra labels survive a
        // node that is currently unreachable, then overlay whatever it reports about itself.
        let mut record = registry
            .nodes
            .iter()
            .find(|n| n.node_id.eq_ignore_ascii_case(&node.node_id))
            .cloned()
            .unwrap_or_else(|| node.clone());
        record.slots = node.slots.max(1);
        record.owned_repos = node.owned_repos.clone();
        let status = match &node.target {
            bn_core::backlog::ExecutionTarget::SshHost(alias) => {
                crate::connect::worker_status(cfg, alias)
            }
            _ => None,
        };
        bn_core::hosts::apply_snapshot(&mut record, status.as_ref(), now);
        registry.upsert(record);
    }
    registry.retain_declared(&declared, cfg.machine());
    registry.refreshed_at = now;
    let _ = registry.save();
    registry.live(now, cfg.queue.heartbeat_ttl())
}

// ---------------------------------------------------------------------------------------------
// the lock
// ---------------------------------------------------------------------------------------------

/// A non-blocking machine-wide tick lock, released on drop.
///
/// Non-blocking on purpose: a tick that waited for its predecessor would pile up behind a slow
/// pass and then fire a burst, which is precisely the over-commitment the queue exists to prevent.
/// Skipping is correct — another pass is already deciding, and there will be one along shortly.
struct TickLock {
    path: PathBuf,
}

impl TickLock {
    fn try_acquire() -> Result<TickLock, LockDenied> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A lock whose owner is gone is not a lock. `Drop` does not run on SIGKILL or a power cut,
        // so the file outlives the process holding it; without a pid the only way to notice is a
        // timeout, during which every pass skips and nothing dispatches. Recording the owner turns
        // that into a fact we can check.
        if let Ok(prev) = std::fs::read_to_string(&path) {
            let owner: Option<u32> = prev.trim().parse().ok();
            let dead = owner.is_none_or(|pid| !pid_alive(pid));
            if dead || bn_core::store::lock_is_stale(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = write!(f, "{}", std::process::id());
                Ok(TickLock { path })
            }
            // Only `AlreadyExists` means another tick holds it. Anything else — a read-only state
            // dir, a full disk — is a different problem entirely, and reporting it as contention
            // sends the reader hunting for a concurrent tick that does not exist.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(LockDenied::Held),
            Err(e) => Err(LockDenied::Unusable(e.to_string())),
        }
    }
}

/// Why a pass could not take the machine lock.
enum LockDenied {
    /// Another tick is running. The healthy case: it is doing the work.
    Held,
    /// The lock could not be created at all — an unwritable state dir, a full disk.
    Unusable(String),
}

/// Whether `pid` names a live process — [`bn_core::queue::build::pid_alive`], re-exported so the
/// tick, the status line and the dashboard cannot disagree about what "alive" means.
pub(crate) use bn_core::queue::build::pid_alive;

impl Drop for TickLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------------------------

fn print_decision(d: &QueueDecision) {
    let dry = if d.dry_run { " (dry run)" } else { "" };
    match d.outcome.as_str() {
        "started" => println!("\u{f0e7} started {}{dry}", d.started),
        "empty" => println!("nothing pending{dry}"),
        _ => println!("\u{f017} started nothing — {} pending{dry}", d.pending),
    }
    if let Some(bind) = &d.bind {
        let why = d.reason.as_deref().unwrap_or("");
        println!("  bind: {bind}{}", if why.is_empty() { String::new() } else { format!(" — {why}") });
    }
    for row in &d.dispatched {
        println!(
            "  → {}/{} · {} · {} on {}{}",
            row.repo,
            row.id,
            row.kind,
            row.exec,
            row.target,
            row.harness
                .as_ref()
                .map(|h| format!(" ({h})"))
                .unwrap_or_default()
        );
    }
    if !d.deferred.is_empty() {
        println!("  held {}:", d.deferred.len());
        for row in d.deferred.iter().take(8) {
            let detail = if row.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", row.detail)
            };
            println!("    {}/{} · {}{detail}", row.repo, row.id, row.reason);
        }
        if d.deferred.len() > 8 {
            println!("    …and {} more", d.deferred.len() - 8);
        }
    }
    for lane in &d.lanes {
        println!(
            "  lane {:<9} {}/{} in flight · {} pending{}",
            lane.lane,
            lane.in_flight,
            lane.cap,
            lane.pending,
            if lane.saturated { " (full)" } else { "" }
        );
    }
}

/// `bn queue nodes` — the node × capability matrix.
///
/// The direct answer to a `no-capable-node` deferral. A queue that can only say "nothing could run
/// this" is a queue you have to guess at; this says which boxes exist, which are actually alive,
/// and exactly what each one serves.
pub(crate) fn emit_nodes(cfg: &Config, skill: Option<&str>, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let ttl = cfg.queue.heartbeat_ttl();
    let nodes = observe_nodes(cfg, now);
    let rows: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| {
            let axes = bn_core::worker::advertise::split_labels(&n.labels);
            serde_json::json!({
                "node_id": n.node_id,
                "target": n.target.label(),
                "status": format!("{:?}", n.status).to_lowercase(),
                "slots": n.slots.max(1),
                "busy": n.active_attempts,
                "free": n.slots.max(1).saturating_sub(n.active_attempts),
                "stale": bn_core::hosts::is_stale(n, now, ttl),
                "heartbeat_age_secs": (n.last_heartbeat > 0)
                    .then(|| now.saturating_sub(n.last_heartbeat)),
                "skills": axes.skills,
                "harnesses": axes.harnesses,
                "mcps": axes.mcps,
                "labels": axes.other,
            })
        })
        .filter(|row| match skill {
            None => true,
            Some(want) => row["skills"]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(want))),
        })
        .collect();

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-nodes",
                serde_json::json!({ "count": rows.len(), "nodes": rows })
            )
            .to_json()
        );
        return;
    }
    if rows.is_empty() {
        match skill {
            Some(want) => println!("no node serves skill '{want}'"),
            None => println!("no nodes"),
        }
        return;
    }
    for row in &rows {
        let stale = if row["stale"].as_bool().unwrap_or(false) {
            "  (heartbeat lapsed)"
        } else {
            ""
        };
        println!(
            "{:<18} {:<9} {:<14} {}/{} free{stale}",
            row["node_id"].as_str().unwrap_or(""),
            row["status"].as_str().unwrap_or(""),
            row["target"].as_str().unwrap_or(""),
            row["free"].as_u64().unwrap_or(0),
            row["slots"].as_u64().unwrap_or(0),
        );
        for (label, key) in [
            ("harness", "harnesses"),
            ("mcp", "mcps"),
            ("skills", "skills"),
        ] {
            let items: Vec<&str> = row[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if items.is_empty() {
                continue;
            }
            println!("  {label:<8} {}", items.join(", "));
        }
    }
}
