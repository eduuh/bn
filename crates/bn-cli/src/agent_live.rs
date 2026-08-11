//! `bn agent live` — the joined agent fleet, the machine-readable "what are all my agents doing".
//!
//! Two independent signals describe a live agent, and neither alone is the whole picture:
//!   - **Plane A** — the per-window `@bn_agent` tmux option (working / waiting / done), set by each
//!     harness's lifecycle hooks. This is the "does it need me *right now*" signal, and it's
//!     harness-agnostic (Claude and Copilot both drive it).
//!   - **Plane B** — [`bn_core::agent::supervise`]'s health (active / idle / **stuck**), derived
//!     from the session logs on disk. This is the "is it wedged" signal.
//!
//! This module joins them on the `repo:branch` key so one row carries both — the input a fleet
//! board (or an orchestrator) needs. The join is **case-insensitive**: Plane B lowercases the repo
//! (via the note key), while a tmux session name is whatever case the worktree dir had, so a naive
//! join would silently drop mixed-case repos. The join stays a pure function over the two inputs so
//! it's unit-testable without a tmux server.

use bn_core::agent::{AgentHealth, Fleet, SessionRecord, SessionStatus};
use bn_core::render::Envelope;
use bn_core::resolver::{Config, Resolved};
use bn_core::runner::{CommandRunner, RealRunner};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The hidden fzf callback stage a `bn fleet pick` session re-invokes (`--pick-stage <s>`). Only
/// `preview` today: rendering the focused row's live pane. Selection + jump happen in the parent
/// process (after fzf closes), so no `list`/action stages are needed.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum FleetPickStage {
    /// Render the detail panel for the focused row — a live `tmux capture-pane` of its window.
    Preview,
}

/// One tmux window's live-agent decoration (Plane A). `state` is the raw `@bn_agent` value
/// (`working`/`waiting`/`done`, or empty for an undecorated window); `since` is `@bn_agent_since`.
#[derive(Debug, Clone, PartialEq)]
struct WinState {
    session: String,
    window: String,
    repo: String,
    branch: String,
    state: String,
    since: i64,
}

/// One joined fleet row: an agent seen by Plane A, Plane B, or both, keyed by `repo:branch`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveView {
    pub repo: String,
    pub branch: String,
    /// Lowercased `repo:branch` — the tmux send-keys target and the join key.
    pub target: String,
    /// `claude` | `copilot`, from Plane B. `None` when only a decorated window is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Plane A live state: `working` | `waiting` | `done` | `""` (undecorated / session-only).
    pub state: String,
    /// Plane B health. `None` when only a window is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<AgentHealth>,
    /// Seconds since the last recorded activity (Plane B).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<i64>,
    /// Whether a live tmux window exists for this agent (Plane A). If false, the session runs but
    /// its window is gone — you can't jump to it, only re-open it.
    pub has_window: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<i64>,
    /// Cumulative **output** tokens burned so far by the live session — the cross-harness usage
    /// signal (the only figure both harnesses expose mid-session; see `LiveAgent`). `None` for a
    /// window-only row whose session log isn't attributable yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
}

pub const FLEET_SNAPSHOT_SCHEMA: &str = "bn.fleet-snapshot.v1";
const RECENT_AGENT_SECS: i64 = 7 * 24 * 60 * 60;

/// One worktree-centric fleet row. The durable session registry supplies identity and artifacts;
/// the live fleet overlay supplies the current lifecycle and pane liveness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetSnapshotRow {
    pub id: String,
    pub lane: String,
    pub target: String,
    pub worktree: String,
    pub repo: String,
    pub branch: String,
    pub state: String,
    pub status: String,
    pub state_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<AgentHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<i64>,
    pub agent_alive: bool,
    pub has_window: bool,
    pub backend: String,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<i64>,
    pub over_budget: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_channel: Option<String>,
    pub session_id: String,
    pub updated_at: i64,
}

/// Stable machine-readable contract rendered by both `bn fleet` surfaces.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetSnapshot {
    pub schema: &'static str,
    pub generated_at: i64,
    pub agents: Vec<FleetSnapshotRow>,
    pub count: usize,
    pub total_tokens: i64,
    pub over_budget: usize,
    pub remote_runs: Vec<crate::cmd::remote_track::RemoteRunView>,
}

/// The join key for a repo/branch pair: lowercased `repo:branch`, so Plane A (raw-case session
/// name) and Plane B (lowercased note key) meet on the same string.
fn key(repo: &str, branch: &str) -> String {
    format!("{}:{}", repo.to_lowercase(), branch.to_lowercase())
}

fn bounded_tmux(args: &[&str], timeout: Duration) -> Option<String> {
    let out = RealRunner.run_with_timeout("tmux", args, timeout);
    out.success.then_some(out.stdout)
}

fn normalized_worktree(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn session_priority(status: SessionStatus) -> u8 {
    match status {
        SessionStatus::Live | SessionStatus::Idle => 2,
        SessionStatus::Gone => 1,
        SessionStatus::Done | SessionStatus::Failed => 0,
    }
}

fn registry_state(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Live => "working",
        SessionStatus::Idle => "idle",
        SessionStatus::Gone => "gone",
        SessionStatus::Done => "done",
        SessionStatus::Failed => "failed",
    }
}

fn live_by_session(rows: &[LiveView]) -> BTreeMap<String, &LiveView> {
    rows.iter()
        .map(|row| {
            let id = row.session_id.as_deref().map_or_else(
                || format!("tmux:{}", row.target.to_lowercase()),
                bn_core::cost::canonical_session_id,
            );
            (id, row)
        })
        .collect()
}

