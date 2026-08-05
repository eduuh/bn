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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item {
            id: id.into(),
            text: "x".into(),
            created: "2026-06-19".into(),
            done: None,
            done_at: None,
            link: None,
            pct: None,
            est: None,
            actual: None,
            q: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn empty_section_starts_at_one() {
        assert_eq!(next_id(&[], 't'), "t1");
    }

    #[test]
    fn tracks_max_not_count_never_reuses() {
        // t2 deleted: max is 3, next is t4 (not t2, not t3).
        let items = [item("t1"), item("t3")];
        assert_eq!(next_id(&items, 't'), "t4");
    }

    #[test]
    fn ignores_foreign_and_malformed_ids() {
        let items = [item("t1"), item("d9"), item("tX"), item("t2")];
        assert_eq!(next_id(&items, 't'), "t3");
    }
}
