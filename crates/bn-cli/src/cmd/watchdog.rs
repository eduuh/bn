//! `bn watchdog` and the tick that evaluates due watchdogs. The rules (what fires when) are
//! [`bn_core::coord::policy`]; this file gathers the facts a rule needs, carries out the effect,
//! and records the outcome.

use crate::cmd::agent::{try_deliver_live, LiveDelivery};
use crate::cli::coord_group::AsSeat;
use crate::*;
use bn_core::attention::{self, NewAttn};
use bn_core::clock::{Clock, RealClock};
use bn_core::coord::policy::{decide, Decision, Facts};
use bn_core::coord::watchdog::{parse_wdid, Kind, NewWatchdog, Spec, Watchdog};
use bn_core::coord::watchdog_eval::{Outcome, DEFER_ALERT_AFTER};
use bn_core::coord::{self, agent_target, is_human, CoordDb, CoordError};
use serde::Serialize;
use serde_json::json;

mod render;
#[cfg(test)]
mod tests;

const CMD: &str = "watchdog";

pub(crate) fn emit_watchdog(cfg: &Config, action: Option<WatchdogCmd>, json: bool) {
    let mut db = CoordDb::open_default().unwrap_or_else(|e| die(json, &e));
    let clock = RealClock;
    let action = action.unwrap_or(WatchdogCmd::List {
        all: false,
        target: None,
        limit: 50,
    });
    match action {
        WatchdogCmd::Register {
            kind,
            target,
            at,
            every,
            text,
            threshold,
            idle,
            max_fires,
            expires,
            who,
        } => {
            let kind = Kind::parse(&kind).unwrap_or_else(|| {
                fail_json(json, CMD, "invalid_kind", &format!("unknown watchdog kind '{kind}'"))
            });
            let every = every.map(|e| duration(&e, json));
            let new = NewWatchdog {
                kind,
                target: target.unwrap_or_else(|| acting(cfg, &who, json)),
                text,
                spec: Spec {
                    threshold_pct: threshold,
                    idle_secs: idle.map(|i| duration(&i, json)),
                },
                first_in_secs: at.map(|a| duration(&a, json)).or(every).unwrap_or(0),
                every_secs: every,
                expires_in_secs: expires.map(|e| duration(&e, json)),
                max_fires,
                created_by: acting(cfg, &who, json),
            };
            let wd = register(&mut db, &clock, new).unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(wd)).to_json());
            } else {
                render::print_one(&wd);
            }
        }
        WatchdogCmd::List { all, target, limit } => {
            let target = target.map(|t| coord::normalize_seat(&t).unwrap_or_else(|e| die(json, &e)));
            let rows = db
                .watchdogs(all, target.as_deref(), limit)
                .unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!({"watchdogs": rows})).to_json());
            } else {
                render::print_list(&rows);
            }
        }
        WatchdogCmd::Show { id } => {
            let wd = db.watchdog(wdnum(&id, json)).unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(wd)).to_json());
            } else {
                render::print_one(&wd);
            }
        }
        WatchdogCmd::Status { id, evals } => {
            let n = wdnum(&id, json);
            let wd = db.watchdog(n).unwrap_or_else(|e| die(json, &e));
            let history = db.watchdog_evals(n, evals).unwrap_or_else(|e| die(json, &e));
            if json {
                let data = json!({"watchdog": wd, "evals": history});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                render::print_one(&wd);
                render::print_evals(&history);
            }
        }
        WatchdogCmd::Stop { id, reason } => {
            let wd = db
                .stop_watchdog(&clock, wdnum(&id, json), &reason)
                .unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(wd)).to_json());
            } else {
                println!("{} {}", wd.id, wd.state);
            }
        }
        WatchdogCmd::Tick { dry_run } => {
            let now = clock.now_epoch_secs();
            if dry_run {
                let due = db.due_watchdogs(now).unwrap_or_else(|e| die(json, &e));
                if json {
                    println!("{}", Envelope::success(CMD, json!({"due": due})).to_json());
                } else if due.is_empty() {
                    println!("Nothing due.");
                } else {
                    render::print_list(&due);
                }
                return;
            }
            let report = tick(cfg, &mut db, now, None);
            if json {
                println!("{}", Envelope::success(CMD, json!(report)).to_json());
            } else {
                println!("{}", report.summary());
            }
        }
        WatchdogCmd::Fire { id } => {
            let report = tick(cfg, &mut db, clock.now_epoch_secs(), Some(wdnum(&id, json)));
            if json {
                println!("{}", Envelope::success(CMD, json!(report)).to_json());
            } else {
                println!("{}", report.summary());
            }
        }
    }
}

/// Register `new`, and for a one-shot a little way out, also arm an exact-time timer so it fires
/// on the minute rather than on the next tick. The timer is best-effort: the row is the durable
/// record, and the tick catches it up if the timer never runs.
pub(crate) fn register(
    db: &mut CoordDb,
    clock: &dyn Clock,
    new: NewWatchdog,
) -> Result<Watchdog, CoordError> {
    register_with(db, clock, new, &mut crate::cmd::agent::schedule_bn_after)
}