fn select_worktree_sessions<'a>(
    registry: &'a [SessionRecord],
    existing_worktrees: &BTreeSet<PathBuf>,
    now: i64,
) -> Vec<&'a SessionRecord> {
    let mut selected: BTreeMap<PathBuf, &SessionRecord> = BTreeMap::new();
    for row in registry.iter().filter(|row| {
        !row.worktree.is_empty()
            && (matches!(row.status, SessionStatus::Live | SessionStatus::Idle)
                || now.saturating_sub(row.updated_at) <= RECENT_AGENT_SECS)
    }) {
        let worktree = normalized_worktree(&row.worktree);
        if !existing_worktrees.contains(&worktree) {
            continue;
        }
        match selected.get(&worktree) {
            Some(current)
                if (
                    session_priority(current.status),
                    current.updated_at,
                    current.created_at,
                ) >= (session_priority(row.status), row.updated_at, row.created_at) => {}
            _ => {
                selected.insert(worktree, row);
            }
        }
    }
    selected.into_values().collect()
}

fn augment_registry_with_live(
    cfg: &Config,
    registry: &mut Vec<SessionRecord>,
    live: &[LiveView],
    now: i64,
) {
    let mut known = registry
        .iter()
        .map(|row| bn_core::cost::canonical_session_id(&row.session_id))
        .collect::<BTreeSet<_>>();
    for row in live {
        let session_id = row
            .session_id
            .clone()
            .unwrap_or_else(|| format!("tmux:{}", row.target.to_lowercase()));
        let canonical_id = bn_core::cost::canonical_session_id(&session_id);
        if !known.insert(canonical_id) {
            continue;
        }
        let worktree = cfg
            .worktree_dir
            .join(&row.repo)
            .join(row.branch.replace('/', "-"));
        let status = match row.health {
            Some(AgentHealth::Idle | AgentHealth::Stuck) => SessionStatus::Idle,
            Some(AgentHealth::Ended) => SessionStatus::Done,
            _ => SessionStatus::Live,
        };
        let note_ctx = Resolved {
            repo: row.repo.clone(),
            branch: row.branch.replace('/', "-"),
        };
        registry.push(SessionRecord {
            task_name: row.branch.clone(),
            harness: row.harness.clone().unwrap_or_default(),
            repo: row.repo.clone(),
            branch: row.branch.clone(),
            worktree: worktree.to_string_lossy().into_owned(),
            session_id,
            controller_id: None,
            status,
            pr_url: bn_core::commands::read_pr(cfg, &note_ctx),
            created_at: now.saturating_sub(row.idle_secs.unwrap_or_default()),
            updated_at: now.saturating_sub(row.idle_secs.unwrap_or_default()),
        });
    }
}

fn snapshot_from(
    registry: &[SessionRecord],
    live: &[LiveView],
    existing_worktrees: &BTreeSet<PathBuf>,
    report_artifacts: &BTreeMap<PathBuf, String>,
    generated_at: i64,
    budget: Option<i64>,
) -> FleetSnapshot {
    let live = live_by_session(live);
    let mut rows = select_worktree_sessions(registry, existing_worktrees, generated_at)
        .into_iter()
        .map(|session| {
            let live_row = live.get(&bn_core::cost::canonical_session_id(&session.session_id));
            let status = live_row
                .map(|row| status_label(row))
                .unwrap_or_else(|| registry_state(session.status));
            let state = live_row
                .map(|row| {
                    if row.state.is_empty() {
                        status.to_string()
                    } else {
                        row.state.clone()
                    }
                })
                .unwrap_or_else(|| registry_state(session.status).to_string());
            let state_source = match live_row {
                Some(row) if !row.state.is_empty() => "tmux",
                Some(_) => "session",
                None => "registry",
            };
            let worktree = normalized_worktree(&session.worktree);
            let artifact = session
                .pr_url
                .clone()
                .or_else(|| report_artifacts.get(&worktree).cloned());
            let lane = live_row.filter(|row| row.has_window).map_or_else(
                // No live window: address it by what the registry records, not by the worktree's
                // basename. That basename is the branch only when the layout happens to be
                // `<root>/<repo>/<branch>` — for a pool slot it is the slot number (`bn:1`) and
                // for a repo checked out at its own path it is the repo name, so the "target" it
                // produced was one `bn agent send` could never resolve.
                || key(&session.repo, &session.branch),
                |row| row.target.clone(),
            );
            FleetSnapshotRow {
                id: session.task_name.clone(),
                lane: lane.clone(),
                target: lane.clone(),
                worktree: worktree.to_string_lossy().into_owned(),
                repo: session.repo.clone(),
                branch: session.branch.clone(),
                state,
                status: status.to_string(),
                state_source: state_source.to_string(),
                health: live_row.and_then(|row| row.health),
                idle_secs: live_row.and_then(|row| row.idle_secs),
                agent_alive: live_row.is_some_and(|row| row.has_window),
                has_window: live_row.is_some_and(|row| row.has_window),
                backend: "local".to_string(),
                harness: session.harness.clone(),
                pipeline_stage: None,
                tokens: live_row.and_then(|row| row.tokens),
                turns: live_row.and_then(|row| row.turns),
                over_budget: live_row.is_some_and(|row| over_budget(row.tokens, budget)),
                artifact,
                return_channel: session
                    .controller_id
                    .as_ref()
                    .map(|id| format!("controller:{id}"))
                    .or_else(|| {
                        live_row
                            .filter(|row| row.has_window)
                            .map(|row| row.target.clone())
                    })
                    .or(Some(lane)),
                session_id: session.session_id.clone(),
                updated_at: session.updated_at,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        board_state_rank(&a.status)
            .cmp(&board_state_rank(&b.status))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.worktree.cmp(&b.worktree))
    });
    let total_tokens = rows.iter().filter_map(|row| row.tokens).sum();
    let over_budget = rows.iter().filter(|row| row.over_budget).count();
    let count = rows.len();
    FleetSnapshot {
        schema: FLEET_SNAPSHOT_SCHEMA,
        generated_at,
        agents: rows,
        count,
        total_tokens,
        over_budget,
        remote_runs: Vec::new(),
    }
}

