//! Append-only event log — the substrate for cron observability (now) and productivity
//! metrics (later). One JSONL file under the state dir. Events are flat (a `kind`, a `ts`,
//! and kind-specific fields flattened alongside) so new event kinds never need a schema
//! migration. Cron calls `bn run`, which appends `cron_run` events here; `bn cron status`
//! and `bn doctor` read them back. AI never touches this at runtime — pure script substrate.

use crate::resolver::state_dir;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// One logged event. `kind` discriminates; `ts` is Unix epoch seconds; everything else is
/// kind-specific and flattened into the same JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub ts: i64,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

/// Severity attached to structured orchestrator events. Serialized lowercase so the flat
/// `events.jsonl` schema remains friendly to shell tools and older readers ignore it safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventLevel {
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for EventLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

/// Typed view over an orchestrator event in the shared flat log. Legacy events without structured
/// fields remain readable as `info`, with their event kind used as the reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrchestratorEvent {
    pub kind: String,
    pub ts: i64,
    pub level: EventLevel,
    pub reason: String,
    pub repo: Option<String>,
    pub item_id: Option<String>,
    pub error: Option<String>,
    pub active: Option<bool>,
    pub fields: Map<String, Value>,
}

impl OrchestratorEvent {
    pub fn from_event(event: &Event) -> Option<Self> {
        if !event.kind.starts_with("orchestrate_") {
            return None;
        }
        let level = event
            .fields
            .get("level")
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
            .unwrap_or(EventLevel::Info);
        let reason = event
            .fields
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or(&event.kind)
            .to_string();
        let string_field = |key: &str| {
            event
                .fields
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Some(Self {
            kind: event.kind.clone(),
            ts: event.ts,
            level,
            reason,
            repo: string_field("repo"),
            item_id: string_field("item_id").or_else(|| string_field("id")),
            error: string_field("error"),
            active: event.fields.get("active").and_then(Value::as_bool),
            fields: event.fields.clone(),
        })
    }
}

pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

/// Append an event stamped at `ts` (epoch secs) to a specific log file, creating its parent
/// dir on demand. The location-agnostic primitive: the machine-local cron log writes here via
/// [`append_at`]; the synced productivity log writes here via [`append_metric_at`].
pub fn append_event_at(
    path: &Path,
    kind: &str,
    ts: i64,
    fields: Map<String, Value>,
) -> std::io::Result<()> {
    let ev = Event {
        kind: kind.to_string(),
        ts,
        fields,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(&ev)?)?;
    Ok(())
}

/// Like [`append_event_at`], but `fsync`s the file before returning (`sync_all`) — a durable
/// append rather than a merely buffered one. Used by [`crate::routine`]'s locked transactions,
/// where "the event line is flushed/synced" is the exact instant that makes a delivery/skip/
/// migration durable (#801 amendment invariant 2/3): once this returns `Ok`, the write survives a
/// crash immediately after, so at-least-once dedupe state is never lost to a buffered write that
/// never made it to disk.
pub fn append_event_at_synced(
    path: &Path,
    kind: &str,
    ts: i64,
    fields: Map<String, Value>,
) -> std::io::Result<()> {
    let ev = Event {
        kind: kind.to_string(),
        ts,
        fields,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(&ev)?)?;
    f.sync_all()?;
    Ok(())
}

/// Every event in a specific log file, in file order. Unparseable lines are skipped, so a torn
/// append never poisons reads. Missing file → empty.
pub fn read_events_at(path: &Path) -> Vec<Event> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
        .collect()
}

/// Union of every event across the given log files, in `paths` order then file order. Backs the
/// whole-person view that reads every machine's synced metrics log.
pub fn read_events_across(paths: &[PathBuf]) -> Vec<Event> {
    paths.iter().flat_map(|p| read_events_at(p)).collect()
}

/// Append an event stamped at `ts` to the machine-local cron/state log. Creates the state dir
/// on demand.
pub fn append_at(kind: &str, ts: i64, fields: Map<String, Value>) -> std::io::Result<()> {
    append_event_at(&log_path(), kind, ts, fields)
}

