//! Today suggestions, synthesis, and hosted-session state.
//!
//! It began as a deterministic offline fold — gather the cron findings store (#576), turn each
//! finding item into one proposed task, upsert it into the [`crate::suggestions`] store — and that
//! fold is still here. What changed is what it *is*: Today's agent is now a **hosted, steerable
//! harness session**, launched into the persistent tmux `planning` session and driven from the
//! dashboard's Today page. The deterministic half became its two supporting halves:
//!
//!   * [`gather_packet_json`] is the agent's **opening packet** — the same JSON a headless variant
//!     would have consumed, now seeded into a live session by [`build_seed`];
//!   * [`run`] is the **offline fallback** — what a box with no tmux and no harness still gets, so
//!     a headless machine keeps its suggestions instead of silently getting nothing.
//!
//! This module owns the *pure* half of that: the packet, the synthesis, the seed, and the small
//! durable record of which session hosts Today ([`AgentRecord`]). Launching the session —
//! tmux windows, agent leases, harness argv — lives in `bn-cli`, next to the `bn control` code it
//! mirrors, because that is where every other launch path lives.
//!
//! Re-runs of the offline fold stay idempotent via the suggestions store's id dedupe.

use crate::findings::{self, FindingsRun};
use crate::resolver::{state_dir, Config};
use crate::suggestions::{self, Suggestion};
use crate::util::{local_date_of, now_epoch_secs};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default number of days of findings the agent looks back over.
pub const DEFAULT_LOOKBACK_DAYS: i64 = 7;
/// Default cap on suggestions synthesized from any one source's latest run (avoids one chatty
/// cron flooding the review list).
pub const DEFAULT_PER_SOURCE: usize = 5;

/// What one Today's-agent run did.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunReport {
    /// Distinct findings sources the packet drew from.
    pub sources: usize,
    /// Finding items considered across those sources.
    pub items: usize,
    /// Newly written suggestions (existing/dismissed ones are skipped — see the store's dedupe).
    pub added: usize,
}

/// The `since` local-date bound for a `lookback_days` window ending today (`today` is `YYYY-MM-DD`).
fn since_date(today: &str, lookback_days: i64) -> String {
    match crate::util::epoch_day(today) {
        Some(d) => {
            let start = (d - lookback_days.max(0)).max(0);
            crate::util::date_of(start * 86_400)
        }
        None => today.to_string(),
    }
}

/// The latest recorded run per source within the lookback window, newest source first. This is the
/// deterministic packet Today's agent reasons over: one representative (most recent) run per source
/// so re-synthesis tracks the *current* findings rather than the full append history.
pub fn gather(today: &str, lookback_days: i64) -> Vec<FindingsRun> {
    let since = since_date(today, lookback_days);
    let mut latest: std::collections::HashMap<String, FindingsRun> =
        std::collections::HashMap::new();
    for run in findings::query(None, Some(&since), 0) {
        latest
            .entry(run.source.clone())
            .and_modify(|cur| {
                if run.gathered_at > cur.gathered_at {
                    *cur = run.clone();
                }
            })
            .or_insert(run);
    }
    let mut runs: Vec<FindingsRun> = latest.into_values().collect();
    runs.sort_by(|a, b| {
        b.gathered_at
            .cmp(&a.gathered_at)
            .then(a.source.cmp(&b.source))
    });
    runs
}

