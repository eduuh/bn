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

/// Poll both harnesses and append any not-yet-seen completed session as an `agent_session` event
/// to the synced metrics log. Idempotent: a session whose id already appears is skipped.
pub fn ingest(cfg: &Config) -> std::io::Result<IngestReport> {
    let home = dirs::home_dir().unwrap_or_default();
    ingest_from(
        cfg,
        &home.join(".copilot").join("session-state"),
        &home.join(".claude").join("projects"),
    )
}

/// [`ingest`] with explicit harness roots — the testable core.
pub fn ingest_from(
    cfg: &Config,
    copilot_root: &Path,
    claude_root: &Path,
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
        if seen
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
    use crate::agent::{test_cfg, write};

    #[test]
    fn ingest_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("working-copilot");
        let sid = "22222222-2222-2222-2222-222222222222";
        let copilot = tmp.join("copilot").join(sid);
        write(
            &copilot.join("workspace.yaml"),
            &format!(
                "id: {sid}\ncwd: {}\nbranch: working-copilot\n",
                cwd.display()
            ),
        );
        write(
            &copilot.join("events.jsonl"),
            &[
                json!({"type":"session.start","timestamp":"2026-06-26T12:00:00.000Z","data":{"startTime":"2026-06-26T12:00:00.000Z","context":{"cwd":cwd,"branch":"working-copilot"}}}).to_string(),
                json!({"type":"session.shutdown","timestamp":"2026-06-26T12:05:00.000Z","data":{}}).to_string(),
            ].join("\n"),
        );
        let claude_root = tmp.join("claude"); // absent → empty

        let first = ingest_from(&cfg, &tmp.join("copilot"), &claude_root).unwrap();
        assert_eq!((first.ingested, first.skipped), (1, 0));
        let second = ingest_from(&cfg, &tmp.join("copilot"), &claude_root).unwrap();
        assert_eq!((second.ingested, second.skipped), (0, 1));

        let recall = sessions_for(&cfg, "bn", "working-copilot");
        assert_eq!(recall.len(), 1);
        assert_eq!(recall[0].session_id, sid);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ingest_stamps_work_class_from_attribution() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-wc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Isolate the machine-local state log so the launch-path attribution the launcher writes
        // (`cost::attribute`) is the one ingest reads back.
        crate::test_support::with_state_dir(|| {
            let cfg = test_cfg(&tmp);
            let cwd = cfg.worktree_dir.join("bn").join("wc-branch");
            let sid = "33333333-3333-3333-3333-333333333333";
            let copilot = tmp.join("copilot").join(sid);
            write(
                &copilot.join("workspace.yaml"),
                &format!("id: {sid}\ncwd: {}\nbranch: wc-branch\n", cwd.display()),
            );
            write(
                &copilot.join("events.jsonl"),
                &[
                    json!({"type":"session.start","timestamp":"2026-06-26T12:00:00.000Z","data":{"startTime":"2026-06-26T12:00:00.000Z","context":{"cwd":cwd,"branch":"wc-branch"}}}).to_string(),
                    json!({"type":"session.shutdown","timestamp":"2026-06-26T12:05:00.000Z","data":{}}).to_string(),
                ].join("\n"),
            );

            // The launch path (e.g. `bn dispatch`) attributes this session as coding.
            crate::cost::attribute(sid, crate::cost::WorkClass::Coding, "bn", "wc-branch");

            let rep = ingest_from(&cfg, &tmp.join("copilot"), &tmp.join("claude")).unwrap();
            assert_eq!(rep.ingested, 1);

            let recall = sessions_for(&cfg, "bn", "wc-branch");
            assert_eq!(recall.len(), 1);
            assert_eq!(
                recall[0].work_class,
                crate::cost::WorkClass::Coding,
                "ingest stamps the launch path's work-class onto the session"
            );
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
