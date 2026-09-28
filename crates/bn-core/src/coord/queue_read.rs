//! Reading rows and their history back out. Nothing here writes.

use super::{qid, CoordDb, CoordError, QState, Result, Tier};
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;

/// One queue row, as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QItem {
    /// `q<N>`.
    pub id: String,
    #[serde(skip)]
    pub num: i64,
    pub dest: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub body: String,
    pub state: String,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_watchdog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_required_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_todo: Option<String>,
    pub mirror_state: String,
    pub nudge_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl QItem {
    pub fn qstate(&self) -> QState {
        QState::parse(&self.state).unwrap_or(QState::Pending)
    }

    pub fn qtier(&self) -> Tier {
        Tier::parse(&self.tier).unwrap_or_default()
    }

    /// In progress past its closure deadline at `now`.
    pub fn is_overdue(&self, now: i64) -> bool {
        self.qstate() == QState::InProgress && self.closure_required_at.is_some_and(|d| d < now)
    }
}

/// One entry of a row's append-only history.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transition {
    pub ts: i64,
    pub actor: String,
    pub verb: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
}

/// Which rows [`CoordDb::list`] returns. Empty `states` means every non-terminal state.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Rows destined for, or owned by, this seat.
    pub seat: Option<String>,
    pub states: Vec<QState>,
    /// Only in-progress rows past their deadline at this epoch.
    pub overdue_at: Option<i64>,
    pub limit: Option<usize>,
}

pub(super) const COLUMNS: &str = "id, dest, source, owner, body, state, tier, parent_id, next_id, \
     ref, blocked_on, closure_note, closure_ref, claimed_at, closure_required_at, mirror_todo, \
     mirror_state, nudge_count, created_at, updated_at, block_summary, evidence_ref, \
     wake_watchdog_id";

pub(super) fn from_row(r: &Row<'_>) -> rusqlite::Result<QItem> {
    let num: i64 = r.get(0)?;
    let link = |n: Option<i64>| n.map(qid);
    Ok(QItem {
        id: qid(num),
        num,
        dest: r.get(1)?,
        source: r.get(2)?,
        owner: r.get(3)?,
        body: r.get(4)?,
        state: r.get(5)?,
        tier: r.get(6)?,
        parent: link(r.get(7)?),
        next: link(r.get(8)?),
        ref_: r.get(9)?,
        blocked_on: r.get(10)?,
        closure_note: r.get(11)?,
        closure_ref: r.get(12)?,
        claimed_at: r.get(13)?,
        closure_required_at: r.get(14)?,
        mirror_todo: r.get(15)?,
        mirror_state: r.get(16)?,
        nudge_count: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
        block_summary: r.get(20)?,
        evidence_ref: r.get(21)?,
        wake_watchdog: r.get::<_, Option<i64>>(22)?.map(super::watchdog::wdid),
    })
}

/// Load row `id` on any connection or transaction.
pub(super) fn load(conn: &Connection, id: i64) -> Result<QItem> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM qitems WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| CoordError::NotFound(qid(id)))
}

impl CoordDb {
    /// Row `id`.
    pub fn get(&self, id: i64) -> Result<QItem> {
        load(&self.conn, id)
    }

    /// Rows matching `filter`, oldest first.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<QItem>> {
        let states: Vec<QState> = if filter.overdue_at.is_some() {
            vec![QState::InProgress]
        } else if filter.states.is_empty() {
            QState::ALL
                .into_iter()
                .filter(|s| !s.is_terminal())
                .collect()
        } else {
            filter.states.clone()
        };
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let marks: Vec<String> = states
            .iter()
            .map(|s| {
                args.push(s.as_str().to_string().into());
                format!("?{}", args.len())
            })
            .collect();
        let mut sql = format!(
            "SELECT {COLUMNS} FROM qitems WHERE state IN ({})",
            marks.join(",")
        );
        if let Some(seat) = &filter.seat {
            args.push(seat.clone().into());
            let n = args.len();
            sql.push_str(&format!(" AND (dest = ?{n} OR owner = ?{n})"));
        }
        if let Some(now) = filter.overdue_at {
            args.push(now.into());
            sql.push_str(&format!(
                " AND closure_required_at IS NOT NULL AND closure_required_at < ?{}",
                args.len()
            ));
        }
        sql.push_str(" ORDER BY id");
        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Row `id`'s history, oldest first.
    pub fn transitions(&self, id: i64) -> Result<Vec<Transition>> {
        load(&self.conn, id)?;
        let mut stmt = self.conn.prepare(
            "SELECT ts, actor, verb, from_state, to_state, note, ref
             FROM qtransitions WHERE qitem_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok(Transition {
                ts: r.get(0)?,
                actor: r.get(1)?,
                verb: r.get(2)?,
                from: r.get(3)?,
                to: r.get(4)?,
                note: r.get(5)?,
                ref_: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