fn board_state_rank(state: &str) -> u8 {
    match state {
        "needs-you" | "blocked" | "needs-decision" | "failed" | "limited" | "stuck" => 0,
        "working" | "active" | "idle" | "starting" => 1,
        "queued" | "gone" => 2,
        "done" => 3,
        _ => 4,
    }
}

/// Collect the read-only fleet snapshot. Only worktrees that still exist locally are retained;
/// the durable registry keeps a completed or pane-less lane visible without probing a network.
pub fn collect_snapshot(cfg: &Config, idle: i64, stuck: i64, budget: Option<i64>) -> FleetSnapshot {
    let mut registry = bn_core::agent::session_registry(cfg).unwrap_or_default();
    let live = collect(cfg, idle, stuck);
    let now = bn_core::util::now_epoch_secs();
    augment_registry_with_live(cfg, &mut registry, &live, now);
    let existing_worktrees = registry
        .iter()
        .filter(|row| !row.worktree.is_empty() && Path::new(&row.worktree).is_dir())
        .map(|row| normalized_worktree(&row.worktree))
        .collect::<BTreeSet<_>>();
    let report_artifacts = existing_worktrees
        .iter()
        .filter_map(|worktree| {
            let report = worktree.join("report.md");
            report
                .is_file()
                .then(|| (worktree.clone(), report.to_string_lossy().into_owned()))
        })
        .collect();
    let mut snapshot = snapshot_from(
        &registry,
        &live,
        &existing_worktrees,
        &report_artifacts,
        now,
        budget,
    );
    for row in &mut snapshot.agents {
        row.pipeline_stage =
            pipeline_stage(cfg, &row.repo, &row.branch).map(|stage| stage.as_str().to_string());
    }
    snapshot.remote_runs =
        crate::cmd::remote_track::fleet_views_snapshot(cfg, &bn_core::runner::RealRunner);
    snapshot
}

/// Join Plane A windows with the Plane B fleet into one row per agent. Pure → unit-testable.
///
/// A window with an **empty** `@bn_agent` that matches no fleet agent is an ordinary shell/build
/// window, not an agent — it's dropped. A decorated window (non-empty state) with no fleet match is
/// kept as a window-only row (the harness hook fired but the session log isn't attributable yet). A
/// fleet agent with no window is kept with `has_window = false`.
fn join(wins: &[WinState], fleet: &Fleet) -> Vec<LiveView> {
    let mut rows: BTreeMap<String, LiveView> = BTreeMap::new();

    // Seed from Plane B (the fleet): authoritative repo/branch/health/harness.
    for a in &fleet.agents {
        let k = key(&a.repo, &a.branch);
        rows.insert(
            k.clone(),
            LiveView {
                repo: a.repo.to_lowercase(),
                branch: a.branch.to_lowercase(),
                target: k,
                harness: Some(a.harness.clone()),
                state: String::new(),
                health: Some(a.health),
                idle_secs: Some(a.idle_secs),
                has_window: false,
                session_id: Some(a.session_id.clone()),
                turns: Some(a.turns),
                tokens: Some(a.output_tokens),
            },
        );
    }

    // Overlay Plane A (tmux windows). Merge onto a fleet row when the key matches; otherwise add a
    // window-only row, but only for a *decorated* window (an actual agent), never a plain shell.
    for w in wins {
        let repo = if w.repo.is_empty() {
            &w.session
        } else {
            &w.repo
        };
        let branch = if w.branch.is_empty() {
            &w.window
        } else {
            &w.branch
        };
        let k = key(repo, branch);
        let target = if w.repo.is_empty() && w.branch.is_empty() {
            k.clone()
        } else {
            format!("{}:{}", w.session, w.window)
        };
        match rows.get_mut(&k) {
            Some(row) => {
                row.has_window = true;
                row.state = w.state.clone();
                row.target = target;
            }
            None if !w.state.is_empty() => {
                rows.insert(
                    k.clone(),
                    LiveView {
                        repo: repo.to_lowercase(),
                        branch: branch.to_lowercase(),
                        target,
                        harness: None,
                        state: w.state.clone(),
                        health: None,
                        idle_secs: None,
                        has_window: true,
                        session_id: None,
                        turns: None,
                        tokens: None,
                    },
                );
            }
            None => {} // undecorated window with no fleet match → an ordinary shell, ignore.
        }
    }

    rows.into_values().collect()
}

/// Read Plane A from tmux: one [`WinState`] per window across every session.
fn read_windows() -> Vec<WinState> {
    let fmt = "#{session_name}\t#{window_name}\t#{@bn_repo}\t#{@bn_branch}\t#{@bn_agent}\t#{@bn_agent_since}";
    let out = match bounded_tmux(&["list-windows", "-a", "-F", fmt], Duration::from_secs(2)) {
        Some(o) => o,
        None => return Vec::new(),
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() >= 6 {
                return Some(WinState {
                    session: f[0].to_string(),
                    window: f[1].to_string(),
                    repo: f[2].to_string(),
                    branch: f[3].to_string(),
                    state: f[4].to_string(),
                    since: f[5].parse().unwrap_or(0),
                });
            }
            (f.len() >= 4).then(|| WinState {
                session: f[0].to_string(),
                window: f[1].to_string(),
                repo: String::new(),
                branch: String::new(),
                state: f[2].to_string(),
                since: f[3].parse().unwrap_or(0),
            })
        })
        .collect()
}

