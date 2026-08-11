//! The staff cost ledger — what each agent's runs actually spent.
//!
//! Split out of `staff.rs` for the 1000-line ratchet, along a seam that file already marked with a
//! banner. It is a read model over the `staff_run` events the pipeline appends, and nothing here
//! decides anything: rows in, rows out, bucketed by month.
//!
//! Kept apart from the pipeline that writes those events so the accounting can be read — and
//! tested — without touching the machinery that dispatches an agent.
use super::*;

/// One row of the staff cost ledger — appended per `bn staff run` as `staff_run` events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEntry {
    pub agent: String,
    /// The run's logical date (`YYYY-MM-DD`), used for month bucketing.
    pub date: String,
    /// Event timestamp (epoch secs).
    pub ts: i64,
    /// Estimated tokens spent (0 for a pre-dispatch-failed run).
    pub tokens: u64,
    /// Dispatch wall-clock in milliseconds (0 when no dispatch happened).
    pub wall_ms: u64,
    pub status: RunStatus,
}

/// Rough token estimate for a dispatched turn — about 4 characters per token over the prompt sent
/// plus the output received.
pub fn estimate_tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

/// The `YYYY-MM` month bucket of a `YYYY-MM-DD` date (or the whole string if it is shorter).
pub fn month_of(date: &str) -> &str {
    if date.len() >= 7 {
        &date[..7]
    } else {
        date
    }
}

/// The whole cost ledger, oldest first — every `staff_run` event read back as a [`LedgerEntry`].
pub fn ledger_entries() -> Vec<LedgerEntry> {
    crate::events::read_all()
        .into_iter()
        .filter(|e| e.kind == "staff_run")
        .filter_map(|e| {
            let agent = e.fields.get("agent").and_then(|v| v.as_str())?.to_string();
            let status = e
                .fields
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(RunStatus::parse)
                .unwrap_or_else(|| match e.fields.get("ok").and_then(|v| v.as_bool()) {
                    Some(true) => RunStatus::Ok,
                    _ => RunStatus::Fail,
                });
            let date = e
                .fields
                .get("date")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| crate::util::local_date_of(e.ts));
            let tokens = e.fields.get("tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let wall_ms = e
                .fields
                .get("wall_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Some(LedgerEntry {
                agent,
                date,
                ts: e.ts,
                tokens,
                wall_ms,
                status,
            })
        })
        .collect()
}
