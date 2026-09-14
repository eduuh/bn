//! Focus mode — a declared deep-work block that protects flow from non-urgent surfacing.
//!
//! Deep work fragments under a steady drip of nudges (next-action, session-start, tmux popups);
//! the proven mitigation is a *quiet window* where only genuinely urgent signals — a failing
//! cron, a true blocker — still interrupt, and everything routine is **suppressed and queued**
//! for a single review when the block ends. `bn focus start <dur>` opens the block, `bn focus
//! end` closes it (or it expires on its own), and `bn focus status` reports it.
//!
//! State lives in the same append-only `events.jsonl` as everything else (no new store, no
//! migration): a `focus_start { id, duration_secs }` event opens a block; a later `focus_end
//! { ref }` closes it early. Folded on read exactly like the attn / PR lifecycles — a block is
//! *active* when the latest `focus_start` hasn't been ended and its `start + duration` is still
//! in the future. The *queue* is derived, not stored: on end/expiry the caller drains the
//! current non-urgent [`crate::next`] actions (the ones the active block was holding back), so
//! the drain is always the live picture rather than a stale snapshot.

use crate::events::{self, Event};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// An active deep-work block, as folded from the event log.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Block {
    /// The block's id (`f<N>`), monotonic and never reused.
    pub id: String,
    /// Unix epoch seconds when the block began (the `focus_start` event's `ts`).
    pub started: i64,
    /// Declared length in seconds.
    pub duration_secs: i64,
    /// `started + duration_secs` — when the block expires on its own.
    pub ends_at: i64,
    /// Whole seconds remaining until `ends_at` (clamped at 0), relative to the query's `now`.
    pub remaining_secs: i64,
}

impl Block {
    /// Whole minutes remaining, rounded up so a block with any time left never reads "0m left".
    /// Drives the statusline label (`focus · 47m left`).
    pub fn remaining_mins(&self) -> i64 {
        (self.remaining_secs + 59) / 60
    }
}

/// Outcome of [`start`]: the new block's id and when it ends.
#[derive(Debug, Serialize)]
pub struct StartOutcome {
    pub id: String,
    pub ends_at: i64,
    pub duration_secs: i64,
}

/// Parse a human duration like `90m`, `2h`, `1d`, `45`, `1h30m` into seconds. A bare number is
/// taken as minutes (the common `bn focus start 90` case). Returns `None` for empty / malformed
/// input or a non-positive result, so the CLI can reject it rather than open a zero-length block.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // A bare integer is minutes.
    if let Ok(mins) = s.parse::<i64>() {
        return (mins > 0).then_some(mins * 60);
    }
    let mut total = 0i64;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            let unit = match ch.to_ascii_lowercase() {
                // `d` was once missing while a help text advertised `1d`, so the one example in
                // the help was the one input the parser rejected.
                'd' => 86_400,
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return None,
            };
            total = total.checked_add(n.checked_mul(unit)?)?;
        }
    }
    // A trailing number with no unit (e.g. "1h30") is taken as minutes — matches the bare-int rule.
    if !num.is_empty() {
        total = total.checked_add(num.parse::<i64>().ok()? * 60)?;
    }
    (total > 0).then_some(total)
}

/// Open a deep-work block of `duration_secs`, stamped at `now`. Appends `focus_start { id,
/// duration_secs }`. Returns the new block's id and end time. Starting while one is already
/// active simply opens a fresh block — the later one wins on read, extending the quiet window.
pub fn start(duration_secs: i64, now: i64) -> std::io::Result<StartOutcome> {
    let id = next_id();
    let mut fields = Map::new();
    fields.insert("id".into(), json!(id));
    fields.insert("duration_secs".into(), json!(duration_secs));
    events::append_at("focus_start", now, fields)?;
    Ok(StartOutcome {
        id,
        ends_at: now + duration_secs,
        duration_secs,
    })
}

/// End the active block early. Appends `focus_end { ref: <id> }` referencing the active block.
/// Returns the ended block (for the drain message) or `None` when no block was active — so `bn
/// focus end` can say "no focus block active" rather than write a dangling event.
pub fn end(now: i64) -> std::io::Result<Option<Block>> {
    let Some(block) = active(now) else {
        return Ok(None);
    };
    let mut fields = Map::new();
    fields.insert("ref".into(), json!(block.id));
    events::append_at("focus_end", now, fields)?;
    Ok(Some(block))
}

/// The currently active block, or `None`. Active = the most-recently-started block whose end
/// time is still in the future and which no later `focus_end` has closed. Folding the latest
/// start (not the first) means a second `bn focus start` cleanly extends the window.
pub fn active(now: i64) -> Option<Block> {
    // `now` is applied here, not inside the memo: the block list is pure log-derived state and so
    // is cacheable, while "is it running *right now*" changes every second and is not.
    running_at(open_blocks(), now)
}

/// The first block in `blocks` (latest start first) still running at `now`. Split out from
/// [`active`] so the `now` decision is testable without an event log behind it.
fn running_at(blocks: Vec<OpenBlock>, now: i64) -> Option<Block> {
    blocks.into_iter().find_map(|b| {
        let ends_at = b.ts + b.duration_secs;
        (ends_at > now).then(|| Block {
            id: b.id,
            started: b.ts,
            duration_secs: b.duration_secs,
            ends_at,
            remaining_secs: (ends_at - now).max(0),
        })
    })
}

