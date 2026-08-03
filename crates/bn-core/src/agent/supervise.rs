use super::parse::{events_json, is_quiet, note_key, read_yaml, ts_of, ws_str};
use crate::cost::{canonical_session_id, LaunchRecord, WorkClass};
use crate::resolver::Config;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Default seconds without pane or transcript activity before a lane still reporting `working`
/// is considered stuck.
pub const DEFAULT_STUCK_LANE_IDLE_SECS: i64 = 600;

/// Why an in-flight worktree lane is stuck. Kept separate from the broader agent health and
/// orchestrator recovery states: this is the controller-actionable subset that should wake an
/// owning controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StuckLaneKind {
    InteractivePrompt,
    McpLoading,
    WorkingIdle,
    IdleWithWip,
}

impl StuckLaneKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::InteractivePrompt => "interactive-prompt",
            Self::McpLoading => "mcp-loading",
            Self::WorkingIdle => "working-idle",
            Self::IdleWithWip => "idle-with-wip",
        }
    }
}

/// A controller-actionable stuck state, persisted on an orchestrate queue item so status and event
/// emission share one source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StuckLane {
    pub kind: StuckLaneKind,
    pub reason: String,
    pub suggested_unblock: String,
}

impl StuckLane {
    pub fn key(&self) -> String {
        match self.kind {
            StuckLaneKind::InteractivePrompt => {
                format!("{}:{}", self.kind.label(), self.reason)
            }
            _ => self.kind.label().to_string(),
        }
    }

    pub fn summary(&self, lane: &str) -> String {
        format!(
            "{lane} is blocked: {}; suggested unblock: {}",
            self.reason, self.suggested_unblock
        )
    }
}

/// Live signals used by [`classify_stuck_lane`]. The CLI gathers these from the existing
/// orchestrate tmux/worktree observation and [`agent_progress_for_cwd`] probe.
#[derive(Debug, Clone, Copy)]
pub struct StuckLaneObservation<'a> {
    pub tail: &'a [String],
    pub has_window: bool,
    pub has_process: bool,
    pub plane_a: &'a str,
    pub has_wip: bool,
    /// Seconds since either the pane fingerprint or harness transcript last changed.
    pub unchanged_secs: i64,
    pub idle_threshold_secs: i64,
}

struct StuckPanePattern {
    marker: &'static str,
    reason: &'static str,
    suggested_unblock: &'static str,
}

/// Harness startup text that means MCP discovery itself is wedged. The table form keeps the set
/// extensible without spreading string checks across the supervisor.
const MCP_LOADING_PATTERNS: &[StuckPanePattern] = &[
    StuckPanePattern {
        marker: "loading… mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading... mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading… servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
];

const INTERACTIVE_BLOCK_PATTERNS: &[StuckPanePattern] = &[
    StuckPanePattern {
        marker: "yes, and add these directories to the allowed list",
        reason: "the harness is waiting for directory trust permission",
        suggested_unblock: "approve the allowed-directory option and resume the lane",
    },
    StuckPanePattern {
        marker: "yes, don't ask again",
        reason: "the harness is waiting for persistent permission",
        suggested_unblock: "approve the trusted permission option and resume the lane",
    },
    StuckPanePattern {
        marker: "yes, don’t ask again",
        reason: "the harness is waiting for persistent permission",
        suggested_unblock: "approve the trusted permission option and resume the lane",
    },
];

fn stuck_pane_pattern<'a>(
    tail: &[String],
    patterns: &'a [StuckPanePattern],
) -> Option<&'a StuckPanePattern> {
    patterns.iter().find(|pattern| {
        tail.iter()
            .any(|line| line.to_ascii_lowercase().contains(pattern.marker))
    })
}

/// Stable fingerprint for the pane/activity clock persisted by the orchestrator. Plane-A is part
/// of the key so a lifecycle transition counts as activity even when the visible tail is unchanged.
pub fn pane_activity_fingerprint(tail: &[String], plane_a: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in plane_a
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(tail.iter().flat_map(|line| {
            line.as_bytes()
                .iter()
                .copied()
                .chain(std::iter::once(b'\n'))
        }))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Classify the controller-actionable stuck subset of an in-flight lane. Pane semantics are
/// delegated to the existing orchestrate classifier; this layer only adds the MCP startup pattern
/// and the durable no-change threshold.
pub fn classify_stuck_lane(obs: &StuckLaneObservation<'_>) -> Option<StuckLane> {
    use crate::orchestrate::{classify, PaneObservation, PaneState};

    let pane = PaneObservation {
        tail: obs.tail,
        has_window: obs.has_window,
        plane_a: obs.plane_a,
        has_wip: obs.has_wip,
    };
    let state = classify(&pane);

    if obs.has_process && obs.unchanged_secs > 0 {
        if let Some(pattern) = stuck_pane_pattern(obs.tail, MCP_LOADING_PATTERNS) {
            return Some(StuckLane {
                kind: StuckLaneKind::McpLoading,
                reason: pattern.reason.to_string(),
                suggested_unblock: pattern.suggested_unblock.to_string(),
            });
        }
        if let Some(pattern) = stuck_pane_pattern(obs.tail, INTERACTIVE_BLOCK_PATTERNS) {
            return Some(StuckLane {
                kind: StuckLaneKind::InteractivePrompt,
                reason: pattern.reason.to_string(),
                suggested_unblock: pattern.suggested_unblock.to_string(),
            });
        }
        let prompt = match state {
            PaneState::TrustMenu => Some((
                "the harness is waiting on a trust menu",
                "approve the trusted worktree or restart the lane with pre-trust enabled",
            )),
            PaneState::CommandApproval => Some((
                "the harness is waiting on command permission",
                "choose the session-scoped allow option and resume the lane",
            )),
            PaneState::QuestionMenu => Some((
                "the harness is waiting on an interactive choice",
                "answer the prompt in the lane and let the agent continue",
            )),
            _ => None,
        };
        if let Some((reason, suggested_unblock)) = prompt {
            return Some(StuckLane {
                kind: StuckLaneKind::InteractivePrompt,
                reason: reason.to_string(),
                suggested_unblock: suggested_unblock.to_string(),
            });
        }
    }

    if state == PaneState::Working && obs.unchanged_secs >= obs.idle_threshold_secs.max(1) {
        return Some(StuckLane {
            kind: StuckLaneKind::WorkingIdle,
            reason: "the lane still reports working beyond the configured idle threshold with no pane or agent activity".to_string(),
            suggested_unblock: "inspect the pane and nudge or restart the stalled agent".to_string(),
        });
    }

    (state == PaneState::IdleWithWip).then(|| StuckLane {
        kind: StuckLaneKind::IdleWithWip,
        reason: "the agent turn is idle while the worktree has uncommitted or unmerged work"
            .to_string(),
        suggested_unblock: "resume or recover the agent so it can finish and commit the work"
            .to_string(),
    })
}

// --- supervise: live-agent fleet view -------------------------------------------------------
//
// The mirror image of [`ingest`]: instead of folding *finished* sessions into metrics, this
// surfaces the *running* ones — the live-supervision layer the 24/7 orchestrator pattern needs.
// A Claude transcript still being written (file touched within [`QUIET_SECS`]) is a running
// session; its last event timestamp is the agent's last sign of life, from which health is
// derived (active / idle / stuck). Read-only and offline, like the rest of agent.rs: it reads
// `~/.claude/projects` and writes nothing.

/// How an agent is faring, from its idle time against the supervisor thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentHealth {
    /// Just dispatched — a launch record exists but no transcript has landed yet (#545). Not a
    /// health the idle thresholds ever produce; the fleet stamps it on a synthesized pending entry
    /// so a starting agent is visible before its first sign of life.
    Starting,
    /// Tool activity within the idle window — making progress.
    Active,
    /// Quiet past the idle window but not yet stuck — may be thinking or awaiting input.
    Idle,
    /// Idle longer than the stuck threshold — flagged for a nudge.
    Stuck,
}

