//! `bn routine` — the routine-aware personal schedule coach (#801, amended for a deterministic
//! lifetime — stable identity, one transaction lock, an honest at-least-once tick, fail-closed
//! invalid config, a deadline-driven watcher, and a managed service). All domain logic lives in
//! [`bn_core::routine`]; this module is thin CLI plumbing — parse args, take the lock via
//! [`bn_core::routine::RoutineTx`], call the domain, render human or `--json`.

use crate::*;
use bn_core::routine::{self, RoutineTx, TickOutcome};

fn now_epoch() -> i64 {
    use bn_core::clock::Clock;
    bn_core::clock::RealClock.now_epoch_secs()
}

/// Begin a locked routine transaction (#801 amendment invariant 2), failing loudly — the same
/// way every routine subcommand always has — on a busy lock or an invalid live config/ambiguous
/// identity migration. Every mutating command and the read-only `now`/`next`/`status` views route
/// through this so no observer ever sees a partially-committed change.
fn begin_tx_or_fail(now: i64, json: bool) -> RoutineTx {
    let tx = RoutineTx::begin(now)
        .unwrap_or_else(|e| fail_json(json, "routine", "lock_error", &format!("bn routine: {e}")));
    if let Some(err) = tx.blocking_error() {
        fail_json(
            json,
            "routine",
            "config_error",
            &format!("bn routine: {err}"),
        );
    }
    tx
}

fn print_block(label: &str, b: &routine::BlockView) {
    println!("{label}: {} ({})", b.name, b.kind);
    println!("  {} → {}", b.start, b.end);
    if b.protected {
        println!("  protected — non-urgent guidance pauses while this is current");
    }
}

fn print_snapshot(snap: &routine::Snapshot) {
    if !snap.enabled {
        println!("Routine: disabled — set [routine].enabled = true to opt in");
        return;
    }
    println!(
        "Routine ({}, UTC{}) as of {}",
        snap.timezone, snap.utc_offset, snap.as_of
    );
    match &snap.current {
        Some(b) => {
            print_block("Now", b);
            if let Some(m) = snap.remaining_minutes {
                println!("  {m}m remaining");
            }
        }
        None => println!("Now: nothing scheduled"),
    }
    if let Some(s) = &snap.skipped {
        println!("Skipped today: {} ({})", s.name, s.id);
    }
    match &snap.next {
        Some(b) => {
            print_block("Next", b);
            if let Some(m) = snap.until_next_minutes {
                println!("  in {m}m");
            }
        }
        None => println!("Next: nothing else scheduled"),
    }
    if !snap.aligned {
        println!("⚠ not currently aligned to any configured block (a scheduling gap)");
    }
    if snap.dst_adjusted {
        println!("ⓘ a boundary was DST-adjusted for this occurrence");
    }
    if let Some(until) = &snap.snoozed_until {
        println!("🔕 notifications snoozed until {until}");
    }
    if let Some(reason) = snap.explanation() {
        println!("{reason}");
    }
}

/// The health + coverage payload folded into `bn routine status`'s JSON, and the human summary
/// line printed alongside the schedule snapshot (#801 amendment invariant 7).
fn print_health_summary(health: &routine::RoutineHealth, coverage: routine::Coverage, now: i64) {
    print!("Coverage: {}", coverage.as_str());
    if let Some(t) = health.last_tick_success {
        print!(" · last tick {}s ago", now.saturating_sub(t));
    }
    if let Some(t) = health.watcher_heartbeat {
        print!(" · watcher heartbeat {}s ago", now.saturating_sub(t));
    }
    println!();
    if let Some(err) = &health.config_error {
        println!(
            "⚠ invalid live config since {}: {err}",
            health.invalid_since.unwrap_or(now)
        );
    }
}

fn coverage_for(health: &routine::RoutineHealth, now: i64) -> routine::Coverage {
    let installed = crate::launchd::routine_installed() || crate::systemd::routine_installed();
    let timer_registered = job_timer_registered();
    routine::classify_coverage(health, installed, timer_registered, now)
}

