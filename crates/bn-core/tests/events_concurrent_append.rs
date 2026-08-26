//! Concurrent appends to one event log must never tear a record.
//!
//! Regression for the corruption found on a real host: 276 lines in `state/events.jsonl` were
//! either two records glued together or a record truncated mid-write, and because
//! `read_events_at` skips unparseable lines the events were lost silently.

use std::io::{BufRead, BufReader};

/// Every record written by many processes' worth of concurrent writers must still parse.
///
/// Each thread writes a distinct kind so a torn line cannot accidentally look valid, and the
/// payload is padded to make a split write more likely to be observable if one ever occurs.
#[test]
fn concurrent_appends_never_tear_a_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");

    const WRITERS: usize = 8;
    const PER_WRITER: usize = 40;

    std::thread::scope(|s| {
        for w in 0..WRITERS {
            let path = path.clone();
            s.spawn(move || {
                for n in 0..PER_WRITER {
                    let mut fields = serde_json::Map::new();
                    fields.insert("writer".into(), serde_json::json!(w));
                    fields.insert("n".into(), serde_json::json!(n));
                    // Padding widens the window a non-atomic write would tear in.
                    fields.insert("pad".into(), serde_json::json!("x".repeat(512)));
                    bn_core::events::append_event_at(&path, "probe", 1_700_000_000 + n as i64, fields)
                        .expect("append");
                }
            });
        }
    });

    let f = std::fs::File::open(&path).expect("open log");
    let mut seen = 0usize;
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("line {} is not valid JSON ({e}): {:.120}", i + 1, line));
        assert_eq!(v["kind"], "probe", "line {} lost its shape", i + 1);
        seen += 1;
    }
    assert_eq!(
        seen,
        WRITERS * PER_WRITER,
        "every appended record must survive"
    );
}