impl AgentHealth {
    /// Classify from idle seconds against the idle/stuck thresholds (both in minutes).
    fn from_idle(idle_secs: i64, idle_mins: i64, stuck_mins: i64) -> AgentHealth {
        if idle_secs >= stuck_mins * 60 {
            AgentHealth::Stuck
        } else if idle_secs >= idle_mins * 60 {
            AgentHealth::Idle
        } else {
            AgentHealth::Active
        }
    }

    /// The lowercase label used in the event/CLI/JSON surfaces.
    pub fn label(self) -> &'static str {
        match self {
            AgentHealth::Starting => "starting",
            AgentHealth::Active => "active",
            AgentHealth::Idle => "idle",
            AgentHealth::Stuck => "stuck",
        }
    }
}

/// An agent's fleet role (#500): a normal **worker** that ships a PR, or an **orchestrator** — the
/// long-lived supervisory agent that dispatches + monitors the wave itself (the fleet-drain monitor)
/// and produces no PR. Data-driven: derived from the launch-path work-class attribution rather than
/// guessed in the UI, so the dashboard can pin orchestrators as their own top tier. Defaults to
/// `Worker`, so any un-attributed agent — and any older fleet JSON without the field — reads as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    #[default]
    Worker,
    Orchestrator,
}

impl AgentRole {
    /// Classify a live agent's role from its launch-path [`WorkClass`]: an `Orchestration` session
    /// runs the fleet, so it's an orchestrator; every other class is a worker. Pure — the fleet
    /// stamps this from the machine's attribution log ([`stamp_roles`]).
    pub fn from_work_class(wc: WorkClass) -> AgentRole {
        match wc {
            WorkClass::Orchestration => AgentRole::Orchestrator,
            _ => AgentRole::Worker,
        }
    }

    /// The lowercase label used in the CLI/JSON surfaces.
    pub fn label(self) -> &'static str {
        match self {
            AgentRole::Worker => "worker",
            AgentRole::Orchestrator => "orchestrator",
        }
    }
}

/// One live agent session under supervision, attributed to a bn note key. Serializes straight
/// into the fleet JSON; `target` is the `repo:branch` tmux window a nudge would be sent to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveAgent {
    pub session_id: String,
    pub harness: String,
    pub repo: String,
    pub branch: String,
    /// `repo:branch` — the public identity every bn surface addresses this agent by
    /// (`bn agent send`), which is *not* necessarily what the hosting tmux window is called.
    pub target: String,
    /// The exact `session:window` hosting this agent, resolved by [`crate::agent::resolve_channels`]
    /// — the target a send-keys or kill actually has to use, since a window may be named after a
    /// reused directory or a pooled slot rather than the branch. `None` when nothing was found.
    /// `#[serde(default)]` keeps older fleet JSON valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_target: Option<String>,
    /// The agent's real checkout — its transcript cwd, refined by the window/lease join. Beats
    /// reconstructing `<worktree_dir>/<repo>/<branch>`, which is wrong for every pooled slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Epoch secs of the last recorded tool/event activity.
    pub last_activity_ts: i64,
    /// Seconds since `last_activity_ts` at the supervisor pass.
    pub idle_secs: i64,
    pub health: AgentHealth,
    /// Worker (ships a PR) vs orchestrator (runs the wave, #500). `#[serde(default)]` keeps older
    /// fleet JSON without the field valid — it reads as [`AgentRole::Worker`].
    #[serde(default)]
    pub role: AgentRole,
    pub turns: i64,
    pub tool_calls: i64,
    /// Cumulative tokens burned so far in the *live* session. `output_tokens` is the headline
    /// usage signal — the only figure both harnesses expose mid-session, and the strongest
    /// cost/runaway proxy (output dominates credit cost; cache reads barely register). Claude
    /// transcripts also carry `input_tokens`/`cache_read_tokens`; a live Copilot log records only
    /// output (its running cost/AIU lands solely in the final `session.shutdown`), so those stay 0
    /// for Copilot until the session finishes and is folded by [`ingest`].
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    /// How — or whether — bn can drive this agent. Supervision *observes* every running session on
    /// the machine, including ones bn never launched, so being in the fleet does not imply being
    /// steerable. Surfacing that lets a client disable steer/relaunch/kill up front instead of
    /// failing after the click. `#[serde(default)]` keeps older fleet JSON valid — it reads as
    /// [`ControlChannel::None`].
    #[serde(default)]
    pub control: ControlChannel,
}

/// The channel `bn agent send` would reach an agent through, in the order it tries them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlChannel {
    /// A recorded `agent-session:` in the branch note — the reliable `--resume` path.
    Resume,
    /// A live tmux window named `repo:branch` — the send-keys path.
    Tmux,
    /// More than one live session shares this `repo:branch`, so neither channel can say *which* one
    /// a write would land on: the tmux window name and the note's `agent-session:` record are both
    /// keyed by branch, not session. Refusing is the only safe answer — a `kill` that might hit
    /// either of two sessions is worse than no kill button at all.
    Ambiguous,
    /// Neither. Observed only: bn can read this agent's transcript but cannot steer, relaunch or
    /// kill it. The common case is a session started outside bn (a plain `claude` in a terminal).
    #[default]
    None,
}

impl ControlChannel {
    /// Whether a write can be aimed at exactly one agent.
    pub fn is_steerable(self) -> bool {
        matches!(self, ControlChannel::Resume | ControlChannel::Tmux)
    }
}

/// Default minutes of quiet before an agent is *idle*.
pub const DEFAULT_IDLE_MINS: i64 = 5;
/// Default minutes of quiet before an agent is flagged *stuck*.
pub const DEFAULT_STUCK_MINS: i64 = 15;
/// How long after launch a just-dispatched agent with no transcript is shown as a pending
/// "starting…" entry (#545). Generous enough to cover a slow cold-worktree boot or an agent parked
/// on a trust/MCP-approval prompt, yet bounded so a launch whose harness never came up (e.g. the
/// tmux window was closed) ages out instead of lingering as a zombie card.
pub const DEFAULT_STARTING_MINS: i64 = 20;

/// The fleet a supervisor pass observed, newest-activity first, plus the counts the FLEET panel
/// and badge want. `stuck` is the actionable number — the agents needing a nudge.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Fleet {
    pub agents: Vec<LiveAgent>,
    /// Just-dispatched agents with no transcript yet (#545) — the pending "starting…" tier.
    pub starting: usize,
    pub active: usize,
    pub idle: usize,
    pub stuck: usize,
    /// Σ output tokens across every live agent — the fleet-wide usage total the board headlines.
    /// Output tokens are the cross-harness comparable signal (see [`LiveAgent`]).
    pub total_tokens: i64,
}

impl Fleet {
    fn from_agents(agents: Vec<LiveAgent>) -> Fleet {
        let count = |h: AgentHealth| agents.iter().filter(|a| a.health == h).count();
        Fleet {
            starting: count(AgentHealth::Starting),
            active: count(AgentHealth::Active),
            idle: count(AgentHealth::Idle),
            stuck: count(AgentHealth::Stuck),
            total_tokens: agents.iter().map(|a| a.output_tokens).sum(),
            agents,
        }
    }
}