/// Whether the native `routine-tick` job (or its `routine-notify` alias) is registered as a
/// scheduled timer — a launchd `StartInterval`/systemd `--user` timer `bn cron register` wrote.
fn job_timer_registered() -> bool {
    crate::timers::registered()
        .iter()
        .any(|t| t.job == "routine-tick" || t.job == "routine-notify")
        || crate::launchd::registered()
            .iter()
            .any(|a| a.job == "routine-tick" || a.job == "routine-notify")
}

pub(crate) fn emit_routine(_cfg: &Config, action: Option<RoutineCmd>, json: bool) {
    let now = now_epoch();
    match action.unwrap_or(RoutineCmd::Status) {
        RoutineCmd::Now => {
            let tx = begin_tx_or_fail(now, json);
            let rc = tx.config.as_ref().expect("blocking_error() checked above");
            let snap = routine::snapshot(rc, now);
            if json {
                println!(
                    "{}",
                    Envelope::success("routine_now", serde_json::json!(snap)).to_json()
                );
            } else {
                print_snapshot(&snap);
            }
        }
        RoutineCmd::Status => {
            let tx = begin_tx_or_fail(now, json);
            let rc = tx.config.as_ref().expect("blocking_error() checked above");
            let snap = routine::snapshot(rc, now);
            let health = tx.load_health();
            let coverage = coverage_for(&health, now);
            if json {
                let mut data = serde_json::to_value(&snap).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = data.as_object_mut() {
                    obj.insert(
                        "health".into(),
                        serde_json::to_value(&health).unwrap_or(serde_json::Value::Null),
                    );
                    obj.insert("coverage".into(), serde_json::json!(coverage.as_str()));
                }
                println!("{}", Envelope::success("routine_status", data).to_json());
            } else {
                print_snapshot(&snap);
                print_health_summary(&health, coverage, now);
            }
        }
        RoutineCmd::Next => {
            let tx = begin_tx_or_fail(now, json);
            let rc = tx.config.as_ref().expect("blocking_error() checked above");
            let snap = routine::snapshot(rc, now);
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "routine_next",
                        serde_json::json!({
                            "enabled": snap.enabled,
                            "next": snap.next,
                            "until_next_minutes": snap.until_next_minutes,
                            "dst_adjusted": snap.dst_adjusted,
                        }),
                    )
                    .to_json()
                );
            } else if !snap.enabled {
                println!("Routine: disabled — set [routine].enabled = true to opt in");
            } else {
                match &snap.next {
                    Some(b) => {
                        print_block("Next", b);
                        if let Some(m) = snap.until_next_minutes {
                            println!("  in {m}m");
                        }
                    }
                    None => println!("Next: nothing else scheduled"),
                }
            }
        }
        RoutineCmd::CheckIn { note } => {
            let tx = begin_tx_or_fail(now, json);
            tx.record_checkin(note.as_deref()).unwrap_or_else(|e| {
                fail_json(json, "routine_check_in", "io_error", &e.to_string())
            });
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "routine_check_in",
                        serde_json::json!({"ts": now, "note": note})
                    )
                    .to_json()
                );
            } else {
                println!("Routine check-in recorded.");
            }
        }
        RoutineCmd::Snooze { minutes } => {
            let tx = begin_tx_or_fail(now, json);
            let until = tx
                .record_snooze(minutes)
                .unwrap_or_else(|e| fail_json(json, "routine_snooze", "io_error", &e.to_string()));
            if json {
                println!(
                    "{}",
                    Envelope::success("routine_snooze", serde_json::json!({"until": until}))
                        .to_json()
                );
            } else {
                println!(
                    "🔕 Routine notifications snoozed for {minutes}m (until {})",
                    bn_core::util::clock_hm(until)
                );
            }
        }
        RoutineCmd::Skip { block, date } => {
            let tx = begin_tx_or_fail(now, json);
            let rc = tx.config.as_ref().expect("blocking_error() checked above");
            let snap = routine::snapshot(rc, now);
            let block_id = match block {
                Some(id) => Some(id),
                None => match snap.current.as_ref() {
                    Some(current) if current.source == "override" => fail_json(
                        json,
                        "routine_skip",
                        "override_active",
                        "bn routine skip: the current block is a temporary override — pass --block <configured-id>",
                    ),
                    Some(current) => Some(current.id.clone()),
                    None => None,
                },
            };
            let Some(block_id) = block_id else {
                fail_json(
                    json,
                    "routine_skip",
                    "no_current_block",
                    "bn routine skip: nothing is current — pass --block <id>",
                );
            };
            let date = date.unwrap_or_else(today);
            tx.record_skip(&block_id, &date)
                .unwrap_or_else(|e| fail_json(json, "routine_skip", "io_error", &e.to_string()));
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "routine_skip",
                        serde_json::json!({"block_id": block_id, "date": date}),
                    )
                    .to_json()
                );
            } else {
                println!("Skipped {block_id}'s occurrence on {date}.");
            }
        }
        RoutineCmd::Override {
            name,
            kind,
            minutes,
        } => {
            let tx = begin_tx_or_fail(now, json);
            let until = now + minutes.max(1) * 60;
            tx.record_override(&name, &kind, until).unwrap_or_else(|e| {
                fail_json(json, "routine_override", "io_error", &e.to_string())
            });
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "routine_override",
                        serde_json::json!({"name": name, "kind": kind, "until": until}),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "Override active: {name} ({kind}) until {}",
                    bn_core::util::clock_hm(until)
                );
            }
        }
        RoutineCmd::ClearOverride => {
            let tx = begin_tx_or_fail(now, json);
            tx.clear_override().unwrap_or_else(|e| {
                fail_json(json, "routine_clear_override", "io_error", &e.to_string())
            });
            if json {
                println!(
                    "{}",
                    Envelope::success("routine_clear_override", serde_json::json!({"ts": now}))
                        .to_json()
                );
            } else {
                println!("Override cleared — recurring schedule resumed.");
            }
        }
        RoutineCmd::Tick => std::process::exit(run_tick(now, json, "routine-tick")),
        RoutineCmd::Notify => std::process::exit(run_tick(now, json, "routine-notify")),
        RoutineCmd::Watch => run_watch(json),
        RoutineCmd::Service { action } => match action {
            RoutineServiceCmd::Install => service_install(json),
            RoutineServiceCmd::Stop => service_stop(json),
            RoutineServiceCmd::Restart => service_restart(json),
            RoutineServiceCmd::Status => service_status(json),
        },
    }
}

