//! Watchdogs: durable, scheduled checks that wake a seat, because a seat cannot wake itself.
//!
//! A watchdog lives in the coordination DB, so it survives a reboot, a logout and a crashed
//! agent — unlike a `sleep N && …` in the background. The periodic tick evaluates every due one
//! ([`super::watchdog_eval`]); what it does when it fires is decided by its kind
//! ([`super::policy`]).

use super::{normalize_seat, CoordDb, CoordError, Result};
use crate::clock::Clock;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use std::fmt;

/// What a watchdog checks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Deliver a message at a time (or every interval).
    Reminder,
    /// Nudge the seat only if it has gone idle.
    Keepalive,
    /// Warn the seat when its context window crosses a threshold.
    Context,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Reminder, Kind::Keepalive, Kind::Context];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Reminder => "reminder",
            Kind::Keepalive => "keepalive",
            Kind::Context => "context",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s.trim())
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Policy knobs a kind reads. Stored as JSON so a new knob needs no migration.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, Serialize)]
pub struct Spec {
    /// `context`: fire at or above this percentage of the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_pct: Option<u8>,
    /// `keepalive`: how long the seat must have been quiet, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<i64>,
}

/// A watchdog to register.
#[derive(Debug, Clone)]
pub struct NewWatchdog {
    pub kind: Kind,
    pub target: String,
    pub text: Option<String>,
    pub spec: Spec,
    /// First evaluation, in seconds from now.
    pub first_in_secs: i64,
    /// Re-evaluate this often; `None` is a one-shot.
    pub every_secs: Option<i64>,
    pub expires_in_secs: Option<i64>,
    pub max_fires: Option<i64>,
    pub created_by: String,
}

/// One registered watchdog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Watchdog {
    /// `wd<N>`.
    pub id: String,
    #[serde(skip)]
    pub num: i64,
    pub kind: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub spec: Spec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every_secs: Option<i64>,
    pub next_due_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fires: Option<i64>,
    pub fires: i64,
    pub state: String,
    pub created_by: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_eval_at: Option<i64>,
    pub defer_count: i64,
}

impl Watchdog {
    pub fn kind(&self) -> Option<Kind> {
        Kind::parse(&self.kind)
    }

    pub fn is_active(&self) -> bool {
        self.state == "active"
    }
}

/// One evaluation of a watchdog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Eval {
    pub ts: i64,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `wd7` for watchdog 7.
pub fn wdid(id: i64) -> String {
    format!("wd{id}")
}

/// Parse `wd7` or `7`.
pub fn parse_wdid(s: &str) -> Option<i64> {
    let s = s.trim();
    s.strip_prefix("wd")
        .unwrap_or(s)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
}

pub(super) const WD_COLUMNS: &str =
    "id, kind, target, text, spec_json, interval_secs, next_due_at, \
     expires_at, max_fires, fires, state, created_by, created_at, stop_reason, \
     last_eval_at, defer_count";

pub(super) fn wd_from_row(r: &Row<'_>) -> rusqlite::Result<Watchdog> {
    let num: i64 = r.get(0)?;
    let spec: String = r.get(4)?;
    Ok(Watchdog {
        id: wdid(num),
        num,
        kind: r.get(1)?,
        target: r.get(2)?,
        text: r.get(3)?,
        spec: serde_json::from_str(&spec).unwrap_or_default(),
        every_secs: r.get(5)?,
        next_due_at: r.get(6)?,
        expires_at: r.get(7)?,
        max_fires: r.get(8)?,
        fires: r.get(9)?,
        state: r.get(10)?,
        created_by: r.get(11)?,
        created_at: r.get(12)?,
        stop_reason: r.get(13)?,
        last_eval_at: r.get(14)?,
        defer_count: r.get(15)?,
    })
}

pub(super) fn load_wd(conn: &rusqlite::Connection, id: i64) -> Result<Watchdog> {
    conn.query_row(
        &format!("SELECT {WD_COLUMNS} FROM watchdogs WHERE id = ?1"),
        [id],
        wd_from_row,
    )
    .optional()?
    .ok_or_else(|| CoordError::NotFound(wdid(id)))
}

pub(super) fn insert_wd(conn: &rusqlite::Connection, now: i64, new: &NewWatchdog) -> Result<i64> {
    let target = normalize_seat(&new.target)?;
    if new.every_secs.is_some_and(|e| e < 60) {
        return Err(CoordError::Invalid(
            "a repeating watchdog needs an interval of at least a minute".into(),
        ));
    }
    let spec = serde_json::to_string(&new.spec).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO watchdogs(kind, target, text, spec_json, interval_secs, next_due_at,
             expires_at, max_fires, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            new.kind.as_str(),
            target,
            new.text,
            spec,
            new.every_secs,
            now + new.first_in_secs.max(0),
            new.expires_in_secs.map(|e| now + e),
            new.max_fires,
            new.created_by,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

impl CoordDb {
    /// Register a watchdog.
    pub fn register_watchdog(&mut self, clock: &dyn Clock, new: &NewWatchdog) -> Result<Watchdog> {
        let id = insert_wd(&self.conn, clock.now_epoch_secs(), new)?;
        load_wd(&self.conn, id)
    }

    pub fn watchdog(&self, id: i64) -> Result<Watchdog> {
        load_wd(&self.conn, id)
    }

    /// Watchdogs, newest first: active ones only unless `all`, optionally for one target seat.
    pub fn watchdogs(
        &self,
        all: bool,
        target: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Watchdog>> {
        let mut sql = format!("SELECT {WD_COLUMNS} FROM watchdogs WHERE 1 = 1");
        if !all {
            sql.push_str(" AND state = 'active'");
        }
        if target.is_some() {
            sql.push_str(" AND target = ?1");
        }
        sql.push_str(&format!(" ORDER BY id DESC LIMIT {limit}"));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: rusqlite::Result<Vec<Watchdog>> = match target {
            Some(t) => stmt.query_map([t], wd_from_row)?.collect(),
            None => stmt.query_map([], wd_from_row)?.collect(),
        };
        Ok(rows?)
    }

    /// Stop a watchdog; the tick skips it from now on. Stopping a stopped one is a no-op.
    pub fn stop_watchdog(&mut self, clock: &dyn Clock, id: i64, reason: &str) -> Result<Watchdog> {
        let wd = load_wd(&self.conn, id)?;
        if wd.is_active() {
            self.conn.execute(
                "UPDATE watchdogs SET state = 'stopped', stopped_at = ?2, stop_reason = ?3
                 WHERE id = ?1",
                params![id, clock.now_epoch_secs(), reason],
            )?;
        }
        load_wd(&self.conn, id)
    }

    /// A watchdog's evaluation history, newest first.
    pub fn watchdog_evals(&self, id: i64, limit: usize) -> Result<Vec<Eval>> {
        load_wd(&self.conn, id)?;
        let mut stmt = self.conn.prepare(
            "SELECT ts, outcome, detail FROM watchdog_evals WHERE watchdog_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit as i64], |r| {
            Ok(Eval {
                ts: r.get(0)?,
                outcome: r.get(1)?,
                detail: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