/// Collect the joined fleet: Plane B (supervise) + Plane A (tmux windows), merged. The shared core
/// behind both `bn agent live` and the `bn fleet` board.
pub fn collect(cfg: &Config, idle: i64, stuck: i64) -> Vec<LiveView> {
    let fleet = bn_core::agent::supervise(cfg, idle, stuck);
    let mut rows = join(&read_windows(), &fleet);
    // Plane C: when Plane A didn't flag it, glance at the live pane. A windowed agent sitting at a
    // bare prompt with no spinner is blocked on input — infer `waiting` so `board_rank` and the
    // mark/label tables surface it as needs-you. This is the signal Copilot never fires when it
    // pauses for input (e.g. a "Shall I proceed?" prompt). A pane showing the usage-limit dialog
    // infers `limited` (bn fleet --fix switches it to the other harness). Only touch rows whose
    // Plane-A state is inferable (empty / stale `working` / `waiting`, which the limit dialog can
    // race); a real `done`/`blocked` is authoritative.
    for r in &mut rows {
        if r.has_window && matches!(r.state.as_str(), "" | "working" | "waiting") {
            r.state = classify_lifecycle(
                &r.state,
                &capture_tail(&r.target, TAIL_SCAN_LINES),
                r.health,
            )
            .state;
        }
    }
    rows
}

/// Is this agent **captain-relevant** (needs you), and why? The escalation classifier behind away
/// mode: `waiting`/`blocked`/`needs-decision`/`failed` states and a Plane-B `stuck` agent escalate;
/// `working`/`active`/`idle` are absorbed (routine); `done` is a light "finished" signal worth
/// surfacing once. Returns the reason label, or `None` to absorb.
pub fn escalation(r: &LiveView) -> Option<&'static str> {
    match lifecycle(r) {
        AgentLifecycle::NeedsYou => Some("needs you"),
        AgentLifecycle::Limited => Some("usage-limited"),
        AgentLifecycle::Blocked => Some("blocked"),
        AgentLifecycle::NeedsDecision => Some("needs a decision"),
        AgentLifecycle::Failed => Some("failed"),
        AgentLifecycle::Done => Some("done"),
        AgentLifecycle::Stuck => Some("stuck"),
        _ => None,
    }
}

/// The captain-relevant agents in the fleet, each with its escalation reason — the fleet side of the
/// away-mode digest. Absorbs the routine (working/active/idle) agents.
pub fn escalations(cfg: &Config, idle: i64, stuck: i64) -> Vec<(LiveView, &'static str)> {
    collect(cfg, idle, stuck)
        .into_iter()
        .filter_map(|r| escalation(&r).map(|why| (r, why)))
        .collect()
}

/// Classify newly actionable fleet transitions and publish them to the shared wake queue. The
/// snapshot contains only the current actionable keys, so a resolved transition can fire again if
/// it later returns.
pub fn publish_actionable_wakes(
    cfg: &Config,
    idle: i64,
    stuck: i64,
    now: i64,
) -> std::io::Result<(usize, usize)> {
    let seen_path = bn_core::resolver::state_dir().join("fleet-wake-seen.json");
    let previous: std::collections::HashSet<String> = std::fs::read_to_string(&seen_path)
        .ok()
        .and_then(|body| serde_json::from_str::<Vec<String>>(&body).ok())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let current = escalations(cfg, idle, stuck);
    let keys: Vec<String> = current
        .iter()
        .map(|(row, reason)| format!("{}:{reason}", row.target))
        .collect();
    let queue = bn_core::wake::WakeQueue::default();
    let mut published = 0;
    for ((row, reason), key) in current.iter().zip(&keys) {
        if previous.contains(key) {
            continue;
        }
        let transition = match *reason {
            "blocked" | "needs a decision" | "needs you" | "usage-limited" => {
                bn_core::wake::FleetTransition::Blocked
            }
            "failed" => bn_core::wake::FleetTransition::Failed,
            "done" => bn_core::wake::FleetTransition::Done,
            "stuck" => bn_core::wake::FleetTransition::Stuck,
            _ => bn_core::wake::FleetTransition::Waiting,
        };
        // The event id names the *escalation*, not the instant it was observed. It used to embed
        // `now`, which meant the wake queue's `seen_event_ids` could never dedup it: the only
        // guard was the `previous` set below, and that file is rewritten unconditionally with
        // whatever this pass could see. One degraded pass (tmux briefly unreachable, a pane
        // capture race) dropped keys from it, and the next healthy pass re-toasted the entire
        // escalating fleet under brand-new ids. With a stable id the queue collapses the repeat,
        // and `previous` keeps its real job: letting a genuinely resolved-then-returned
        // escalation fire again.
        let wake = bn_core::wake::ActionableWake::classify(
            format!("fleet:{key}"),
            now,
            transition,
            format!("{} — {reason}", row.target),
            bn_core::wake::WakeOwner::None,
        )
        .expect("fleet escalation is actionable");
        if queue.publish(wake)? {
            published += 1;
        }
    }
    if let Some(parent) = seen_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        seen_path,
        serde_json::to_vec(&keys).map_err(std::io::Error::other)?,
    )?;
    Ok((published, current.len()))
}

/// The last `n` non-empty lines of a window's active pane — the "what is it doing right now" glance
/// for the fleet board. Targets the window exactly (its active pane); callers that already resolved
/// a window name to an exact target pass it through untouched rather than picking up a second `=`.
fn capture_tail(target: &str, n: usize) -> Vec<String> {
    let t = crate::cmd::agent::tmux_exact_target(target);
    let out = match bounded_tmux(
        &["capture-pane", "-p", "-t", &t],
        Duration::from_millis(500),
    ) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// How many non-empty tail lines to scan for the pane's live signal — enough to cover the input
/// box (top border / prompt / bottom border) plus the footer/spinner line beneath it.
const TAIL_SCAN_LINES: usize = 6;

/// A pane tail's read on what the agent is doing *right now*, for when Plane A stayed silent. The
/// fleet board and `bn agent live` already glance at the live pane; this classifies that glance so
/// a Copilot paused at a "Shall I proceed?" prompt (which never fires a `waiting` hook) still
/// surfaces as needs-you instead of a bare idle/active. Pure over the tail → unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailSignal {
    /// A live spinner / interrupt hint — the agent is mid-turn, not blocked on input.
    Working,
    /// The harness's usage-limit prompt ("Stop and wait for limit to reset / Upgrade your plan") —
    /// the agent is dead in the water until the limit resets or you switch harnesses.
    Limited,
    /// A bare, idle input prompt with no spinner — sitting waiting for you.
    Waiting,
    /// Neither could be read (no window, or an ambiguous pane) — infer nothing.
    Unknown,
}