/// The deterministic input packet as JSON — the same shape a headless-agent variant would be fed:
/// `{generated_at, date, sources:[{source, run_id, gathered_at, items:[{key,text,url}]}]}`.
pub fn gather_packet_json(today: &str, lookback_days: i64) -> String {
    let runs = gather(today, lookback_days);
    let packet = serde_json::json!({
        "generated_at": now_epoch_secs(),
        "date": today,
        "sources": runs.iter().map(|r| serde_json::json!({
            "source": r.source,
            "run_id": r.run_id,
            "gathered_at": r.gathered_at,
            "items": r.items,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".to_string())
}

/// Deterministically turn the gathered runs into proposed tasks: each finding item → one
/// suggestion (title = the finding headline, rationale = which cron surfaced it and when), capped
/// at `per_source` per source. Items with an empty key fall back to their index so the id stays
/// stable for that source's slot.
pub fn synthesize(runs: &[FindingsRun], per_source: usize) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for run in runs {
        let date = local_date_of(run.gathered_at);
        for (i, item) in run.items.iter().enumerate() {
            if per_source > 0 && i >= per_source {
                break;
            }
            let text = item.text.trim();
            if text.is_empty() {
                continue;
            }
            let key = if item.key.trim().is_empty() {
                format!("i{i}")
            } else {
                item.key.trim().to_string()
            };
            let rationale = format!(
                "Surfaced by the '{}' cron on {} — review whether this is worth acting on.",
                run.source, date
            );
            out.push(Suggestion::new(
                &run.source,
                &key,
                text,
                rationale,
                &item.url,
            ));
        }
    }
    out
}

/// The full pipeline shared by `bn today` and the `today-agent` cron: gather the findings packet →
/// synthesize proposals → merge them into the suggestions store (idempotently). Returns a summary
/// of what it did. Fully offline; a store write error propagates so a cron surfaces it.
pub fn run(today: &str, lookback_days: i64, per_source: usize) -> std::io::Result<RunReport> {
    let runs = gather(today, lookback_days);
    let fresh = synthesize(&runs, per_source);
    let items = runs.iter().map(|r| r.items.len()).sum();
    let added = suggestions::upsert(fresh)?;
    Ok(RunReport {
        sources: runs.len(),
        items,
        added,
    })
}

// ---------------------------------------------------------------------------
// The hosted agent: identity, durable record, and seed
// ---------------------------------------------------------------------------

/// Where a recorded agent sits in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// The window has been opened; the harness may still be booting.
    Starting,
    /// Launched and recorded — the steady state while the session lives.
    Running,
    /// Deliberately stopped (`bn today agent stop`).
    Stopped,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Starting => "starting",
            AgentStatus::Running => "running",
            AgentStatus::Stopped => "stopped",
        }
    }
}

/// What bn knows about the Today agent it launched: enough to *find* the session again (so the
/// dashboard can stream and steer it) and no more. The live truth — is it actually alive, is it
/// waiting on you — comes from [`crate::agent::supervise`], never from this file; a record whose
/// session has since died is stale, not authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// The day this agent was started for (`YYYY-MM-DD`) — also its branch.
    pub date: String,
    /// The harness session id, pinned at launch so the resume channel can aim at it.
    pub session_id: String,
    pub harness: String,
    pub repo: String,
    pub branch: String,
    /// The `repo:branch` steering target.
    pub target: String,
    /// The `session:window` tmux target.
    pub tmux_target: String,
    /// The directory the session runs in.
    pub cwd: String,
    pub status: AgentStatus,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<i64>,
}

fn record_path() -> PathBuf {
    state_dir().join("today-agent.json")
}

/// The recorded agent, or `None` when none was ever launched. A corrupt file reads as `None` — the
/// same forgiving posture the suggestions store takes, because the fix is always "start it again".
pub fn read_record() -> Option<AgentRecord> {
    let text = std::fs::read_to_string(record_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist the record, creating the state dir on first write.
pub fn write_record(record: &AgentRecord) -> std::io::Result<()> {
    let path = record_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(record)?)
}

/// Mark the recorded agent stopped. A no-op returning `false` when there is nothing recorded.
pub fn mark_stopped(now: i64) -> std::io::Result<bool> {
    let Some(mut record) = read_record() else {
        return Ok(false);
    };
    record.status = AgentStatus::Stopped;
    record.stopped_at = Some(now);
    write_record(&record)?;
    Ok(true)
}

/// The seed prompt Today's agent launches with: the operating brief from the `today-agent` agent
/// file (`config/agents/today-agent.md`), rendered with the supplied date and packet.
///
/// The packet is [`gather_packet_json`] — the same deterministic gather the offline fold has always
/// done. The agent therefore starts already knowing what the crons found, instead of spending its
/// first turns rediscovering it.
pub fn build_seed(
    def: &crate::agents::AgentDef,
    date: &str,
    packet: &str,
) -> Result<String, String> {
    let mut args = serde_json::Map::new();
    args.insert("date".into(), serde_json::Value::String(date.to_string()));
    args.insert(
        "packet".into(),
        serde_json::Value::String(packet.to_string()),
    );
    def.render(Some(&args)).map_err(|error| error.to_string())
}

/// The whole truth about Today's agent, as both `bn today agent status` and
/// `GET /api/today/agent` serve it — one builder so the CLI and the dashboard can never disagree
/// about whether the agent is up.
///
/// `running` is decided by the **live fleet**, never by the record. The record says what bn
/// launched; [`crate::agent::supervise`] says what is actually alive. A record whose session has
/// died is stale, and reporting it as running would put a dead Steer panel on the Today page.
pub fn status_view(cfg: &Config) -> serde_json::Value {
    let Some(record) = read_record() else {
        return serde_json::json!({ "running": false, "agent": serde_json::Value::Null });
    };
    let fleet = crate::agent::supervise(
        cfg,
        crate::agent::DEFAULT_IDLE_MINS,
        crate::agent::DEFAULT_STUCK_MINS,
    );
    let live = fleet
        .agents
        .iter()
        .find(|a| a.session_id == record.session_id);
    serde_json::json!({
        "running": live.is_some() && record.status != AgentStatus::Stopped,
        "agent": record,
        "health": live.map(|a| a.health.label()),
        "control": live.map(|a| a.control),
        "last_activity_ts": live.map(|a| a.last_activity_ts),
        "output_tokens": live.map(|a| a.output_tokens),
    })
}
