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
        SessionStatus::Gone => "queued",
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
                || {
                    let branch = worktree
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(&session.branch);
                    key(&session.repo, branch)
                },
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
        "queued" => 2,
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
        "queued" | "starting" => SnapshotGroup::Queued,
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
    render_snapshot_section(&snapshot, SnapshotGroup::Queued, "Queued", preview);
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
        "starting" | "queued" => ("2", "queue"),
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

// ── `bn fleet pick` — the interactive jump companion to the read-only `bn fleet` board ──────────
//
// Reads tmux as the source of truth (no separate state store to drift): every live agent window
// the fleet knows about, plus the orchestrator cockpit (the fleet/cockpit session, #345). fzf shows the
// whole fleet with a live per-row pane preview; selecting a row jumps focus to that window
// (`switch-client` inside tmux, `attach` from outside). The row-building core is a pure function
// over the fleet + the tmux window list, so it's unit-testable without a tmux server.

/// How many trailing pane lines the `--preview` panel shows for the highlighted agent.
const PICK_PREVIEW_LINES: usize = 60;

/// One tmux window as the picker sees it: the server-global `window_id` (the stable jump/preview
/// handle), its raw-case `session`/`window` names, and `@bn_agent_since` (0 when undecorated).
#[derive(Debug, Clone, PartialEq)]
struct TmuxWin {
    window_id: String,
    session: String,
    window: String,
    since: i64,
}

/// One picker row: the colored line fzf shows (field 1) plus the hidden `window_id`/`session`
/// columns its `--preview` and the post-close jump reference.
#[derive(Debug, Clone, PartialEq)]
struct PickRow {
    display: String,
    window_id: String,
    session: String,
}

impl PickRow {
    /// `display \t window_id \t session` — the exact line fed to fzf; `{2}` is the preview/jump
    /// window id and `{3}` the session to switch/attach to.
    fn line(&self) -> String {
        format!("{}\t{}\t{}", self.display, self.window_id, self.session)
    }
}

/// A compact time-in-state badge: `Ns` / `Nm` / `Nh`. Empty when `since` is unset (`<= 0`); a
/// clock skew into the future clamps to `0s`. Mirrors `bn jump`'s `dur()`.
fn age_label(since: i64, now: i64) -> String {
    if since <= 0 {
        return String::new();
    }
    let d = (now - since).max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else {
        format!("{}h", d / 3600)
    }
}

