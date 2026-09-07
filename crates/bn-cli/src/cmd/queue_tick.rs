//! `bn queue tick` — one dispatcher pass, and the loop that repeats it.
//!
//! The IO half of the queue dispatcher: sample the machine, call the pure
//! [`bn_core::queue::tick::plan_tick`], persist the decision, and realize the intent. All the
//! *policy* lives in bn-core; what happens here is reading the world and acting on the answer.
//!
//! Two entry points, one code path: `bn queue run` (the resident dispatcher bn supervises) and
//! `bn queue tick` (a single pass, by hand or from the inline fallback). They interlock on a
//! non-blocking lock file, so whichever wins the race runs and the other cleanly skips — one pass
//! per machine, no matter how it was started.
mod direct;
mod event_fields;
mod heavy;
mod launch;
mod lock;
mod progress;
mod render;

use crate::*;
use bn_core::backlog::{self, BacklogKind, LaunchFailure};
use bn_core::queue::decision::QueueDecision;
use bn_core::queue::tick::{plan_tick, Dispatch, TickPlan};
use crate::cmd::queue_daemon::{Lifecycle, Supervision};
use event_fields::{dispatch_fields, finish_fields, launch_failure_fields};
use launch::{express_timeout, launch_timeout, wait_bounded, Launch};
use lock::LockDenied;
use progress::stamp_pass;
use render::print_decision;
use std::path::PathBuf;

pub(crate) use direct::{dispatch_now, hold_tick_lock, DirectStart};
pub(crate) use lock::pid_alive;
pub(crate) use lock::TickLock;
pub(crate) use render::emit_nodes;

/// `bn queue tick` — one pass.
pub(crate) fn emit_tick(cfg: &Config, dry_run: bool, json: bool) {
    if !cfg.queue_enabled_here() {
        fail_json(
            json,
            "queue-tick",
            "not_enabled",
            &format!(
                "bn queue tick: not enabled on this machine ('{}')",
                cfg.machine()
            ),
        );
    }
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
                    Envelope::success(
                        "queue-tick",
                        serde_json::to_value(&decision).unwrap_or_default()
                    )
                    .to_json()
                );
            } else {
                print_decision(&decision);
            }
        }
    }
}

