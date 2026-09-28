//! The note mirror: a row addressed to an agent seat shows up as a `[q<N>]` todo in that branch's
//! note, so the agent sees it where it already looks (`bn cat`, `bn_context`), and closing the row
//! ticks the todo.
//!
//! The DB is the record; the note is a projection. Every mirror write happens after the row's
//! transaction has committed, never inside it, so a slow or locked note can't hold the queue's
//! write lock. What happened is stored on the row (`mirror_state`) so a sweep can retry.

use super::{agent_target, CoordDb, QItem, Result};
use crate::commands::{self, CmdError};
use crate::resolver::{self, Config, Resolved};
use rusqlite::params;

/// How long the mirrored todo's text may run before it is cut. The row keeps the full body.
const TODO_TEXT_MAX: usize = 120;

/// What a mirror write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// The todo exists (or was ticked); carries its `t<N>` id.
    Ok(String),
    /// Nothing to mirror into: a human seat, or a branch with no note (a mirror never creates
    /// a note, so a typo'd seat can't litter the notes tree).
    Skipped,
    /// The write failed; the sweep retries it.
    Failed(String),
}

impl MirrorOutcome {
    /// The `mirror_state` column value.
    pub fn state(&self) -> &'static str {
        match self {
            MirrorOutcome::Ok(_) => "ok",
            MirrorOutcome::Skipped => "skipped",
            MirrorOutcome::Failed(_) => "pending",
        }
    }
}

/// The note a row's destination seat mirrors into, if it is an agent seat with a note.
fn target(cfg: &Config, seat: &str) -> Option<Resolved> {
    let (repo, branch) = agent_target(seat)?;
    let ctx = resolver::resolve_target(
        cfg,
        std::path::Path::new("."),
        Some(repo.to_string()),
        Some(branch.to_string()),
    )
    .ok()?;
    cfg.note_file(&ctx).exists().then_some(ctx)
}

/// The todo line: `[q12] <first line of the body> (from <source>)`. The `[q12]` tag is the dedup
/// key, so re-mirroring the same row never adds a second todo.
pub fn todo_text(item: &QItem) -> String {
    let first = item.body.lines().next().unwrap_or("").trim();
    let body: String = if first.chars().count() > TODO_TEXT_MAX {
        let cut: String = first.chars().take(TODO_TEXT_MAX).collect();
        format!("{cut}…")
    } else {
        first.to_string()
    };
    format!("[{}] {body} (from {})", item.id, item.source)
}

/// Add `item`'s todo to its destination's note.
pub fn open_mirror(cfg: &Config, item: &QItem, today: &str) -> MirrorOutcome {
    let Some(ctx) = target(cfg, &item.dest) else {
        return MirrorOutcome::Skipped;
    };
    let tag = format!("[{}]", item.id);
    match commands::add(cfg, &ctx, "todo", &todo_text(item), Some(&tag), today) {
        Ok(added) => MirrorOutcome::Ok(added.id),
        Err(e) => MirrorOutcome::Failed(e.message()),
    }
}

/// Tick `item`'s mirrored todo. A todo that is already done, or was never mirrored, is fine.
pub fn close_mirror(cfg: &Config, item: &QItem, today: &str) -> MirrorOutcome {
    let Some(todo) = item.mirror_todo.as_deref() else {
        return MirrorOutcome::Skipped;
    };
    let Some(ctx) = target(cfg, &item.dest) else {
        return MirrorOutcome::Skipped;
    };
    match commands::done(cfg, &ctx, todo, today, None) {
        Ok(_) | Err(CmdError::AlreadyDone(_)) => MirrorOutcome::Ok(todo.to_string()),
        Err(e) => MirrorOutcome::Failed(e.message()),
    }
}

impl CoordDb {
    /// Record a mirror write's outcome on row `id`. A failed close keeps the todo id so the retry
    /// knows which todo to tick.
    pub fn set_mirror(&mut self, id: i64, outcome: &MirrorOutcome) -> Result<()> {
        let todo = match outcome {
            MirrorOutcome::Ok(t) => Some(t.as_str()),
            _ => None,
        };
        self.conn.execute(
            "UPDATE qitems SET mirror_todo = COALESCE(?2, mirror_todo), mirror_state = ?3
             WHERE id = ?1",
            params![id, todo, outcome.state()],
        )?;
        Ok(())
    }

    /// Closed or handed-off rows whose closing mirror write failed. The open-row listing the sweep
    /// works from never includes them, so without this their `[q<N>]` todo would stay open.
    pub fn closed_with_pending_mirror(&self) -> Result<Vec<QItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM qitems WHERE mirror_state = 'pending'
               AND state NOT IN ('pending', 'in-progress', 'blocked') ORDER BY id",
            super::queue_read::COLUMNS
        ))?;
        let rows = stmt.query_map([], super::queue_read::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