/// Read Plane A from tmux for the picker: one [`TmuxWin`] per window across every session, carrying
/// the `window_id` (so jump/preview are case- and rename-proof, unlike the lowercased `repo:branch`
/// key) alongside the raw names and `@bn_agent_since`.
fn read_pick_windows() -> Vec<TmuxWin> {
    let fmt = "#{window_id}\t#{session_name}\t#{window_name}\t#{@bn_agent_since}";
    let Some(out) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 3 || f[0].is_empty() {
                return None;
            }
            Some(TmuxWin {
                window_id: f[0].to_string(),
                session: f[1].to_string(),
                window: f[2].to_string(),
                since: f.get(3).and_then(|s| s.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// A fleet agent row: `<state glyph>  repo:branch  harness  age  tokens`. `w` supplies the live
/// window handle (for jump/preview) and its time-in-state; `v` supplies the ranked state, harness
/// and token burn — the same signals the board renders.
fn agent_pick_row(v: &LiveView, w: &TmuxWin, now: i64) -> PickRow {
    let (color, mark) = board_style(v);
    let harness = v.harness.as_deref().unwrap_or("?");
    let age = age_label(w.since, now);
    let age_col = if age.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m{age}\x1b[0m")
    };
    let tok = match v.tokens {
        Some(t) if t > 0 => format!("  \x1b[2m{} tok\x1b[0m", fmt_tokens(t)),
        _ => String::new(),
    };
    let display = format!(
        "\x1b[{color}m{mark}\x1b[0m  \x1b[1m{}\x1b[0m  \x1b[2m{harness}\x1b[0m{age_col}{tok}",
        v.target
    );
    PickRow {
        display,
        window_id: w.window_id.clone(),
        session: w.session.clone(),
    }
}

/// An orchestrator cockpit row (a cockpit-session window, #345): a distinct gear glyph so it
/// reads apart from the agents it drives, `session:window`, and its age.
fn orchestrator_pick_row(w: &TmuxWin, now: i64) -> PickRow {
    let age = age_label(w.since, now);
    let age_col = if age.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m{age}\x1b[0m")
    };
    let display = format!(
        "\x1b[1;36m\u{f085} orchestrator\x1b[0m  \x1b[1m{}:{}\x1b[0m{age_col}",
        w.session, w.window
    );
    PickRow {
        display,
        window_id: w.window_id.clone(),
        session: w.session.clone(),
    }
}

/// Build the ordered picker rows from the joined fleet + the live tmux window list. Pure over its
/// inputs → unit-testable without a tmux server.
///
/// 1. Every fleet agent that currently has a live window, ranked by who needs you (the board's
///    order). A fleet row whose window has since vanished (`has_window` was set but no live match)
///    is dropped — you can't jump to a gone window.
/// 2. Every orchestrator cockpit window (in the fleet/cockpit session) not already shown as an agent.
fn build_pick_rows(views: &[LiveView], wins: &[TmuxWin], now: i64, cockpit: &str) -> Vec<PickRow> {
    build_pick_rows_scoped(views, wins, now, false, cockpit)
}

/// A dispatched agent's worktree branch — `bn dispatch` / `bn orchestrate` name it `dispatch/<slug>`
/// (see `cmd::dispatch`). The `bn fleet pick --dispatched` scope keys off this prefix.
fn is_dispatch_branch(branch: &str) -> bool {
    branch.starts_with("dispatch/")
}

/// [`build_pick_rows`] with a `dispatched` scope: when set, keep only dispatched agent windows
/// (`dispatch/*` branches) and drop the orchestrator cockpit rows — the `bn fleet pick --dispatched`
/// view. `cockpit` is the resolved cockpit session name ([`wt::orchestrator_session`]). Pure over
/// its inputs → unit-testable without a tmux server.
fn build_pick_rows_scoped(
    views: &[LiveView],
    wins: &[TmuxWin],
    now: i64,
    dispatched: bool,
    cockpit: &str,
) -> Vec<PickRow> {
    let winmap: BTreeMap<String, &TmuxWin> = wins
        .iter()
        .map(|w| (key(&w.session, &w.window), w))
        .collect();

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut rows = Vec::new();

    // 1) Fleet agents with a live window, ranked (needs-you first), matched on the lowercased
    //    `repo:branch` key both the fleet target and the window key share. The `--dispatched` scope
    //    narrows this to `dispatch/*` branches.
    let mut agents: Vec<(&LiveView, &TmuxWin)> = views
        .iter()
        .filter(|v| !dispatched || is_dispatch_branch(&v.branch))
        .filter_map(|v| winmap.get(v.target.as_str()).map(|w| (v, *w)))
        .collect();
    agents.sort_by_key(|(v, _)| board_rank(v));
    for (v, w) in agents {
        rows.push(agent_pick_row(v, w, now));
        seen.insert(w.window_id.as_str());
    }

    // 2) Orchestrator cockpit windows not already surfaced as an agent row — omitted under the
    //    `--dispatched` scope, which is agents-only.
    if !dispatched {
        for w in wins.iter().filter(|w| {
            w.session.eq_ignore_ascii_case(cockpit) && !seen.contains(w.window_id.as_str())
        }) {
            rows.push(orchestrator_pick_row(w, now));
        }
    }

    rows
}

/// `bn fleet pick` — the interactive fzf picker over the whole live fleet + the orchestrator
/// cockpit. Each row previews a live `tmux capture-pane` of its window; selecting one jumps focus
/// there. Degrades to the read-only board when fzf is absent, and no-ops cleanly on an empty fleet.
pub fn run_pick(cfg: &Config, idle: i64, stuck: i64, dispatched: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows_scoped(
        &views,
        &read_pick_windows(),
        now,
        dispatched,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!(
            "{}",
            if dispatched {
                "No live dispatched agents to jump to."
            } else {
                "No live agents or orchestrator windows to jump to."
            }
        );
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let scope = if dispatched { " · dispatched" } else { "" };
    let header = format!(
        "fleet{scope} · {} window(s) · ↵ jump · live preview · ctrl-c cancel",
        rows.len()
    );

    // {2}=window_id (the preview/jump handle). Enter accepts the row (default bind → prints it &
    // closes); we jump below, once fzf has torn down, so `switch-client`/`attach` land in the real
    // client instead of the fzf popup.
    let child = Command::new("fzf")
        .arg("--ansi")
        .arg("--reverse")
        .arg("--with-nth=1")
        .arg("--delimiter=\t")
        .arg(format!(
            "--preview={bn} fleet pick --pick-stage preview {{2}}"
        ))
        .arg("--preview-window=right:60%:wrap")
        .arg(format!("--header={header}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the read-only board (the same rows, non-interactive).
            run_board(cfg, idle, stuck, None, false);
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // The fleet is small (one line per live window), so a synchronous write then read of the
        // selection can't deadlock: fzf drains stdin at load and only writes stdout on close.
        let body = rows
            .iter()
            .map(PickRow::line)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }

    let Ok(out) = child.wait_with_output() else {
        return;
    };
    let sel = String::from_utf8_lossy(&out.stdout);
    let cols: Vec<&str> = sel.trim_end_matches('\n').split('\t').collect();
    let window_id = cols.get(1).copied().unwrap_or("");
    let session = cols.get(2).copied().unwrap_or("");
    if window_id.is_empty() {
        return; // cancelled (Esc / ctrl-c) — nothing selected.
    }
    pick_jump(session, window_id);
}

/// The `--pick-stage preview` callback: a live tail of the focused window's active pane, captured
/// fresh on every fzf selection change (so the preview is genuinely live, not a snapshot).
pub fn pick_stage(stage: FleetPickStage, window_id: Option<&str>) {
    match stage {
        FleetPickStage::Preview => {
            let Some(wid) = window_id.filter(|w| !w.is_empty()) else {
                return;
            };
            if let Some(cap) = crate::proc::tmux_raw(&["capture-pane", "-p", "-t", wid]) {
                let lines: Vec<&str> = cap.lines().collect();
                let start = lines.len().saturating_sub(PICK_PREVIEW_LINES);
                for l in &lines[start..] {
                    println!("{l}");
                }
            }
        }
    }
}

/// Jump focus to a window: inside tmux, switch the client to its session and select it; outside
/// tmux, make it the session's current window and attach the terminal. `window_id` is server-global
/// so both paths land exactly on the chosen window regardless of case or later renames.
fn pick_jump(session: &str, window_id: &str) {
    use crate::proc::tmux_run;
    if std::env::var_os("TMUX").is_some() {
        if !session.is_empty() {
            let _ = tmux_run(&["switch-client", "-t", session]);
        }
        let _ = tmux_run(&["select-window", "-t", window_id]);
    } else {
        // No client yet: set the target as its session's current window, then attach to it.
        let _ = tmux_run(&["select-window", "-t", window_id]);
        if !session.is_empty() {
            let _ = tmux_run(&["attach-session", "-t", session]);
        }
    }
}

/// The accelerator-key pool for the single-key jumper (#633), in press order: `1-9` (numeric is
/// fastest one-handed on a phone), then `a-z`. `q` is omitted — tmux's `display-menu` reserves it
/// (with Esc) to cancel, so binding it would be shadowed.
fn menu_accelerators() -> impl Iterator<Item = char> {
    "123456789abcdefghijklmnoprstuvwxyz".chars()
}

/// One `display-menu` entry: the accelerator `key`, the plain `name` shown, and the `session` +
/// `window_id` its select command jumps to.
#[derive(Debug, Clone, PartialEq)]
struct MenuEntry {
    key: char,
    name: String,
    session: String,
    window_id: String,
}

/// Strip ANSI SGR escape sequences (`\x1b[…m`) from `s` — the picker rows carry color codes for
/// fzf/`--ansi`, but `tmux display-menu` names are plain text (raw escapes render as garbage).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break; // SGR terminator (e.g. 'm')
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Build the ordered `display-menu` entries from the picker rows: assign each row the next
/// accelerator key (see [`menu_accelerators`]) and a plain name, preserving the needs-you-first
/// order. Rows beyond the accelerator pool are dropped (a fleet that large wants `bn fleet pick`).
/// Pure over its inputs → unit-testable without a tmux server.
fn build_menu_entries(rows: &[PickRow]) -> Vec<MenuEntry> {
    rows.iter()
        .zip(menu_accelerators())
        .map(|(r, key)| MenuEntry {
            key,
            name: strip_ansi(&r.display),
            session: r.session.clone(),
            window_id: r.window_id.clone(),
        })
        .collect()
}

/// The `tmux display-menu` argv for these entries: `-T <title>`, centered, then a `name key
/// command` triple per entry. Selecting an entry runs `switch-client -t <session> ; select-window
/// -t <window_id>` so focus lands on the chosen window. When `search_cmd` is set (#633 P2), a
/// trailing `/` entry is appended that runs it — the search-overlay hook that drops into the fzf
/// `bn fleet pick` inside a `display-popup`. Pure over its inputs → unit-testable.
fn menu_args(entries: &[MenuEntry], title: &str, search_cmd: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        title.into(),
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for e in entries {
        let cmd = if e.session.is_empty() {
            format!("select-window -t {}", e.window_id)
        } else {
            format!(
                "switch-client -t {} ; select-window -t {}",
                e.session, e.window_id
            )
        };
        args.push(e.name.clone());
        args.push(e.key.to_string());
        args.push(cmd);
    }
    if let Some(cmd) = search_cmd {
        args.push("search /".into());
        args.push("/".into());
        args.push(cmd.into());
    }
    args
}

/// The `display-menu` command a `/` entry runs (#633 P2): open the fzf `bn fleet pick` inside a
/// centered `display-popup` so you can type a few chars (repo / branch / issue#) to filter the
/// fleet on a phone. `-E` closes the popup when the picker exits (after it has jumped). `bn` is the
/// already-shell-quoted path to this executable. Pure over its input → unit-testable.
fn search_overlay_cmd(bn: &str) -> String {
    format!("display-popup -w 90% -h 80% -E {bn} fleet pick")
}

/// How a `bn fleet jump <query>` resolves against the live picker rows.
#[derive(Debug, Clone, PartialEq)]
enum JumpResolution {
    /// No row matched the query — nothing to jump to.
    None,
    /// Exactly one row matched → jump straight to its window.
    One { session: String, window_id: String },
    /// Several rows matched → too ambiguous to auto-jump; carries their plain names to show.
    Many(Vec<String>),
}

/// Case-insensitively filter picker rows by a free-text `query` matched as a substring of each
/// row's plain (ANSI-stripped) display text — repo, branch, issue#, harness and state are all
/// searchable, so a numeric issue# is the fastest one-handed filter. An empty query returns every
/// row. Pure over its inputs → unit-testable without a tmux server.
fn filter_pick_rows<'a>(rows: &'a [PickRow], query: &str) -> Vec<&'a PickRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|r| strip_ansi(&r.display).to_ascii_lowercase().contains(&q))
        .collect()
}

/// Resolve a `bn fleet jump <query>` to a single window: filter the rows, then require a unique
/// match. Zero matches → [`JumpResolution::None`]; exactly one → [`JumpResolution::One`] with its
/// session + window id; more than one → [`JumpResolution::Many`] carrying the candidate names so
/// the caller can list them instead of guessing. Pure over its inputs → unit-testable.
fn resolve_jump(rows: &[PickRow], query: &str) -> JumpResolution {
    let matches = filter_pick_rows(rows, query);
    match matches.as_slice() {
        [] => JumpResolution::None,
        [r] => JumpResolution::One {
            session: r.session.clone(),
            window_id: r.window_id.clone(),
        },
        many => JumpResolution::Many(many.iter().map(|r| strip_ansi(&r.display)).collect()),
    }
}

/// `bn fleet jump` — the single-key `display-menu` jumper (#633): a native tmux popup, one entry
/// per live agent window plus the orchestrator cockpit, each with an accelerator key + state badge,
/// and a trailing `/` search entry that drops into the fzf `bn fleet pick` in a `display-popup`
/// (#633 P2). Press the key → focus jumps to that window. Zero typing, big tap targets — the
/// mobile/one-handed companion to the fzf-based `bn fleet pick`. Falls back to the read board when
/// there's no tmux client to raise the menu in.
pub fn run_menu_jump(cfg: &Config, idle: i64, stuck: i64) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to jump to.");
        return;
    }
    // display-menu needs a client to raise the popup in; outside tmux, degrade to the board.
    if std::env::var_os("TMUX").is_none() {
        println!("\x1b[2m(no tmux client — attach first; showing the board)\x1b[0m");
        run_board(cfg, idle, stuck, None, false);
        return;
    }
    let entries = build_menu_entries(&rows);
    let title = format!("fleet · {} · press a key or / to search", entries.len());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let search = search_overlay_cmd(&bn);
    crate::proc::tmux_run(&menu_args(&entries, &title, Some(&search)));
}

/// `bn fleet jump <issue|repo>` — direct numeric/name jump (#633 P2): resolve `query` against the
/// live fleet + orchestrator rows and jump straight to the one window it names (no menu, no fzf).
/// A numeric issue# or a repo/branch fragment is the fastest one-handed jump on a phone. No match
/// or an ambiguous one prints the candidates instead of guessing. Reuses the same rows and
/// `window_id` mapping as the menu jumper, so behavior stays consistent.
pub fn run_direct_jump(cfg: &Config, idle: i64, stuck: i64, query: &str) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to jump to.");
        return;
    }
    match resolve_jump(&rows, query) {
        JumpResolution::One { session, window_id } => pick_jump(&session, &window_id),
        JumpResolution::None => {
            println!(
                "No fleet window matches '{query}'. Try `bn fleet jump` (menu) or `bn fleet pick`."
            );
        }
        JumpResolution::Many(names) => {
            println!("'{query}' matches {} windows — narrow it:", names.len());
            for n in &names {
                println!("  {n}");
            }
        }
    }
}