/// Supervise the live agent fleet: every running Claude **and** Copilot session with its health.
/// Reads the real `~/.claude/projects` and `~/.copilot/session-state`; thresholds default to
/// [`DEFAULT_IDLE_MINS`]/[`DEFAULT_STUCK_MINS`].
pub fn supervise(cfg: &Config, idle_mins: i64, stuck_mins: i64) -> Fleet {
    let (claude_root, copilot_root) = supervision_roots();
    let now = now_epoch_secs();
    let mut fleet = supervise_from(cfg, &claude_root, &copilot_root, now, idle_mins, stuck_mins);
    // Stamp each agent's #500 role from the machine's launch-path work-class attribution — the same
    // bn-owned record `bn dispatch` writes and ingest reads. Kept out of the offline testable core
    // so [`supervise_from`] stays a pure filesystem read.
    stamp_roles(&mut fleet.agents, &crate::cost::attribution_map());
    // #545: fold in a pending "starting…" entry for every just-dispatched agent that has a launch
    // record but no transcript yet, so a freshly-dispatched (or startup-stalled) agent is visible
    // in the fleet for ANY client — not just the one that clicked. Deduped against the live and
    // already-finished sessions so a running or completed agent is never resurrected as pending.
    let live_ids: HashSet<String> = fleet
        .agents
        .iter()
        .map(|a| canonical_session_id(&a.session_id))
        .collect();
    let finished_ids = finished_session_ids(cfg);
    let pending = pending_agents(
        &crate::cost::launch_records(),
        &live_ids,
        &finished_ids,
        now,
        DEFAULT_STARTING_MINS,
    );
    if !pending.is_empty() {
        let mut agents = std::mem::take(&mut fleet.agents);
        agents.extend(pending);
        agents.sort_by_key(|a| std::cmp::Reverse(a.last_activity_ts));
        fleet = Fleet::from_agents(agents);
        stamp_roles(&mut fleet.agents, &crate::cost::attribution_map());
    }
    // Last, so it covers the pending entries folded in above too: can bn actually write to each of
    // these, or is it only watching? Kept out of `supervise_from` for the same reason as roles —
    // it shells out to tmux, and that core stays a pure filesystem read.
    stamp_control(cfg, &mut fleet.agents);
    fleet
}

pub fn supervision_roots() -> (PathBuf, PathBuf) {
    let home = dirs::home_dir().unwrap_or_default();
    (
        home.join(".claude").join("projects"),
        home.join(".copilot").join("session-state"),
    )
}

/// The harness session ids of already-finished (ingested) sessions, read from the machine-local
/// metrics log — the same `agent_session` events [`crate::agent::session::ingest`] appends. Used to
/// dedup pending "starting…" entries so a completed run whose launch record is still recent is not
/// resurrected as pending. Keys are [`canonical_session_id`]-normalized to match the launch side.
fn finished_session_ids(cfg: &Config) -> HashSet<String> {
    crate::events::read_events_at(&cfg.metrics_log_path())
        .into_iter()
        .filter(|e| e.kind == "agent_session")
        .filter_map(|e| {
            e.fields
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(canonical_session_id)
        })
        .collect()
}

/// Synthesize pending "starting…" [`LiveAgent`]s from launch records (#545): a launch within
/// `starting_mins` whose session id is neither live (already has a transcript in `live_ids`) nor
/// finished (already ingested in `finished_ids`) was dispatched but hasn't come up yet. Pure over
/// its inputs so it is unit-testable without touching the filesystem. The latest launch record per
/// session wins (a re-dispatch re-pins the same id), and entries carry the launch time as their
/// `last_activity_ts` so they sort to the top of the fleet as the newest work.
fn pending_agents(
    launches: &[LaunchRecord],
    live_ids: &HashSet<String>,
    finished_ids: &HashSet<String>,
    now: i64,
    starting_mins: i64,
) -> Vec<LiveAgent> {
    // `launches` is in chronological (file) order, so a later record for the same session
    // overwrites an earlier one — latest write wins.
    let mut latest: BTreeMap<String, &LaunchRecord> = BTreeMap::new();
    for l in launches {
        latest.insert(canonical_session_id(&l.session_id), l);
    }
    let window = starting_mins * 60;
    latest
        .into_iter()
        .filter_map(|(canon, l)| {
            if live_ids.contains(&canon) || finished_ids.contains(&canon) {
                return None;
            }
            let age = now - l.launched_at;
            if age < 0 || age >= window {
                return None;
            }
            Some(LiveAgent {
                session_id: l.session_id.clone(),
                harness: String::new(),
                repo: l.repo.clone(),
                branch: l.branch.clone(),
                target: format!("{}:{}", l.repo, l.branch),
                // A launch record carries no checkout; the window join still finds one by session
                // id if the launcher already stamped or leased its window.
                worktree: None,
                tmux_target: None,
                last_activity_ts: l.launched_at,
                idle_secs: age,
                health: AgentHealth::Starting,
                role: AgentRole::Worker,
                turns: 0,
                tool_calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                // Stamped by `stamp_control` once the whole fleet is assembled.
                control: ControlChannel::None,
            })
        })
        .collect()
}

/// Stamp each live agent's [`AgentRole`] from the launch-path attribution map (`session_id →`
/// [`WorkClass`], keyed on the harness session id `bn dispatch` pins). Pure over the map so it is
/// unit-testable; agents with no attribution keep their default [`AgentRole::Worker`].
fn stamp_roles(agents: &mut [LiveAgent], attribution: &BTreeMap<String, WorkClass>) {
    for a in agents.iter_mut() {
        if let Some(wc) = attribution.get(&crate::cost::canonical_session_id(&a.session_id)) {
            a.role = AgentRole::from_work_class(*wc);
        }
    }
}

/// Stamp each agent's [`ControlChannel`] — whether bn has a way to write to it at all — along with
/// the exact window and checkout the join found.
///
/// tmux is enumerated **once** for the whole pass rather than probed per agent, because the fleet
/// endpoint is polled every couple of seconds and a `tmux display-message` per agent would put a
/// process spawn on that hot path. The lease registry is one file read for the pass; the note lookup
/// that follows is a single small file read per agent.
///
/// The join itself is [`crate::agent::resolve_channels`] — a window is matched by lease, by session
/// id, by checkout, by its `@bn_*` stamps, and only last by name. Matching on name alone (as this
/// did) misses every window called after a pooled slot or a reused directory, which is why the
/// `Tmux` channel used to be unreachable in practice.
fn stamp_control(cfg: &Config, agents: &mut [LiveAgent]) {
    let windows = crate::agent::parse_window_rows(
        &crate::tmux::tmux_raw(&["list-windows", "-a", "-F", crate::agent::WINDOW_FORMAT])
            .unwrap_or_default(),
    );
    let leases = crate::agent::load_agent_leases(&crate::agent::agent_lease_registry_path())
        .unwrap_or_default()
        .leases;

    let keys: Vec<crate::agent::AgentKey<'_>> = agents
        .iter()
        .map(|a| crate::agent::AgentKey {
            session_id: &a.session_id,
            repo: &a.repo,
            branch: &a.branch,
            worktree: a.worktree.as_deref(),
        })
        .collect();
    let resolved = crate::agent::resolve_channels(&keys, &windows, &leases);

    for (a, ch) in agents.iter_mut().zip(resolved) {
        let recorded = recorded_state(
            recorded_session(cfg, &a.repo, &a.branch)
                .map(|(_, id)| id)
                .as_deref(),
            &a.session_id,
        );
        // Settle the checkout *before* asking for the channel: the join's answer beats the
        // transcript's cwd (a pane may have wandered into a subdirectory), and the resume fallback
        // needs a directory that still exists — a `--resume` turn spawned in a deleted worktree
        // fails after a full harness invocation, which is worse than saying "no channel".
        a.tmux_target = ch.tmux_target.clone();
        a.worktree = ch
            .worktree
            .clone()
            .or_else(|| a.worktree.take())
            .filter(|p| std::path::Path::new(p).is_dir());
        a.control = crate::agent::control_channel(
            &ch,
            recorded,
            crate::agent::ObservedSession {
                session_id: &a.session_id,
                worktree: a.worktree.as_deref(),
            },
        );
    }
}

/// Whether a branch note's recorded `agent-session:` id names the live session in front of us.
/// Pure — the branch-keyed record can outlive the agent it named (a previous dispatch, or a
/// `--continue` launch that adopted the harness's own id), and resuming into a dead id steers
/// nothing at all.
fn recorded_state(recorded: Option<&str>, live_session: &str) -> crate::agent::RecordedSession {
    match recorded {
        None => crate::agent::RecordedSession::None,
        Some(id) if canonical_session_id(id) == canonical_session_id(live_session) => {
            crate::agent::RecordedSession::Same
        }
        Some(_) => crate::agent::RecordedSession::Other,
    }
}