/// Durable ([`append_event_at_synced`]) twin of [`append_at`] for the machine-local log.
pub fn append_at_synced(kind: &str, ts: i64, fields: Map<String, Value>) -> std::io::Result<()> {
    append_event_at_synced(&log_path(), kind, ts, fields)
}

/// Append an event stamped with the current time to the machine-local cron/state log.
pub fn append(kind: &str, fields: Map<String, Value>) -> std::io::Result<()> {
    append_at(kind, now_epoch_secs(), fields)
}

/// Append a productivity event to the **synced** metrics log at `path` (typically
/// `cfg.metrics_log_path()`). Stamped at `ts`; pass [`now_epoch_secs`] for "now". This is the
/// write path behind `bn event` and the per-mutation activity events.
pub fn append_metric_at(
    path: &Path,
    kind: &str,
    ts: i64,
    fields: Map<String, Value>,
) -> std::io::Result<()> {
    append_event_at(path, kind, ts, fields)
}

/// Record one cron/job run. `started_at` is when the job began (epoch secs); the event is
/// stamped at the start so duration + start line up. `scope` is the job tier (branch/repo/
/// global); `repo`/`branch` locate non-global jobs so per-branch jobs of the same name don't
/// collide. `output_tail` is the last lines of the job's combined output, kept for failures.
#[allow(clippy::too_many_arguments)]
pub fn log_cron_run(
    job: &str,
    scope: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    started_at: i64,
    duration_ms: i64,
    exit_code: i32,
    output_tail: &str,
) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("job".into(), json!(job));
    fields.insert("scope".into(), json!(scope));
    if let Some(r) = repo {
        fields.insert("repo".into(), json!(r));
    }
    if let Some(b) = branch {
        fields.insert("branch".into(), json!(b));
    }
    fields.insert("duration_ms".into(), json!(duration_ms));
    fields.insert("exit_code".into(), json!(exit_code));
    fields.insert("ok".into(), json!(exit_code == 0));
    fields.insert("output_tail".into(), json!(output_tail));
    append_at("cron_run", started_at, fields)
}

/// Markers older than this are treated as dead — a runner SIGKILLed before its [`RunningGuard`]
/// could clear the file. No job in this repo runs anywhere near 30 minutes.
pub const RUNNING_STALE_SECS: i64 = 1800;

fn running_dir() -> PathBuf {
    state_dir().join("running")
}

/// RAII marker that a job is currently running. While alive, a file `running/<job>` (content:
/// start epoch) exists; dropping the guard removes it. This is what lets `bn cron running`
/// show a live "a cron is firing" badge in the tmux bar. Drop runs on every normal return from
/// the job runner; a crash leaves a stale file that [`running_jobs`] ages out.
pub struct RunningGuard {
    path: PathBuf,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        // Clear the running badge; a failed remove just leaves a marker that `running_jobs`
        // ages out as stale, so the error is safe to drop.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Mark `job` running until the returned guard drops. Best-effort: a filesystem hiccup just
/// means no badge, never a failed job.
pub fn running_marker(job: &str) -> RunningGuard {
    let dir = running_dir();
    // Both writes are best-effort (see fn doc): a filesystem hiccup just means no badge.
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(job.replace(['/', '\\'], "_"));
    let _ = std::fs::write(&path, now_epoch_secs().to_string());
    RunningGuard { path }
}

/// Jobs currently marked running (oldest-started first), skipping stale markers from a crashed
/// runner. Empty when nothing is running. Backs `bn cron running`.
pub fn running_jobs() -> Vec<String> {
    let now = now_epoch_secs();
    let Ok(entries) = std::fs::read_dir(running_dir()) else {
        return Vec::new();
    };
    let mut jobs: Vec<(i64, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let started = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(0);
            (now - started <= RUNNING_STALE_SECS)
                .then(|| (started, e.file_name().to_string_lossy().into_owned()))
        })
        .collect();
    jobs.sort_by_key(|(t, _)| *t);
    jobs.into_iter().map(|(_, n)| n).collect()
}

/// Every event in the machine-local cron/state log, in file order. Unparseable lines are
/// skipped, so a torn append never poisons reads.
pub fn read_all() -> Vec<Event> {
    read_events_at(&log_path())
}