/// A quick-steer action (#633 P3): steer an agent from a fleet row *without* navigating into its
/// pane. `Approve1`/`Approve2` answer the common numeric prompt agents block on (command-approval,
/// folder-trust, usage-limit); `Continue` nudges a paused agent onward; `SendText` prompts for a
/// free-text steer; `Interrupt` sends Ctrl-C; `Kill` terminates the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteerAction {
    Approve1,
    Approve2,
    Continue,
    SendText,
    Interrupt,
    Kill,
}

/// The steer actions in menu order — approve-first (the most common phone answer), then the rest.
const STEER_ACTIONS: [SteerAction; 6] = [
    SteerAction::Approve1,
    SteerAction::Approve2,
    SteerAction::Continue,
    SteerAction::SendText,
    SteerAction::Interrupt,
    SteerAction::Kill,
];

impl SteerAction {
    /// The single-key accelerator for this action's `display-menu` entry — numeric approvals first
    /// (fastest one-handed), then mnemonic letters. `q` is avoided (tmux reserves it to cancel).
    fn key(self) -> char {
        match self {
            SteerAction::Approve1 => '1',
            SteerAction::Approve2 => '2',
            SteerAction::Continue => 'c',
            SteerAction::SendText => 's',
            SteerAction::Interrupt => 'i',
            SteerAction::Kill => 'k',
        }
    }

