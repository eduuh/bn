//! Away mode — batched fleet supervision (firstmate's `/afk`, bn-native). While you're away, routine
//! agent activity is absorbed and only captain-relevant events (an agent that needs you, a stuck
//! one, an urgent attention item) are surfaced — batched into one digest when you return
//! (`bn afk end`). State is two append-only events in the same log as everything else
//! (`afk_start` / `afk_end`) — no new store, mirroring [`crate::focus`]. Open-ended: away until ended.

use crate::events;
use crate::wake::{ActionableWake, WakeQueue, WakeTarget};
use serde_json::Map;
use std::collections::BTreeSet;

/// Enter away mode: append `afk_start`. A second start while already away simply re-opens (the
/// latest event wins on read), so it's safe to call again.
pub fn start(now: i64) -> std::io::Result<()> {
    acknowledge_pending()?;
    events::append_at("afk_start", now, Map::new())
}

/// Leave away mode: append `afk_end`. Returns the epoch the away block began (for the "away for N"
/// digest header), or `None` when not away — so `bn afk end` can say "not away" without writing a
/// dangling event.
pub fn end(now: i64) -> std::io::Result<Option<i64>> {
    let Some(started) = active_since(now) else {
        return Ok(None);
    };
    events::append_at("afk_end", now, Map::new())?;
    Ok(Some(started))
}

/// Epoch seconds the current away block began, or `None` if not away. Away = the most recent
/// afk event is an `afk_start` with no `afk_end` after it.
pub fn active_since(_now: i64) -> Option<i64> {
    let events = events::read_all();
    let last = events
        .iter()
        .rev()
        .find(|e| e.kind == "afk_start" || e.kind == "afk_end")?;
    (last.kind == "afk_start").then_some(last.ts)
}

/// Whether away mode is active right now.
pub fn is_active(now: i64) -> bool {
    active_since(now).is_some()
}

/// Actionable wakes collected by the presence-gated AFK consumer during the current away block.
pub fn pending() -> std::io::Result<Vec<ActionableWake>> {
    WakeQueue::default().pending(&WakeTarget::Afk)
}

/// Acknowledge the supplied AFK deliveries without affecting watch/controller/orchestrator
/// consumers for the same wake.
pub fn acknowledge(event_ids: &BTreeSet<String>) -> std::io::Result<usize> {
    WakeQueue::default().acknowledge(&WakeTarget::Afk, event_ids)
}

fn acknowledge_pending() -> std::io::Result<()> {
    let event_ids = pending()?.into_iter().map(|wake| wake.event_id).collect();
    acknowledge(&event_ids)?;
    Ok(())
}