/// The `agent-session: <harness> <id>` the dispatcher records in a branch note's collab section,
/// latest wins — the same record `bn agent send` resumes against. `None` means no resume channel.
fn recorded_session(cfg: &Config, repo: &str, branch: &str) -> Option<(String, String)> {
    let ctx = crate::resolver::Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    let note = crate::store::read(&cfg.note_file(&ctx)).ok()?;
    note.front.collab.iter().rev().find_map(|it| {
        let rest = it.text.trim().strip_prefix("agent-session:")?;
        let mut w = rest.split_whitespace();
        Some((w.next()?.to_string(), w.next()?.to_string()))
    })
}

/// [`supervise`] with explicit Claude + Copilot roots and `now` — the testable core. A session is
/// *running* iff its log was modified within [`QUIET_SECS`] (the same window the finished-session
/// scanners use, inverted), so a session is either live here or ingestable there, never both. Both
/// harnesses are attributed to the same `repo:branch` note key, so the fleet is uniform.
pub fn supervise_from(
    cfg: &Config,
    claude_root: &Path,
    copilot_root: &Path,
    now: i64,
    idle_mins: i64,
    stuck_mins: i64,
) -> Fleet {
    let mut agents = Vec::new();

    // Claude: one transcript file per session under <root>/<encoded-cwd>/<session>.jsonl.
    if let Ok(projects) = std::fs::read_dir(claude_root) {
        for proj in projects.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if is_quiet(&path, now) {
                    continue; // finished — that's ingest's job, not supervise's.
                }
                if let Some(a) = parse_live_claude(cfg, &path, now, idle_mins, stuck_mins) {
                    agents.push(a);
                }
            }
        }
    }

    // Copilot: one directory per session (events.jsonl + workspace.yaml) under <root>/<session>/.
    if let Ok(dirs) = std::fs::read_dir(copilot_root) {
        for d in dirs.flatten() {
            let dir = d.path();
            if !dir.is_dir() {
                continue;
            }
            if is_quiet(&dir.join("events.jsonl"), now) {
                continue; // finished (or has a shutdown record) — ingest's job.
            }
            if let Some(a) = parse_live_copilot(cfg, &dir, now, idle_mins, stuck_mins) {
                agents.push(a);
            }
        }
    }

    agents.sort_by_key(|a| std::cmp::Reverse(a.last_activity_ts));
    Fleet::from_agents(agents)
}

/// Read a still-running Claude transcript into a [`LiveAgent`]. Reuses the same attribution
/// (cwd + branch hint → note key) and turn/tool counting as [`parse_claude_session`], but keeps
/// only what the live view needs and computes idle from the last event timestamp.
fn parse_live_claude(
    cfg: &Config,
    path: &Path,
    now: i64,
    idle_mins: i64,
    stuck_mins: i64,
) -> Option<LiveAgent> {
    let lines = events_json(path);
    if lines.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };

    let session_id = lines
        .iter()
        .find_map(|v| {
            v.get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;

    let (mut cwd, mut branch_hint) = (String::new(), String::new());
    for v in &lines {
        if let Some(b) = v.get("gitBranch").and_then(|x| x.as_str()) {
            if !b.is_empty() {
                branch_hint = b.to_string();
                if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                    cwd = c.to_string();
                }
            }
        }
    }
    let (repo, branch) = note_key(cfg, &cwd, &branch_hint, None)?;

    // Last sign of life: the newest event timestamp, falling back to the file's mtime (which we
    // already know is recent, since the file isn't quiet).
    let last_activity_ts = lines
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(path))
        .unwrap_or(now);
    let idle_secs = (now - last_activity_ts).max(0);

    let (mut turns, mut tools) = (0i64, 0i64);
    let (mut input, mut output, mut cache_read) = (0i64, 0i64, 0i64);
    for v in &lines {
        if typ(v) != "assistant" {
            continue;
        }
        turns += 1;
        let msg = v.get("message");
        if let Some(u) = msg.and_then(|m| m.get("usage")) {
            input += u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            output += u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            cache_read += u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
        }
        if let Some(content) = msg
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            tools += content
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                .count() as i64;
        }
    }

    Some(LiveAgent {
        session_id,
        harness: "claude".into(),
        target: format!("{repo}:{branch}"),
        // The transcript's own cwd is the strongest hint about where this agent lives, and the key
        // [`stamp_control`] joins it to a tmux window with.
        worktree: Some(cwd).filter(|c| !c.is_empty()),
        tmux_target: None,
        repo,
        branch,
        last_activity_ts,
        idle_secs,
        health: AgentHealth::from_idle(idle_secs, idle_mins, stuck_mins),
        role: AgentRole::default(),
        turns,
        tool_calls: tools,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        control: ControlChannel::None,
    })
}