    /// The plain menu label shown for this action.
    fn label(self) -> &'static str {
        match self {
            SteerAction::Approve1 => "Approve (1)",
            SteerAction::Approve2 => "Approve (2)",
            SteerAction::Continue => "Continue",
            SteerAction::SendText => "Send text…",
            SteerAction::Interrupt => "Interrupt (C-c)",
            SteerAction::Kill => "Kill window",
        }
    }
}

/// Resolve a picker row's `window_id` to the exact tmux target the steer commands act on. The
/// server-global window id (`@N`) is rename- and renumber-proof, so keys never land in a neighbor's
/// pane; [`tmux_exact_target`](crate::tmux_exact_target) passes ids through untouched and `=`-guards
/// a bare name. Pure over its input → unit-testable.
fn steer_target(window_id: &str) -> String {
    crate::tmux_exact_target(window_id)
}

/// Assemble the `display-menu` entry command that performs `action` against pane `target` (#633
/// P3). The three text steers reuse the reliable `bn agent send … --keys` channel (paste + raw-CR
/// submit + confirm) via a backgrounded `run-shell`, so the menu closes immediately; `SendText`
/// first prompts with `command-prompt` (`%%` is the typed text). `Interrupt` is a raw `send-keys
/// C-c`; `Kill` is a `confirm-before`-guarded `kill-window`. `bn` is the shell-quoted path to this
/// executable. Pure over its inputs → unit-testable without a tmux server.
fn steer_action_command(action: SteerAction, target: &str, bn: &str) -> String {
    match action {
        SteerAction::Approve1 => format!("run-shell -b \"{bn} agent send {target} 1 --keys\""),
        SteerAction::Approve2 => format!("run-shell -b \"{bn} agent send {target} 2 --keys\""),
        SteerAction::Continue => {
            format!("run-shell -b \"{bn} agent send {target} continue --keys\"")
        }
        SteerAction::SendText => format!(
            "command-prompt -p \"steer {target}:\" \"run-shell -b \\\"{bn} agent send {target} '%%' --keys\\\"\""
        ),
        SteerAction::Interrupt => format!("send-keys -t {target} C-c"),
        SteerAction::Kill => {
            format!("confirm-before -p \"kill {target}? (y/n)\" \"kill-window -t {target}\"")
        }
    }
}