/// Render one [`TickOutcome`] for a human or `--json` — shared by `bn routine tick`, the
/// `notify` alias, `bn routine watch`'s per-pass log line, and the native cron/timer job.
fn render_tick(outcome: &TickOutcome, json: bool, label: &str) {
    if json {
        println!(
            "{}",
            Envelope::success(label.replace('-', "_"), serde_json::json!(outcome)).to_json()
        );
        return;
    }
    if let Some(err) = &outcome.error {
        eprintln!(
            "{label}: invalid [routine] config — {err} (fail-closed: no notifications until it's fixed)"
        );
        return;
    }
    if !outcome.enabled {
        println!("{label}: disabled — set [routine].enabled to opt in");
        return;
    }
    if outcome.snoozed {
        println!("{label}: snoozed — nothing sent");
        return;
    }
    for e in &outcome.sent {
        println!("{label}: sent — {}", e.message);
    }
    for e in &outcome.expired {
        println!(
            "{label}: expired past the catch-up horizon (not sent) — {}",
            e.message
        );
    }
    if outcome.sent.is_empty() && outcome.expired.is_empty() && outcome.deduped.is_empty() {
        println!("{label}: nothing due");
    }
    if !outcome.backend_available {
        eprintln!("{label}: no desktop-notification backend available on this host");
    }
}