/// Read a still-running Copilot session directory into a [`LiveAgent`]. Mirrors [`parse_live_claude`]
/// for the Copilot layout: a `session.shutdown` record marks the session *finished* (that's
/// [`scan_copilot`]'s job), so its presence returns `None` here. Attribution reuses the same
/// `workspace.yaml`-then-`session.start` resolution as [`parse_copilot_session`], and health is the
/// same idle-vs-thresholds classification — so Copilot agents sit in the fleet beside Claude ones.
fn parse_live_copilot(
    cfg: &Config,
    dir: &Path,
    now: i64,
    idle_mins: i64,
    stuck_mins: i64,
) -> Option<LiveAgent> {
    let evs = events_json(&dir.join("events.jsonl"));
    if evs.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    // A shutdown record means the session ended — leave it to the finished-session scanner. But
    // Copilot RESUMES a session by appending to the same events.jsonl, so an old shutdown can sit
    // mid-stream with fresh activity after it. Treat the session as finished only when the shutdown
    // is the LAST event (nothing resumed it); if any event follows the last shutdown, it's live.
    if evs
        .iter()
        .rposition(|v| typ(v) == "session.shutdown")
        .is_some_and(|i| i == evs.len() - 1)
    {
        return None;
    }
    let start = evs.iter().find(|v| typ(v) == "session.start");
    let ctx = start
        .and_then(|s| s.get("data"))
        .and_then(|d| d.get("context"))
        .cloned()
        .unwrap_or(Value::Null);

    let ws = read_yaml(&dir.join("workspace.yaml"));
    let session_id =
        ws_str(&ws, "id").or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let cwd = ws_str(&ws, "cwd")
        .or_else(|| ctx.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let branch_hint = ws_str(&ws, "branch")
        .or_else(|| ctx.get("branch").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let repository = ws_str(&ws, "repository").or_else(|| {
        ctx.get("repository")
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let (repo, branch) = note_key(cfg, &cwd, &branch_hint, repository.as_deref())?;

    let last_activity_ts = evs
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(&dir.join("events.jsonl")))
        .unwrap_or(now);
    let idle_secs = (now - last_activity_ts).max(0);

    let count = |t: &str| evs.iter().filter(|v| typ(v) == t).count() as i64;

    // A live Copilot log carries no running cost/AIU (that lands only in `session.shutdown`), but
    // each `assistant.message` records the turn's `outputTokens` — enough for a live burn signal.
    let output_tokens: i64 = evs
        .iter()
        .filter(|v| typ(v) == "assistant.message")
        .filter_map(|v| v.get("data").and_then(|d| d.get("outputTokens")))
        .filter_map(|v| v.as_i64())
        .sum();

    Some(LiveAgent {
        session_id,
        harness: "copilot".into(),
        target: format!("{repo}:{branch}"),
        worktree: Some(cwd).filter(|c| !c.is_empty()),
        tmux_target: None,
        repo,
        branch,
        last_activity_ts,
        idle_secs,
        health: AgentHealth::from_idle(idle_secs, idle_mins, stuck_mins),
        role: AgentRole::default(),
        turns: count("assistant.turn_start"),
        tool_calls: count("tool.execution_start"),
        input_tokens: 0,
        output_tokens,
        cache_read_tokens: 0,
        control: ControlChannel::None,
    })
}

/// A file's mtime as epoch seconds (the activity fallback when a transcript carries no parseable
/// timestamp).
fn file_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

// --- agent progress probe (#374) ------------------------------------------------------------
//
// The orchestrator's #374 liveness signal: given a worktree path, how much has the agent working
// there actually *done* (cumulative AIC) and when did it last do anything. Unlike [`supervise`],
// this reads a session **regardless of quiet/finished** — a *stalled* agent's log is untouched for
// far longer than [`QUIET_SECS`], yet that's precisely the agent we must measure to tell it apart
// from a just-launched one (no tokens yet).

/// The cumulative progress of the newest harness session whose worktree is a given cwd — the #374
/// liveness probe the orchestrator uses to distinguish a *stalled* agent (worked, then died at an
/// idle prompt) from a *just-launched* one (no turn produced yet).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentProgress {
    pub harness: String,
    pub session_id: String,
    /// Cumulative output tokens burned (AIC, #309) — `0` when the agent produced no assistant turn.
    pub aic: i64,
    /// Epoch secs of the newest event (last sign of life), falling back to the log's mtime.
    pub last_activity_ts: i64,
}

/// [`agent_progress_from`] against the real `~/.claude/projects` + `~/.copilot/session-state`.
pub fn agent_progress_for_cwd(cwd: &Path) -> Option<AgentProgress> {
    let home = dirs::home_dir().unwrap_or_default();
    agent_progress_from(
        &home.join(".claude").join("projects"),
        &home.join(".copilot").join("session-state"),
        cwd,
    )
}

/// The newest Claude or Copilot session whose recorded cwd is `cwd`, with its cumulative output
/// tokens + last-activity ts — read regardless of quiet/finished (unlike [`supervise`]), so a
/// *stalled* agent (log untouched for > [`QUIET_SECS`]) is still measured. `None` when no session
/// targets `cwd`. The testable core: explicit roots, no `now`/`Config` needed.
pub fn agent_progress_from(
    claude_root: &Path,
    copilot_root: &Path,
    cwd: &Path,
) -> Option<AgentProgress> {
    let mut best: Option<AgentProgress> = None;
    let mut consider = |p: AgentProgress| {
        if best
            .as_ref()
            .map(|b| p.last_activity_ts > b.last_activity_ts)
            .unwrap_or(true)
        {
            best = Some(p);
        }
    };
    // Copilot: one directory per session (events.jsonl + workspace.yaml).
    if let Ok(dirs) = std::fs::read_dir(copilot_root) {
        for d in dirs.flatten() {
            let dir = d.path();
            if dir.is_dir() {
                if let Some(p) = copilot_progress(&dir, cwd) {
                    consider(p);
                }
            }
        }
    }
    // Claude: one transcript per session under <encoded-cwd>/<session>.jsonl.
    if let Ok(projects) = std::fs::read_dir(claude_root) {
        for proj in projects.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Some(p) = claude_progress(&path, cwd) {
                    consider(p);
                }
            }
        }
    }
    best
}

/// Whether two paths point at the same directory — literal match first (the common case: the
/// harness records the exact worktree path it was launched in), then canonicalized (tolerates a
/// `/private` symlink or trailing slash on macOS).
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Progress for a Copilot session dir, iff its recorded cwd is `cwd`. Sums each `assistant.message`'s
/// `outputTokens` (the live AIC signal) and takes the newest event ts as last-activity.
fn copilot_progress(dir: &Path, cwd: &Path) -> Option<AgentProgress> {
    let events = dir.join("events.jsonl");
    let evs = events_json(&events);
    if evs.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ws = read_yaml(&dir.join("workspace.yaml"));
    let ctx = evs
        .iter()
        .find(|v| typ(v) == "session.start")
        .and_then(|s| s.get("data"))
        .and_then(|d| d.get("context"))
        .cloned()
        .unwrap_or(Value::Null);
    let session_cwd = ws_str(&ws, "cwd")
        .or_else(|| ctx.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    if session_cwd.is_empty() || !same_dir(Path::new(&session_cwd), cwd) {
        return None;
    }
    let session_id =
        ws_str(&ws, "id").or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let last_activity_ts = evs
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(&events))
        .unwrap_or(0);
    let aic: i64 = evs
        .iter()
        .filter(|v| typ(v) == "assistant.message")
        .filter_map(|v| v.get("data").and_then(|d| d.get("outputTokens")))
        .filter_map(|v| v.as_i64())
        .sum();
    Some(AgentProgress {
        harness: "copilot".into(),
        session_id,
        aic,
        last_activity_ts,
    })
}

