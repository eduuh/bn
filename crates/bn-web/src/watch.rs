//! The single background watcher that powers `GET /api/stream`.
//!
//! One tokio task polls the cheap status sources on the `[web].fleet_poll_secs` cadence and pushes typed
//! SSE frames onto a `broadcast` channel that every connected client subscribes to — so N browsers
//! cost ONE set of file reads, not N (design §2.2). No inotify dependency: 1–2s polling of one file
//! tail plus one directory mtime scan is cross-platform and negligible.
//!
//! Frames (each `data` is the matching design §3 viewmodel):
//! - `fleet` — [`bn_core::agent::supervise`] re-run each tick, emitted only when the snapshot hash
//!   changes.
//! - `orchestrate` — the `orchestrate-*.json` run files, emitted when their combined mtime changes;
//!   `data` is the same `{runs:[…]}` shape `GET /api/orchestrate` and `bn orchestrate status
//!   --json` return.
//! - `log` — each newly appended `events.jsonl` line (byte-offset tail; a torn tail line is retried
//!   next pass), one frame per [`Event`].
//! - `attn` — [`bn_core::attention::open_items`], emitted when the open-items snapshot changes.
//! - `bearings` — the bounded shared current-state snapshot, emitted when any of its source
//!   content changes.

use crate::state::AppState;
use bn_core::bearings::BearingsSnapshot;
use bn_core::events::{self, Event};
use bn_core::{agent, attention, orchestrate, payload};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

/// One server-sent event: a named channel plus its JSON payload line. Broadcast behind an `Arc` so
/// fanning out to many subscribers is a refcount bump, not a per-client string clone.
#[derive(Debug, Clone)]
pub struct StreamFrame {
    /// The SSE `event:` name — `fleet | orchestrate | log | attn | bearings`.
    pub event: &'static str,
    /// The SSE `data:` line — a single-line JSON value.
    pub data: String,
}

impl StreamFrame {
    fn arc(event: &'static str, data: String) -> Arc<StreamFrame> {
        Arc::new(StreamFrame { event, data })
    }
}

/// Spawn the watcher task, wired to `state`'s broadcast sender. Call once from `main` after the
/// state is built. The task runs for the life of the process.
pub fn spawn(state: AppState) {
    tokio::spawn(async move { run(state).await });
}

async fn run(state: AppState) {
    let cfg = state.cfg.clone();
    let period = Duration::from_secs(cfg.web.fleet_poll_secs.max(1));

    let log_path = events::log_path();
    // Start tailing at the current end so the stream carries only *new* lines, never the backlog.
    let mut log_offset = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    let mut last_fleet: u64 = 0;
    let mut last_orch: u64 = 0;
    let mut last_attn: u64 = 0;
    let mut last_bearings: u64 = 0;

    let mut ticker = tokio::time::interval(period);
    loop {
        ticker.tick().await;
        // Nobody listening → keep advancing the log offset (so a late connector isn't hit with a
        // backlog burst) but skip the heavier fleet/orchestrate/attn sampling entirely.
        let listening = state.tx.receiver_count() > 0;

        if listening {
            let fleet = agent::supervise(&cfg, agent::DEFAULT_IDLE_MINS, agent::DEFAULT_STUCK_MINS);
            let data = payload::agent_fleet_data(&fleet).to_string();
            emit_on_change(&state, "fleet", data, &mut last_fleet);

            let runs = orchestrate::run_paths()
                .iter()
                .filter_map(|path| orchestrate::OrchestrateState::load(path))
                .collect::<Vec<_>>();
            let items = attention::open_items(&bn_core::util::today());
            emit_bearings_on_change(&state, &fleet, &runs, &items, &mut last_bearings);
            let data = payload::attn_list_data(&items).to_string();
            emit_on_change(&state, "attn", data, &mut last_attn);

            let sig = orchestrate_mtime_sig();
            if sig != last_orch {
                last_orch = sig;
                let _ = state.tx.send(StreamFrame::arc(
                    "orchestrate",
                    orchestrate_runs_json(None).to_string(),
                ));
            }
        }

        // Event-log tail runs every tick so the offset stays current; frames go out only when a
        // client is attached.
        if let Ok((new_events, new_offset)) = tail_events(&log_path, log_offset) {
            log_offset = new_offset;
            if listening {
                for ev in new_events {
                    if let Ok(data) = serde_json::to_string(&ev) {
                        let _ = state.tx.send(StreamFrame::arc("log", data));
                    }
                }
            }
        }
    }
}

fn emit_bearings_on_change(
    state: &AppState,
    fleet: &agent::Fleet,
    runs: &[orchestrate::OrchestrateState],
    items: &[attention::AttnItem],
    last: &mut u64,
) {
    let mut content = BearingsSnapshot::from_parts(0, fleet, runs, items);
    for agent in &mut content.fleet.agents {
        agent.idle_secs = 0;
    }
    let hash = hash_str(&bearings_data(&content).to_string());
    if hash == *last {
        return;
    }
    *last = hash;
    let snapshot =
        BearingsSnapshot::from_parts(bn_core::util::now_epoch_secs(), fleet, runs, items);
    let _ = state.tx.send(StreamFrame::arc(
        "bearings",
        bearings_data(&snapshot).to_string(),
    ));
}

/// The byte-identical snapshot data used by the REST envelope and the SSE frame.
pub(crate) fn bearings_data(snapshot: &BearingsSnapshot) -> serde_json::Value {
    payload::bearings_data(snapshot)
}

/// Broadcast `data` on `event` only when its hash differs from `last` — the change-gate that keeps
/// an unchanging fleet/attn snapshot from re-emitting every tick.
fn emit_on_change(state: &AppState, event: &'static str, data: String, last: &mut u64) {
    let h = hash_str(&data);
    if h != *last {
        *last = h;
        let _ = state.tx.send(StreamFrame::arc(event, data));
    }
}