/// Exit code for one tick render: 1 on an invalid config or a failed delivery attempt, else 0 —
/// shared by the CLI command and the native job.
fn tick_exit_code(outcome: &TickOutcome) -> i32 {
    let failed = outcome.error.is_some()
        || (outcome.enabled && !outcome.snoozed && !outcome.backend_available);
    i32::from(failed)
}

/// Run one shared tick (#801 amendment invariant 3/4) and render it — the body behind
/// `bn routine tick`, its `notify` alias, and the native `routine-tick`/`routine-notify` job.
fn run_tick(now: i64, json: bool, label: &str) -> i32 {
    match routine::tick(now) {
        Ok(outcome) => {
            render_tick(&outcome, json, label);
            tick_exit_code(&outcome)
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    Envelope::failure(label.replace('-', "_"), "lock_error", &e).to_json()
                );
            } else {
                eprintln!("{label}: {e}");
            }
            1
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `bn routine watch` — a foreground, deadline-driven reconciliation loop (#801 amendment
// invariant 5). Sleeps in short bounded chunks (checking the stop flag between each) rather than
// one long `sleep`, so SIGTERM/Ctrl-C is responsive; always re-fetches wall-clock `now` before
// each pass, so suspend/resume and clock jumps are handled by simply re-evaluating from scratch
// rather than tracking elapsed time.
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
static STOP_WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_stop_signal(_sig: i32) {
    STOP_WATCH.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Install SIGTERM/SIGINT handlers via a raw `signal(2)` call (no extra crate dependency — same
/// raw-syscall approach `serve.rs` uses for `setsid`), so `bn routine watch` exits cleanly instead
/// of being killed mid-transaction.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "installing process signal handlers requires the Unix signal FFI"
)]
fn install_stop_signal_handlers() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    // SAFETY: the handler has C ABI, touches only an atomic, and remains valid for process lifetime.
    unsafe {
        signal(SIGINT, handle_stop_signal as *const () as usize);
        signal(SIGTERM, handle_stop_signal as *const () as usize);
    }
}
#[cfg(not(unix))]
fn install_stop_signal_handlers() {}

#[cfg(unix)]
fn stop_requested() -> bool {
    STOP_WATCH.load(std::sync::atomic::Ordering::SeqCst)
}
#[cfg(not(unix))]
fn stop_requested() -> bool {
    false
}

/// Sleep up to `total_secs`, checking the stop flag every 5s so a SIGTERM/Ctrl-C during a long
/// sleep is honored promptly instead of only at the next wake.
fn sleep_in_chunks(total_secs: i64) {
    let mut remaining = total_secs.max(1);
    while remaining > 0 && !stop_requested() {
        let chunk = remaining.min(5);
        std::thread::sleep(std::time::Duration::from_secs(chunk.max(1) as u64));
        remaining -= chunk;
    }
}

fn run_watch(json: bool) -> ! {
    install_stop_signal_handlers();
    let pid = std::process::id();
    let start = now_epoch();
    if let Err(e) = routine::record_watcher_start(start, pid) {
        eprintln!("bn routine watch: warning: could not record watcher start health — {e}");
    }
    if !json {
        println!(
            "bn routine watch: started (pid {pid}) — deadline-driven, at-least-once; SIGTERM/Ctrl-C to stop."
        );
    }

    while !stop_requested() {
        let now = now_epoch();
        match routine::watch_pass(now) {
            Ok(pass) => {
                render_tick(&pass.tick, json, "routine-watch");
                let _ = routine::record_watcher_heartbeat(now_epoch(), pid);
                sleep_in_chunks(pass.sleep_secs);
            }
            Err(e) => {
                eprintln!("bn routine watch: {e}");
                sleep_in_chunks(5);
            }
        }
    }

    let _ = routine::record_watcher_stop(now_epoch());
    if !json {
        println!("bn routine watch: stopped.");
    }
    std::process::exit(0);
}

// ---------------------------------------------------------------------------------------------
// `bn routine service` — managed lifecycle for `bn routine watch` (#801 amendment invariant 6):
// launchd on macOS, systemd `--user` on Linux, honest "no resident coverage" (never a detached
// fallback claiming reboot/crash coverage it doesn't have) otherwise.
// ---------------------------------------------------------------------------------------------

fn current_bn_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "bn".to_string())
}