/// The canonical lifecycle classification used by every fleet presentation. Plane-A states and
/// pane-tail signals take precedence when they describe the live turn; otherwise the supervisor's
/// typed [`AgentHealth`] is the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentLifecycle {
    NeedsYou,
    Limited,
    Blocked,
    NeedsDecision,
    Failed,
    Working,
    Done,
    Starting,
    Active,
    Idle,
    Stuck,
}

struct LifecycleClassification {
    state: String,
    lifecycle: AgentLifecycle,
}

/// Spinner / interrupt markers a harness prints only while a turn is in flight (and interruptible):
/// Claude's `esc to interrupt`, Copilot's `esc cancel`, and the `Working` spinner word. If the tail
/// carries any, the agent is busy — never waiting.
fn tail_has_spinner(tail: &[String]) -> bool {
    tail.iter().any(|l| {
        let s = l.to_ascii_lowercase();
        s.contains("esc to interrupt") || s.contains("esc cancel") || s.contains("working")
    })
}

/// The usage-limit pane signature: the option text of Claude's limit dialog ("Stop and wait for
/// limit to reset" / "Upgrade your plan"), plus the generic "usage limit reached" banner. Matched
/// case-insensitively over the tail. Checked *after* the spinner so an agent merely *talking about*
/// usage limits mid-turn (its pane still shows `esc to interrupt`) never reads as limited.
fn tail_has_usage_limit(tail: &[String]) -> bool {
    tail.iter().any(|l| {
        let s = l.to_ascii_lowercase();
        s.contains("wait for limit to reset")
            || s.contains("upgrade your plan")
            || s.contains("usage limit reached")
    })
}

/// Does a tail line show a bare, idle input prompt — just the harness prompt glyph, nothing typed?
/// Copilot's empty box is a lone `┃`; Claude's is `❯` (with a trailing NBSP, stripped by `trim`);
/// `>` / `│` cover generic prompts. A prompt with text after it, or a shell `$`/`%`, won't match.
fn tail_has_bare_prompt(tail: &[String]) -> bool {
    tail.iter()
        .any(|l| matches!(l.trim(), "┃" | "❯" | ">" | "│"))
}

/// Classify a pane tail: a spinner wins (busy), else the usage-limit dialog (limited), else a bare
/// prompt (waiting), else unknown. Spinner is checked first so a `❯ Thinking…` line under a live
/// `esc cancel` reads as working, not waiting; limited beats the bare-prompt check because the
/// limit dialog's selector can look prompt-like.
fn detect_tail_signal(tail: &[String]) -> TailSignal {
    if tail_has_spinner(tail) {
        TailSignal::Working
    } else if tail_has_usage_limit(tail) {
        TailSignal::Limited
    } else if tail_has_bare_prompt(tail) {
        TailSignal::Waiting
    } else {
        TailSignal::Unknown
    }
}

/// The one bounded retry policy for a tmux steer: resend the carriage return at most
/// `submit_attempts` times, and after each send poll the pane on the increasing delay schedule.
/// The 1.5s verification window gives a loaded harness time to start rendering without tight-looping
/// tmux; exhausting all three sends takes at most 4.5s before the caller escalates to resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubmitBackoff {
    submit_attempts: usize,
    verify_delays_ms: &'static [u64],
}

impl SubmitBackoff {
    pub(crate) fn submit_attempts(self) -> usize {
        self.submit_attempts
    }

    fn verify_delays_ms(self) -> &'static [u64] {
        self.verify_delays_ms
    }
}

pub(crate) const SUBMIT_BACKOFF: SubmitBackoff = SubmitBackoff {
    submit_attempts: 3,
    verify_delays_ms: &[100, 200, 400, 800],
};

/// Decide from a pane tail whether a just-submitted steer was accepted: a live spinner means the
/// turn started, so the message was submitted. A spinner-less pane still sitting at a primed input
/// means the harness's compose/autocomplete TUI ate the submit key. Pure over the tail → testable.
fn tail_confirms_submit(tail: &[String]) -> bool {
    tail_has_spinner(tail)
}

/// Verify a `bn agent send --keys` submit landed in pane `target`: after the carriage return, poll
/// the pane for a working spinner (the turn started). Returns true as soon as one appears, else
/// false after [`SUBMIT_BACKOFF`]'s verification window — the signal that the TUI swallowed the
/// Enter and the message is sitting unsent in the box (#306), so the caller can retry or escalate
/// to `--resume`.
pub(crate) fn steer_landed(target: &str) -> bool {
    for delay_ms in SUBMIT_BACKOFF.verify_delays_ms() {
        std::thread::sleep(std::time::Duration::from_millis(*delay_ms));
        if tail_confirms_submit(&capture_tail(target, TAIL_SCAN_LINES)) {
            return true;
        }
    }
    false
}

fn inferred_plane_state(plane_a: &str, signal: TailSignal) -> String {
    let inferable = plane_a.is_empty() || plane_a == "working";
    match signal {
        TailSignal::Limited if inferable || plane_a == "waiting" => "limited".to_string(),
        TailSignal::Working if plane_a == "waiting" => "working".to_string(),
        TailSignal::Waiting if inferable => "waiting".to_string(),
        _ => plane_a.to_string(),
    }
}