/// Build the `tmux display-menu` argv for the quick-steer actions against `target` (#633 P3): a
/// titled, centered menu, one entry per [`SteerAction`] with its single-key accelerator and the
/// command from [`steer_action_command`]. `name` is the row's plain label (for the title). Pure
/// over its inputs → unit-testable.
fn steer_menu_args(target: &str, name: &str, bn: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        format!("steer · {name}"),
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for a in STEER_ACTIONS {
        args.push(a.label().into());
        args.push(a.key().to_string());
        args.push(steer_action_command(a, target, bn));
    }
    args
}

/// Build the `display-menu` argv for the fleet-row picker whose entries open the quick-steer submenu
/// (#633 P3) instead of jumping into the pane. Reuses [`build_menu_entries`] for the ordered rows +
/// accelerators (needs-you first), then maps each to a backgrounded `run-shell` that re-invokes `bn
/// fleet steer <window_id>` — which raises the action submenu for that row. `bn` is the shell-quoted
/// path to this executable. Pure over its inputs → unit-testable.
fn steer_row_menu_args(entries: &[MenuEntry], title: &str, bn: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        title.into(),
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for e in entries {
        let cmd = format!("run-shell -b \"{bn} fleet steer {}\"", e.window_id);
        args.push(e.name.clone());
        args.push(e.key.to_string());
        args.push(cmd);
    }
    args
}

