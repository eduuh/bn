//! Machine-scoped state directories: enumerating the hosts a notes tree holds state for, and
//! folding one host's append-only history into another's.
//!
//! Exists because a host's identity used to be able to drift. Machine-scoped paths hang off
//! [`crate::resolver::Config::machine`], so a laptop answering `mac.lan` on one network and
//! `vivs-MacBook-Pro.local` on another wrote two directories, each holding a *partial* history that
//! looked complete. The identity is pinned now, which stops new splits; this is how you repair one
//! that already happened.
//!
//! Distinct from [`crate::machine`], which is CPU/RAM saturation for the health widget.

use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The append-only histories a merge folds, relative to `<personal_root>/<machine>/`.
///
/// Every entry is a JSONL log that is only ever appended to and read chronologically, which is what
/// makes merging meaningful: interleaving two hosts' lines by timestamp reconstructs the single
/// history they should always have been.
///
/// Deliberately excludes `state/backlog.jsonl`. That is a *live store*, not history — folding a
/// stale copy back in would resurrect requests a controller already resolved and hand them out as
/// fresh work. Point-in-time snapshots (`attention.json`, `doctor.json`, leases) are out
/// for the same reason: they describe a moment, so there is no correct way to combine two of them.
pub const MERGEABLE_LOGS: &[&str] = &[
    "state/events.jsonl",
    "metrics/events.jsonl",
    "dojo/cards.jsonl",
    "dojo/reviews.jsonl",
    "dojo/sessions.jsonl",
];

/// What merging one log would do, or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogMerge {
    /// Path relative to the machine dir, e.g. `state/events.jsonl`.
    pub log: String,
    /// Lines already in the destination.
    pub kept: usize,
    /// Lines taken from the source because the destination lacked them.
    pub added: usize,
    /// Source lines the destination already had, byte for byte.
    pub skipped: usize,
}

impl LogMerge {
    pub fn changed(&self) -> bool {
        self.added > 0
    }
}

/// One host's state inside the notes tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineDir {
    pub name: String,
    pub path: PathBuf,
    /// Total lines across [`MERGEABLE_LOGS`] — the size signal for "which of these is the real one".
    pub events: usize,
    /// Newest `ts` in those logs; `None` when it holds no timestamped history.
    pub last_activity: Option<i64>,
}

/// Every machine directory under `personal_root` that holds state, newest activity first.
///
/// A machine directory is any child with a `state/` or `metrics/` subdirectory — the shape
/// [`crate::resolver::Config::state_dir`] creates. Other children (per-repo note trees, `archive/`)
/// are skipped, so this stays a list of hosts rather than a directory listing.
pub fn machine_dirs(personal_root: &Path) -> Vec<MachineDir> {
    let Ok(entries) = std::fs::read_dir(personal_root) else {
        return Vec::new();
    };
    let mut out: Vec<MachineDir> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("state").is_dir() || e.path().join("metrics").is_dir())
        .map(|e| {
            let path = e.path();
            let (events, last_activity) = summarize(&path);
            MachineDir {
                name: e.file_name().to_string_lossy().into_owned(),
                path,
                events,
                last_activity,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| b.events.cmp(&a.events))
    });
    out
}

/// The on-disk machine dir for `name`, matched case-insensitively.
///
/// Hostnames are case-insensitive (RFC 4343) and the filesystems bn runs on disagree about whether
/// paths are: the same laptop reports `mac.lan` while its directory is `Mac.lan`. Matching exactly
/// would make a merge pick its destination by luck on macOS and pick the wrong one on Linux. Falls
/// back to the literal join so a first-ever write still lands somewhere sensible.
pub fn resolve_machine_dir(personal_root: &Path, name: &str) -> PathBuf {
    machine_dirs(personal_root)
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(name))
        .map(|d| d.path)
        .unwrap_or_else(|| personal_root.join(name))
}