/// [`register`] with the timer injected, so a test can see what would be armed without arming it.
fn register_with(
    db: &mut CoordDb,
    clock: &dyn Clock,
    new: NewWatchdog,
    arm: &mut dyn FnMut(i64, &[&str]) -> bool,
) -> Result<Watchdog, CoordError> {
    let wd = db.register_watchdog(clock, &new)?;
    if wd.every_secs.is_none() && new.first_in_secs >= 60 {
        let mins = (new.first_in_secs + 59) / 60;
        arm(mins, &["watchdog", "fire", &wd.id]);
    }
    Ok(wd)
}

/// The watchdog pass the 2-minute `watch` job runs: tick `db` if it opened, else skip quietly —
/// a missing or broken coordination DB must never fail the wake reactor.
pub(crate) fn tick_opened(
    cfg: &Config,
    db: Result<CoordDb, CoordError>,
    now: i64,
) -> Option<TickReport> {
    db.ok().map(|mut db| tick(cfg, &mut db, now, None))
}

/// What a tick did.
#[derive(Debug, Default, Serialize)]
pub(crate) struct TickReport {
    pub(crate) fired: Vec<String>,
    pub(crate) skipped: Vec<String>,
    pub(crate) deferred: Vec<String>,
    pub(crate) unavailable: Vec<String>,
    pub(crate) errors: Vec<String>,
}

impl TickReport {
    pub(crate) fn summary(&self) -> String {
        format!(
            "watchdog tick: {} fired, {} skipped, {} deferred, {} unavailable{}",
            self.fired.len(),
            self.skipped.len(),
            self.deferred.len(),
            self.unavailable.len(),
            if self.errors.is_empty() {
                String::new()
            } else {
                format!(", {} error(s)", self.errors.len())
            }
        )
    }
}

/// Evaluate every due watchdog (or just `only`, if it is due).
pub(crate) fn tick(cfg: &Config, db: &mut CoordDb, now: i64, only: Option<i64>) -> TickReport {
    let mut report = TickReport::default();
    let due: Vec<Watchdog> = match db.due_watchdogs(now) {
        Ok(due) => due
            .into_iter()
            .filter(|w| only.is_none_or(|o| o == w.num))
            .collect(),
        Err(e) => {
            report.errors.push(e.to_string());
            return report;
        }
    };
    // The fleet scan reads every live transcript; only pay for it when a due watchdog needs it.
    let needs_fleet = due
        .iter()
        .any(|w| matches!(w.kind(), Some(Kind::Keepalive | Kind::Context)));
    let fleet = needs_fleet.then(|| {
        bn_core::agent::supervise(
            cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        )
    });
    for wd in due {
        match db.claim_due(&wd, now) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                report.errors.push(format!("{}: {e}", wd.id));
                continue;
            }
        }
        let facts = gather(&wd, fleet.as_ref());
        let (outcome, detail) = match decide(&wd, &facts) {
            Decision::Fire(text) => act(cfg, &wd, &text),
            Decision::Skip(why) => (Outcome::Skipped, why),
            Decision::Unavailable(why) => (Outcome::Unavailable, why),
        };
        match db.finish_eval(wd.num, outcome, &detail, now) {
            Ok(after) => after_eval(&after, outcome),
            Err(e) => report.errors.push(format!("{}: {e}", wd.id)),
        }
        let bucket = match outcome {
            Outcome::Fired => &mut report.fired,
            Outcome::Skipped => &mut report.skipped,
            Outcome::Deferred => &mut report.deferred,
            Outcome::Unavailable => &mut report.unavailable,
            Outcome::Error => &mut report.errors,
        };
        bucket.push(if detail.is_empty() {
            wd.id.clone()
        } else {
            format!("{} ({detail})", wd.id)
        });
    }
    report
}

/// The facts `wd`'s policy needs.
fn gather(wd: &Watchdog, fleet: Option<&bn_core::agent::Fleet>) -> Facts {
    let agent = fleet.and_then(|f| {
        f.agents
            .iter()
            .find(|a| coord::normalize_seat(&a.target).ok().as_deref() == Some(&wd.target))
    });
    let idle_secs = agent.map(|a| a.idle_secs);
    Facts {
        idle_secs,
        // `None` (Copilot, just compacted, no usage yet) stays unknown — never read as 0%.
        context_pct: agent.and_then(|a| a.context.as_ref()).and_then(|c| c.pct),
    }
}