/// One pass, as a named job — the inline fallback's entry point.
///
/// It was a once-a-minute cron, back when the queue leaned on the init system for both its drivers.
/// bn runs a resident dispatcher of its own now, so this is what remains: the degraded path
/// [`crate::cmd::queue_install`] takes on a machine where a dispatcher cannot be started, and a
/// hand-runnable single pass. The machine lock keeps it from overlapping a resident loop.
pub(crate) fn job_queue_tick(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !cfg.queue_enabled_here() {
        eprintln!(
            "queue tick: not enabled on this machine ('{}')",
            cfg.machine()
        );
        return 0;
    }
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
/// **It holds the machine's dispatcher lock for its whole life.** That is what makes "one
/// dispatcher per machine" true without anyone coordinating: a second `bn queue run` is refused
/// rather than admitted alongside the first, and the lock is released by the kernel on any exit at
/// all, so the next `bn` invocation starts a replacement. Nothing has to notice this process died.
///
/// **It re-reads config, and it hands over to a newer binary — but only one that can actually
/// replace it.** The first two follow from the same fact: a process that lives for weeks
/// accumulates a stale view of a world that keeps moving, and a daemon holding the config you edited
/// an hour ago is worse than one that is down, because it looks like it is working. The
/// qualification is the lesson from the one time this went wrong: handing over is irreversible, so
/// the successor is proved before the incumbent leaves.
///
/// `--interval` sets the idle ceiling. The busy floor is fixed and short — the point of a resident
/// loop is that arriving work does not wait for the next round number on the clock.
pub(crate) fn emit_run(cfg: &Config, interval: u64) {
    if !cfg.queue_enabled_here() {
        fail(&format!(
            "bn queue run: not enabled on this machine ('{}')",
            cfg.machine()
        ));
    }
    use bn_core::clock::Clock;
    let idle = interval.clamp(1, 3600);
    // Taken before anything else and held for the whole run: this *is* the machine's statement that
    // it has a dispatcher, and every producer's `capture_active` reads it. Acquiring it first also
    // settles the start-up race — two `bn` invocations may both spawn a dispatcher, and the loser
    // finds out here rather than by running a second loop.
    let _lock = match bn_core::queue::daemon::DaemonLock::acquire() {
        Ok(lock) => lock,
        Err(bn_core::queue::daemon::Denied::Running) => {
            println!("queue dispatcher: one is already running on this machine — nothing to do");
            return;
        }
        Err(bn_core::queue::daemon::Denied::Unusable(why)) => fail(&format!(
            "bn queue run: cannot take the dispatcher lock ({}) — {why}",
            bn_core::queue::daemon::lock_path().display()
        )),
    };
    let mut cfg = cfg.clone();
    let mut supervision = Supervision::start();
    let mut watches = vec![
        crate::event_loop::Watch::file(backlog::backlog_path()),
        crate::event_loop::Watch::file(bn_core::resolver::config_file_path()),
    ];
    if let Some(path) = crate::cmd::queue_daemon::executable_path() {
        watches.push(crate::event_loop::Watch::file(path));
    }
    let mut reactor = crate::event_loop::EventLoop::with_wake_queue("bn queue", watches, &[]);
    // Consecutive passes that could not take the machine lock. Reset by any pass that decides.
    let mut skipped_passes: u32 = 0;
    println!(
        "queue dispatcher running — event-driven, {BUSY_SECS}s busy / {idle}s idle fallback · ctrl-c to stop"
    );
    loop {
        // Two facts about the world this process was started in can change under it: the binary it
        // runs and the config it read. Both are handled by `queue_daemon::Supervision`, because
        // both are questions about the dispatcher *process*, not about a pass.
        match supervision.check(&mut cfg) {
            Lifecycle::Continue => {}
            Lifecycle::Retire => return,
        }
        supervision.cap_own_log();

        let now = bn_core::clock::RealClock.now_epoch_secs();
        let mut wait = idle;
        let outcome = tick_once(&cfg, now, false);
        if let TickOutcome::Ran(decision) = outcome {
            // Stamped only for a pass that actually decided, so the age measures *progress*. A
            // skipped pass must not count, however healthy the skip looks: this stamp is the one
            // signal that separates a wedged dispatcher from a working one, and a loop that stamps
            // it while contending forever reports perfect health while dispatching nothing. That is
            // not hypothetical — it is exactly how an orphaned lock stayed invisible for hours.
            stamp_pass();
            skipped_passes = 0;
            if decision.started > 0 {
                println!(
                    "started {} · {} pending",
                    decision.started, decision.pending
                );
            }
            // Come back immediately while there is anything left to consider. "Started something"
            // is not the test — an entry held on capacity is also a reason to look again soon,
            // because a lane frees the moment an agent finishes.
            if decision.pending > 0 {
                wait = BUSY_SECS;
            }
        } else {
            // Contention is normal for a pass or two; a loop that never gets in is not contention,
            // it is a machine that has stopped dispatching. Say so on the way past the threshold —
            // once, not every pass, so the log stays readable while the condition persists.
            skipped_passes += 1;
            if skipped_passes == SKIPS_BEFORE_WARNING {
                eprintln!(
                    "queue dispatcher: {skipped_passes} consecutive passes could not take the \
                     machine lock ({}) — nothing is being dispatched. If no other tick is running, \
                     the lock is orphaned; remove the file to recover.",
                    lock::lock_path().display()
                );
            }
        }
        if reactor.wait(std::time::Duration::from_secs(wait))
            == crate::event_loop::LoopEvent::Shutdown
        {
            return;
        }
    }
}

/// How long the resident loop waits between passes while the queue still has work.
const BUSY_SECS: u64 = 3;

/// Consecutive lock-denied passes before the loop says so. High enough that ordinary contention
/// with a hand-run `bn queue tick` stays quiet, low enough that a stuck machine is named in minutes
/// rather than hours.
const SKIPS_BEFORE_WARNING: u32 = 20;

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
        reconcile_producers(cfg);
    }
    let snapshot = bn_core::queue::snapshot::Snapshot::capture(cfg, now);
    let inputs = snapshot.inputs();
    let plan = plan_tick(&inputs);
    // Read before the write clobbers it: whether this pass is worth logging depends on what the
    // last one said.
    let previous = QueueDecision::read();
    let decision = QueueDecision::build(&inputs, &plan, dry_run);
    let _ = decision.write();

    if !dry_run {
        log_pass(&decision, previous.as_ref(), now);
        // Declare the dispatcher's extent. Inside it no producer captures, so `realize` may call
        // ordinary entry points — `run_job`, `bn dispatch` — without re-queueing what it just
        // dequeued. See `queue_producers::Dispatching`.
        let _dispatching = crate::cmd::queue_producers::Dispatching::begin();
        realize(cfg, &plan, now);
        compact_backlog();
    }
    TickOutcome::Ran(Box::new(decision))
}

