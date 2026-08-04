//! Per-section monotonic id allocation. Port of `_next_id` (bn:774): scan the owning
//! section's ids matching `^<prefix>[0-9]+$`, take the numeric max, return
//! `<prefix><max+1>`. Tracks max (not count) so ids of deleted items are never reused.

use crate::model::Item;

/// Next id for `prefix` given the section's existing items. Empty section ⇒ `<prefix>1`.
pub fn next_id(items: &[Item], prefix: char) -> String {
    next_id_from_ids(items.iter().map(|i| i.id.as_str()), prefix)
}

/// Next id for `prefix` given an iterator over the existing ids — the id-only core that
/// [`next_id`] delegates to, so non-[`Item`] lists (e.g. daily log entries) can allocate ids
/// with the same monotonic-max semantics.
pub fn next_id_from_ids<'a>(ids: impl Iterator<Item = &'a str>, prefix: char) -> String {
    let max = ids.filter_map(|id| parse_id(id, prefix)).max().unwrap_or(0);
    format!("{prefix}{}", max + 1)
}

/// Parse the integer part of an id that exactly matches `<prefix><digits>`.
/// Foreign-prefix or malformed ids ⇒ None (ignored), mirroring the oracle's awk regex.
fn parse_id(id: &str, prefix: char) -> Option<u64> {
    let rest = id.strip_prefix(prefix)?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}