/// `bn fleet steer` — the quick-steer row picker (#633 P3): a native `display-menu` of the live
/// fleet (one entry per agent window + orchestrator cockpit, needs-you first), each opening a
/// single-key action submenu (approve/continue/send/interrupt/kill) that steers the agent *without*
/// navigating into its pane. The mobile/one-handed counterpart to hand-navigating and typing. Falls
/// back to the read board when there's no tmux client to raise the menu in.
pub fn run_steer_menu(cfg: &Config, idle: i64, stuck: i64) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to steer.");
        return;
    }
    if std::env::var_os("TMUX").is_none() {
        println!("\x1b[2m(no tmux client — attach first; showing the board)\x1b[0m");
        run_board(cfg, idle, stuck, None, false);
        return;
    }
    let entries = build_menu_entries(&rows);
    let title = format!("steer · {} · pick an agent", entries.len());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    crate::proc::tmux_run(&steer_row_menu_args(&entries, &title, &bn));
}

/// `bn fleet steer <target>` — raise the quick-steer action submenu for one window (#633 P3). The
/// row picker's entries call this per row; passing a `window_id` (or `repo:branch`) jumps straight
/// to the action menu (approve/continue/send/interrupt/kill) for that pane. Reuses the same exact
/// target resolution as the steer commands. Falls back to a note when there's no tmux client.
pub fn run_steer_target(target: &str) {
    if std::env::var_os("TMUX").is_none() {
        println!("bn fleet steer needs a tmux client — attach first.");
        return;
    }
    let resolved = steer_target(target);
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    crate::proc::tmux_run(&steer_menu_args(&resolved, target, &bn));
}