/// Structured orchestrator events in file order. Other event kinds are excluded.
pub fn orchestrator_events() -> Vec<OrchestratorEvent> {
    read_all()
        .iter()
        .filter_map(OrchestratorEvent::from_event)
        .collect()
}

/// Latest active degradation per `(repo, reason)`, oldest first. An event with `active: false`
/// resolves the preceding warning without deleting history.
pub fn active_orchestrator_degradations(
    events: &[OrchestratorEvent],
    repo: Option<&str>,
) -> Vec<OrchestratorEvent> {
    let mut latest: HashMap<(Option<String>, String), OrchestratorEvent> = HashMap::new();
    for event in events {
        if event.active.is_none() || repo.is_some_and(|want| event.repo.as_deref() != Some(want)) {
            continue;
        }
        latest.insert((event.repo.clone(), event.reason.clone()), event.clone());
    }
    let mut active: Vec<_> = latest
        .into_values()
        .filter(|event| event.active == Some(true))
        .collect();
    active.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.reason.cmp(&b.reason)));
    active
}

/// Rolled-up status for one cron job, derived from its `cron_run` events. Identified by the
/// (job, repo, branch) triple so a per-branch job's runs don't merge across branches.
#[derive(Debug, Serialize)]
pub struct CronJobStatus {
    pub job: String,
    pub scope: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub last_run: i64,
    pub last_exit: i32,
    pub last_duration_ms: i64,
    pub ok: bool,
    pub runs: usize,
    pub failures: usize,
}

/// Per-job cron status from the event log: latest run, its exit, and run/failure counts.
/// Most-recently-run first. The machine-health view (vs `bn doctor`, the attention view).
pub fn cron_status() -> Vec<CronJobStatus> {
    // key by (job, repo, branch) so per-branch jobs of the same name stay distinct
    let mut by_key: HashMap<(String, String, String), Vec<Event>> = HashMap::new();
    for ev in read_all() {
        if ev.kind != "cron_run" {
            continue;
        }
        let Some(job) = field_str(&ev, "job") else {
            continue;
        };
        let key = (
            job,
            field_str(&ev, "repo").unwrap_or_default(),
            field_str(&ev, "branch").unwrap_or_default(),
        );
        by_key.entry(key).or_default().push(ev);
    }

    let mut out: Vec<CronJobStatus> = by_key
        .into_values()
        .filter_map(|mut runs| {
            runs.sort_by_key(|e| e.ts);
            let last = runs.last()?;
            let last_exit = field_i64(last, "exit_code").unwrap_or(-1) as i32;
            Some(CronJobStatus {
                job: field_str(last, "job").unwrap_or_default(),
                scope: field_str(last, "scope").unwrap_or_else(|| "global".into()),
                repo: field_str(last, "repo"),
                branch: field_str(last, "branch"),
                last_run: last.ts,
                last_exit,
                last_duration_ms: field_i64(last, "duration_ms").unwrap_or(0),
                ok: last_exit == 0,
                runs: runs.len(),
                failures: runs
                    .iter()
                    .filter(|e| field_i64(e, "exit_code").unwrap_or(-1) != 0)
                    .count(),
            })
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.last_run));
    out
}

/// One cron run's detail, including its captured output tail — the substrate for
/// `bn cron show` and the failure listing.
#[derive(Debug, Serialize)]
pub struct CronRunDetail {
    pub job: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub ts: i64,
    pub exit_code: i32,
    pub duration_ms: i64,
    pub output_tail: String,
}

fn run_detail(e: &Event) -> CronRunDetail {
    CronRunDetail {
        job: field_str(e, "job").unwrap_or_default(),
        repo: field_str(e, "repo"),
        branch: field_str(e, "branch"),
        ts: e.ts,
        exit_code: field_i64(e, "exit_code").unwrap_or(-1) as i32,
        duration_ms: field_i64(e, "duration_ms").unwrap_or(0),
        output_tail: field_str(e, "output_tail").unwrap_or_default(),
    }
}

/// The most recent run of `job` (any scope) with its captured output tail. `None` if it has
/// never run. Backs `bn cron show <job>` — how you read what a job last produced, including the
/// error output of a failed run.
pub fn last_run(job: &str) -> Option<CronRunDetail> {
    read_all()
        .into_iter()
        .filter(|e| e.kind == "cron_run" && field_str(e, "job").as_deref() == Some(job))
        .max_by_key(|e| e.ts)
        .map(|e| run_detail(&e))
}