/// Carry out a fire. Delivering to an agent that isn't reachable right now is a deferral, not a
/// failure: the watchdog tries again in a few minutes.
fn act(cfg: &Config, wd: &Watchdog, text: &str) -> (Outcome, String) {
    let line = format!("[bn watchdog {}] {text}", wd.id);
    if is_human(&wd.target) {
        return match raise(&wd.target, &format!("watchdog:{}", wd.id), &line, "warn") {
            Ok(()) => (Outcome::Fired, "raised for a person".into()),
            Err(e) => (Outcome::Error, e.to_string()),
        };
    }
    match try_deliver_live(cfg, &wd.target, &line) {
        LiveDelivery::Delivered => (Outcome::Fired, String::new()),
        LiveDelivery::NotLive => (Outcome::Deferred, "seat not live".into()),
        LiveDelivery::Dialog => (Outcome::Deferred, "seat is on a dialog".into()),
        LiveDelivery::Failed => (Outcome::Error, "paste did not land".into()),
    }
}

/// After recording: a watchdog that keeps deferring is a timer that can't land — surface it once;
/// a fire clears that alert.
fn after_eval(wd: &Watchdog, outcome: Outcome) {
    let key = format!("watchdog:{}:stuck", wd.id);
    if outcome == Outcome::Fired || !wd.is_active() {
        if let Some(item) = attention::live_by_key(&key) {
            bn_core::util::best_effort(attention::ack(&item.id), "watchdog: clear stuck alert");
        }
    } else if outcome == Outcome::Deferred && wd.defer_count > DEFER_ALERT_AFTER {
        let title = format!(
            "{} ({}) can't reach {} — deferred {} times",
            wd.id, wd.kind, wd.target, wd.defer_count
        );
        bn_core::util::best_effort(raise(&wd.target, &key, &title, "warn"), "watchdog: alert");
    }
}

pub(super) fn raise(seat: &str, key: &str, title: &str, severity: &str) -> std::io::Result<()> {
    let (repo, branch) = agent_target(seat).unzip();
    attention::add_new(
        &NewAttn {
            source: "watchdog",
            title,
            severity,
            repo,
            branch,
            key: Some(key),
            ..Default::default()
        },
        &today(),
    )
    .map(|_| ())
}

/// `bn agent checkin` on top of a watchdog: a one-shot reminder to `target` in `mins` minutes.
pub(crate) fn checkin(
    target: &str,
    mins: u64,
    note: &str,
    created_by: &str,
) -> Result<Watchdog, CoordError> {
    let mut db = CoordDb::open_default()?;
    checkin_in(
        &mut db,
        &RealClock,
        target,
        mins,
        note,
        created_by,
        &mut crate::cmd::agent::schedule_bn_after,
    )
}

fn checkin_in(
    db: &mut CoordDb,
    clock: &dyn Clock,
    target: &str,
    mins: u64,
    note: &str,
    created_by: &str,
    arm: &mut dyn FnMut(i64, &[&str]) -> bool,
) -> Result<Watchdog, CoordError> {
    register_with(
        db,
        clock,
        NewWatchdog {
            kind: Kind::Reminder,
            target: target.to_string(),
            text: Some(note.to_string()),
            spec: Spec::default(),
            first_in_secs: (mins as i64) * 60,
            every_secs: None,
            expires_in_secs: None,
            max_fires: Some(1),
            created_by: created_by.to_string(),
        },
        arm,
    )
}

fn duration(s: &str, json: bool) -> i64 {
    bn_core::focus::parse_duration(s).unwrap_or_else(|| {
        fail_json(
            json,
            CMD,
            "invalid_duration",
            &format!("'{s}' is not a duration (e.g. 90s, 30m, 2h, 1d)"),
        )
    })
}

fn wdnum(id: &str, json: bool) -> i64 {
    parse_wdid(id).unwrap_or_else(|| {
        fail_json(json, CMD, "invalid_id", &format!("'{id}' is not a watchdog id (wd<N>)"))
    })
}

/// The seat this invocation acts as — see [`coord::acting_seat`].
fn acting(cfg: &Config, who: &AsSeat, json: bool) -> String {
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolver::resolve(cfg, &cwd).ok())
        .map(|r| format!("{}:{}", r.repo, r.branch));
    let env = std::env::var("BN_SEAT").ok();
    let user = std::env::var("USER").unwrap_or_default();
    coord::acting_seat(who.as_seat.as_deref(), env.as_deref(), here.as_deref(), &user)
        .unwrap_or_else(|e| die(json, &e))
}

fn die(json: bool, e: &CoordError) -> ! {
    let code = match e {
        CoordError::Sqlite(_) => "db_error",
        CoordError::NotFound(_) => "not_found",
        CoordError::Invalid(_) => "invalid",
    };
    fail_json(json, CMD, code, &e.to_string())
}

/// `coord-tick` — the watchdog tick, on the `tick-5m` cluster.
pub(crate) fn job_coord_tick(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let mut db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => {
            crate::cmd::jobs::native_job_line(&format!("coord-tick: {e}"), true);
            return 1;
        }
    };
    let dogs = tick(cfg, &mut db, bn_core::util::now_epoch_secs(), None);
    let failed = !dogs.errors.is_empty();
    crate::cmd::jobs::native_job_line(&dogs.summary(), failed);
    i32::from(failed)
}
