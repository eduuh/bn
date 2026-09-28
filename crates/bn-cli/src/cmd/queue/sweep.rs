//! Carrying out a [`bn_core::coord::sweep::Plan`]: typing nudges into live panes, raising and
//! acking attention items, retrying note mirrors. `bn queue sweep` and the `coord-tick` job run
//! the whole thing; create and handoff run it for the one row they just opened.

use crate::cmd::agent::{try_deliver_live, LiveDelivery};
use crate::*;
use bn_core::attention::{self, NewAttn};
use bn_core::coord::sweep::{self, Alert, Plan, KEY_PREFIX};
use bn_core::coord::{self, agent_target, CoordDb, MirrorOutcome, QItem};
use serde::Serialize;

/// What a sweep did, for `--json` and the job line.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub(crate) nudged: Vec<String>,
    pub(crate) not_live: Vec<String>,
    pub(crate) raised: Vec<String>,
    pub(crate) acked: Vec<String>,
    pub(crate) mirrored: Vec<String>,
    pub(crate) errors: Vec<String>,
}

impl Report {
    pub(crate) fn summary(&self) -> String {
        format!(
            "queue sweep: {} nudged, {} not live, {} alert(s) raised, {} cleared, {} mirror(s) \
             repaired{}",
            self.nudged.len(),
            self.not_live.len(),
            self.raised.len(),
            self.acked.len(),
            self.mirrored.len(),
            if self.errors.is_empty() {
                String::new()
            } else {
                format!(", {} error(s)", self.errors.len())
            }
        )
    }
}

/// Plan and carry out a sweep over every open row.
pub(crate) fn sweep_all(cfg: &Config, db: &mut CoordDb, now: i64, dry_run: bool) -> (Plan, Report) {
    let mut rows = db
        .list(&coord::ListFilter::default())
        .unwrap_or_default();
    rows.extend(db.closed_with_pending_mirror().unwrap_or_default());
    let live: Vec<String> = attention::live_with_key_prefix(KEY_PREFIX)
        .into_iter()
        .filter_map(|a| a.key)
        .collect();
    let plan = plan_for(db, &rows, &live, now);
    let report = if dry_run {
        Report::default()
    } else {
        apply(cfg, db, &plan, now)
    };
    (plan, report)
}

/// Nudge or alert for one row just opened, without touching any other row's alerts.
pub(crate) fn notify_new(cfg: &Config, db: &mut CoordDb, item: &QItem, now: i64) -> Report {
    let plan = plan_for(db, std::slice::from_ref(item), &[], now);
    apply(cfg, db, &plan, now)
}

fn plan_for(db: &CoordDb, rows: &[QItem], live: &[String], now: i64) -> Plan {
    sweep::plan(
        rows,
        &|id| db.nudged_at(id).ok().flatten(),
        &|id| db.has_wake(id).unwrap_or(false),
        live,
        now,
    )
}

fn apply(cfg: &Config, db: &mut CoordDb, plan: &Plan, now: i64) -> Report {
    let mut r = Report::default();
    for &id in &plan.mirror_retry {
        let Ok(item) = db.get(id) else { continue };
        let out = if item.qstate().is_terminal() {
            coord::close_mirror(cfg, &item, &today())
        } else {
            coord::open_mirror(cfg, &item, &today())
        };
        match &out {
            MirrorOutcome::Failed(why) => r.errors.push(format!("{} mirror: {why}", item.id)),
            _ => r.mirrored.push(item.id.clone()),
        }
        bn_core::util::best_effort(db.set_mirror(id, &out), "queue sweep: record mirror");
    }
    for &id in &plan.nudge {
        let Ok(item) = db.get(id) else { continue };
        match try_deliver_live(cfg, &item.dest, &sweep::nudge_text(&item)) {
            LiveDelivery::Delivered => {
                bn_core::util::best_effort(db.record_nudge(id, now), "queue sweep: record nudge");
                r.nudged.push(item.id);
            }
            LiveDelivery::NotLive | LiveDelivery::Dialog => r.not_live.push(item.id),
            LiveDelivery::Failed => r.errors.push(format!("{}: paste did not land", item.id)),
        }
    }
    for alert in &plan.raise {
        match raise(alert) {
            Ok(true) => r.raised.push(alert.key.clone()),
            Ok(false) => {}
            Err(e) => r.errors.push(format!("{}: {e}", alert.key)),
        }
    }
    for key in &plan.ack {
        if let Some(item) = attention::live_by_key(key) {
            match attention::ack(&item.id) {
                Ok(_) => r.acked.push(key.clone()),
                Err(e) => r.errors.push(format!("{key}: {e}")),
            }
        }
    }
    r
}

/// Raise (or refresh in place) one alert. `true` when it is new.
fn raise(alert: &Alert) -> std::io::Result<bool> {
    let (repo, branch) = agent_target(&alert.seat).unzip();
    let out = attention::add_new(
        &NewAttn {
            source: "queue",
            title: &alert.title,
            severity: alert.severity,
            repo,
            branch,
            key: Some(&alert.key),
            detail: Some(&alert.detail),
            link: alert.link.as_deref(),
            ..Default::default()
        },
        &today(),
    )?;
    Ok(!out.skipped)
}

/// `coord-tick` — the periodic queue sweep plus the watchdog tick, on the `tick-5m` cluster.
pub(crate) fn job_coord_tick(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let mut db = match CoordDb::open_default() {
        Ok(db) => db,
        Err(e) => {
            crate::cmd::jobs::native_job_line(&format!("coord-tick: {e}"), true);
            return 1;
        }
    };
    let now = bn_core::util::now_epoch_secs();
    let (_, report) = sweep_all(cfg, &mut db, now, false);
    let dogs = crate::cmd::watchdog::tick(cfg, &mut db, now, None);
    let failed = !report.errors.is_empty() || !dogs.errors.is_empty();
    let line = format!("{}; {}", report.summary(), dogs.summary());
    crate::cmd::jobs::native_job_line(&line, failed);
    i32::from(failed)
}