fn service_install(json: bool) {
    let bn_path = current_bn_path();
    let (mode, ok) = if crate::launchd::routine_install(&bn_path).is_ok() {
        ("launchd", true)
    } else if crate::systemd::routine_install(&bn_path).is_ok() {
        ("systemd", true)
    } else {
        ("unsupported", false)
    };
    if json {
        let data = serde_json::json!({ "installed": ok, "mode": mode });
        println!(
            "{}",
            Envelope::success("routine_service_install", data).to_json()
        );
        if !ok {
            std::process::exit(1);
        }
        return;
    }
    if ok {
        println!(
            "bn routine service: {mode} service installed — bn routine watch runs in the \
             background, starting at login/reboot and restarting on crash."
        );
        println!("  status: bn routine service status   stop: bn routine service stop");
    } else {
        println!(
            "bn routine service: no supported service manager (launchd/systemd) on this host — \
             no resident coverage."
        );
        println!(
            "  use `bn cron register` for the routine-tick timer fallback instead (periodic, not \
             deadline-driven)."
        );
        std::process::exit(1);
    }
}

fn service_stop(json: bool) {
    let was_installed = crate::launchd::routine_installed() || crate::systemd::routine_installed();
    crate::launchd::routine_uninstall();
    crate::systemd::routine_uninstall();
    if json {
        let data = serde_json::json!({ "stopped": true, "was_installed": was_installed });
        println!(
            "{}",
            Envelope::success("routine_service_stop", data).to_json()
        );
    } else if was_installed {
        println!("bn routine service: stopped and unregistered.");
    } else {
        println!("bn routine service: nothing was installed.");
    }
}

fn service_restart(json: bool) {
    crate::launchd::routine_uninstall();
    crate::systemd::routine_uninstall();
    service_install(json);
}

fn service_status(json: bool) {
    let installed = crate::launchd::routine_installed() || crate::systemd::routine_installed();
    let timer_registered = job_timer_registered();
    let health = routine::load_health();
    let now = now_epoch();
    let coverage = routine::classify_coverage(&health, installed, timer_registered, now);
    if json {
        let data = serde_json::json!({
            "installed": installed,
            "timer_registered": timer_registered,
            "coverage": coverage.as_str(),
            "health": health,
        });
        println!(
            "{}",
            Envelope::success("routine_service_status", data).to_json()
        );
        return;
    }
    println!(
        "bn routine service: {} · timer registered: {timer_registered} · coverage: {}",
        if installed {
            "installed"
        } else {
            "not installed"
        },
        coverage.as_str(),
    );
    if let Some(t) = health.last_tick_success {
        println!("  last tick success: {}s ago", now.saturating_sub(t));
    }
    if let Some(t) = health.watcher_heartbeat {
        println!(
            "  watcher heartbeat: {}s ago (pid {:?})",
            now.saturating_sub(t),
            health.watcher_pid
        );
    }
    if let Some(err) = &health.config_error {
        println!("  ⚠ invalid config: {err}");
    }
}

/// The native `routine-tick` cron/timer job (#801 amendment) — the shared engine every scheduled
/// invocation runs. Opt-in: a no-op (exit 0) whenever `[routine].enabled` is unset/false; fails
/// closed (no notifications, exit 1) on an invalid live config rather than a stale schedule.
pub(crate) fn job_routine_tick(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    run_tick(now, false, "routine-tick")
}

/// Compatibility alias for [`job_routine_tick`] — identical delivery through the same engine.
pub(crate) fn job_routine_notify(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    job_routine_tick(cfg, ctx)
}