/// A focus block exactly as the log records it, before `now` is taken into account.
#[derive(Serialize, Deserialize)]
struct OpenBlock {
    id: String,
    ts: i64,
    duration_secs: i64,
}

/// Open (not early-ended) focus blocks, latest start first — a pure function of the event log, so
/// it is memoised against that log's identity.
///
/// This matters because [`is_active`] is on the status bar's path (`bn staff feed --badge` gates
/// on it) and its honest answer meant parsing the whole 50 MB log every refresh — measured at
/// 227 ms, one full parse, several times a minute.
fn open_blocks() -> Vec<OpenBlock> {
    crate::event_memo::memoised("focus-blocks.json", "", || {
        let events = events::read_all();

        // ids that have been ended early.
        let ended: std::collections::HashSet<String> = events
            .iter()
            .filter(|e| e.kind == "focus_end")
            .filter_map(|e| field_str(e, "ref"))
            .collect();

        // The latest start wins, so scan in reverse file order (chronological append); the
        // still-running check belongs to the caller, which knows `now`.
        events
            .iter()
            .rev()
            .filter(|e| e.kind == "focus_start")
            .filter_map(|e| {
                let id = field_str(e, "id")?;
                if ended.contains(&id) {
                    return None;
                }
                Some(OpenBlock {
                    id,
                    ts: e.ts,
                    duration_secs: field_i64(e, "duration_secs")?,
                })
            })
            .collect()
    })
}

/// Whether a deep-work block is active right now — the gate [`crate::next`] consults to decide
/// whether to suppress non-urgent actions.
pub fn is_active(now: i64) -> bool {
    active(now).is_some()
}

/// Realized deep-work seconds inside the window `[start, end)`, summed over every block *started*
/// in it. A block's realized length is from its `focus_start` to whichever comes first: an early
/// `focus_end`, its declared expiry (`start + duration_secs`), or the window end `end` (so an
/// active block only counts the time already elapsed, never its unspent minutes). Pure over the
/// event slice — the caller picks the source (`events::read_all`) — so `bn insights` can size the
/// deep-work ratio without a new store. Blocks started outside the window are ignored even if
/// they overlap it; v1 attributes a block to the window it opened in.
pub fn secs_in_window(events: &[Event], start: i64, end: i64) -> i64 {
    // Earliest early-end per block id (a re-issued focus_end shouldn't lengthen the block).
    let mut ended: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for e in events.iter().filter(|e| e.kind == "focus_end") {
        if let Some(id) = field_str(e, "ref") {
            ended
                .entry(id)
                .and_modify(|t| *t = (*t).min(e.ts))
                .or_insert(e.ts);
        }
    }
    let mut total = 0i64;
    for e in events.iter().filter(|e| e.kind == "focus_start") {
        if e.ts < start || e.ts >= end {
            continue;
        }
        let Some(duration_secs) = field_i64(e, "duration_secs") else {
            continue;
        };
        let mut block_end = e.ts + duration_secs.max(0);
        if let Some(id) = field_str(e, "id") {
            if let Some(&eend) = ended.get(&id) {
                block_end = block_end.min(eend);
            }
        }
        total += (block_end.min(end) - e.ts).max(0);
    }
    total
}

/// Next `f<N>` id: one past the highest `f<N>` ever started, so ids of ended blocks are never
/// reused (matching the attn / note-section id discipline).
fn next_id() -> String {
    let max = events::read_all()
        .iter()
        .filter(|e| e.kind == "focus_start")
        .filter_map(|e| field_str(e, "id"))
        .filter_map(|id| id.strip_prefix('f').and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    format!("f{}", max + 1)
}

fn field_str(ev: &Event, key: &str) -> Option<String> {
    ev.fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn field_i64(ev: &Event, key: &str) -> Option<i64> {
    ev.fields.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, ts: i64, duration_secs: i64) -> OpenBlock {
        OpenBlock {
            id: id.to_string(),
            ts,
            duration_secs,
        }
    }

    #[test]
    fn a_running_block_is_active_and_reports_its_remaining_time() {
        let b = running_at(vec![block("a", 1_000, 600)], 1_100).expect("active");
        assert_eq!(b.id, "a");
        assert_eq!(b.ends_at, 1_600);
        assert_eq!(b.remaining_secs, 500);
    }

    #[test]
    fn an_expired_block_is_not_active() {
        assert!(running_at(vec![block("a", 1_000, 600)], 1_600).is_none());
        assert!(running_at(vec![block("a", 1_000, 600)], 9_999).is_none());
    }

    #[test]
    fn the_boundary_is_exclusive() {
        // ends_at > now, so a block is over at exactly its end second — matching the pre-memo
        // implementation, which used the same comparison.
        assert!(running_at(vec![block("a", 0, 10)], 9).is_some());
        assert!(running_at(vec![block("a", 0, 10)], 10).is_none());
    }

    #[test]
    fn the_latest_start_wins_and_an_expired_newer_block_falls_through() {
        // `open_blocks` yields latest-start-first; a newer block that has already expired must not
        // mask an older one that is still running.
        let blocks = vec![block("new", 1_000, 10), block("old", 900, 5_000)];
        let b = running_at(blocks, 1_500).expect("older block still running");
        assert_eq!(b.id, "old");
    }

    #[test]
    fn no_blocks_means_no_focus() {
        assert!(running_at(vec![], 1_000).is_none());
    }
}
