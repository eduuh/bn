//! The normalized [`AgentSession`] model, the idempotent [`ingest`] poller, and
//! the per-branch recall view.

use super::parse::{scan_claude, scan_copilot};
use crate::cost::WorkClass;
use crate::events::{self, Event};
use crate::resolver::Config;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// One agent session, normalized across harnesses. Serializes straight into the recall payload;
/// [`to_fields`] / [`from_event`] are the round-trip to and from the `agent_session` event.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentSession {
    /// Globally-unique session UUID — the idempotency key for ingest.
    pub session_id: String,
    /// "claude" | "copilot".
    pub harness: String,
    /// bn-canonical note key (repo lowercased, branch slashes→dashes).
    pub repo: String,
    pub branch: String,
    pub started_ts: i64,
    pub ended_ts: i64,
    pub turns: i64,
    pub tool_calls: i64,
    pub permission_prompts: i64,
    /// Models seen, in first-seen order, joined with ", " in the event.
    pub models: Vec<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    /// Prompt tokens written *into* the provider's cache at the 5-minute TTL — and, for a harness
    /// that reports writes without a TTL, all of them.
    pub cache_write_5m_tokens: i64,
    /// Prompt tokens written into the cache at the 1-hour TTL, which bills at 2× fresh input
    /// rather than 1.25×. Kept apart from the 5-minute figure all the way to the price table
    /// because Claude Code writes at this TTL: collapsing the two would understate its dominant
    /// cost bucket by 60% on every session.
    pub cache_write_1h_tokens: i64,
    /// Cost in nano-AIU (Copilot only; Claude transcripts carry no cost).
    pub cost_nanoaiu: Option<i64>,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub files_modified: i64,
    /// "completed" (Copilot clean shutdown) | "quiet" (Claude, inferred from idle).
    pub outcome: String,
    /// Which launch path produced this run (#381) — stamped at [`ingest`] time from the launch
    /// path's [`crate::cost::attribute`] record, keyed on `session_id`. `Unattributed` when no
    /// launch path claimed it (an interactive `agent` launch, or a session recorded before the
    /// attribution existed), so the cost view still folds it in.
    pub work_class: WorkClass,
}

impl AgentSession {
    pub fn duration_secs(&self) -> i64 {
        (self.ended_ts - self.started_ts).max(0)
    }

    /// Flatten into the `agent_session` event fields. `cost_nanoaiu` is omitted when absent so a
    /// Claude session never implies a zero cost.
    fn to_fields(&self) -> Map<String, Value> {
        let mut f = Map::new();
        f.insert("session_id".into(), json!(self.session_id));
        f.insert("harness".into(), json!(self.harness));
        f.insert("repo".into(), json!(self.repo));
        f.insert("branch".into(), json!(self.branch));
        f.insert("started_ts".into(), json!(self.started_ts));
        f.insert("duration_secs".into(), json!(self.duration_secs()));
        f.insert("turns".into(), json!(self.turns));
        f.insert("tool_calls".into(), json!(self.tool_calls));
        f.insert("permission_prompts".into(), json!(self.permission_prompts));
        f.insert("models".into(), json!(self.models.join(", ")));
        f.insert("input_tokens".into(), json!(self.input_tokens));
        f.insert("output_tokens".into(), json!(self.output_tokens));
        f.insert("cache_read_tokens".into(), json!(self.cache_read_tokens));
        f.insert(
            "cache_write_5m_tokens".into(),
            json!(self.cache_write_5m_tokens),
        );
        f.insert(
            "cache_write_1h_tokens".into(),
            json!(self.cache_write_1h_tokens),
        );
        if let Some(c) = self.cost_nanoaiu {
            f.insert("cost_nanoaiu".into(), json!(c));
        }
        f.insert("lines_added".into(), json!(self.lines_added));
        f.insert("lines_removed".into(), json!(self.lines_removed));
        f.insert("files_modified".into(), json!(self.files_modified));
        f.insert("outcome".into(), json!(self.outcome));
        f.insert("work_class".into(), json!(self.work_class.as_str()));
        f
    }