fn lifecycle_from_state(state: &str, health: Option<AgentHealth>) -> AgentLifecycle {
    match state {
        "waiting" => AgentLifecycle::NeedsYou,
        "limited" => AgentLifecycle::Limited,
        "blocked" => AgentLifecycle::Blocked,
        "needs-decision" => AgentLifecycle::NeedsDecision,
        "failed" => AgentLifecycle::Failed,
        "working" => AgentLifecycle::Working,
        "done" => AgentLifecycle::Done,
        _ => match health {
            Some(AgentHealth::Starting) => AgentLifecycle::Starting,
            Some(AgentHealth::Idle) => AgentLifecycle::Idle,
            Some(AgentHealth::Stuck) => AgentLifecycle::Stuck,
            // Nothing hosts the session any more — it finished, so it reads as done rather than as
            // a stalled agent someone should go and nudge.
            Some(AgentHealth::Ended) => AgentLifecycle::Done,
            Some(AgentHealth::Active) | None => AgentLifecycle::Active,
        },
    }
}

/// Reconcile lifecycle hooks, pane evidence, and supervisor health into one state.
fn classify_lifecycle(
    plane_a: &str,
    tail: &[String],
    health: Option<AgentHealth>,
) -> LifecycleClassification {
    let state = inferred_plane_state(plane_a, detect_tail_signal(tail));
    let lifecycle = lifecycle_from_state(&state, health);
    LifecycleClassification { state, lifecycle }
}

fn lifecycle(r: &LiveView) -> AgentLifecycle {
    classify_lifecycle(&r.state, &[], r.health).lifecycle
}

impl AgentLifecycle {
    fn status_label(self) -> &'static str {
        match self {
            AgentLifecycle::NeedsYou => "needs-you",
            AgentLifecycle::Limited => "limited",
            AgentLifecycle::Blocked => "blocked",
            AgentLifecycle::NeedsDecision => "needs-decision",
            AgentLifecycle::Failed => "failed",
            AgentLifecycle::Working => "working",
            AgentLifecycle::Done => "done",
            AgentLifecycle::Starting => "starting",
            AgentLifecycle::Active => "active",
            AgentLifecycle::Idle => "idle",
            AgentLifecycle::Stuck => "stuck",
        }
    }

    fn board_style(self) -> (&'static str, &'static str) {
        match self {
            AgentLifecycle::NeedsYou => ("1;33", "\u{f0f3} needs you"),
            AgentLifecycle::Limited => ("1;35", "\u{f254} limited  "),
            AgentLifecycle::Blocked => ("1;31", "\u{f05e} blocked  "),
            AgentLifecycle::NeedsDecision => ("1;33", "◆ decide   "),
            AgentLifecycle::Failed => ("1;31", "✗ failed   "),
            AgentLifecycle::Working => ("33", "\u{f013} working  "),
            AgentLifecycle::Done => ("32", "\u{f00c} done     "),
            AgentLifecycle::Starting => ("2", "◍ starting "),
            AgentLifecycle::Stuck => ("31", "○ stuck    "),
            AgentLifecycle::Idle => ("2", "◐ idle     "),
            AgentLifecycle::Active => ("2", "· active   "),
        }
    }
}

/// Rank a row for the board: who needs you first. needs-you states > stuck > working > done > rest.
fn board_rank(r: &LiveView) -> u8 {
    match lifecycle(r) {
        AgentLifecycle::NeedsYou
        | AgentLifecycle::Limited
        | AgentLifecycle::Blocked
        | AgentLifecycle::NeedsDecision
        | AgentLifecycle::Failed => 0,
        AgentLifecycle::Stuck => 1,
        AgentLifecycle::Working => 2,
        AgentLifecycle::Done => 3,
        AgentLifecycle::Idle => 4,
        AgentLifecycle::Starting | AgentLifecycle::Active => 5,
    }
}

/// A compact token count for the board: `950` → "950", `12_300` → "12.3k", `1_250_000` → "1.2M".
fn fmt_tokens(n: i64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Has this agent burned past its per-run token budget? A `None` budget (or unknown token count)
/// is never over — so an undecorated window-only row is never falsely flagged.
fn over_budget(tokens: Option<i64>, budget: Option<i64>) -> bool {
    matches!((tokens, budget), (Some(t), Some(b)) if t > b)
}

/// The `(ansi-color, glyph-label)` for a row's effective state — the fleet board's status column.
/// Shared by the read-only board ([`run_board`]) and the interactive picker ([`run_pick`]) so both
/// speak the same visual language (needs-you states first, then working/done, then Plane-B health).
fn board_style(r: &LiveView) -> (&'static str, &'static str) {
    lifecycle(r).board_style()
}

/// The machine-readable effective status for a row — the `--json` mirror of [`board_style`]'s
/// human label, so `bn fleet --json` exposes the same needs-you-first taxonomy the board shows:
/// `needs-you` | `limited` | `blocked` | `needs-decision` | `failed` | `working` | `done` |
/// `starting` | `stuck` | `idle` | `active`. Pure over the row → unit-testable.
fn status_label(r: &LiveView) -> &'static str {
    lifecycle(r).status_label()
}

