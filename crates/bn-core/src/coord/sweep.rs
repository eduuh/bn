//! The sweep: what the periodic `coord-tick` does about open rows. [`plan`] is pure — it looks at
//! the rows and the attention items already raised and says what to do — so every rule here is
//! unit-tested without tmux, a notes tree or an event log. The caller carries the plan out.
//!
//! What it catches is silence. A row nobody claimed, a claim past its deadline, a park with
//! nothing to wake it: each looks, from outside, exactly like an agent that crashed.

use super::{agent_target, is_human, CoordDb, QItem, QState, Result, Tier};
use rusqlite::params;

/// Every attention key the queue raises starts with this, so the sweep can find (and ack) its own.
pub const KEY_PREFIX: &str = "queue:";

/// Seconds between nudges of the same pending row.
pub const NUDGE_EVERY_SECS: i64 = 15 * 60;
/// Nudges before the sweep stops typing at a seat and leaves it to the overdue alert.
pub const NUDGE_MAX: i64 = 5;

/// An attention item the queue wants raised (or kept raised).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub key: String,
    pub title: String,
    pub severity: &'static str,
    pub detail: String,
    /// The seat the alert is about, for the item's repo/branch.
    pub seat: String,
    /// Evidence to judge by, for an alert that asks a person to decide.
    pub link: Option<String>,
}

/// What one sweep should do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Rows whose note mirror failed last time.
    pub mirror_retry: Vec<i64>,
    /// Pending agent rows to (re-)nudge.
    pub nudge: Vec<i64>,
    /// Alerts that should be live after this sweep.
    pub raise: Vec<Alert>,
    /// Live queue alert keys whose condition is over.
    pub ack: Vec<String>,
}

pub fn overdue_key(item: &QItem) -> String {
    format!("{KEY_PREFIX}overdue:{}", item.id)
}

pub fn human_key(item: &QItem) -> String {
    format!("{KEY_PREFIX}human:{}", item.id)
}

/// The one line typed into a destination agent's pane. It informs; it does not claim.
pub fn nudge_text(item: &QItem) -> String {
    let first = item.body.lines().next().unwrap_or("").trim();
    format!(
        "[bn queue {} from {}] {first} — take it with: bn queue claim {}",
        item.id, item.source, item.id
    )
}

/// Should this pending row be nudged now? First time as soon as it is seen, then every
/// [`NUDGE_EVERY_SECS`] up to [`NUDGE_MAX`] times.
pub fn due_for_nudge(item: &QItem, nudged_at: Option<i64>, now: i64) -> bool {
    if item.qstate() != QState::Pending || agent_target(&item.dest).is_none() {
        return false;
    }
    if item.nudge_count >= NUDGE_MAX {
        return false;
    }
    nudged_at.is_none_or(|at| now - at >= NUDGE_EVERY_SECS)
}

fn severity(tier: Tier) -> &'static str {
    if tier == Tier::Critical {
        "urgent"
    } else {
        "warn"
    }
}

/// The alert a row deserves right now, if any.
fn alert_for(item: &QItem, has_wake: bool, now: i64) -> Option<Alert> {
    let window = item.qtier().window_secs();
    let title_body = item.body.lines().next().unwrap_or("").trim();
    let (key, title, detail) = match item.qstate() {
        QState::InProgress if item.is_overdue(now) => (
            overdue_key(item),
            format!("{} overdue at {}: {title_body}", item.id, item.dest),
            format!(
                "Claimed by {} and past its {} deadline. Close it with a note, hand it on, or \
                 block it with a wake trigger: bn queue show {}",
                item.owner.as_deref().unwrap_or(&item.dest),
                item.tier,
                item.id
            ),
        ),
        QState::Pending if is_human(&item.dest) => (
            human_key(item),
            format!("{} for you from {}: {title_body}", item.id, item.source),
            format!(
                "A queue row is waiting on {}. Claim it with `bn queue claim {}`, or answer and \
                 close it with `bn queue done {} --note …`.",
                item.dest, item.id, item.id
            ),
        ),
        QState::Pending if now - item.created_at > window => (
            overdue_key(item),
            format!("{} unclaimed at {}: {title_body}", item.id, item.dest),
            format!(
                "Nobody has claimed it within its {} window. Is {} alive? `bn agent live`, or \
                 hand it elsewhere: bn queue handoff {} --to <seat> --note …",
                item.tier, item.dest, item.id
            ),
        ),
        QState::Blocked if item.blocked_on.as_deref().is_some_and(is_human) => {
            let who = item.blocked_on.clone().unwrap_or_default();
            return Some(Alert {
                key: human_key(item),
                title: format!("{} waits on {who}: {title_body}", item.id),
                severity: severity(item.qtier()),
                detail: format!(
                    "{} Decide, then unpark it: bn queue resolve {} --note \"<decision>\"",
                    item.block_summary.as_deref().unwrap_or(""),
                    item.id
                ),
                seat: who,
                link: item.evidence_ref.clone(),
            });
        }
        QState::Blocked if !has_wake => (
            overdue_key(item),
            format!("{} parked with nothing to wake it", item.id),
            format!(
                "Blocked on {} with no watchdog. A silent park looks like a crash: attach a wake \
                 trigger or resolve it.",
                item.blocked_on.as_deref().unwrap_or("?")
            ),
        ),
        _ => return None,
    };
    Some(Alert {
        key,
        title,
        severity: severity(item.qtier()),
        detail,
        seat: item.dest.clone(),
        link: None,
    })
}

/// Plan one sweep over the open `rows`. `nudged_at(id)` and `has_wake(id)` read per-row fields
/// the listing doesn't carry; `live_keys` are the queue alert keys currently raised.
pub fn plan(
    rows: &[QItem],
    nudged_at: &dyn Fn(i64) -> Option<i64>,
    has_wake: &dyn Fn(i64) -> bool,
    live_keys: &[String],
    now: i64,
) -> Plan {
    let mut out = Plan::default();
    for item in rows {
        if item.mirror_state == "pending" {
            out.mirror_retry.push(item.num);
        }
        if due_for_nudge(item, nudged_at(item.num), now) {
            out.nudge.push(item.num);
        }
        if let Some(alert) = alert_for(item, has_wake(item.num), now) {
            out.raise.push(alert);
        }
    }
    out.ack = live_keys
        .iter()
        .filter(|k| k.starts_with(KEY_PREFIX) && !out.raise.iter().any(|a| &a.key == *k))
        .cloned()
        .collect();
    out
}

impl CoordDb {
    /// When row `id` was last nudged, if ever.
    pub fn nudged_at(&self, id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT nudged_at FROM qitems WHERE id = ?1", [id], |r| {
                r.get(0)
            })?)
    }

    /// Whether row `id` has a wake trigger attached.
    pub fn has_wake(&self, id: i64) -> Result<bool> {
        let w: Option<i64> = self.conn.query_row(
            "SELECT wake_watchdog_id FROM qitems WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(w.is_some())
    }

    /// Record that row `id` was typed into its seat's pane. A nudge changes no state — a send is
    /// not a claim — but it is history, so it lands in the transitions.
    pub fn record_nudge(&mut self, id: i64, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE qitems SET nudged_at = ?2, nudge_count = nudge_count + 1 WHERE id = ?1",
            params![id, now],
        )?;
        tx.execute(
            "INSERT INTO qtransitions(qitem_id, ts, actor, verb) VALUES (?1, ?2, 'bn', 'nudge')",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}