/// Collapse the backlog's superseded lines, once this pass has finished writing to it.
///
/// `backlog::compact_if_needed` has always documented itself as "the daemon calls this once per
/// tick"; the daemon never did. Its only caller was `bn orchestrate`'s reconcile, which runs when
/// the queue dispatches — so the moment the queue stopped dispatching, the store stopped being
/// compacted, and every deferral the stall produced was appended to a file nothing would ever
/// collapse. The machine this was written on reached 153,779 raw lines for 130 live entries: a
/// 100 MB append-only log re-read and folded on every pass. The stall was its own amplifier.
///
/// Here it is bounded by the tick instead, so recovery does not depend on the very dispatching
/// that a wedged queue is not doing. Cheap when there is nothing to do — `should_compact` requires
/// the file to have grown past a floor *and* superseded lines to outnumber live entries 2:1.
fn compact_backlog() {
    let path = bn_core::backlog::backlog_path();
    if let Err(e) = bn_core::backlog::compact_if_needed(&path) {
        eprintln!("bn queue tick: could not compact the backlog — {e}");
    }
}

/// Queue mandatory pool maintenance only when it has real work to do.
///
/// Every dispatcher pass may enqueue one deduplicated sweep when the planner finds a missed daily
/// deadline or a pool at the 24-hour backstop.
/// Re-run the producers whose job is to notice that something *became* due.
///
/// Both are sweeps rather than callbacks, and both belong on the tick rather than on a schedule of
/// their own. `bn today sync` ran only from the 08:30 cron, so an enhance pass that resolved a task's
/// repo at 09:15 sat unqueued until the next morning — the work was ready, the machine was idle, and
/// nothing was looking. The pipeline sweep has the same shape: a stage finishes whenever it finishes,
/// and its successor is due immediately.
///
/// Cheap enough for every pass: each reads the task store and the backlog and enqueues only what is
/// missing, so a quiet machine pays two file reads a minute. Deliberately *before* the snapshot is
/// captured, so work either sweep enqueues is admitted in this same pass rather than the next one.
fn reconcile_producers(cfg: &Config) {
    let _ = crate::cmd::pipeline_sweep::sweep_tasks(cfg);
}

