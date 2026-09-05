//! Memoising values that are derived purely from the append-only event log.
//!
//! `events::read_all()` parses the whole `events.jsonl`. That was cheap when the log was small; on
//! this machine it reached 50 MB / 200k lines with no retention, and one parse costs ~228 ms in a
//! release build. Several status-bar commands sit on that path and run every `status-interval`
//! (5s here) — the badge parsed it twice per draw, and the staff badge once more through
//! [`crate::focus`].
//!
//! Anything derived *only* from that log needs no time-based TTL, which would trade staleness for
//! speed on signals you want current. Keying on the log's `(mtime, len)` is exact instead: an
//! append changes the length, so a new event invalidates every memo the moment it lands. Length is
//! part of the key because two writes inside one mtime second share an mtime, and keying on mtime
//! alone would serve the pre-append answer.
//!
//! `key` carries any *other* input the value depends on — today's date for the badge, since a
//! snoozed item opens when the date rolls with no event written. A value that depends on `now`
//! must not be memoised directly; memoise the log-derived part and apply `now` to it at the call
//! site, as [`crate::focus::active`] does.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::resolver::state_dir;

#[derive(Serialize, Deserialize)]
struct Memo<T> {
    events_mtime: i64,
    events_len: u64,
    key: String,
    value: T,
}

/// `(mtime, len)` of the event log, or `None` when it cannot be stated.
pub fn identity() -> Option<(i64, u64)> {
    let meta = std::fs::metadata(crate::events::log_path()).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())?;
    Some((mtime, meta.len()))
}

fn read_fresh<T: DeserializeOwned>(path: &Path, id: (i64, u64), key: &str) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    let memo = serde_json::from_str::<Memo<T>>(&text).ok()?;
    (memo.events_mtime == id.0 && memo.events_len == id.1 && memo.key == key).then_some(memo.value)
}

fn write_memo<T: Serialize>(
    path: &Path,
    id: (i64, u64),
    key: &str,
    value: &T,
) -> std::io::Result<()> {
    let memo = Memo {
        events_mtime: id.0,
        events_len: id.1,
        key: key.to_string(),
        value,
    };
    let json = serde_json::to_string(&memo)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)
}

/// Return `compute()`'s result, reusing a memo in `<state_dir>/<file>` while the event log and
/// `key` are both unchanged.
///
/// Exact, not approximate: a memo that no longer matches is skipped, never returned, and a log
/// that cannot be stated falls through to computing live — so an unreadable cache degrades to the
/// uncached behaviour rather than to a wrong answer.
pub fn memoised<T, F>(file: &str, key: &str, compute: F) -> T
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> T,
{
    let Some(id) = identity() else {
        return compute();
    };
    let path: PathBuf = state_dir().join(file);
    if let Some(hit) = read_fresh::<T>(&path, id, key) {
        return hit;
    }
    let value = compute();
    crate::util::best_effort(
        write_memo(&path, id, key, &value),
        &format!("event-memo: warm {file}"),
    );
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Tally(usize);

    fn memo(mtime: i64, len: u64, key: &str) -> Memo<Tally> {
        Memo {
            events_mtime: mtime,
            events_len: len,
            key: key.to_string(),
            value: Tally(7),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let json = serde_json::to_string(&memo(100, 2048, "2026-09-05")).expect("serialise");
        let back: Memo<Tally> = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.value, Tally(7));
    }

    #[test]
    fn an_append_changes_the_key_within_one_mtime_second() {
        let a = memo(1_700_000_000, 1_000, "k");
        let b = memo(1_700_000_000, 1_200, "k");
        assert_eq!(a.events_mtime, b.events_mtime);
        assert_ne!(a.events_len, b.events_len);
    }

    #[test]
    fn the_extra_key_discriminates_an_untouched_log() {
        // The badge's date lives here: a snoozed item opens when the date rolls and no event is
        // written, so log identity alone cannot detect it.
        let a = memo(1_700_000_000, 1_000, "2026-09-05");
        let b = memo(1_700_000_000, 1_000, "2026-09-06");
        assert_eq!(
            (a.events_mtime, a.events_len),
            (b.events_mtime, b.events_len)
        );
        assert_ne!(a.key, b.key);
    }
}