    /// Reconstruct from a folded `agent_session` event (the read path for the recall view).
    fn from_event(ev: &Event) -> Option<AgentSession> {
        let s = |k: &str| {
            ev.fields
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let i = |k: &str| ev.fields.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let sid = ev.fields.get("session_id")?.as_str()?.to_string();
        let started = ev
            .fields
            .get("started_ts")
            .and_then(|v| v.as_i64())
            .unwrap_or(ev.ts);
        let models = s("models")
            .split(", ")
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .collect();
        Some(AgentSession {
            session_id: sid,
            harness: s("harness"),
            repo: s("repo"),
            branch: s("branch"),
            started_ts: started,
            ended_ts: ev.ts,
            turns: i("turns"),
            tool_calls: i("tool_calls"),
            permission_prompts: i("permission_prompts"),
            models,
            input_tokens: i("input_tokens"),
            output_tokens: i("output_tokens"),
            cache_read_tokens: i("cache_read_tokens"),
            // Absent on events recorded before cache writes were captured; zero there means "never
            // measured", not "none written" — those sessions under-report their real spend, and
            // re-deriving it would need transcripts the harnesses have long since rotated away.
            cache_write_5m_tokens: i("cache_write_5m_tokens"),
            cache_write_1h_tokens: i("cache_write_1h_tokens"),
            cost_nanoaiu: ev.fields.get("cost_nanoaiu").and_then(|v| v.as_i64()),
            lines_added: i("lines_added"),
            lines_removed: i("lines_removed"),
            files_modified: i("files_modified"),
            outcome: s("outcome"),
            work_class: ev
                .fields
                .get("work_class")
                .and_then(|v| v.as_str())
                .map(WorkClass::parse_or_unattributed)
                .unwrap_or(WorkClass::Unattributed),
        })
    }
}

/// What [`ingest`] did this run.
#[derive(Debug, Serialize)]
pub struct IngestReport {
    pub ingested: usize,
    pub skipped: usize,
}

/// Whether a poll re-reads sessions it has already recorded.
///
/// The metrics log is append-only and every read path folds the *latest* snapshot per session id
/// ([`latest_session_events`], [`crate::cost::rollup`]), so a corrected re-read supersedes the row
/// it replaces instead of double-counting it. That is what makes a backfill safe to express as
/// "poll again, ignoring what you've seen".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    /// Record only sessions not already in the log — the steady-state poll the `agent-observe`
    /// job runs every few minutes.
    NewOnly,
    /// Re-read every session still on disk, replacing its recorded row.
    ///
    /// For when the *parser* changed rather than the data: a row recorded by an older bn carries
    /// whatever that version could measure, and no amount of re-polling in [`NewOnly`] mode will
    /// revisit it. Two such corrections shipped together — cache-creation tokens were not captured
    /// at all, and a response's usage was counted once per transcript line rather than once per
    /// response (a ~2.4× overcount) — leaving every pre-existing Claude row both short a cost
    /// bucket and inflated in the ones it had.
    Backfill,
}

impl IngestMode {
    /// The mode a `--reingest` flag selects.
    pub fn poll(reingest: bool) -> IngestMode {
        if reingest {
            IngestMode::Backfill
        } else {
            IngestMode::NewOnly
        }
    }

    /// Whether an already-recorded session should be re-read rather than skipped.
    fn revisits_recorded_sessions(self) -> bool {
        self == IngestMode::Backfill
    }
}

impl IngestReport {
    /// The one-line human summary. Mode-dependent on purpose: a backfill's count is sessions
    /// whose rows were *replaced*, and calling those "new" would misreport a correction as growth.
    pub fn summary(&self, mode: IngestMode) -> String {
        let s = if self.ingested == 1 { "" } else { "s" };
        match mode {
            IngestMode::Backfill => format!(
                "re-read {} session{s} from disk, superseding their recorded rows",
                self.ingested
            ),
            IngestMode::NewOnly => format!(
                "ingested {} new session{s} ({} already seen)",
                self.ingested, self.skipped
            ),
        }
    }
}