/// Whether two machine names denote the same host, ignoring case as DNS does.
pub fn same_machine(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn summarize(machine_dir: &Path) -> (usize, Option<i64>) {
    let mut events = 0;
    let mut newest: Option<i64> = None;
    for log in MERGEABLE_LOGS {
        for line in read_lines(&machine_dir.join(log)) {
            events += 1;
            if let Some(ts) = line_ts(&line) {
                newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
            }
        }
    }
    (events, newest)
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|body| {
            body.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The `ts` field of a JSONL line, when it parses and has one.
fn line_ts(line: &str) -> Option<i64> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("ts").and_then(Value::as_i64))
}

/// Fold `source` lines into `dest`: keep every destination line, append the source lines the
/// destination lacks byte for byte, then order the result by `ts`.
///
/// Deduplication is against the destination only, never within a file. Two byte-identical lines
/// already sitting in one log are that log's business — a merge is not a compaction pass, and
/// silently dropping one would make this destructive in a way its name does not promise.
///
/// The sort is stable and lines without a parseable `ts` sort as 0, so malformed or schema-less
/// lines drift to the front intact rather than vanishing: a history repair must not double as a
/// filter for lines it does not understand. Pure over line vectors, so the ordering and dedupe
/// rules are testable without a filesystem.
pub fn merge_lines(dest: &[String], source: &[String]) -> (Vec<String>, LogMerge) {
    let existing: HashSet<&str> = dest.iter().map(String::as_str).collect();
    let mut added = 0;
    let mut skipped = 0;
    let mut out: Vec<String> = dest.to_vec();
    for line in source {
        if existing.contains(line.as_str()) {
            skipped += 1;
            continue;
        }
        out.push(line.clone());
        added += 1;
    }
    out.sort_by_key(|line| line_ts(line).unwrap_or(0));
    (
        out,
        LogMerge {
            log: String::new(),
            kept: dest.len(),
            added,
            skipped,
        },
    )
}

/// Plan — and with `apply`, perform — folding the `source` machine dir into `dest`.
///
/// `apply == false` computes exactly what `apply == true` would write without touching disk, so a
/// preview and the real run cannot disagree. The source is never modified or removed: the point is
/// repairing history, and a migration that consumes its own input leaves nothing to check against.
pub fn merge_machine(
    dest: &Path,
    source: &Path,
    apply: bool,
) -> Result<Vec<LogMerge>, std::io::Error> {
    let mut merges = Vec::new();
    for log in MERGEABLE_LOGS {
        let source_path = source.join(log);
        if !source_path.is_file() {
            continue;
        }
        let dest_path = dest.join(log);
        let (merged, mut summary) = merge_lines(&read_lines(&dest_path), &read_lines(&source_path));
        summary.log = (*log).to_string();
        if apply && summary.changed() {
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut body = merged.join("\n");
            body.push('\n');
            write_atomic(&dest_path, &body)?;
        }
        merges.push(summary);
    }
    Ok(merges)
}

/// Write through a temp file in the destination directory, then rename.
///
/// These logs are read by crons that can fire mid-write; a truncate-then-write would let one
/// observe an empty history and conclude the machine had never run anything.
fn write_atomic(path: &Path, body: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn merging_interleaves_both_histories_by_timestamp() {
        let dest = lines(&[r#"{"ts":10,"kind":"a"}"#, r#"{"ts":30,"kind":"c"}"#]);
        let source = lines(&[r#"{"ts":20,"kind":"b"}"#]);
        let (merged, summary) = merge_lines(&dest, &source);
        assert_eq!(
            merged,
            lines(&[
                r#"{"ts":10,"kind":"a"}"#,
                r#"{"ts":20,"kind":"b"}"#,
                r#"{"ts":30,"kind":"c"}"#,
            ])
        );
        assert_eq!(summary.added, 1);
        assert_eq!(summary.skipped, 0);
    }

    #[test]
    fn a_line_the_destination_already_has_is_not_added_twice() {
        let shared = r#"{"ts":10,"kind":"a"}"#;
        let (merged, summary) = merge_lines(&lines(&[shared]), &lines(&[shared]));
        assert_eq!(merged, lines(&[shared]));
        assert_eq!(summary.added, 0);
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn duplicates_already_inside_the_destination_are_preserved() {
        // Dedupe is across the two files, not within one: a merge is not a compaction pass.
        let dup = r#"{"ts":10,"kind":"a"}"#;
        let (merged, summary) = merge_lines(&lines(&[dup, dup]), &lines(&[dup]));
        assert_eq!(merged.len(), 2, "the destination's own pair survives");
        assert_eq!(summary.added, 0);
    }

    #[test]
    fn lines_without_a_timestamp_survive_the_merge() {
        // A history repair must not double as a filter for lines it cannot parse.
        let dest = lines(&[r#"{"ts":10,"kind":"a"}"#]);
        let source = lines(&["not json at all", r#"{"kind":"no-ts"}"#]);
        let (merged, summary) = merge_lines(&dest, &source);
        assert_eq!(summary.added, 2);
        assert!(merged.contains(&"not json at all".to_string()));
        assert!(merged.contains(&r#"{"kind":"no-ts"}"#.to_string()));
    }

    #[test]
    fn the_live_backlog_and_snapshots_are_never_merged() {
        // Folding a stale backlog back in would hand already-resolved entries to a worker as new.
        assert!(!MERGEABLE_LOGS.contains(&"state/backlog.jsonl"));
        assert!(!MERGEABLE_LOGS.iter().any(|l| l.ends_with(".json")));
    }

    #[test]
    fn a_dry_run_writes_nothing_but_reports_the_same_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dest = temp.path().join("dest");
        let source = temp.path().join("source");
        std::fs::create_dir_all(dest.join("state")).expect("dest state");
        std::fs::create_dir_all(source.join("state")).expect("source state");
        std::fs::write(dest.join("state/events.jsonl"), "{\"ts\":1}\n").expect("dest log");
        std::fs::write(source.join("state/events.jsonl"), "{\"ts\":2}\n").expect("source log");

        let planned = merge_machine(&dest, &source, false).expect("plan");
        assert_eq!(
            std::fs::read_to_string(dest.join("state/events.jsonl")).expect("unchanged"),
            "{\"ts\":1}\n"
        );

        let applied = merge_machine(&dest, &source, true).expect("apply");
        assert_eq!(planned, applied, "preview and apply must not disagree");
        assert_eq!(
            std::fs::read_to_string(dest.join("state/events.jsonl")).expect("merged"),
            "{\"ts\":1}\n{\"ts\":2}\n"
        );
        assert!(
            source.join("state/events.jsonl").is_file(),
            "the source is evidence; merging must not consume it"
        );
    }

    #[test]
    fn merging_is_idempotent() {
        // The repair may be re-run (two machines, a retry after a sync); the second pass must be a
        // no-op rather than doubling every line.
        let temp = tempfile::tempdir().expect("tempdir");
        let dest = temp.path().join("dest");
        let source = temp.path().join("source");
        std::fs::create_dir_all(dest.join("state")).expect("dest state");
        std::fs::create_dir_all(source.join("state")).expect("source state");
        std::fs::write(dest.join("state/events.jsonl"), "{\"ts\":1}\n").expect("dest log");
        std::fs::write(source.join("state/events.jsonl"), "{\"ts\":2}\n").expect("source log");

        merge_machine(&dest, &source, true).expect("first");
        let second = merge_machine(&dest, &source, true).expect("second");
        assert!(second.iter().all(|m| m.added == 0));
        assert_eq!(
            std::fs::read_to_string(dest.join("state/events.jsonl")).expect("stable"),
            "{\"ts\":1}\n{\"ts\":2}\n"
        );
    }

    #[test]
    fn a_host_is_matched_regardless_of_case() {
        // The same laptop reports `mac.lan` while its directory is `Mac.lan`; hostnames are
        // case-insensitive (RFC 4343), so exact matching would treat one host as two.
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("Mac.lan").join("state");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::write(state.join("events.jsonl"), "{\"ts\":1}\n").expect("events");

        assert!(same_machine("mac.lan", "Mac.lan"));
        assert_eq!(
            resolve_machine_dir(temp.path(), "mac.lan"),
            temp.path().join("Mac.lan"),
            "must resolve to the directory that exists, not a second one differing only in case"
        );
    }

    #[test]
    fn an_unknown_host_resolves_to_its_literal_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_machine_dir(temp.path(), "brand-new"),
            temp.path().join("brand-new")
        );
    }

    #[test]
    fn machine_dirs_lists_hosts_newest_first_and_skips_plain_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (name, ts) in [("old-host", 100), ("new-host", 900)] {
            let state = temp.path().join(name).join("state");
            std::fs::create_dir_all(&state).expect("state dir");
            std::fs::write(state.join("events.jsonl"), format!("{{\"ts\":{ts}}}\n"))
                .expect("events");
        }
        std::fs::create_dir_all(temp.path().join("some-repo")).expect("non-machine dir");

        let dirs = machine_dirs(temp.path());
        assert_eq!(
            dirs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["new-host", "old-host"]
        );
        assert_eq!(dirs[0].last_activity, Some(900));
        assert_eq!(dirs[0].events, 1);
    }
}