/// The `{runs:[…]}` orchestrate viewmodel — the exact shape `bn orchestrate status --json` emits
/// in its `data` and the byte-for-byte contract `GET /api/orchestrate` must honor (#389). `repo`
/// selects a single run (mirroring `--repo`); `None` lists every run on the machine.
pub fn orchestrate_runs_json(repo: Option<&str>) -> serde_json::Value {
    let paths = match repo {
        Some(r) => vec![orchestrate::state_path_for(r)],
        None => orchestrate::run_paths(),
    };
    let reviews = bn_core::plan_review::open_reviews();
    let runs: Vec<serde_json::Value> = paths
        .iter()
        .filter_map(|p| orchestrate::OrchestrateState::load(p))
        .map(|st| payload::orchestrate_status_data(&st, &reviews))
        .collect();
    serde_json::json!({ "runs": runs })
}

/// A hash over every run file's `(path, mtime)` — changes iff a run file is added, removed, or
/// rewritten, which is exactly when the orchestrate viewmodel needs re-emitting.
fn orchestrate_mtime_sig() -> u64 {
    let mut h = DefaultHasher::new();
    for p in orchestrate::run_paths() {
        p.to_string_lossy().hash(&mut h);
        let secs = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        secs.hash(&mut h);
    }
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Tail an append-only JSONL log from `offset`: return the events on every *complete* line since
/// `offset` and the new offset (advanced only past the last newline, so a torn final line is
/// retried next pass). A shrunk file (rotation/truncation) restarts from the top; a missing file
/// yields nothing and resets the offset to 0.
fn tail_events(path: &Path, offset: u64) -> std::io::Result<(Vec<Event>, u64)> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok((Vec::new(), 0)),
    };
    let start = if len < offset { 0 } else { offset };
    if len == start {
        return Ok((Vec::new(), start));
    }
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
        return Ok((Vec::new(), start)); // no complete line yet — retry next pass
    };
    let consumed = last_nl + 1;
    let events = buf[..consumed]
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice::<Event>(l).ok())
        .collect();
    Ok((events, start + consumed as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::agent::{AgentHealth, AgentRole, Fleet, LiveAgent};
    use std::io::Write;

    fn tmp_log(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "bn-web-watch-{}-{}.jsonl",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn append(path: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    #[test]
    fn tail_returns_only_new_complete_lines() {
        let path = tmp_log("basic");
        append(&path, r#"{"kind":"a","ts":1}"#);
        // Start from EOF: nothing new yet.
        let offset = std::fs::metadata(&path).unwrap().len();
        let (evs, off) = tail_events(&path, offset).unwrap();
        assert!(evs.is_empty());
        assert_eq!(off, offset);

        // Two appends → two events, offset advances to the new EOF.
        append(&path, r#"{"kind":"b","ts":2}"#);
        append(&path, r#"{"kind":"c","ts":3}"#);
        let (evs, off2) = tail_events(&path, off).unwrap();
        assert_eq!(
            evs.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(off2, std::fs::metadata(&path).unwrap().len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tail_holds_back_a_torn_final_line() {
        let path = tmp_log("torn");
        // A complete line followed by a partial (no trailing newline).
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"{\"kind\":\"done\",\"ts\":1}\n{\"kind\":\"partial")
                .unwrap();
        }
        let (evs, off) = tail_events(&path, 0).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, "done");

        // Completing the torn line then reading from the held offset yields the second event.
        // The completion closes the open `"kind"` string, so the joined line is valid JSON.
        append(&path, r#"","ts":2}"#);
        let (evs2, _) = tail_events(&path, off).unwrap();
        assert_eq!(evs2.len(), 1);
        assert_eq!(evs2[0].kind, "partial");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tail_restarts_after_truncation() {
        let path = tmp_log("rotate");
        append(&path, r#"{"kind":"old","ts":1}"#);
        let big_offset = 10_000; // pretend we'd read far past a now-smaller file
        let (evs, off) = tail_events(&path, big_offset).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, "old");
        assert_eq!(off, std::fs::metadata(&path).unwrap().len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_empty_and_resets_offset() {
        let path = tmp_log("absent");
        let (evs, off) = tail_events(&path, 999).unwrap();
        assert!(evs.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn bearings_frame_data_is_the_typed_snapshot_json() {
        let snapshot = BearingsSnapshot::from_parts(100, &Fleet::default(), &[], &[]);
        assert_eq!(bearings_data(&snapshot), payload::bearings_data(&snapshot));
    }

    #[test]
    fn unchanged_bearings_content_emits_only_once() {
        let state = AppState::for_test(bn_core::resolver::Config::default());
        let mut receiver = state.tx.subscribe();
        let mut last = 0;
        let live_agent = |idle_secs| LiveAgent {
            session_id: "s1".into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "feature".into(),
            target: "bn:feature".into(),
            worktree: None,
            tmux_target: None,
            last_activity_ts: 100,
            idle_secs,
            health: AgentHealth::Active,
            role: AgentRole::Worker,
            turns: 1,
            tool_calls: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            control: bn_core::agent::ControlChannel::None,
        };
        let first = Fleet {
            active: 1,
            agents: vec![live_agent(1)],
            ..Default::default()
        };
        let later = Fleet {
            active: 1,
            agents: vec![live_agent(30)],
            ..Default::default()
        };

        emit_bearings_on_change(&state, &first, &[], &[], &mut last);
        emit_bearings_on_change(&state, &later, &[], &[], &mut last);

        let frame = receiver.try_recv().unwrap();
        assert_eq!(frame.event, "bearings");
        assert!(receiver.try_recv().is_err());
    }
}
