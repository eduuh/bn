use super::parse::{events_json, is_quiet, note_key, read_yaml, ts_of, ws_str};
use crate::cost::{canonical_session_id, LaunchRecord, WorkClass};
use crate::resolver::Config;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

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
    /// The task this agent is working on, when one is bound to its `(repo, branch)`.
    ///
    /// The fleet could always say an agent was *running*; it could not say what it was running.
    /// Bound by [`crate::todo::claim`] (which `bn dispatch --task` and `bn task claim` call), and
    /// absent — rendered as nothing, never as an error — for an agent whose worktree no task
    /// governs. `#[serde(default)]` keeps older fleet JSON valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<AgentTask>,
}

/// The task an agent is working on, as much of it as a fleet card needs: enough to recognize the
/// work and to link through to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    /// The positional handle (`t42`) — what `/tasks/:id` is addressed by.
    pub id: String,
    /// The task text, metadata stripped.
    pub summary: String,
    /// The pipeline stage, when the task records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The SMART gate's verdict (`ok` / `fail` / `stuck`), when judged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<String>,
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
    stamp_tasks(cfg, &mut fleet.agents);
    fleet
}

/// Attach each agent's governing task, so the fleet can say what an agent is working on.
///
/// One read of the task store for the whole pass, not one per agent — the same budget
/// [`stamp_control`] accepts for its lease-registry read, and this runs on a 2-second dashboard
/// poll. An unreadable or empty store leaves every `task` as `None`, which is the honest answer and
/// renders as nothing.
fn stamp_tasks(cfg: &Config, agents: &mut [LiveAgent]) {
    if agents.is_empty() {
        return;
    }
    let Ok(file) = crate::todo::load(&cfg.tasks_file()) else {
        return;
    };
    let tasks: Vec<crate::todo::Task> = file.tasks().cloned().collect();
    let today = crate::util::today();
    for a in agents.iter_mut() {
        a.task = task_for(&tasks, &a.repo, &a.branch, &today).map(|t| AgentTask {
            id: t.id().unwrap_or_default().to_string(),
            summary: t.summary(),
            stage: t.meta("stage").map(str::to_string),
            smart: t.meta("smart").map(str::to_string),
        });
    }
}

/// The task governing `(repo, branch)`, or `None`.
///
/// Keyed exactly like [`crate::pipeline::gate::governing_task`] and the pipeline-stages API —
/// case-insensitive repo, case-insensitive branch — so the three joins cannot drift apart and
/// disagree about which task owns a worktree. An open task wins over a finished one: an agent that
/// has just closed its task is still *on* it, but if it has moved on to a second task on the same
/// branch, that is the one to show. Pure → unit-testable.
fn task_for<'a>(
    tasks: &'a [crate::todo::Task],
    repo: &str,
    branch: &str,
    today: &str,
) -> Option<&'a crate::todo::Task> {
    let matches = |t: &&crate::todo::Task| {
        t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(repo))
            && t.meta("branch")
                .is_some_and(|b| b.eq_ignore_ascii_case(branch))
    };
    tasks
        .iter()
        .filter(matches)
        .max_by_key(|t| (t.is_open(today), t.modified()))
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
                task: None,
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
        task: None,
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
        task: None,
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
