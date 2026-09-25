//! The highest bn id a Forge task list has handed out, remembered per endpoint.
//!
//! A load lists open activities and those completed in the last
//! [`super::store::COMPLETED_WINDOW_DAYS`] days. A task finished before that is not in the list, so
//! its `t<N>` is invisible to `next_id`, which would hand it out again and give two activities the
//! same handle. The mark closes that gap from two sides:
//!
//! - **Remembered:** every load records the highest id it saw (and every id this machine creates)
//!   in `<state_dir>/forge-ids.json`, keyed by endpoint. The mark only goes up.
//! - **Refreshed:** a task's id is in every load from the moment it is assigned until a week after
//!   it is completed. So a machine that has loaded within the last week has seen every id a task
//!   outside the list can carry. A mark older than [`STALE_SECS`], or none at all (a new machine),
//!   is refreshed with one list over [`ID_WINDOW_DAYS`], the widest window Forge serves.
//!
//! The residue is a brand-new machine and a task completed more than a year ago, which the widest
//! list cannot see.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::client::BnTaskRow;

/// The mark file's name inside the state directory.
pub const FILE: &str = "forge-ids.json";

/// The widest `includeCompletedSinceDays` Forge accepts.
pub const ID_WINDOW_DAYS: u32 = 365;

/// How old a mark may be before the next load refreshes it with the wide list: a day inside the
/// completed window, so a task completed just before the last load is still in the next one.
pub const STALE_SECS: i64 = (super::store::COMPLETED_WINDOW_DAYS as i64 - 1) * 86_400;

/// How often an unchanged mark is re-stamped, so every load does not rewrite the file.
const RESTAMP_SECS: i64 = 3_600;

/// What is known for one endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// The highest `t<N>` seen or created.
    pub max: u64,
    /// When a load last confirmed it (epoch seconds).
    pub checked: i64,
}

impl Mark {
    /// Whether the next load must refresh this mark from the wide list.
    pub fn is_stale(mark: Option<Mark>, now: i64) -> bool {
        mark.is_none_or(|m| now - m.checked > STALE_SECS)
    }
}

/// The `t<N>` number of an id, when it is one.
pub fn number(id: &str) -> Option<u64> {
    id.trim()
        .strip_prefix('t')
        .and_then(|n| n.parse::<u64>().ok())
}

/// The highest bn id stored in `rows`' meta.
pub fn max_in(rows: &[BnTaskRow]) -> u64 {
    rows.iter()
        .filter_map(|r| r.agent_meta.get("id").and_then(|id| number(id)))
        .max()
        .unwrap_or(0)
}

/// The mark for one endpoint in one state file.
#[derive(Debug, Clone)]
pub struct IdMark {
    path: PathBuf,
    url: String,
}

impl IdMark {
    pub fn new(path: impl Into<PathBuf>, url: &str) -> IdMark {
        IdMark {
            path: path.into(),
            url: url.trim().to_string(),
        }
    }

    fn read_all(&self) -> BTreeMap<String, Mark> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// The endpoint's mark, if one was ever written.
    pub fn get(&self) -> Option<Mark> {
        self.read_all().get(&self.url).copied()
    }

    /// Record `max` for this endpoint. `confirmed` means a load just saw the whole id range, so
    /// the check time moves too. Writes only when something changed or the stamp is an hour old.
    pub fn record(&self, max: u64, now: i64, confirmed: bool) -> io::Result<()> {
        let mut all = self.read_all();
        let old = all.get(&self.url).copied();
        let mut next = old.unwrap_or_default();
        next.max = next.max.max(max);
        if confirmed {
            next.checked = now;
        }
        let changed = match old {
            None => true,
            Some(o) => o.max != next.max || (confirmed && now - o.checked >= RESTAMP_SECS),
        };
        if !changed {
            return Ok(());
        }
        all.insert(self.url.clone(), next);
        crate::store::save_json_atomic(&self.path, &all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_only_rises_and_restamps_at_most_hourly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let a = IdMark::new(&path, "https://forge.example/mcp");
        let b = IdMark::new(&path, "http://localhost:5000/mcp");
        assert_eq!(a.get(), None);
        assert!(Mark::is_stale(a.get(), 1_000));

        a.record(12, 1_000, true).unwrap();
        b.record(3, 1_000, true).unwrap();
        a.record(9, 1_100, true).unwrap();
        assert_eq!(
            a.get(),
            Some(Mark {
                max: 12,
                checked: 1_000
            }),
            "lower max ignored, stamp inside the hour not rewritten"
        );
        a.record(13, 1_200, false).unwrap();
        assert_eq!(a.get().map(|m| (m.max, m.checked)), Some((13, 1_000)));
        a.record(13, 1_000 + RESTAMP_SECS, true).unwrap();
        assert_eq!(a.get().map(|m| m.checked), Some(1_000 + RESTAMP_SECS));
        assert_eq!(b.get().map(|m| m.max), Some(3), "endpoints are separate");

        assert!(!Mark::is_stale(a.get(), 1_000 + STALE_SECS));
        assert!(Mark::is_stale(
            a.get(),
            1_000 + RESTAMP_SECS + STALE_SECS + 1
        ));
    }
}