fn pipeline_stage(
    cfg: &Config,
    repo: &str,
    branch: &str,
) -> Option<bn_core::pipeline::stage::Stage> {
    let ctx = Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    bn_core::store::read(&cfg.note_file(&ctx))
        .ok()
        .map(|note| {
            bn_core::pipeline::stage::current_for_note(&note)
                .unwrap_or(bn_core::pipeline::stage::Stage::DEFAULT)
        })
        .or_else(|| bn_core::pipeline::stage::current(repo, branch))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotGroup {
    UnderWay,
    Queued,
    Done,
}

fn snapshot_group(state: &str) -> SnapshotGroup {
    match state {
        "queued" | "starting" | "gone" => SnapshotGroup::Queued,
        "done" => SnapshotGroup::Done,
        _ => SnapshotGroup::UnderWay,
    }
}

fn snapshot_style(state: &str) -> (&'static str, &'static str) {
    match state {
        "needs-you" => ("1;33", "needs you"),
        "limited" => ("1;35", "limited"),
        "blocked" => ("1;31", "blocked"),
        "needs-decision" => ("1;33", "decision"),
        "failed" => ("1;31", "failed"),
        "stuck" => ("1;31", "stuck"),
        "working" | "active" => ("1;32", "working"),
        "idle" => ("33", "idle"),
        "queued" | "starting" => ("36", "queued"),
        "gone" => ("2", "gone"),
        "done" => ("2", "done"),
        _ => ("2", "unknown"),
    }
}

fn render_snapshot_section(
    snapshot: &FleetSnapshot,
    group: SnapshotGroup,
    title: &str,
    preview: bool,
) {
    let rows = snapshot
        .agents
        .iter()
        .filter(|row| snapshot_group(&row.status) == group)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }
    println!("\x1b[1m{title}\x1b[0m — {}", rows.len());
    for row in rows {
        let (color, label) = snapshot_style(&row.status);
        let alive = if row.agent_alive { "alive" } else { "absent" };
        let artifact = row
            .artifact
            .as_deref()
            .map(|value| format!("  \x1b[2m{value}\x1b[0m"))
            .unwrap_or_default();
        let stage = row
            .pipeline_stage
            .as_deref()
            .map(|value| format!("  \x1b[36m{value}\x1b[0m"))
            .unwrap_or_default();
        let tokens = row
            .tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| format!("  \x1b[2m{} tok\x1b[0m", fmt_tokens(tokens)))
            .unwrap_or_default();
        let over = if row.over_budget {
            "  \x1b[1;31m⚠ over budget\x1b[0m"
        } else {
            ""
        };
        println!(
            "  \x1b[{color}m{label:<10}\x1b[0m \x1b[1m{}\x1b[0m  \x1b[2m{} · {} · {alive}\x1b[0m{stage}{tokens}{over}{artifact}",
            row.worktree, row.lane, row.harness
        );
        if preview && row.agent_alive {
            if let Some(target) = row
                .return_channel
                .as_deref()
                .filter(|target| !target.starts_with("controller:"))
            {
                for line in capture_tail(target, 4) {
                    println!("      \x1b[2m│ {line}\x1b[0m");
                }
            }
        }
    }
    println!();
}

/// `bn fleet` — render the worktree-centric snapshot, grouped by lifecycle. Collection happens
/// once; this renderer never re-reads tmux, notes, registry state, or artifacts.
pub fn run_board(cfg: &Config, idle: i64, stuck: i64, budget: Option<i64>, preview: bool) {
    let snapshot = collect_snapshot(cfg, idle, stuck, budget);
    if snapshot.agents.is_empty() && snapshot.remote_runs.is_empty() {
        println!("No live agents.");
        return;
    }
    let needs_you = snapshot
        .agents
        .iter()
        .filter(|row| {
            matches!(
                row.status.as_str(),
                "needs-you" | "blocked" | "needs-decision" | "failed" | "limited" | "stuck"
            )
        })
        .count();
    let alive = snapshot.agents.iter().filter(|row| row.agent_alive).count();
    let mut header = format!(
        "\x1b[1mFleet\x1b[0m — {} worktree(s) · {alive} alive · {needs_you} need you",
        snapshot.agents.len()
    );
    if snapshot.total_tokens > 0 {
        header += &format!(" · {} tok", fmt_tokens(snapshot.total_tokens));
    }
    if snapshot.over_budget > 0 {
        header += &format!(" · \x1b[1;31m{} over budget\x1b[0m", snapshot.over_budget);
    }
    println!("{header}\n");
    render_snapshot_section(&snapshot, SnapshotGroup::UnderWay, "Under Way", preview);
    render_snapshot_section(&snapshot, SnapshotGroup::Queued, "Not running", preview);
    render_snapshot_section(&snapshot, SnapshotGroup::Done, "Done", preview);
    if !snapshot.remote_runs.is_empty() {
        let hdr = format!(
            "\x1b[1mRemote runs\x1b[0m — {} \x1b[2m(gh)\x1b[0m",
            snapshot.remote_runs.len()
        );
        println!("{hdr}");
        for v in &snapshot.remote_runs {
            println!("{}", crate::cmd::remote_track::render_line(v));
        }
        println!();
    }
    let limited = snapshot
        .agents
        .iter()
        .filter(|row| row.status == "limited")
        .collect::<Vec<_>>();
    if !limited.is_empty() {
        println!(
            "\x1b[1;35m{} agent(s) hit a usage limit\x1b[0m — switch them to the other harness:",
            limited.len()
        );
        for row in limited {
            println!(
                "  bn switch --repo {} --branch {} --in-place",
                row.repo, row.branch
            );
        }
        println!("\x1b[2mor `bn fleet --fix` to switch them all\x1b[0m\n");
    }
    println!(
        "\x1b[2mjump in: prefix j (or `bn jump`) · steer: bn agent send <target> \"msg\"\x1b[0m"
    );
}

/// The target visible width for the `--mobile` reflow — a narrow phone SSH terminal is ~40 cols.
const MOBILE_WIDTH: usize = 40;