/// Poll both harnesses and append any not-yet-seen completed session as an `agent_session` event
/// to the synced metrics log. Idempotent: a session whose id already appears is skipped.
pub fn ingest(cfg: &Config) -> std::io::Result<IngestReport> {
    ingest_with_mode(cfg, IngestMode::NewOnly)
}

/// [`ingest`] under an explicit [`IngestMode`] — the entry point `bn agent ingest --reingest` uses
/// to re-read sessions whose recorded row predates a parser fix.
pub fn ingest_with_mode(cfg: &Config, mode: IngestMode) -> std::io::Result<IngestReport> {
    let home = dirs::home_dir().unwrap_or_default();
    ingest_from_with_mode(
        cfg,
        &home.join(".copilot").join("session-state"),
        &home.join(".claude").join("projects"),
        mode,
    )
}

/// `ingest_from` under an explicit [`IngestMode`].
pub fn ingest_from_with_mode(
    cfg: &Config,
    copilot_root: &Path,
    claude_root: &Path,
    mode: IngestMode,
) -> std::io::Result<IngestReport> {
    let mut seen: BTreeMap<String, i64> = events::read_events_at(&cfg.metrics_log_path())
        .into_iter()
        .filter(|e| e.kind == "agent_session")
        .filter_map(|e| {
            e.fields
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|id| (crate::cost::canonical_session_id(id), e.ts))
        })
        .collect();

    let mut found = scan_copilot(cfg, copilot_root);
    found.extend(scan_claude(cfg, claude_root));

    // Stamp the launch path's work-class (#381) onto each session before it lands, keyed on the
    // session id the launcher pinned via `bn dispatch --session`. Unclaimed sessions stay
    // `unattributed`, so the cost view still folds them.
    let attribution = crate::cost::attribution_map();

    let (mut ingested, mut skipped) = (0, 0);
    for mut s in found {
        let canonical = crate::cost::canonical_session_id(&s.session_id);
        if !mode.revisits_recorded_sessions()
            && seen
                .get(&canonical)
                .is_some_and(|last_ended| *last_ended >= s.ended_ts)
        {
            skipped += 1;
            continue;
        }
        if let Some(wc) = attribution.get(&crate::cost::canonical_session_id(&s.session_id)) {
            s.work_class = *wc;
        }
        events::append_metric_at(
            &cfg.metrics_log_path(),
            "agent_session",
            s.ended_ts,
            s.to_fields(),
        )?;
        seen.insert(canonical, s.ended_ts);
        ingested += 1;
    }
    Ok(IngestReport { ingested, skipped })
}

/// The `agent_session` events attributed to one note key, newest-first — the recall view. Reads
/// the whole-person log so a branch's sessions surface regardless of which machine recorded them.
pub fn sessions_for(cfg: &Config, repo: &str, branch: &str) -> Vec<AgentSession> {
    let mut out: Vec<AgentSession> = sessions_all(cfg)
        .into_iter()
        .filter(|s| s.repo == repo && s.branch == branch)
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.ended_ts));
    out
}

/// Every completed agent session in the synced metrics logs, newest-first.
pub fn sessions_all(cfg: &Config) -> Vec<AgentSession> {
    let events = events::read_events_across(&cfg.metrics_logs_all_machines());
    let mut out: Vec<AgentSession> = latest_session_events(&events)
        .into_iter()
        .filter(|e| e.kind == "agent_session")
        .filter_map(AgentSession::from_event)
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.ended_ts));
    out
}