/// Progress for a Claude transcript, iff its recorded cwd is `cwd`. Sums each assistant turn's
/// `usage.output_tokens` and takes the newest event ts as last-activity.
fn claude_progress(path: &Path, cwd: &Path) -> Option<AgentProgress> {
    let lines = events_json(path);
    if lines.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let session_cwd = lines
        .iter()
        .find_map(|v| v.get("cwd").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default();
    if session_cwd.is_empty() || !same_dir(Path::new(&session_cwd), cwd) {
        return None;
    }
    let session_id = lines
        .iter()
        .find_map(|v| {
            v.get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let last_activity_ts = lines
        .iter()
        .filter_map(ts_of)
        .max()
        .or_else(|| file_mtime(path))
        .unwrap_or(0);
    let aic: i64 = lines
        .iter()
        .filter(|v| typ(v) == "assistant")
        .filter_map(|v| {
            v.get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_i64())
        })
        .sum();
    Some(AgentProgress {
        harness: "claude".into(),
        session_id,
        aic,
        last_activity_ts,
    })
}

#[cfg(test)]
mod tests {
    use super::super::parse::QUIET_SECS;
    use super::*;
    use crate::agent::{test_cfg, write};
    use serde_json::json;

    fn stuck_obs<'a>(
        tail: &'a [String],
        plane_a: &'a str,
        has_wip: bool,
        unchanged_secs: i64,
    ) -> StuckLaneObservation<'a> {
        StuckLaneObservation {
            tail,
            has_window: true,
            has_process: true,
            plane_a,
            has_wip,
            unchanged_secs,
            idle_threshold_secs: 600,
        }
    }

    #[test]
    fn stuck_classifier_detects_known_permission_prompts() {
        for line in [
            "❯ 2. Yes, and add these directories to the allowed list",
            "❯ 2. Yes, don't ask again",
        ] {
            let tail = vec![line.to_string()];
            let stuck = classify_stuck_lane(&stuck_obs(&tail, "working", false, 15)).unwrap();
            assert_eq!(stuck.kind, StuckLaneKind::InteractivePrompt);
        }
    }

    #[test]
    fn stuck_classifier_detects_trust_and_question_menus_via_orchestrate_policy() {
        let trust = vec!["Do you trust the files in this folder?".to_string()];
        assert_eq!(
            classify_stuck_lane(&stuck_obs(&trust, "working", false, 15))
                .unwrap()
                .kind,
            StuckLaneKind::InteractivePrompt
        );

        let question = vec![
            "Select an option".to_string(),
            "1. Continue".to_string(),
            "2. Stop".to_string(),
        ];
        assert_eq!(
            classify_stuck_lane(&stuck_obs(&question, "working", false, 15))
                .unwrap()
                .kind,
            StuckLaneKind::InteractivePrompt
        );
    }

    #[test]
    fn stuck_classifier_detects_mcp_loading() {
        let tail = vec!["Loading… MCP servers".to_string()];
        assert!(classify_stuck_lane(&stuck_obs(&tail, "working", false, 0)).is_none());
        let stuck = classify_stuck_lane(&stuck_obs(&tail, "working", false, 15)).unwrap();
        assert_eq!(stuck.kind, StuckLaneKind::McpLoading);
    }

    #[test]
    fn stuck_classifier_requires_the_idle_threshold_for_working_lanes() {
        let tail = vec!["agent output with no spinner".to_string()];
        assert!(classify_stuck_lane(&stuck_obs(&tail, "working", false, 599)).is_none());
        assert_eq!(
            classify_stuck_lane(&stuck_obs(&tail, "working", false, 600))
                .unwrap()
                .kind,
            StuckLaneKind::WorkingIdle
        );
    }

    #[test]
    fn stuck_classifier_detects_idle_with_uncommitted_work() {
        let tail = vec!["┃".to_string()];
        let stuck = classify_stuck_lane(&stuck_obs(&tail, "waiting", true, 0)).unwrap();
        assert_eq!(stuck.kind, StuckLaneKind::IdleWithWip);
    }

    #[test]
    fn pane_activity_fingerprint_is_stable_and_sensitive() {
        let tail = vec!["one".to_string(), "two".to_string()];
        assert_eq!(
            pane_activity_fingerprint(&tail, "working"),
            pane_activity_fingerprint(&tail, "working")
        );
        assert_ne!(
            pane_activity_fingerprint(&tail, "working"),
            pane_activity_fingerprint(&tail, "waiting")
        );
    }

    // --- supervise tests --------------------------------------------------------------------

    #[test]
    fn health_classifies_against_thresholds() {
        // idle=5m, stuck=15m.
        assert_eq!(AgentHealth::from_idle(60, 5, 15), AgentHealth::Active);
        assert_eq!(AgentHealth::from_idle(5 * 60, 5, 15), AgentHealth::Idle);
        assert_eq!(AgentHealth::from_idle(10 * 60, 5, 15), AgentHealth::Idle);
        assert_eq!(AgentHealth::from_idle(15 * 60, 5, 15), AgentHealth::Stuck);
        assert_eq!(AgentHealth::from_idle(60 * 60, 5, 15), AgentHealth::Stuck);
    }

    // --- pending "starting…" entries (#545) -------------------------------------------------

    fn launch(sid: &str, repo: &str, branch: &str, ts: i64) -> LaunchRecord {
        LaunchRecord {
            session_id: sid.to_string(),
            task_name: String::new(),
            harness: String::new(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            worktree: String::new(),
            work_class: WorkClass::Coding,
            launched_at: ts,
            controller_id: None,
        }
    }

    #[test]
    fn pending_synthesizes_a_starting_entry_for_a_launch_without_transcript() {
        // A just-dispatched agent: a launch record exists, but no live transcript and no ingested
        // session yet. It must surface as a pending `Starting` entry so it is visible pre-transcript.
        let now = 10_000;
        let launches = vec![launch("sess-new", "bn", "dispatch/web-545", now - 30)];
        let pending = pending_agents(&launches, &HashSet::new(), &HashSet::new(), now, 20);
        assert_eq!(pending.len(), 1);
        let a = &pending[0];
        assert_eq!(a.health, AgentHealth::Starting);
        assert_eq!(a.session_id, "sess-new");
        assert_eq!(a.repo, "bn");
        assert_eq!(a.branch, "dispatch/web-545");
        assert_eq!(a.target, "bn:dispatch/web-545");
        assert_eq!(a.last_activity_ts, now - 30);
        assert_eq!(a.idle_secs, 30);
        assert_eq!(a.turns, 0);
    }

    #[test]
    fn pending_skips_launches_that_are_live_or_finished_or_aged_out() {
        let now = 10_000;
        let launches = vec![
            launch("live-1", "bn", "a", now - 10), // already has a transcript
            launch("done-1", "bn", "b", now - 10), // already ingested
            launch("old-1", "bn", "c", now - 21 * 60), // past the 20-min starting window
            launch("new-1", "bn", "d", now - 60),  // the only genuine pending one
        ];
        let live: HashSet<String> = ["live-1".to_string()].into_iter().collect();
        let finished: HashSet<String> = ["done-1".to_string()].into_iter().collect();
        let pending = pending_agents(&launches, &live, &finished, now, 20);
        assert_eq!(
            pending
                .iter()
                .map(|a| a.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["new-1"]
        );
    }

    #[test]
    fn pending_starting_window_is_half_open_at_the_exact_cutoff() {
        let now = 10_000;
        let window = DEFAULT_STARTING_MINS * 60;
        let launches = vec![
            launch("inside", "bn", "a", now - window + 1),
            launch("cutoff", "bn", "b", now - window),
            launch("future", "bn", "c", now + 1),
        ];
        let pending = pending_agents(
            &launches,
            &HashSet::new(),
            &HashSet::new(),
            now,
            DEFAULT_STARTING_MINS,
        );
        assert_eq!(
            pending
                .iter()
                .map(|a| a.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["inside"],
            "age < window is starting; age == window and future launches are excluded"
        );
    }

    #[test]
    fn pending_dedup_is_case_insensitive_for_live_and_finished_sessions() {
        let now = 10_000;
        let launches = vec![
            launch("LIVE-1", "bn", "a", now - 30),
            launch("DONE-1", "bn", "b", now - 30),
            launch("pending-1", "bn", "c", now - 30),
        ];
        let live: HashSet<String> = ["live-1".to_string()].into_iter().collect();
        let finished: HashSet<String> = ["done-1".to_string()].into_iter().collect();
        let pending = pending_agents(&launches, &live, &finished, now, DEFAULT_STARTING_MINS);
        assert_eq!(
            pending
                .iter()
                .map(|a| a.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["pending-1"]
        );
    }

    #[test]
    fn pending_uses_the_latest_launch_record_for_a_session() {
        let now = 10_000;
        let launches = vec![
            launch("SESS-1", "old", "old-branch", now - 120),
            launch("sess-1", "bn", "new-branch", now - 30),
        ];
        let pending = pending_agents(
            &launches,
            &HashSet::new(),
            &HashSet::new(),
            now,
            DEFAULT_STARTING_MINS,
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id, "sess-1");
        assert_eq!(pending[0].repo, "bn");
        assert_eq!(pending[0].branch, "new-branch");
        assert_eq!(pending[0].last_activity_ts, now - 30);
    }

    // --- role model (#500) ------------------------------------------------------------------

    #[test]
    fn agent_role_defaults_to_worker() {
        // An agent constructed without an explicit role — like every one [`supervise_from`] builds
        // before [`stamp_roles`] runs — is a worker.
        assert_eq!(AgentRole::default(), AgentRole::Worker);
    }

    #[test]
    fn agent_role_classifies_from_work_class() {
        // Only the orchestration work-class marks an orchestrator; every other class is a worker.
        assert_eq!(
            AgentRole::from_work_class(WorkClass::Orchestration),
            AgentRole::Orchestrator
        );
        for wc in [
            WorkClass::Coding,
            WorkClass::Scheduled,
            WorkClass::Research,
            WorkClass::Remote,
            WorkClass::Unattributed,
        ] {
            assert_eq!(AgentRole::from_work_class(wc), AgentRole::Worker);
        }
    }

    #[test]
    fn orchestrator_agent_round_trips_through_serde() {
        // A live agent built via the real supervisor (default worker), re-stamped orchestrator,
        // serializes with `role:"orchestrator"` and deserializes back byte-identically.
        let tmp = std::env::temp_dir().join(format!("bn-agent-role-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = crate::test_support::config(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("orchestrator");
        let claude = tmp.join("claude");
        let now = now_epoch_secs();
        write_live_claude(&claude, "orch-sid", &cwd, "orchestrator", now);
        let mut fleet = supervise_from(
            &cfg,
            &claude,
            &tmp.join("copilot"),
            now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 1);
        assert_eq!(fleet.agents[0].role, AgentRole::Worker);

        let mut attribution = BTreeMap::new();
        attribution.insert("orch-sid".to_string(), WorkClass::Orchestration);
        stamp_roles(&mut fleet.agents, &attribution);
        let agent = &fleet.agents[0];
        assert_eq!(agent.role, AgentRole::Orchestrator);

        let json = serde_json::to_string(agent).unwrap();
        assert!(json.contains("\"role\":\"orchestrator\""), "{json}");
        let back: LiveAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, agent);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fleet_json_without_role_defaults_to_worker() {
        // Older fleet JSON (recorded before #500) carries no `role`; `#[serde(default)]` reads it
        // back as a worker rather than failing to deserialize.
        let legacy = json!({
            "session_id": "s1",
            "harness": "copilot",
            "repo": "bn",
            "branch": "feat-x",
            "target": "bn:feat-x",
            "last_activity_ts": 100,
            "idle_secs": 0,
            "health": "active",
            "turns": 1,
            "tool_calls": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0
        });
        let agent: LiveAgent = serde_json::from_value(legacy).unwrap();
        assert_eq!(agent.role, AgentRole::Worker);
    }

    #[test]
    fn a_record_naming_another_session_is_not_this_agents_resume_channel() {
        // The note's `agent-session:` is keyed by branch, so it outlives the agent it named. Only
        // an id that matches the live session earns `Resume`; a stale one must fall through to the
        // window join rather than offering a button that resumes a dead session.
        assert_eq!(
            recorded_state(Some("A1B2-c3"), "a1b2-C3"),
            crate::agent::RecordedSession::Same,
            "ids are compared canonically, as the role join already does"
        );
        assert_eq!(
            recorded_state(Some("older-session"), "live-session"),
            crate::agent::RecordedSession::Other
        );
        assert_eq!(
            recorded_state(None, "live-session"),
            crate::agent::RecordedSession::None
        );
    }

    #[test]
    fn stamp_roles_only_marks_orchestration_sessions() {
        let worker = |sid: &str, branch: &str| LiveAgent {
            session_id: sid.into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: branch.into(),
            target: format!("bn:{branch}"),
            worktree: None,
            tmux_target: None,
            last_activity_ts: 0,
            idle_secs: 0,
            health: AgentHealth::Active,
            role: AgentRole::Worker,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            control: ControlChannel::None,
        };
        let mut agents = vec![worker("orch", "wave"), worker("ship", "ship")];
        let mut attribution = BTreeMap::new();
        attribution.insert("orch".to_string(), WorkClass::Orchestration);
        attribution.insert("ship".to_string(), WorkClass::Coding);
        stamp_roles(&mut agents, &attribution);
        assert_eq!(agents[0].role, AgentRole::Orchestrator);
        assert_eq!(agents[1].role, AgentRole::Worker);
    }

    #[test]
    fn stamp_roles_joins_case_insensitively() {
        // Regression guard for #543: `bn dispatch --orchestrator` pins the session id it generates
        // while the harness records the id in its own case. The role join canonicalizes both sides
        // (via `cost::canonical_session_id`), so an `Orchestration` session still stamps
        // `orchestrator` even when the live agent's id case differs from the attribution key.
        let mut agent = LiveAgent {
            session_id: "A1B2C3D4-0000-0000-0000-000000000000".into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "wave".into(),
            target: "bn:wave".into(),
            worktree: None,
            tmux_target: None,
            last_activity_ts: 0,
            idle_secs: 0,
            health: AgentHealth::Active,
            role: AgentRole::Worker,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            control: ControlChannel::None,
        };
        let mut attribution = BTreeMap::new();
        attribution.insert(
            "a1b2c3d4-0000-0000-0000-000000000000".to_string(),
            WorkClass::Orchestration,
        );
        stamp_roles(std::slice::from_mut(&mut agent), &attribution);
        assert_eq!(agent.role, AgentRole::Orchestrator);
    }

    /// Write a running Claude transcript whose newest event is at `last_ts` (epoch secs). The
    /// file is freshly written, so its mtime is ~wall-clock-now; tests pick `now` relative to
    /// that so the liveness window (`is_quiet`) and the idle window are both controlled.
    fn write_live_claude(root: &Path, sid: &str, cwd: &Path, branch: &str, last_ts: i64) {
        let last_iso = chrono::DateTime::from_timestamp(last_ts, 0)
            .unwrap()
            .to_rfc3339();
        let path = root.join("encoded-cwd").join(format!("{sid}.jsonl"));
        write(
            &path,
            &[
                json!({"type":"user","sessionId":sid,"gitBranch":branch,"cwd":cwd,"timestamp":last_iso}).to_string(),
                json!({"type":"assistant","sessionId":sid,"gitBranch":branch,"cwd":cwd,"timestamp":last_iso,"message":{"content":[{"type":"tool_use"}],"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":5}}}).to_string(),
            ].join("\n"),
        );
    }

    /// Write a running Copilot session dir (events.jsonl + workspace.yaml) whose newest event is at
    /// `last_ts`. `shutdown` adds a `session.shutdown` record so the session reads as *finished*.
    fn write_live_copilot(
        root: &Path,
        sid: &str,
        cwd: &Path,
        branch: &str,
        last_ts: i64,
        shutdown: bool,
    ) {
        let last_iso = chrono::DateTime::from_timestamp(last_ts, 0)
            .unwrap()
            .to_rfc3339();
        let dir = root.join(sid);
        let mut lines = vec![
            json!({"type":"session.start","timestamp":last_iso,"data":{"context":{"cwd":cwd,"branch":branch}}}).to_string(),
            json!({"type":"assistant.turn_start","timestamp":last_iso}).to_string(),
            json!({"type":"assistant.message","timestamp":last_iso,"data":{"outputTokens":42}}).to_string(),
            json!({"type":"tool.execution_start","timestamp":last_iso}).to_string(),
        ];
        if shutdown {
            lines.push(
                json!({"type":"session.shutdown","timestamp":last_iso,"data":{}}).to_string(),
            );
        }
        write(&dir.join("events.jsonl"), &lines.join("\n"));
        write(
            &dir.join("workspace.yaml"),
            &format!("id: {sid}\ncwd: {}\nbranch: {branch}\n", cwd.display()),
        );
    }

    #[test]
    fn supervise_lists_running_sessions_with_health() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-supervise-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("feat-live");
        let claude = tmp.join("claude");

        // The file is fresh (mtime ~ real now); the last *event* is 20 wall-minutes "ago" from
        // the `now` we pass — but only 20m off the freshly-written file, so it's still live
        // (< QUIET_SECS), yet idle 20m → stuck (> 15m).
        let real_now = now_epoch_secs();
        let last_ts = real_now;
        let now = real_now + 20 * 60;
        write_live_claude(
            &claude,
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            &cwd,
            "feat-live",
            last_ts,
        );

        let copilot = tmp.join("copilot");
        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            now,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 1);
        let a = &fleet.agents[0];
        assert_eq!((a.repo.as_str(), a.branch.as_str()), ("bn", "feat-live"));
        assert_eq!(a.target, "bn:feat-live");
        assert_eq!(a.health, AgentHealth::Stuck);
        assert_eq!(a.idle_secs, 20 * 60);
        assert_eq!(a.tool_calls, 1);
        assert_eq!((fleet.active, fleet.idle, fleet.stuck), (0, 0, 1));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_marks_recent_activity_active() {
        let tmp =
            std::env::temp_dir().join(format!("bn-agent-supervise-active-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("feat-live");
        let claude = tmp.join("claude");

        let real_now = now_epoch_secs();
        // Activity 1 minute ago → active; now is 1 min past the event, file fresh → live.
        write_live_claude(
            &claude,
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            &cwd,
            "feat-live",
            real_now,
        );
        let copilot = tmp.join("copilot");
        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            real_now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 1);
        assert_eq!(fleet.agents[0].health, AgentHealth::Active);
        assert_eq!((fleet.active, fleet.idle, fleet.stuck), (1, 0, 0));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_skips_quiet_finished_transcripts() {
        let tmp =
            std::env::temp_dir().join(format!("bn-agent-supervise-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("feat-done");
        let claude = tmp.join("claude");
        let real_now = now_epoch_secs();
        write_live_claude(
            &claude,
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            &cwd,
            "feat-done",
            real_now,
        );
        // Choose `now` far enough past the (fresh) file mtime that QUIET_SECS has elapsed →
        // the transcript reads as finished, not live, and supervise drops it.
        let now = real_now + QUIET_SECS + 60;
        let copilot = tmp.join("copilot");
        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            now,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert!(fleet.agents.is_empty(), "a quiet transcript is not live");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_lists_copilot_sessions_beside_claude() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-fleet-mix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let real_now = now_epoch_secs();

        // A live Claude agent (active) and a live Copilot agent (active), different worktrees.
        write_live_claude(
            &claude,
            "dddddddd-dddd-dddd-dddd-dddddddddddd",
            &cfg.worktree_dir.join("bn").join("feat-cc"),
            "feat-cc",
            real_now,
        );
        write_live_copilot(
            &copilot,
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
            &cfg.worktree_dir.join("bn").join("feat-cop"),
            "feat-cop",
            real_now,
            false,
        );

        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            real_now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 2, "both harnesses appear in the fleet");
        let harnesses: std::collections::HashSet<_> =
            fleet.agents.iter().map(|a| a.harness.as_str()).collect();
        assert!(harnesses.contains("claude") && harnesses.contains("copilot"));
        let cop = fleet
            .agents
            .iter()
            .find(|a| a.harness == "copilot")
            .expect("copilot agent present");
        assert_eq!(cop.target, "bn:feat-cop");
        assert_eq!(cop.tool_calls, 1);
        assert_eq!(cop.turns, 1);
        assert_eq!(cop.health, AgentHealth::Active);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_skips_copilot_session_with_shutdown() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-cop-done-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let real_now = now_epoch_secs();
        // A recently-touched Copilot session that carries a shutdown record → finished, not live.
        write_live_copilot(
            &copilot,
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            &cfg.worktree_dir.join("bn").join("feat-cop"),
            "feat-cop",
            real_now,
            true,
        );
        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            real_now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert!(
            fleet.agents.is_empty(),
            "a copilot session with a shutdown record is finished, not live"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_sums_live_token_usage_and_fleet_total() {
        // A live Claude agent carries full per-message usage; a live Copilot agent exposes only
        // per-message output tokens. The fleet total sums the headline (output) tokens: 20 + 42.
        let tmp =
            std::env::temp_dir().join(format!("bn-agent-supervise-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let real_now = now_epoch_secs();

        write_live_claude(
            &claude,
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            &cfg.worktree_dir.join("bn").join("feat-a"),
            "feat-a",
            real_now,
        );
        write_live_copilot(
            &copilot,
            "dddddddd-dddd-dddd-dddd-dddddddddddd",
            &cfg.worktree_dir.join("bn").join("feat-b"),
            "feat-b",
            real_now,
            false,
        );

        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            real_now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 2);
        let claude_a = fleet.agents.iter().find(|a| a.harness == "claude").unwrap();
        assert_eq!(
            (
                claude_a.input_tokens,
                claude_a.output_tokens,
                claude_a.cache_read_tokens
            ),
            (100, 20, 5)
        );
        let copilot_a = fleet
            .agents
            .iter()
            .find(|a| a.harness == "copilot")
            .unwrap();
        // Copilot has no live input/cache tokens — only per-message output.
        assert_eq!(
            (
                copilot_a.input_tokens,
                copilot_a.output_tokens,
                copilot_a.cache_read_tokens
            ),
            (0, 42, 0)
        );
        // Fleet total is the headline (output) tokens only, so it stays comparable across harnesses.
        assert_eq!(fleet.total_tokens, 62);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn supervise_treats_resumed_copilot_as_live() {
        // Copilot resumes by appending to the same events.jsonl, so an OLD shutdown can sit
        // mid-stream with fresh activity after it. That session is live again, not finished.
        let tmp = std::env::temp_dir().join(format!("bn-agent-cop-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let real_now = now_epoch_secs();
        let iso = chrono::DateTime::from_timestamp(real_now, 0)
            .unwrap()
            .to_rfc3339();
        let cwd = cfg.worktree_dir.join("bn").join("feat-cop");
        let dir = copilot.join("gggggggg-gggg-gggg-gggg-gggggggggggg");
        // start → turn → SHUTDOWN → (resume) turn : the shutdown is NOT the last event.
        write(
            &dir.join("events.jsonl"),
            &[
                json!({"type":"session.start","timestamp":iso,"data":{"context":{"cwd":cwd,"branch":"feat-cop"}}}).to_string(),
                json!({"type":"assistant.turn_start","timestamp":iso}).to_string(),
                json!({"type":"session.shutdown","timestamp":iso,"data":{}}).to_string(),
                json!({"type":"assistant.turn_start","timestamp":iso}).to_string(),
                json!({"type":"tool.execution_start","timestamp":iso}).to_string(),
            ]
            .join("\n"),
        );
        write(
            &dir.join("workspace.yaml"),
            &format!("id: g\ncwd: {}\nbranch: feat-cop\n", cwd.display()),
        );
        let fleet = supervise_from(
            &cfg,
            &claude,
            &copilot,
            real_now + 60,
            DEFAULT_IDLE_MINS,
            DEFAULT_STUCK_MINS,
        );
        assert_eq!(fleet.agents.len(), 1, "resumed session must read as live");
        assert_eq!(fleet.agents[0].harness, "copilot");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- agent progress probe (#374) --------------------------------------------------------

    #[test]
    fn agent_progress_reads_a_stalled_quiet_session() {
        // #374: the probe must measure a *stalled* agent — its log is quiet (untouched for far more
        // than QUIET_SECS), yet that's exactly the agent we need the AIC of. `supervise` would drop
        // it; `agent_progress_for_cwd` reads it anyway, keyed on the worktree cwd.
        let tmp = std::env::temp_dir().join(format!("bn-agent-progress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let cwd = cfg.worktree_dir.join("bn").join("dispatch-352");
        // One assistant.message with outputTokens 42 → AIC 42; last activity long ago (stalled).
        write_live_copilot(&copilot, "sess-352", &cwd, "dispatch-352", 1_000_000, false);

        let prog = agent_progress_from(&claude, &copilot, &cwd).expect("session found by cwd");
        assert_eq!(prog.harness, "copilot");
        assert_eq!(prog.aic, 42, "sums assistant.message outputTokens");
        assert_eq!(prog.last_activity_ts, 1_000_000);

        // A different worktree has no session → None (don't measure what we can't see).
        let other = cfg.worktree_dir.join("bn").join("dispatch-999");
        assert!(agent_progress_from(&claude, &copilot, &other).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_progress_picks_the_newest_session_for_the_cwd() {
        // After a hard-restart re-seed, a NEW session shares the worktree cwd with the old stalled
        // one. The probe must return the newest (freshest activity) — so a just-restarted agent
        // reads as recently-active, not still-stalled.
        let tmp =
            std::env::temp_dir().join(format!("bn-agent-progress-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let claude = tmp.join("claude");
        let copilot = tmp.join("copilot");
        let cwd = cfg.worktree_dir.join("bn").join("dispatch-352");
        write_live_copilot(&copilot, "old-352", &cwd, "dispatch-352", 1_000_000, false);
        write_live_copilot(&copilot, "new-352", &cwd, "dispatch-352", 2_000_000, false);

        let prog = agent_progress_from(&claude, &copilot, &cwd).unwrap();
        assert_eq!(prog.session_id, "new-352", "newest activity wins");
        assert_eq!(prog.last_activity_ts, 2_000_000);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