/// The most recent failed (`exit_code != 0`) cron runs, newest first, capped at `limit`. Backs
/// `bn cron failures` — the detail behind the failure counts in `bn cron status`.
pub fn recent_failures(limit: usize) -> Vec<CronRunDetail> {
    let mut fails: Vec<CronRunDetail> = read_all()
        .iter()
        .filter(|e| e.kind == "cron_run" && field_i64(e, "exit_code").unwrap_or(0) != 0)
        .map(run_detail)
        .collect();
    fails.sort_by_key(|d| std::cmp::Reverse(d.ts));
    fails.truncate(limit);
    fails
}

fn field_str(ev: &Event, key: &str) -> Option<String> {
    ev.fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn field_i64(ev: &Event, key: &str) -> Option<i64> {
    ev.fields.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_state_dir_path as with_state_dir;

    #[test]
    fn append_and_read_roundtrip() {
        with_state_dir(|_| {
            log_cron_run("refresh", "global", None, None, 1000, 250, 0, "ok\n").unwrap();
            log_cron_run("refresh", "global", None, None, 2000, 300, 1, "boom\n").unwrap();
            let evs = read_all();
            assert_eq!(evs.len(), 2);
            assert_eq!(evs[0].kind, "cron_run");
            assert_eq!(evs[1].ts, 2000);
        });
    }

    #[test]
    fn orchestrator_event_view_preserves_legacy_events_and_structured_context() {
        let legacy = Event {
            kind: "orchestrate_start".into(),
            ts: 10,
            fields: Map::from_iter([("repo".into(), json!("bn"))]),
        };
        let legacy = OrchestratorEvent::from_event(&legacy).unwrap();
        assert_eq!(legacy.level, EventLevel::Info);
        assert_eq!(legacy.reason, "orchestrate_start");
        assert_eq!(legacy.repo.as_deref(), Some("bn"));

        let structured = Event {
            kind: "orchestrate_degradation".into(),
            ts: 20,
            fields: Map::from_iter([
                ("level".into(), json!("warn")),
                ("reason".into(), json!("tmux-unreachable")),
                ("repo".into(), json!("bn")),
                ("item_id".into(), json!("710")),
                ("error".into(), json!("no server running")),
                ("active".into(), json!(true)),
            ]),
        };
        let structured = OrchestratorEvent::from_event(&structured).unwrap();
        assert_eq!(structured.level, EventLevel::Warn);
        assert_eq!(structured.reason, "tmux-unreachable");
        assert_eq!(structured.item_id.as_deref(), Some("710"));
        assert_eq!(structured.error.as_deref(), Some("no server running"));
        assert_eq!(structured.active, Some(true));
    }

    #[test]
    fn active_degradations_follow_latest_warning_recovery_transition() {
        let event = |ts, repo: &str, reason: &str, active| OrchestratorEvent {
            kind: "orchestrate_degradation".into(),
            ts,
            level: if active {
                EventLevel::Warn
            } else {
                EventLevel::Info
            },
            reason: reason.into(),
            repo: Some(repo.into()),
            item_id: None,
            error: None,
            active: Some(active),
            fields: Map::new(),
        };
        let events = vec![
            event(10, "bn", "tmux-unreachable", true),
            event(20, "other", "backlog-compact-skipped", true),
            event(30, "bn", "tmux-unreachable", false),
            event(40, "bn", "daemon-detached-fallback", true),
        ];

        let active = active_orchestrator_degradations(&events, Some("bn"));
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].reason, "daemon-detached-fallback");
    }

    #[test]
    fn running_marker_present_then_cleared_on_drop() {
        with_state_dir(|_| {
            assert!(running_jobs().is_empty());
            let guard = running_marker("refresh-all");
            assert_eq!(running_jobs(), vec!["refresh-all".to_string()]);
            drop(guard);
            assert!(running_jobs().is_empty(), "drop clears the marker");
        });
    }

    #[test]
    fn running_jobs_ignores_stale_markers() {
        with_state_dir(|dir| {
            let run = dir.join("running");
            std::fs::create_dir_all(&run).unwrap();
            // A crashed runner's marker, far older than the stale threshold.
            let old = now_epoch_secs() - RUNNING_STALE_SECS - 60;
            std::fs::write(run.join("zombie"), old.to_string()).unwrap();
            std::fs::write(run.join("fresh"), now_epoch_secs().to_string()).unwrap();
            assert_eq!(running_jobs(), vec!["fresh".to_string()]);
        });
    }

    #[test]
    fn cron_status_groups_and_counts() {
        with_state_dir(|_| {
            log_cron_run("refresh", "global", None, None, 1000, 250, 0, "").unwrap();
            log_cron_run("refresh", "global", None, None, 3000, 300, 1, "fail").unwrap();
            log_cron_run("fetch", "global", None, None, 2000, 100, 0, "").unwrap();
            let st = cron_status();
            assert_eq!(st.len(), 2);
            // sorted most-recent-run first → refresh (ts 3000) before fetch (ts 2000)
            assert_eq!(st[0].job, "refresh");
            assert_eq!(st[0].last_run, 3000);
            assert_eq!(st[0].last_exit, 1);
            assert!(!st[0].ok);
            assert_eq!(st[0].runs, 2);
            assert_eq!(st[0].failures, 1);
            assert_eq!(st[1].job, "fetch");
            assert!(st[1].ok);
        });
    }

    #[test]
    fn same_job_name_on_different_branches_stays_distinct() {
        with_state_dir(|_| {
            log_cron_run(
                "check",
                "branch",
                Some("bn"),
                Some("feat-x"),
                1000,
                5,
                0,
                "",
            )
            .unwrap();
            log_cron_run(
                "check",
                "branch",
                Some("bn"),
                Some("feat-y"),
                2000,
                5,
                1,
                "",
            )
            .unwrap();
            let st = cron_status();
            assert_eq!(st.len(), 2, "per-branch jobs of the same name don't merge");
            // most recent first → feat-y (failed)
            assert_eq!(st[0].branch.as_deref(), Some("feat-y"));
            assert!(!st[0].ok);
            assert_eq!(st[1].branch.as_deref(), Some("feat-x"));
            assert!(st[1].ok);
        });
    }

    #[test]
    fn last_run_returns_latest_with_tail() {
        with_state_dir(|_| {
            log_cron_run("j", "global", None, None, 1000, 5, 0, "old").unwrap();
            log_cron_run("j", "global", None, None, 2000, 7, 1, "boom\ntrace").unwrap();
            let d = last_run("j").unwrap();
            assert_eq!(d.ts, 2000);
            assert_eq!(d.exit_code, 1);
            assert_eq!(d.output_tail, "boom\ntrace");
            assert!(last_run("nope").is_none());
        });
    }

    #[test]
    fn recent_failures_newest_first_and_capped() {
        with_state_dir(|_| {
            log_cron_run("a", "global", None, None, 1000, 5, 1, "f1").unwrap();
            log_cron_run("b", "global", None, None, 2000, 5, 0, "ok").unwrap();
            log_cron_run("c", "global", None, None, 3000, 5, 2, "f2").unwrap();
            let f = recent_failures(10);
            assert_eq!(f.len(), 2);
            assert_eq!(f[0].job, "c"); // newest first
            assert_eq!(f[1].job, "a");
            assert_eq!(recent_failures(1).len(), 1);
        });
    }

    #[test]
    fn read_all_empty_when_no_log() {
        with_state_dir(|_| {
            assert!(read_all().is_empty());
            assert!(cron_status().is_empty());
        });
    }

    #[test]
    fn skips_non_cron_events_in_status() {
        with_state_dir(|_| {
            append_at("pr_opened", 500, {
                let mut m = Map::new();
                m.insert("pr".into(), json!("url"));
                m
            })
            .unwrap();
            log_cron_run("refresh", "global", None, None, 1000, 10, 0, "").unwrap();
            assert_eq!(read_all().len(), 2);
            let st = cron_status();
            assert_eq!(st.len(), 1, "only cron_run events become job status");
            assert_eq!(st[0].job, "refresh");
        });
    }
}