/// Latest snapshot per canonical harness session id. Resumed sessions append a newer telemetry
/// snapshot under the same id; consumers must fold the latest one rather than double-counting.
pub fn latest_session_events(events: &[Event]) -> Vec<&Event> {
    let mut latest: BTreeMap<String, &Event> = BTreeMap::new();
    let mut anonymous = Vec::new();
    for event in events.iter().filter(|event| event.kind == "agent_session") {
        let Some(id) = event.fields.get("session_id").and_then(Value::as_str) else {
            anonymous.push(event);
            continue;
        };
        let key = crate::cost::canonical_session_id(id);
        if latest
            .get(&key)
            .is_none_or(|current| event.ts >= current.ts)
        {
            latest.insert(key, event);
        }
    }
    anonymous.extend(latest.into_values());
    anonymous
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_session() -> AgentSession {
        AgentSession {
            session_id: "S-1".into(),
            harness: "claude".into(),
            repo: "bn".into(),
            branch: "complete-claude-costs".into(),
            started_ts: 1_000,
            ended_ts: 2_000,
            turns: 12,
            tool_calls: 30,
            permission_prompts: 0,
            models: vec!["claude-opus-5".into()],
            input_tokens: 24,
            output_tokens: 8_400,
            cache_read_tokens: 1_200_000,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 412_000,
            cost_nanoaiu: None,
            lines_added: 90,
            lines_removed: 12,
            files_modified: 4,
            outcome: "quiet".into(),
            work_class: WorkClass::Coding,
        }
    }

    #[test]
    fn cache_writes_survive_the_event_round_trip() {
        // The event log is the only thing between the parser and the cost fold, so a bucket that
        // isn't written here is a bucket the benchmark never sees again.
        let s = claude_session();
        let ev = Event {
            kind: "agent_session".into(),
            ts: s.ended_ts,
            fields: s.to_fields(),
        };
        let back = AgentSession::from_event(&ev).expect("round-trips");
        assert_eq!(back.cache_write_1h_tokens, 412_000);
        assert_eq!(back.cache_write_5m_tokens, 0);
        assert_eq!(back, s);
    }

    #[test]
    fn an_event_written_before_the_buckets_existed_still_reads_back() {
        let mut fields = claude_session().to_fields();
        fields.remove("cache_write_5m_tokens");
        fields.remove("cache_write_1h_tokens");
        let ev = Event {
            kind: "agent_session".into(),
            ts: 2_000,
            fields,
        };
        let back = AgentSession::from_event(&ev).expect("older events stay readable");
        assert_eq!(back.cache_write_1h_tokens, 0);
        assert_eq!(back.cache_read_tokens, 1_200_000);
    }

    /// Two snapshots of one session id, as a backfill produces: the row an older bn recorded
    /// (no cache writes, inflated output) and the corrected re-read appended after it.
    fn snapshots() -> Vec<Event> {
        let mut old = claude_session();
        old.cache_write_1h_tokens = 0;
        old.output_tokens = 20_400; // the per-line double-count
        let new = claude_session();
        [old, new]
            .into_iter()
            .map(|s| Event {
                kind: "agent_session".into(),
                ts: s.ended_ts,
                fields: s.to_fields(),
            })
            .collect()
    }

    #[test]
    fn a_backfilled_row_supersedes_the_one_it_corrects() {
        // The property the whole backfill rests on: the log is append-only, so the corrected row
        // must *win* rather than sum with the row it replaces.
        let events = snapshots();
        let latest = latest_session_events(&events);
        assert_eq!(latest.len(), 1, "one session id must fold to one row");
        let folded = AgentSession::from_event(latest[0]).expect("reads back");
        assert_eq!(folded.cache_write_1h_tokens, 412_000);
        assert_eq!(folded.output_tokens, 8_400);
    }

    #[test]
    fn re_reading_the_same_session_twice_is_still_one_row() {
        // A backfill run twice (or a synced log carrying both machines' copies) must not
        // double-count: the fold is by session id, not by event.
        let mut events = snapshots();
        events.extend(snapshots());
        assert_eq!(latest_session_events(&events).len(), 1);
    }

    #[test]
    fn only_the_backfill_mode_revisits_a_recorded_session() {
        assert!(IngestMode::Backfill.revisits_recorded_sessions());
        assert!(!IngestMode::NewOnly.revisits_recorded_sessions());
    }
}