/// A compact `(ansi-color, 5-char ascii tag)` for a row's state — the mobile board's status column.
/// ASCII-only (no Nerd Font glyphs, which render as double-width or tofu in a phone SSH client) and
/// fixed-width so rows align; needs-you states carry a leading `!` so they pop when scanning. Pure
/// over the row → unit-testable. Mirrors [`snapshot_style`]'s needs-you-first taxonomy.
fn mobile_status(state: &str) -> (&'static str, &'static str) {
    match state {
        "needs-you" => ("1;33", "!need"),
        "limited" => ("1;35", "!limt"),
        "blocked" => ("1;31", "!blok"),
        "needs-decision" => ("1;33", "!decd"),
        "failed" => ("1;31", "!fail"),
        "working" => ("33", "work "),
        "done" => ("32", "done "),
        "starting" | "queued" | "gone" => ("2", "queue"),
        "stuck" => ("31", "stuck"),
        "idle" => ("2", "idle "),
        "active" => ("2", "actv "),
        _ => ("2", "???? "),
    }
}

/// Truncate `s` to at most `max` display chars, marking a cut with a trailing `…`. Counts by
/// `char` (fleet targets are ASCII `repo:branch`, so char-count == display width). Pure.
fn truncate_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max == 0 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One mobile board line: `<5-char badge> <truncated target> <tok>` capped to `width` visible
/// chars so it never wraps at ~40 cols. The badge is fixed-width; the target absorbs the remaining
/// budget after the token suffix and a `*` window marker are reserved. ANSI colors wrap the plain
/// segments (they don't count toward the width). Pure over its inputs → unit-testable.
fn mobile_row(row: &FleetSnapshotRow, width: usize) -> String {
    let (color, tag) = mobile_status(&row.status);
    let tok = match row.tokens {
        Some(t) if t > 0 => format!(" {}", fmt_tokens(t)),
        _ => String::new(),
    };
    // `*` flags a row whose window has vanished (can't jump to it) — kept short for narrow screens.
    let win = if row.agent_alive { "" } else { " *" };
    // badge(5) + one space + token suffix + window marker are fixed; the target takes the rest.
    let reserved = 5 + 1 + tok.chars().count() + win.chars().count();
    let avail = width.saturating_sub(reserved).max(4);
    let target = truncate_chars(&row.lane, avail);
    format!("\x1b[{color}m{tag}\x1b[0m \x1b[1m{target}\x1b[0m\x1b[2m{tok}{win}\x1b[0m")
}

/// `bn fleet --mobile` — the narrow-screen reflow (#633): a one-line-per-agent board that never
/// wraps at ~40 cols, so the fleet stays readable in a phone SSH client. Same needs-you-first
/// ranking as [`run_board`], rendered through [`mobile_row`]. Remote runs are noted as a count in
/// the header (their wide detail lines belong to the full board).
pub fn run_board_mobile(cfg: &Config, idle: i64, stuck: i64, budget: Option<i64>) {
    let snapshot = collect_snapshot(cfg, idle, stuck, budget);
    if snapshot.agents.is_empty() && snapshot.remote_runs.is_empty() {
        println!("No live agents.");
        return;
    }
    let needs_you = snapshot
        .agents
        .iter()
        .filter(|row| board_state_rank(&row.status) == 0)
        .count();
    let remote_hdr = if snapshot.remote_runs.is_empty() {
        String::new()
    } else {
        format!(" · {} remote", snapshot.remote_runs.len())
    };
    println!(
        "\x1b[1mFleet\x1b[0m {}wt · {needs_you} need you{remote_hdr}",
        snapshot.agents.len()
    );
    for row in &snapshot.agents {
        println!("{}", mobile_row(row, MOBILE_WIDTH));
        if row.over_budget {
            println!("\x1b[1;31m  ! over budget\x1b[0m");
        }
    }
    println!("\x1b[2mjump: bn fleet jump\x1b[0m");
}

/// `bn fleet --json` — the raw worktree-centric snapshot contract, retaining the established
/// `agents`/`count` keys for existing consumers.
pub fn emit_board_json(cfg: &Config, idle: i64, stuck: i64, budget: Option<i64>) {
    let snapshot = collect_snapshot(cfg, idle, stuck, budget);
    println!(
        "{}",
        Envelope::success("fleet", serde_json::to_value(snapshot).unwrap_or_default()).to_json()
    );
}

/// `bn agent live [--json]` — collect and render the joined fleet.
pub fn run(cfg: &Config, idle: i64, stuck: i64, as_json: bool) {
    let rows = collect(cfg, idle, stuck);

    if as_json {
        let total_tokens: i64 = rows.iter().filter_map(|r| r.tokens).sum();
        let remote = crate::cmd::remote_track::fleet_views(cfg, &bn_core::runner::RealRunner);
        let data = serde_json::json!({
            "agents": rows,
            "count": rows.len(),
            "total_tokens": total_tokens,
            "remote_runs": remote,
        });
        println!("{}", Envelope::success("agent_live", data).to_json());
        return;
    }
    let remote = crate::cmd::remote_track::fleet_views(cfg, &bn_core::runner::RealRunner);
    if rows.is_empty() && remote.is_empty() {
        println!("No live agents.");
        return;
    }
    if !rows.is_empty() {
        println!("{} live agent(s):", rows.len());
        for r in &rows {
            // Plane A glyph (needs-you), falling back to Plane B health when the window is undecorated.
            let mark = lifecycle(r).board_style().1;
            let harness = r.harness.as_deref().unwrap_or("?");
            let win = if r.has_window { "" } else { "  (no window)" };
            let tok = match r.tokens {
                Some(t) if t > 0 => format!("  {} tok", fmt_tokens(t)),
                _ => String::new(),
            };
            println!("  {mark}  {:<24} {:<7}{tok}{win}", r.target, harness);
        }
    }
    if !remote.is_empty() {
        println!("{} remote run(s) (gh):", remote.len());
        for v in &remote {
            println!("{}", crate::cmd::remote_track::render_line(v));
        }
    }
}

mod picker;
mod steer_menu;
pub(crate) use picker::*;
pub(crate) use steer_menu::{run_steer_menu, run_steer_target};
