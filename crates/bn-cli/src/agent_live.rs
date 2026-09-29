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

use bn_core::agent::{AgentHealth, Fleet};
use bn_core::render::Envelope;
use bn_core::resolver::Config;
use bn_core::runner::{CommandRunner, RealRunner};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

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
    /// Context-window use, 0–100, when measured (Claude only; never guessed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_pct: Option<u8>,
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
                ctx_pct: a.context.as_ref().and_then(|c| c.pct),
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
                        ctx_pct: None,
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
/// behind `bn agent live` and internal agent monitoring.
pub fn collect(cfg: &Config, idle: i64, stuck: i64) -> Vec<LiveView> {
    let fleet = bn_core::agent::supervise(cfg, idle, stuck);
    let mut rows = join(&read_windows(), &fleet);
    // Plane C: when Plane A didn't flag it, glance at the live pane. A windowed agent sitting at a
    // bare prompt with no spinner is blocked on input — infer `waiting` so `board_rank` and the
    // mark/label tables surface it as needs-you. This is the signal Copilot never fires when it
    // pauses for input (e.g. a "Shall I proceed?" prompt). A pane showing the usage-limit dialog
    // infers `limited` for agent monitoring. Only touch rows whose
    // Plane-A state is inferable (empty / stale `working` / `waiting`, which the limit dialog can
    // race); a real `done`/`blocked` is authoritative.
    for r in &mut rows {
        if r.has_window && matches!(r.state.as_str(), "" | "working" | "waiting") {
            let before = r.state.clone();
            r.state = classify_lifecycle(
                &r.state,
                &capture_tail(&r.target, TAIL_SCAN_LINES),
                r.health,
            )
            .state;
            // Persist what we just worked out. Until now this inference lived and died inside a
            // single `bn agent live` / `bn watch` pass: the classifier could tell that an agent was
            // blocked, or had hit a usage cap, and the tab bar, jump picker and attention badge —
            // every cheap reader of `@bn_agent` — never learned it, because nothing wrote it back.
            // That is why `blocked` and `needs-decision` were dead vocabulary on BOTH sides.
            //
            // Only on an actual change, so a steady fleet costs no tmux calls, and only from here:
            // `collect` already runs off the 5s path (`bn watch` is the *:0/5 cron), whereas the
            // status bar must never pay for a capture-pane sweep.
            if r.state != before {
                crate::cmd::setup::set_tmux_window_option_on(
                    Some(&r.target),
                    "@bn_agent",
                    &r.state,
                );
            }
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
pub(crate) const TAIL_SCAN_LINES: usize = 6;

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

    pub(crate) fn verify_delays_ms(self) -> &'static [u64] {
        self.verify_delays_ms
    }
}

pub(crate) const SUBMIT_BACKOFF: SubmitBackoff = SubmitBackoff {
    submit_attempts: 3,
    verify_delays_ms: &[100, 200, 400, 800],
};

/// Decide from a pane tail whether a just-submitted steer was accepted: a live spinner means the
/// turn started, and a bare empty Claude Code prompt means the box cleared after submission. A
/// spinner-less pane still showing typed text means the harness's compose/autocomplete TUI ate the
/// submit key. Pure over the tail → testable.
pub(crate) fn tail_confirms_submit(tail: &[String]) -> bool {
    tail_has_spinner(tail) || tail_has_bare_prompt(tail)
}

/// Verify a `bn agent send --keys` submit landed in pane `target`: after the carriage return, poll
/// the pane for a working spinner (the turn started). Returns true as soon as one appears, else
/// false after [`SUBMIT_BACKOFF`]'s verification window — the signal that the TUI swallowed the
/// Enter and the message is sitting unsent in the box (#306), so the caller can retry or escalate
/// to `--resume`.
#[allow(dead_code)]
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
            let ctx = r.ctx_pct.map(|p| format!("  ctx {p}%")).unwrap_or_default();
            println!("  {mark}  {:<24} {:<7}{tok}{ctx}{win}", r.target, harness);
        }
    }
    if !remote.is_empty() {
        println!("{} remote run(s) (gh):", remote.len());
        for v in &remote {
            println!("{}", crate::cmd::remote_track::render_line(v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_claude_prompt_confirms_submit() {
        let tail = vec![
            "some prior output".to_string(),
            "\u{276f}\u{00a0}".to_string(),
            "  auto mode on (shift+tab to cycle) · esc to interrupt · ← for agents".to_string(),
        ];
        assert!(tail_confirms_submit(&tail));
    }

    #[test]
    fn text_still_in_box_does_not_confirm_submit() {
        let tail = vec![
            "some prior output".to_string(),
            "\u{276f} Please keep typing".to_string(),
            "  auto mode on (shift+tab to cycle) · ← for agents".to_string(),
        ];
        assert!(!tail_confirms_submit(&tail));
    }
}