fn maybe_enqueue_spare_work(cfg: &Config, now: i64) {
    let _ = crate::wt_pool_refresh::enqueue_due_warms(cfg, now);
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
fn log_pass(decision: &QueueDecision, previous: Option<&QueueDecision>, now: i64) {
    if decision.started == 0 && !stall_changed(decision, previous) {
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

/// Whether a pass that started nothing is saying something new.
///
/// `queue-decision.json` records every pass, including the silent ones — that is what `bn queue
/// why` reads, and its module doc is explicit that "a dispatcher that only records its successes
/// makes its silences unexplainable". The event *log* did exactly that: it dropped every
/// zero-start pass, so a stall left no history at all. Five days of this machine starting nothing
/// appear in the log only as the scheduled work expiring around it — the cause was never written
/// down, and `queue::log::runs` says in as many words that held passes are supposed to survive
/// precisely so "the evidence for why nothing moved" is not erased.
///
/// Logging every silent pass is not the answer either: the resident loop ticks every three
/// seconds, so a quiet weekend would be a hundred thousand identical events in a file nothing
/// rotates. What is worth recording is the *transition* — entering a stall, and any change in why
/// — with the clearing already covered, since a pass that starts something logs on its own.
fn stall_changed(decision: &QueueDecision, previous: Option<&QueueDecision>) -> bool {
    match previous {
        // Nothing to compare against: this is the first thing the log will know about the stall.
        None => true,
        Some(prev) => {
            prev.started > 0
                || prev.outcome != decision.outcome
                || prev.reason != decision.reason
                || prev.bind != decision.bind
        }
    }
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
    // Only agents still running keep a lease alive. A session whose transcript is merely still
    // readable is exactly the executor this pass exists to notice has died.
    let live: Vec<(String, String)> = bn_core::agent::supervise(cfg, 0, 0)
        .running()
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
        by_repo
            .entry(e.repo.clone())
            .or_default()
            .push(e.id.clone());
    }
    for (repo, ids) in by_repo {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        match backlog::release_many(&path, &repo, &refs, now) {
            Ok(n) if n > 0 => {
                eprintln!(
                    "bn queue tick: reclaimed {n} stale lease(s) in {repo} — their executor died"
                );
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
            f.insert(
                "repo".to_string(),
                serde_json::Value::String(e.repo.clone()),
            );
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
        let _ =
            bn_core::events::append_at("queue_dispatch", now, dispatch_fields(d, cfg.machine()));
        let outcome = execute(cfg, d, now);
        match outcome {
            // An agent launch hands off to a long-running harness; the entry stays leased until the
            // run reconciles its terminal outcome, exactly as an orchestrate-dispatched item does.
            //
            // Clearing the failure count here is what keeps it *consecutive*. A lease reclaimed
            // after a healthy launch returns the entry to pending, and without this it would carry
            // an old entry's worth of failures into the next attempt and give up early on work
            // that had been starting fine.
            Executed::Launched => {
                let _ = backlog::clear_launch_failures(&path, &d.repo, &d.id);
            }
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
            Executed::Deferred(reason) => {
                let deferred_at = bn_core::util::now_epoch_secs();
                let refs = [d.id.as_str()];
                let _ = backlog::release_many(&path, &d.repo, &refs, deferred_at);
                let retry_at =
                    deferred_at + bn_core::queue::tick::launch_retry_delay(1).unwrap_or(60);
                let _ = backlog::snooze(&path, &d.repo, &d.id, retry_at, deferred_at);
                eprintln!(
                    "bn queue tick: {}/{} waiting until {}: {reason}",
                    d.repo,
                    d.id,
                    bn_core::util::clock_hm(retry_at)
                );
            }
            Executed::Failed(err) => {
                record_launch_failure(&path, d, &err, bn_core::queue::tick::launch_retry_delay)
            }
            // Same record, no second chance: the count still goes up and the event still lands,
            // but the retry schedule is "never".
            Executed::Refused(err) => record_launch_failure(&path, d, &err, give_up),
        }
    }
}

/// Count a launch that did not start, and either snooze the entry per `delay` or resolve it failed.
fn record_launch_failure(
    path: &std::path::Path,
    d: &Dispatch,
    err: &str,
    delay: fn(u32) -> Option<i64>,
) {
    let finished_at = bn_core::util::now_epoch_secs();
    // Released straight back to pending, this entry is picked again on the very next
    // pass — seconds later — and fails for the same reason. Backing off fixed the
    // spin; it did not fix the forever. `bn dispatch` refuses an item whose branch is
    // already active, and a merged branch's leftover ref satisfies that test with no
    // agent alive and nothing left to finish, so the condition the wait is waiting for
    // never arrives. Counting the attempts is what turns that into an ending.
    //
    // `fail_launch` is one mutation: it counts the attempt and then either snoozes the
    // entry past `launch_retry_delay` or, once the attempts run out, resolves it
    // `Failed` — where a person and the queue page can both see it, instead of it
    // sitting in HELD WORK reading `snoozed` for as long as the machine is up.
    let outcome = backlog::fail_launch(
        path,
        &d.repo,
        &d.id,
        finished_at,
        delay,
    )
    .ok()
    .flatten();
    let (attempt, gave_up) = match outcome {
        Some(LaunchFailure::Retrying { attempt, retry_at }) => {
            eprintln!(
                "bn queue tick: {}/{} did not start (attempt {attempt}, retrying at \
                 {}): {err}",
                d.repo,
                d.id,
                bn_core::util::clock_hm(retry_at)
            );
            (Some(attempt), false)
        }
        Some(LaunchFailure::GaveUp { attempt }) => {
            eprintln!(
                "bn queue tick: {}/{} did not start after {attempt} attempts — giving \
                 up: {err}",
                d.repo, d.id
            );
            (Some(attempt), true)
        }
        None => {
            eprintln!("bn queue tick: {}/{} did not start: {err}", d.repo, d.id);
            (None, false)
        }
    };
    let _ = bn_core::events::append_at(
        "queue_finish",
        finished_at,
        launch_failure_fields(&d.repo, &d.id, err, attempt, gave_up),
    );
}

/// The retry schedule for a refusal: none.
fn give_up(_attempt: u32) -> Option<i64> {
    None
}

/// What happened when the tick tried to run a dispatch.
enum Executed {
    /// A long-running agent was started; its terminal outcome is reconciled elsewhere.
    Launched,
    /// `bn dispatch` said no for a reason that will not change ([`crate::EXIT_REFUSED`]). Resolved
    /// failed at once rather than backed off: the backoff exists for a moment that may pass, and
    /// this is not one.
    Refused(String),
    /// A headless job ran to completion here, with this exit status.
    Finished { code: i32 },
    /// Capacity is temporarily unavailable; return the entry to pending without spending a retry.
    Deferred(String),
    /// Nothing started — the entry goes back to pending.
    Failed(String),
}

/// Run one planned dispatch according to its executor.
fn execute(cfg: &Config, d: &Dispatch, now: i64) -> Executed {
    match &d.exec {
        // Heavy work is spawned, never run inline — a minutes-long build inside the tick would
        // hold the dispatcher for minutes and make every pass behind it skip on the lock.
        bn_core::queue::kind::ExecSpec::Job { job, repo, branch } if d.heavy => spawn_heavy(
            cfg,
            d,
            &heavy::job_command(job, repo.as_deref(), branch.as_deref()),
        ),
        bn_core::queue::kind::ExecSpec::Shell { cmd } if d.heavy => spawn_heavy(cfg, d, cmd),
        // Express work runs in a child process because this function is inside the machine tick
        // lock. An in-process job that hangs would prevent Drop from releasing that lock and make
        // every future pass skip forever. `Dispatching` is inherited, so re-entering `bn run`
        // executes rather than re-queues the job.
        bn_core::queue::kind::ExecSpec::Job { job, repo, branch } => {
            run_express_job(cfg, job, repo.as_deref(), branch.as_deref())
        }
        bn_core::queue::kind::ExecSpec::Shell { cmd } => {
            let mut child = std::process::Command::new("/bin/sh");
            child
                .arg("-c")
                .arg(cmd)
                .env("PATH", crate::cmd::cron::job_path());
            run_express(child, "shell command")
        }
        bn_core::queue::kind::ExecSpec::Agent => launch_agent(cfg, d, None, now),
        bn_core::queue::kind::ExecSpec::Skill { skill, arg } => {
            launch_agent(cfg, d, Some((skill.as_str(), arg.as_deref())), now)
        }
    }
}

/// Launch a worktree agent by re-entering `bn dispatch` — the one launcher, rather than a second
/// copy of the worktree/lease/tmux sequence that would drift from it.
fn launch_agent(
    cfg: &Config,
    d: &Dispatch,
    skill: Option<(&str, Option<&str>)>,
    now: i64,
) -> Executed {
    if d.agent.with_main
        && !crate::wt_pool::paired_capacity_available(cfg, &d.repo, d.agent.branch.as_deref())
    {
        return Executed::Deferred(
            "--with-main needs two available worker slots in the repo pool".to_string(),
        );
    }
    let item = match skill.and_then(|(_, arg)| arg) {
        Some(arg) => arg.to_string(),
        None => d.item.clone(),
    };
    let harness = dispatch_harness(d, now);
    let mut inv = crate::cmd::dispatch::DispatchInvocation::new(&item, &d.repo, &harness);
    if let Some((name, _)) = skill {
        inv = inv.skill(Some(name));
    }
    if matches!(d.kind, BacklogKind::Scout | BacklogKind::Research) {
        inv = inv.scout(true);
    }
    // Launch the work the entry *declared*, not an approximation rebuilt from its subject line.
    // Dropping these was invisible: the agent started, on a branch slugified from its own prompt,
    // under a name no already-running guard would recognise, waiting on approvals nobody had been
    // asked for. See `AgentSpec`.
    inv = inv
        .branch(d.agent.branch.as_deref())
        .name(d.agent.name.as_deref())
        .yolo(d.agent.yolo)
        .no_plan_review(d.agent.no_plan_review)
        .with_main(d.agent.with_main)
        .stage(d.agent.stage);
    let Ok(exe) = std::env::current_exe() else {
        return Executed::Failed("cannot resolve the bn executable".to_string());
    };
    // No special flag needed: the child inherits the dispatch extent through the environment, so
    // its own capture check is inert. `BN_DISPATCH_NOW` remains, but as *your* escape hatch rather
    // than the dispatcher's workaround.
    let mut cmd = std::process::Command::new(exe);
    cmd.args(inv.args());
    match wait_bounded(cmd, launch_timeout()) {
        Launch::Exited(st) if st.success() => Executed::Launched,
        Launch::Exited(st) if st.code() == Some(crate::EXIT_REFUSED) => Executed::Refused(
            "bn dispatch refused it, and retrying would not change the answer — see the \
             dispatcher log"
                .to_string(),
        ),
        Launch::Exited(st) => {
            Executed::Failed(format!("bn dispatch exited {}", st.code().unwrap_or(-1)))
        }
        Launch::TimedOut(secs) => Executed::Failed(format!(
            "bn dispatch did not return within {secs}s — killed it and everything it started"
        )),
        Launch::Failed(e) => Executed::Failed(e),
    }
}

fn dispatch_harness(d: &Dispatch, now: i64) -> String {
    if crate::cmd::dispatch::policy::dispatch_copilot_only_window_open(now) {
        "copilot".to_string()
    } else {
        // Keep the resolver's documented default: absent [orchestrate].agent means Copilot.
        d.harness.clone().unwrap_or_else(|| "copilot".to_string())
    }
}

fn spawn_heavy(cfg: &Config, d: &Dispatch, cmd: &str) -> Executed {
    match heavy::spawn(cfg, d, cmd) {
        Ok(()) => Executed::Launched,
        Err(e) => Executed::Failed(e),
    }
}

#[cfg(not(test))]
fn run_express_job(_cfg: &Config, job: &str, repo: Option<&str>, branch: Option<&str>) -> Executed {
    let Ok(exe) = std::env::current_exe() else {
        return Executed::Failed("cannot resolve the bn executable".to_string());
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["run", job, "--now"]);
    if let Some(repo) = repo {
        cmd.args(["--repo", repo]);
    }
    if let Some(branch) = branch {
        cmd.args(["--branch", branch]);
    }
    run_express(cmd, &format!("job {job}"))
}

// Rust's unit-test executable is not the `bn` CLI and cannot be re-entered with `bn run`.
#[cfg(test)]
fn run_express_job(cfg: &Config, job: &str, repo: Option<&str>, branch: Option<&str>) -> Executed {
    let ctx = if repo.is_none() && branch.is_none() {
        None
    } else {
        target_ctx(
            cfg,
            repo.map(ToOwned::to_owned),
            branch.map(ToOwned::to_owned),
        )
    };
    Executed::Finished {
        code: crate::cmd::cron::run_job(cfg, ctx.as_ref(), job),
    }
}

fn run_express(cmd: std::process::Command, label: &str) -> Executed {
    run_express_with_timeout(cmd, label, express_timeout())
}

fn run_express_with_timeout(
    cmd: std::process::Command,
    label: &str,
    timeout: std::time::Duration,
) -> Executed {
    match wait_bounded(cmd, Some(timeout)) {
        Launch::Exited(st) => Executed::Finished {
            code: st.code().unwrap_or(1),
        },
        Launch::TimedOut(secs) => {
            eprintln!(
                "bn queue tick: express {label} exceeded {secs}s — killed it and everything it started"
            );
            Executed::Finished { code: 124 }
        }
        Launch::Failed(e) => Executed::Failed(e),
    }
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

#[cfg(test)]
mod express_tests {
    use super::*;

    #[test]
    fn a_hung_express_job_ends_instead_of_holding_the_tick_lock_forever() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 60"]);

        let started = std::time::Instant::now();
        let outcome = run_express_with_timeout(cmd, "test job", std::time::Duration::from_secs(1));

        assert!(matches!(outcome, Executed::Finished { code: 124 }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "express work must end at its deadline"
        );
    }
}
