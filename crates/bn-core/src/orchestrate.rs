//! `bn orchestrate` — the native, event-driven fleet supervisor (issues #338 / #340).
//!
//! This module is the **pure core**: the agent-pane state machine (#340) and the persisted queue
//! model (#338). It has zero side effects — no tmux, no git, no launchd — so every classification
//! and scheduling decision is a unit-testable function. The CLI layer (`bn-cli/src/cmd/orchestrate`)
//! captures the live pane / git / PR signals, feeds them in here, and *realizes* the actions this
//! module decides. Keeping the policy pure is the whole point of #340: the pane-state → action
//! mapping lives in code and is unit-tested, instead of being re-authored per wave as an ad-hoc
//! natural-language scheduled prompt.
//!
//! Two halves:
//!   - [`PaneState`] + [`classify`] + [`SupervisorAction`] + [`action_for`] — the state machine.
//!   - [`OrchestrateState`] + [`QueueItem`] — the persisted, drain-until-empty queue.

use crate::backlog::{BacklogKind, ExecutionTarget, Substrate};
use crate::hosts::NodeRecord;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ===========================================================================
// The agent-pane state machine (#340)
// ===========================================================================

/// The classified state of one live agent pane — the #340 state set. Each maps to exactly one
/// deterministic [`SupervisorAction`] via [`action_for`], so a supervision pass never re-derives
/// pane-string heuristics in prose. Serialized kebab-case for the `bn orchestrate status` surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaneState {
    /// A live spinner / interrupt hint — the agent is mid-turn. Leave it alone.
    Working,
    /// A bare idle prompt with **no** uncommitted or unpushed work — finished-and-clean, or a
    /// just-launched agent that hasn't started. Nothing for the orchestrator to recover.
    IdleClean,
    /// A bare idle prompt but the worktree carries WIP (dirty tree or commits ahead of base) — the
    /// agent died / stalled with unpushed work. The recovery case that was hand-driven all session.
    IdleWithWip,
    /// The harness's "do you trust the files in this folder?" launch prompt is up, blocking boot.
    TrustMenu,
    /// The harness is asking whether it may run a command/tool, with an explicit session-scoped
    /// allow option. Trusted ship lanes may clear this without guessing an agent decision.
    CommandApproval,
    /// A blocking choice prompt ("Shall I proceed?" / a numbered menu / a `[y/n]`) awaiting a pick.
    QuestionMenu,
    /// The pane shows a harness/API error (5xx, "API Error", request failed) and is not retrying.
    ApiError,
    /// The pane shows a network/DNS failure (ENOTFOUND, getaddrinfo, connection refused/timeout).
    DnsOutage,
    /// The harness hit its usage/rate limit (its "wait for limit to reset / upgrade your plan"
    /// prompt is up). No relaunch can clear it — the fix is a harness switch (#382), so the
    /// daemon falls this lane over to the other harness in place.
    Limited,
    /// The agent signalled completion (Plane A `@bn_agent=done`) — harvest + land + reap.
    Done,
    /// No live tmux window for this agent — the process is gone. Recover any WIP, else reap.
    Dead,
}

impl PaneState {
    /// The kebab-case label used in status output and event logs.
    pub fn label(self) -> &'static str {
        match self {
            PaneState::Working => "working",
            PaneState::IdleClean => "idle-clean",
            PaneState::IdleWithWip => "idle-with-wip",
            PaneState::TrustMenu => "trust-menu",
            PaneState::CommandApproval => "command-approval",
            PaneState::QuestionMenu => "question-menu",
            PaneState::ApiError => "api-error",
            PaneState::DnsOutage => "dns-outage",
            PaneState::Limited => "limited",
            PaneState::Done => "done",
            PaneState::Dead => "dead",
        }
    }

    /// Whether this state is an outage the supervisor should retry with backoff (api / dns). Used
    /// to decide when to consult the [`Backoff`] curve vs. act immediately.
    pub fn is_outage(self) -> bool {
        matches!(self, PaneState::ApiError | PaneState::DnsOutage)
    }
}

/// The deterministic action a supervision pass takes for a [`PaneState`]. The whole #340 mapping is
/// [`action_for`]; the CLI layer realizes each variant against the live fleet (tmux send-keys, the
/// merge-queue, `bn agent recover`, `bn dispatch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupervisorAction {
    /// Do nothing — the agent is making progress (or idle-clean with nothing to recover).
    Leave,
    /// Harvest the agent's result, gate + land its PR via the merge-queue, then reap the worktree.
    Land,
    /// Clear the trust-folder menu (send Down+Enter to pick "trust"). Rare when dispatch pre-trusts.
    AnswerTrustMenu,
    /// Handle a harness command approval according to the lane's trust policy.
    AnswerCommandApproval,
    /// Answer a blocking question menu (surface for a decision, or auto-answer the default).
    AnswerQuestion,
    /// Relaunch the agent to resume its work (recover unpushed WIP first), with exponential backoff.
    RelaunchToResume,
    /// Fall a usage-limited agent over to the other harness in place (`bn switch --in-place`) so the
    /// work continues instead of waiting for the limit to reset — the #382 bias+fallback.
    SwitchHarness,
}

impl SupervisorAction {
    /// The kebab-case label used in status output and event logs.
    pub fn label(self) -> &'static str {
        match self {
            SupervisorAction::Leave => "leave",
            SupervisorAction::Land => "land",
            SupervisorAction::AnswerTrustMenu => "answer-trust-menu",
            SupervisorAction::AnswerCommandApproval => "answer-command-approval",
            SupervisorAction::AnswerQuestion => "answer-question",
            SupervisorAction::RelaunchToResume => "relaunch-to-resume",
            SupervisorAction::SwitchHarness => "switch-harness",
        }
    }
}

/// The **total** #340 pane-state → action mapping. Deterministic and exhaustive — the single source
/// of truth for "given this state, what does the supervisor do?", replacing the per-wave prose.
pub fn action_for(state: PaneState) -> SupervisorAction {
    match state {
        PaneState::Working => SupervisorAction::Leave,
        PaneState::IdleClean => SupervisorAction::Leave,
        PaneState::IdleWithWip => SupervisorAction::RelaunchToResume,
        PaneState::TrustMenu => SupervisorAction::AnswerTrustMenu,
        PaneState::CommandApproval => SupervisorAction::AnswerCommandApproval,
        PaneState::QuestionMenu => SupervisorAction::AnswerQuestion,
        PaneState::ApiError => SupervisorAction::RelaunchToResume,
        PaneState::DnsOutage => SupervisorAction::RelaunchToResume,
        PaneState::Limited => SupervisorAction::SwitchHarness,
        PaneState::Done => SupervisorAction::Land,
        PaneState::Dead => SupervisorAction::RelaunchToResume,
    }
}

/// The harness the orchestrate daemon dispatches under when nothing else is configured — the
/// back-compat default for a run whose state predates `bn orchestrate --agent` (#382).
pub const DEFAULT_HARNESS: &str = "copilot";

/// The other harness to fall a usage-limited lane over to (#382): `claude` ↔ `copilot`. An
/// unknown/unset harness falls back to the safe [`DEFAULT_HARNESS`]. Pure — the fallback-selection
/// policy, so the daemon's auto-switch target is unit-testable without any live fleet.
pub fn other_harness(current: &str) -> &'static str {
    match current {
        "copilot" => "claude",
        "claude" => "copilot",
        _ => DEFAULT_HARNESS,
    }
}

/// [`classify`] so the classifier needs no tmux/git — a `capture-pane` tail, the window's Plane-A
/// `@bn_agent` option, whether a live window exists, and whether the worktree has WIP.
#[derive(Debug, Clone)]
pub struct PaneObservation<'a> {
    /// The recent non-empty lines of the agent's pane (already trimmed), oldest→newest.
    pub tail: &'a [String],
    /// Whether a live tmux window exists for this agent. `false` ⇒ [`PaneState::Dead`].
    pub has_window: bool,
    /// The window's `@bn_agent` Plane-A state (`working` | `waiting` | `done` | `""`). Authoritative
    /// for `done`; a hint otherwise.
    pub plane_a: &'a str,
    /// Whether the worktree carries unpushed/uncommitted work (dirty tree or commits ahead of base).
    pub has_wip: bool,
}

/// Spinner / interrupt markers a harness prints only while a turn is in flight: Claude's
/// `esc to interrupt`, Copilot's `esc to cancel`. A `Working…` spinner word is matched only in its
/// animated form (trailing ellipsis) so an agent merely *saying* "working on the fix" at an idle
/// prompt never reads as busy — the sharper cousin of `bn agent live`'s spinner heuristic.
fn is_spinner_line(lower: &str) -> bool {
    lower.contains("esc to interrupt")
        || lower.contains("esc to cancel")
        || lower.contains("esc cancel")
        || lower.contains("esc interrupt")
        || lower.contains("working…")
        || lower.contains("working...")
        || lower.contains("● working")
        || lower.contains("· working")
}

/// Network/DNS-outage markers: name-resolution and connection failures. A pane wedged on any of
/// these can't reach the API — relaunching once the network returns is the fix (with backoff).
const DNS_OUTAGE_MARKERS: &[&str] = &[
    "enotfound",
    "getaddrinfo",
    "eai_again",
    "could not resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "no address associated with hostname",
    "network is unreachable",
    "no route to host",
    "connection refused",
    "econnrefused",
    "connection reset",
    "econnreset",
    "connection timed out",
    "etimedout",
];

/// API/server-error markers: the harness reached the network but the API returned an error (5xx,
/// overloaded, request failed). Distinct from a DNS outage so status/observability can tell a
/// network drop from a server-side wobble; both relaunch-to-resume with backoff.
const API_ERROR_MARKERS: &[&str] = &[
    "api error",
    "api request failed",
    "request failed with status",
    "internal server error",
    "500 internal",
    "502 bad gateway",
    "bad gateway",
    "503 service unavailable",
    "service unavailable",
    "504 gateway timeout",
    "gateway timeout",
    "overloaded_error",
    "overloaded",
    "upstream error",
];

/// Usage/rate-limit markers: the harness reached its plan/session cap and put up its blocking limit
/// prompt. Distinct from an outage because no relaunch can clear it — the fix is a harness switch
/// (#382). Matched case-insensitively; checked *after* the spinner so an agent merely *mentioning* a
/// limit mid-turn (spinner still up) never reads as limited. Mirrors `bn agent live`'s detection.
const USAGE_LIMIT_MARKERS: &[&str] = &[
    "wait for limit to reset",
    "upgrade your plan",
    "usage limit reached",
    "reached your usage limit",
    "hit your session limit",
    "session limit reached",
];

/// Trust-menu launch markers — both the harness's "trust the files in this folder?" boot prompt
/// and Claude's "N new MCP servers found · Select any to trust" approval menu (#545). Dispatch can
/// pre-trust the folder (`bn trust-folder` under `--yolo`) so the folder prompt rarely fires, but a
/// non-yolo web dispatch parks on one of these until answered; the supervisor clears it
/// deterministically instead of a human sending keystrokes, and the fleet surfaces it as needs-you.
const TRUST_MENU_MARKERS: &[&str] = &[
    "do you trust the files",
    "do you trust the authors",
    "trust the files in this folder",
    "do you want to trust",
    "trust this folder",
    "new mcp server",
    "new mcp servers",
    "select any to trust",
];

/// Harness-owned command/tool approval markers. These are deliberately narrower than generic
/// numbered menus: only prompts that explicitly identify a command/tool permission and offer a
/// session-scoped allow choice classify as [`PaneState::CommandApproval`].
const COMMAND_APPROVAL_MARKERS: &[&str] = &[
    "allow command:",
    "do you want to run this command",
    "would you like to run this command",
    "do you want to use this tool",
    "would you like to use this tool",
];

const SESSION_ALLOW_MARKERS: &[&str] = &[
    "don't ask again this session",
    "do not ask again this session",
    "always allow for this session",
    "allow for this session",
];

/// Explicit "blocking question" phrases — a pane sitting on one of these is waiting on a decision.
/// A numbered-choice menu is detected separately by [`is_numbered_menu`].
const QUESTION_MENU_MARKERS: &[&str] = &[
    "shall i proceed",
    "do you want me to proceed",
    "would you like me to proceed",
    "do you want to proceed",
    "do you want to continue",
    "proceed? (y/n)",
    "continue? (y/n)",
    "(y/n)",
    "[y/n]",
    "select an option",
    "choose an option",
];

/// Bare idle-prompt glyphs: Copilot's empty box (`┃`), Claude's `❯`, and generic `>` / `│`. A line
/// that trims to exactly one of these is an empty input box awaiting you — nothing typed.
const BARE_PROMPT_GLYPHS: &[&str] = &["┃", "❯", ">", "│"];

fn any_marker(tail: &[String], markers: &[&str]) -> bool {
    tail.iter().any(|l| {
        let lower = l.to_ascii_lowercase();
        markers.iter().any(|m| lower.contains(m))
    })
}

/// Does the tail show a numbered selection menu — at least two `1.` / `2)` / `❯ 1.` option lines?
/// The generic shape of a harness's "pick one" prompt, so a menu with non-canonical wording still
/// classifies as [`PaneState::QuestionMenu`] rather than falling through to idle.
fn is_numbered_menu(tail: &[String]) -> bool {
    tail.iter().filter(|l| menu_option(l).is_some()).count() >= 2
}

fn menu_option(line: &str) -> Option<u8> {
    let t = line
        .trim_start_matches(['❯', '>', '●', '○', ' '])
        .trim_start();
    let mut chars = t.chars();
    let digit = chars.next()?.to_digit(10)? as u8;
    if !matches!(chars.next(), Some('.') | Some(')')) || t.len() <= 2 || t[2..].trim().is_empty() {
        return None;
    }
    Some(digit)
}

/// The numbered session-scoped allow choice in the active trailing command-approval menu.
/// Restricting the match to the final contiguous option block prevents stale approval scrollback
/// from turning a newer arbitrary question into an auto-answer target.
pub fn command_approval_option(tail: &[String]) -> Option<u8> {
    let last_option = tail.iter().rposition(|line| menu_option(line).is_some())?;
    if last_option + 1 != tail.len() {
        return None;
    }
    let mut menu_start = last_option;
    while menu_start > 0 && menu_option(&tail[menu_start - 1]).is_some() {
        menu_start -= 1;
    }

    let heading_start = tail[..menu_start]
        .iter()
        .rposition(|line| menu_option(line).is_some())
        .map_or(0, |idx| idx + 1);
    if !any_marker(&tail[heading_start..menu_start], COMMAND_APPROVAL_MARKERS) {
        return None;
    }

    tail[menu_start..=last_option].iter().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        SESSION_ALLOW_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
            .then(|| menu_option(line))
            .flatten()
    })
}

fn has_bare_prompt(tail: &[String]) -> bool {
    tail.iter().any(|l| BARE_PROMPT_GLYPHS.contains(&l.trim()))
}

/// Classify an agent pane into a [`PaneState`] — the pure #340 classifier. Priority is deliberate
/// and deterministic:
///   1. no window ⇒ `Dead` (structural — nothing else can be true).
///   2. Plane A `done` ⇒ `Done` (the harness's own lifecycle signal is authoritative).
///   3. a live spinner ⇒ `Working` (busy beats everything below — an error or menu merely *mentioned*
///      mid-turn, with the spinner still up, must not trigger a relaunch/keystroke).
///   4. trust menu ⇒ `TrustMenu`.
///   5. command approval ⇒ `CommandApproval`.
///   6. network/DNS failure ⇒ `DnsOutage` (checked before API so a resolve failure isn't mislabeled).
///   7. API/server error ⇒ `ApiError`.
///   8. usage/rate limit ⇒ `Limited` (before the menu check — its dialog is a numbered menu that
///      would otherwise read as `QuestionMenu`; the daemon switches it rather than surface it).
///   9. a question / numbered menu ⇒ `QuestionMenu`.
///   10. a bare idle prompt ⇒ `IdleWithWip` if the worktree has WIP, else `IdleClean`.
///   11. fallback: Plane A `working` ⇒ `Working`; otherwise WIP decides idle-with-wip vs idle-clean.
///
/// Note: a `Limited` pane maps to a harness switch (#382), not a relaunch — relaunching can't clear
/// a usage limit. Before #382 this state was folded into `QuestionMenu` and merely surfaced; now the
/// daemon falls it over to the other harness automatically.
pub fn classify(obs: &PaneObservation) -> PaneState {
    if !obs.has_window {
        return PaneState::Dead;
    }
    if obs.plane_a == "done" {
        return PaneState::Done;
    }
    let has_spinner = obs
        .tail
        .iter()
        .any(|l| is_spinner_line(&l.to_ascii_lowercase()));
    if has_spinner {
        return PaneState::Working;
    }
    if any_marker(obs.tail, TRUST_MENU_MARKERS) {
        return PaneState::TrustMenu;
    }
    if command_approval_option(obs.tail).is_some() {
        return PaneState::CommandApproval;
    }
    if any_marker(obs.tail, DNS_OUTAGE_MARKERS) {
        return PaneState::DnsOutage;
    }
    if any_marker(obs.tail, API_ERROR_MARKERS) {
        return PaneState::ApiError;
    }
    if any_marker(obs.tail, USAGE_LIMIT_MARKERS) {
        return PaneState::Limited;
    }
    if any_marker(obs.tail, QUESTION_MENU_MARKERS) || is_numbered_menu(obs.tail) {
        return PaneState::QuestionMenu;
    }
    if has_bare_prompt(obs.tail) {
        return if obs.has_wip {
            PaneState::IdleWithWip
        } else {
            PaneState::IdleClean
        };
    }
    // Ambiguous pane (spinner scrolled off, no prompt captured): trust Plane A when it says working,
    // else let WIP decide whether there's anything to recover.
    if obs.plane_a == "working" {
        PaneState::Working
    } else if obs.has_wip {
        PaneState::IdleWithWip
    } else {
        PaneState::IdleClean
    }
}

/// Classify a pane and resolve its action in one step — the supervisor's per-agent decision.
pub fn decide(obs: &PaneObservation) -> (PaneState, SupervisorAction) {
    let state = classify(obs);
    (state, action_for(state))
}

// ===========================================================================
// Idle-clean liveness refinement (#374)
// ===========================================================================

/// Progress / liveness signals for an in-flight agent sitting at an **idle-clean** prompt (#374).
/// Gathered by the CLI from the harness session logs (cumulative AIC, #309), gh (PR state), and the
/// session's last-activity clock. This is the input that tells three look-alike agents apart — a
/// bare idle prompt with a clean tree is identical whether the agent **just launched** (nothing done
/// yet), **finished** (its PR is up, awaiting the merge-queue), or **stalled** (worked, then died at
/// an empty prompt with nothing committed and no PR). Without it, [`classify`] can only call them all
/// `IdleClean` → `Leave`, so a stalled agent hogs its concurrency slot forever and starves the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Liveness {
    /// Cumulative agent output tokens burned so far (AIC, #309). `0` ⇒ the agent produced no turn —
    /// it hasn't started, so an idle-clean prompt means *just-launched*, never *stalled*.
    pub aic: u64,
    /// The item's branch already carries an **open or merged** PR — the work is delivered and landing
    /// is the merge-queue's job. A finished agent is never relaunched (that would re-do landed work).
    pub has_pr: bool,
    /// Seconds since the agent's last sign of life (its newest harness-log event). A large value at a
    /// bare idle-clean prompt means the agent stopped and won't resume on its own — the stall signal.
    pub idle_secs: i64,
}

/// Whether an agent's pane tail positively shows a **bare idle prompt** — an empty input box (`❯`,
/// `┃`, `>`, `│`) with nothing typed. This distinguishes an `IdleClean` that came from a *real* idle
/// prompt from one that came from [`classify`]'s ambiguous fallback (no prompt **and** no spinner
/// captured). The #374 stall recovery gates on it: a fallback-idle pane might be a working agent
/// mid-long-turn whose spinner scrolled off, so only a positively-observed idle prompt is ever
/// eligible to be judged *stalled* — the ambiguous case is always left alone.
pub fn shows_bare_prompt(obs: &PaneObservation) -> bool {
    has_bare_prompt(obs.tail)
}

/// Refine an [`PaneState::IdleClean`] pane into its true action (#374) — the liveness half of the
/// state machine that [`action_for`] can't decide from the pane alone. The bare idle prompt with a
/// clean tree is ambiguous, so given the gathered [`Liveness`], resolve which of the three it is:
///   - branch has a PR ⇒ **finished** ⇒ `Leave` (the merge-queue lands it; reconcile marks it done),
///   - no tokens burned (`aic == 0`) ⇒ **just-launched** / never-started ⇒ `Leave` (give it room),
///   - worked (`aic > 0`), no PR, idle past `stale_secs` ⇒ **stalled** ⇒ `RelaunchToResume` — recover
///     it (a hard restart re-seeds a fresh session, #383) so its slot stops starving the queue (#374),
///   - otherwise (worked, no PR, but not yet stale) ⇒ `Leave` — a brief between-turns idle or an
///     agent about to open its PR / run `bn agent done` is not cut off before the dwell elapses.
///
/// Pure + unit-tested: the single source of truth for "is this idle-clean agent actually stalled?".
pub fn idle_clean_action(live: Liveness, stale_secs: i64) -> SupervisorAction {
    if live.has_pr || live.aic == 0 {
        return SupervisorAction::Leave;
    }
    if live.idle_secs >= stale_secs {
        SupervisorAction::RelaunchToResume
    } else {
        SupervisorAction::Leave
    }
}

// ===========================================================================
// Exponential backoff for relaunch-to-resume
// ===========================================================================

/// Exponential backoff for relaunch-to-resume attempts. A wedged agent (api-error / dns-outage /
/// idle-with-WIP / dead) is relaunched after `delay(attempt)` seconds, doubling each retry up to a
/// cap — so a flapping outage doesn't hot-loop the fleet. Pure and saturating (never overflows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The first retry delay, in seconds.
    pub base_secs: u64,
    /// The multiplier applied per attempt.
    pub factor: u64,
    /// The delay cap, in seconds — the longest the supervisor ever waits between relaunches.
    pub max_secs: u64,
    /// The attempt count after which the supervisor gives up relaunching (marks the item failed).
    pub max_attempts: u32,
    /// Relaunch attempts on a **wedged** pane (api-error / dns-outage) after which a soft resume is
    /// abandoned for a **hard restart** (kill the process + respawn a fresh harness). A hard-wedged
    /// process — e.g. a cached `ENOTFOUND` after a DNS outage — never clears via send-keys, so once
    /// this many soft resumes haven't recovered it, only a fresh process will (#370 defect 1).
    pub hard_restart_after: u32,
}

impl Default for Backoff {
    /// 30s → 1m → 2m → 4m … capped at 30m, giving up after 6 attempts (~1h of retries). A wedged
    /// outage escalates from soft resume to a hard restart after 2 soft attempts.
    fn default() -> Self {
        Backoff {
            base_secs: 30,
            factor: 2,
            max_secs: 1800,
            max_attempts: 6,
            hard_restart_after: 2,
        }
    }
}

impl Backoff {
    /// The delay before the `attempt`-th relaunch (0-based: `delay(0)` is the first retry). Doubles
    /// per attempt from `base_secs`, saturating at `max_secs`.
    pub fn delay(&self, attempt: u32) -> u64 {
        let mut d = self.base_secs;
        for _ in 0..attempt {
            d = d.saturating_mul(self.factor);
            if d >= self.max_secs {
                return self.max_secs;
            }
        }
        d.min(self.max_secs)
    }

    /// Whether a further relaunch is allowed given the attempts made so far. Past the cap, the
    /// supervisor stops flapping and marks the item failed for a human.
    pub fn should_retry(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }
}

/// How a relaunch is realized against the live fleet — the #370-1 distinction the CLI must honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelaunchKind {
    /// Nudge a still-alive pane to resume (send-keys a resume message). Works when the agent is
    /// merely idle/waiting — the process is healthy, it just stopped.
    Soft,
    /// Kill the pane's process and respawn a **fresh** harness (tmux respawn/kill + re-dispatch).
    /// The only thing that clears a hard-wedged process — a cached `ENOTFOUND` after a DNS outage,
    /// a dead window — which a soft send-keys can't touch.
    Hard,
}

impl RelaunchKind {
    pub fn label(self) -> &'static str {
        match self {
            RelaunchKind::Soft => "soft-resume",
            RelaunchKind::Hard => "hard-restart",
        }
    }
}

/// Choose how to realize a relaunch (#370 defect 1). A dead pane (no window) can't be nudged — a
/// fresh dispatch *is* the hard restart. A **stalled idle-clean** pane (#374) hard-restarts straight
/// away: soft-resuming its idle session just restores the same dead prompt (#383). A live but
/// **wedged** pane (api-error / dns-outage) that `hard_restart_after` soft resumes haven't cleared is
/// hard-wedged (cached resolver failure) and needs a fresh process. Everything else (an idle agent
/// with WIP — alive, healthy, just stopped) resumes softly. Pure.
pub fn relaunch_kind(
    state: PaneState,
    attempts: u32,
    has_window: bool,
    policy: &Backoff,
) -> RelaunchKind {
    if !has_window {
        return RelaunchKind::Hard;
    }
    // A stalled idle-clean agent (#374) only reached relaunch because it worked then died at an empty
    // prompt: its harness session is idle/useless, and a soft resume just re-wakes that same dead
    // session (comes back idle at `❯` having done nothing — #383). Only a fresh, re-seeded process
    // resumes the *work*, so skip straight to a hard restart.
    if matches!(state, PaneState::IdleClean) {
        return RelaunchKind::Hard;
    }
    let wedged = matches!(
        state,
        PaneState::ApiError | PaneState::DnsOutage | PaneState::Dead
    );
    if wedged && attempts >= policy.hard_restart_after {
        RelaunchKind::Hard
    } else {
        RelaunchKind::Soft
    }
}

/// The decision a supervision pass reaches for a relaunch-to-resume item — the whole #370 relaunch
/// policy (cap → give up, backoff window → wait, else relaunch soft/hard) in one pure value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchDecision {
    /// The relaunch cap is hit — give up and mark the item failed / needs-attention (#370 defect 4).
    GiveUp,
    /// The backoff window since the last relaunch hasn't elapsed — do nothing this pass.
    Wait,
    /// Relaunch now, realized via this [`RelaunchKind`].
    Relaunch(RelaunchKind),
}

/// Resolve the full relaunch policy for an item, given its wedged pane `state`, `attempts` so far,
/// the epoch secs of its `last_relaunch_at` (0 = never), `now`, whether a live window still exists,
/// and the `policy`. Deterministic and pure — the single unit-tested source of truth so the CLI
/// executor never re-derives "cap? / backoff? / soft or hard?" in prose (the whole point of #340).
///   1. past the attempt cap ⇒ `GiveUp` (surface a failed item instead of looping unbounded).
///   2. inside the backoff window since the last relaunch ⇒ `Wait`.
///   3. otherwise ⇒ `Relaunch` via [`relaunch_kind`] (soft resume, escalating to hard restart).
pub fn plan_relaunch(
    state: PaneState,
    attempts: u32,
    last_relaunch_at: i64,
    now: i64,
    has_window: bool,
    policy: &Backoff,
) -> RelaunchDecision {
    if !policy.should_retry(attempts) {
        return RelaunchDecision::GiveUp;
    }
    if last_relaunch_at != 0 && now - last_relaunch_at < policy.delay(attempts) as i64 {
        return RelaunchDecision::Wait;
    }
    RelaunchDecision::Relaunch(relaunch_kind(state, attempts, has_window, policy))
}

/// External completion signals for an in-flight item, gathered by the CLI for reconciliation
/// (#370 defect 2). The daemon must mark an item **done** when its work has landed *outside* its
/// own PR tracking — a merged PR on the branch, or a closed linked issue — not only when the daemon
/// opened the PR itself. Pure so "is this item actually already complete?" is one unit-tested
/// predicate, keyed on the branch/issue's real state rather than a daemon-held PR handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExternalState {
    /// The item's branch has a **merged** pull request (opened by anyone).
    pub branch_pr_merged: bool,
    /// The item's linked issue is **closed**.
    pub issue_closed: bool,
    /// The daemon's own worktree for the branch has vanished (the merge-queue reaped it) — the
    /// pre-existing "it merged" signal, folded in so all completion logic lives in one predicate.
    pub worktree_gone: bool,
}

impl ExternalState {
    /// Whether the item is already complete and should be reconciled to a terminal `merged`, even if
    /// the daemon never opened its PR. Any one signal suffices — the fix for the infinite-relaunch
    /// loop on an externally-finished item (#370 defect 2).
    pub fn is_resolved(self) -> bool {
        self.branch_pr_merged || self.issue_closed || self.worktree_gone
    }

    /// The signal that resolved the item (for the recovery note / event log), or `None` if unresolved.
    /// Prefers the most specific evidence: a merged PR, then a closed issue, then a reaped worktree.
    pub fn via(self) -> Option<&'static str> {
        if self.branch_pr_merged {
            Some("branch-merged-pr")
        } else if self.issue_closed {
            Some("issue-closed")
        } else if self.worktree_gone {
            Some("worktree-reaped")
        } else {
            None
        }
    }
}

// ===========================================================================
// The persisted, drain-until-empty queue (#338)
// ===========================================================================

/// The lifecycle status of one queued work item as it flows dispatch → land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemStatus {
    /// Not yet dispatched — waiting for a concurrency slot.
    #[default]
    Queued,
    /// `bn dispatch` fired: a worktree + agent exist and are in flight (counts against the cap).
    Dispatched,
    /// The PR landed via the gated merge-queue — terminal success.
    Merged,
    /// Gave up (relaunch cap hit, or repeated gate failure) — terminal, surfaced for a human.
    Failed,
}

impl ItemStatus {
    pub fn label(self) -> &'static str {
        match self {
            ItemStatus::Queued => "queued",
            ItemStatus::Dispatched => "dispatched",
            ItemStatus::Merged => "merged",
            ItemStatus::Failed => "failed",
        }
    }
    /// Whether this item occupies an in-flight concurrency slot.
    pub fn in_flight(self) -> bool {
        matches!(self, ItemStatus::Dispatched)
    }
    /// Whether this item has reached a terminal state (merged or failed).
    pub fn is_terminal(self) -> bool {
        matches!(self, ItemStatus::Merged | ItemStatus::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneReset {
    pub id: String,
    pub item: String,
    pub repo: String,
    pub branch: String,
    pub harness: Option<String>,
    pub work_class: WorkClass,
    pub previous_status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneResetError {
    Missing(String),
    Ambiguous(String),
    Queued(String),
    Merged(String),
    NonLocal(String),
    MissingBranch(String),
}

impl std::fmt::Display for LaneResetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(lane) => write!(f, "lane '{lane}' was not found by item id or branch"),
            Self::Ambiguous(lane) => write!(
                f,
                "lane '{lane}' matches more than one item; use its unique item id"
            ),
            Self::Queued(lane) => write!(f, "lane '{lane}' has not been dispatched yet"),
            Self::Merged(lane) => write!(f, "lane '{lane}' is already merged"),
            Self::NonLocal(lane) => write!(
                f,
                "lane '{lane}' is not a local tmux/worktree lane and cannot be reset here"
            ),
            Self::MissingBranch(lane) => {
                write!(f, "lane '{lane}' has no dispatched branch to re-seed")
            }
        }
    }
}

/// Durable dispatch transaction phase for one queue item (#712).
///
/// The lifecycle [`ItemStatus`] remains the operator-facing queue/dispatch/terminal state. This
/// finer-grained phase closes the crash window inside `Queued`: a claim is persisted before any
/// external launch, then promoted after the launch is observed. Missing fields in legacy state
/// default to `pending` and [`OrchestrateState::from_json`] backfills already-dispatched items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchPhase {
    /// No launch is reserved; the scheduler may select this item.
    #[default]
    Pending,
    /// Stable dispatch identity is durable, but launch completion is not yet recorded.
    Claimed,
    /// The launch was observed and the item is in its normal dispatched lifecycle.
    Dispatched,
}

impl DispatchPhase {
    fn is_pending(&self) -> bool {
        *self == DispatchPhase::Pending
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// The concurrency **lane** a queued item belongs to (#385) — the typed work-class the scheduler
/// caps independently. Heavy, PR-shipping coding agents (`ship`) and light, read-only research
/// agents (`scout`) scale on separate budgets (`--coding N --research M`), so a wave of scouts
/// never starves the coding lane and vice-versa. Derived from the leased
/// [`crate::backlog::BacklogKind`] ([`WorkClass::from_kind`]); defaults to [`WorkClass::Coding`]
/// (the `ship` default), so a pre-#385 run's state — and every `--queue` item, which has no kind —
/// reads as a coding lane. Serialized kebab-case for the status surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WorkClass {
    /// A heavy coding agent that opens and lands a PR (a `ship` backlog entry) — the default lane.
    #[default]
    Coding,
    /// A light, read-only research/scout agent that produces findings, not a merge (a `scout`
    /// backlog entry).
    Research,
}

impl WorkClass {
    /// The kebab-case label persisted on the run state + shown in status (`coding` / `research`).
    pub fn label(self) -> &'static str {
        match self {
            WorkClass::Coding => "coding",
            WorkClass::Research => "research",
        }
    }

    /// The lane a leased backlog entry belongs to: a `ship` change is a coding lane, a `scout`
    /// investigation is a research lane. The single mapping that turns the durable
    /// [`crate::backlog::BacklogKind`] into a schedulable work-class (#385).
    pub fn from_kind(kind: BacklogKind) -> WorkClass {
        match kind {
            BacklogKind::Ship => WorkClass::Coding,
            BacklogKind::Scout => WorkClass::Research,
        }
    }
}

/// One item in the orchestration queue: the dispatch subject plus the live lifecycle it accretes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    /// Stable id — the issue number when the item is an issue/PR (`#338` / a URL → `338`), else a
    /// slug of the text. Used to address the item across passes without positional drift.
    pub id: String,
    /// The raw dispatch subject as declared (`338`, an issue/PR URL, or free text).
    pub item: String,
    /// The repo this item dispatches into — the other half of its **`(repo, item)` key** (#453).
    /// Multi-repo orchestration keys the queue by `(repo, id)`, not `id` alone, so two repos'
    /// identically-named items (e.g. both issue `#338`, or two free-text items slugging alike)
    /// coexist in one run with no state/window/branch-slug collision. Defaults to the run's
    /// [`OrchestrateState::repo`]: every constructor sets it, and [`OrchestrateState::from_json`]
    /// backfills it on any pre-#453 state (items persisted without a `repo` key), so a single-repo
    /// run behaves exactly as before — the bare `id` still displays and addresses items; the repo
    /// namespace only disambiguates cross-repo collisions. See [`scoped_key`] / [`QueueItem::key`].
    #[serde(default)]
    pub repo: String,
    pub status: ItemStatus,
    /// Crash-safe dispatch transaction phase (#712). A claimed item still has lifecycle status
    /// `queued`, but is not schedulable and occupies its intended slot until reconciliation.
    #[serde(default, skip_serializing_if = "DispatchPhase::is_pending")]
    pub dispatch_phase: DispatchPhase,
    /// The dispatched branch, once `bn dispatch` created the worktree.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch: Option<String>,
    /// The PR url/number, once opened.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pr: Option<String>,
    /// Relaunch-to-resume attempts so far — the [`Backoff`] index for this item.
    #[serde(default)]
    pub attempts: u32,
    /// Epoch secs when the item was dispatched (0 while queued).
    #[serde(default)]
    pub dispatched_at: i64,
    /// Epoch secs of the last relaunch (0 if never), gating the backoff wait.
    #[serde(default)]
    pub last_relaunch_at: i64,
    /// Epoch secs of the last status/state change.
    #[serde(default)]
    pub updated_at: i64,
    /// The last pane state observed for this item, for `bn orchestrate status`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_state: Option<PaneState>,
    /// Stable fingerprint of the latest observed pane tail + Plane-A state. The supervisor uses
    /// this with the transcript clock to detect a `working` lane that has made no progress.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pane_activity_fingerprint: Option<u64>,
    /// Epoch seconds when [`QueueItem::pane_activity_fingerprint`] last changed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pane_activity_changed_at: i64,
    /// Current controller-actionable stuck state, shared by status and back-channel emission.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stuck: Option<crate::agent::StuckLane>,
    /// The stuck-state key successfully emitted to the owning controller. Kept distinct from
    /// `stuck` so an append failure retries on the next tick instead of silently losing the wake.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stuck_notified_key: Option<String>,
    /// The harness this item was dispatched under (`copilot` / `claude`), set once dispatched — so
    /// `bn orchestrate status` and the web board can show *what* is running in the window (#372).
    /// Flipped by the #382 usage-limit auto-fallback when the lane is switched to the other harness.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub harness: Option<String>,
    /// Epoch secs of the last usage-limit harness switch (#382), 0 if never. Gates the switch
    /// cooldown so a just-respawned harness gets time to boot before the lane could be flipped
    /// again — a lingering limit banner in the old pane buffer never causes a flip-flop.
    #[serde(default)]
    pub last_switch_at: i64,
    /// Path globs this item is expected to touch — the conflict-aware scheduling hint (#376),
    /// carried over from its durable [`crate::backlog::BacklogEntry::touches`] when the daemon
    /// leases it into the run. Empty = no declared hint, so it's schedulable alongside anything
    /// ([`conflicts`] treats an absent set as overlapping nothing); land-time `bn wt merge --all`
    /// remains the safety net. `["**"]` is the `--solo` degenerate case (overlaps everything).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
    /// Controller-agent that owns this lane, if it entered through a controller's durable backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// While this item is queued, the id of the in-flight item whose touch-area it overlaps —
    /// *why* it's held rather than dispatched (#376). Refreshed each dispatch pass by
    /// [`OrchestrateState::refresh_holds`] from the live [`conflicts`] policy, so `bn orchestrate
    /// status` and the web board show "held: conflicts with #NNN" without re-deriving, and a hold is
    /// logged once per episode. `None` = not held (or not queued). The authoritative scheduling
    /// decision itself is always live ([`next_dispatchable`](OrchestrateState::next_dispatchable)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
    /// Where this item runs (#377): carried from its durable [`crate::backlog::BacklogEntry::substrate`]
    /// when leased. `Local`/`Auto` dispatch into a local worktree + agent (counting against the
    /// resource-aware local concurrency cap); `Remote` fans out to a GitHub-hosted runner via
    /// `bn dispatch --remote` and occupies **no** local slot (supervised by PR/issue reconcile, not
    /// the tmux pane machine). `Auto` items may *also* fan out to remote when local is full and the
    /// item is [`crate::scheduler::remote_eligible`]. Defaults to `Auto` on a pre-#377 run's state.
    #[serde(default)]
    pub substrate: Substrate,
    /// **Which node this item executes on** (#717) — a *separate axis* from [`substrate`]: substrate
    /// chooses local-worktree vs. GitHub fan-out, `target` chooses *the node* a local/auto item
    /// lands on (the implicit local box or a chosen SSH worker). Carried from the leased backlog
    /// entry; the pure scheduler ([`crate::scheduler::pick`]) places a local/auto item on this
    /// target after a hard eligibility filter. Defaults to [`ExecutionTarget::Local`] on a pre-#717
    /// run's state, so the implicit-local-box behavior is preserved unchanged.
    #[serde(default, skip_serializing_if = "ExecutionTarget::is_local")]
    pub target: ExecutionTarget,
    /// Capability tags the placed node must advertise (#728), resolved from `[repo.<name>]` by the
    /// orchestrator before each scheduler pass. Persisted so status snapshots explain the policy
    /// used for a decision; empty preserves pre-affinity state files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    /// Capability tags that make a node ineligible for this repo (#728).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_capabilities: Vec<String>,
    /// Which typed concurrency lane this item is capped in (#385): `Coding` for a `ship` change,
    /// `Research` for a `scout` investigation, derived from the leased
    /// [`crate::backlog::BacklogKind`] via [`WorkClass::from_kind`]. The resource-aware scheduler
    /// counts in-flight items **per lane** against that lane's cap, so coding and research scale
    /// independently. Defaults to `Coding` on a pre-#385 run's state and for every `--queue` item
    /// (which carries no kind), preserving the single-pool behavior.
    #[serde(default)]
    pub work_class: WorkClass,
    /// Whether this local lane was launched by orchestrate as a trusted `--yolo` ship. Explicit and
    /// fail-closed: adopted standalone lanes and pre-#618 state default to false, even when coding.
    #[serde(default, skip_serializing_if = "is_false")]
    pub command_approvals_trusted: bool,
    /// The harvested answer for a scout/ask item (#378), set once its findings are routed back —
    /// see [`OrchestrateState::record_answer`]. A `ship` never carries one (it lands a PR, not an
    /// answer); a `scout`/`ask` produces findings, not a merge, so *this* — not a PR — is its
    /// deliverable. `bn orchestrate answers` / `ask --wait` read it straight off the state file the
    /// daemon already writes (no new IPC). `None` until harvested (or on any pre-#378 run's state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// A claimed dispatch is targeting a GitHub-hosted runner rather than a local/SSH slot. Kept
    /// separate from `substrate` so a failed auto-overflow claim can return to its original policy.
    #[serde(default, skip_serializing_if = "is_false")]
    pub claimed_remote: bool,
    /// **The remote worker placement** for an item dispatched onto an SSH node (#729): the
    /// `node_id` it was placed on, the `request_id`/`attempt_id` the remote `bn worker start` was
    /// admitted under (idempotent atomic admission, #720), and the last-observed worker attempt
    /// state polled over SSH (`QUEUED`/`RUNNING`/`SUCCEEDED`/`FAILED`, or `UNKNOWN` when the node is
    /// unreachable). Persisted on *both* sides — the control-plane state here mirrors the worker's
    /// own registry — so a reconnect/reconcile can line them up. All `None`/empty for a local
    /// dispatch (the implicit box needs no remote handle), keeping pre-#729 state files unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_state: Option<String>,
}

pub type QueueEntrySpec = (String, Vec<String>, Substrate, WorkClass, Option<String>);

/// The routed-back answer for a scout/ask item (#378) — the "delegate-for-answers" deliverable. A
/// scout runs the read-only `investigate` skill, writes findings to its branch note, and opens **no
/// PR**; when it reaches `Done` the daemon harvests those findings and records this on the run item
/// (a pure state field), so `bn orchestrate answers` and `ask --wait` surface the answer with zero
/// new IPC — they just read the state file the daemon already persists. `path` points at the
/// findings write-up in the item's (synced, worktree-independent) branch-note dir, so it stays
/// readable after the scout worktree is reaped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// The question that was asked — the scout item's dispatch subject, echoed back beside the
    /// findings so `answers` / `--wait` show *what* was answered without re-deriving it.
    pub question: String,
    /// The harvested findings file in the item's branch-note dir (a sibling write-up when the scout
    /// wrote one, else the note itself) — the link the notification and `answers` point a human at.
    pub path: String,
    /// Epoch secs when the answer was harvested (the scout reached `Done`).
    pub harvested_at: i64,
}

impl QueueItem {
    fn new(repo: &str, item: &str, now: i64) -> QueueItem {
        QueueItem {
            id: derive_id(item),
            item: item.to_string(),
            repo: repo.to_string(),
            status: ItemStatus::Queued,
            dispatch_phase: DispatchPhase::Pending,
            branch: None,
            pr: None,
            attempts: 0,
            dispatched_at: 0,
            last_relaunch_at: 0,
            updated_at: now,
            last_state: None,
            pane_activity_fingerprint: None,
            pane_activity_changed_at: 0,
            stuck: None,
            stuck_notified_key: None,
            harness: None,
            last_switch_at: 0,
            touches: Vec::new(),
            controller_id: None,
            held_by: None,
            substrate: Substrate::Auto,
            work_class: WorkClass::Coding,
            target: ExecutionTarget::Local,
            required_capabilities: Vec::new(),
            excluded_capabilities: Vec::new(),
            answer: None,
            command_approvals_trusted: false,
            claimed_remote: false,
            node: None,
            request_id: None,
            attempt_id: None,
            worker_state: None,
        }
    }

    /// Build a queued item carrying its touch-globs (#376), substrate (#377) and typed work-class
    /// lane (#385) from a leased [`crate::backlog::BacklogEntry`], so the resource-aware scheduler
    /// knows where it should run, what it overlaps, and which lane caps it before it's dispatched.
    fn with_entry(
        repo: &str,
        item: &str,
        touches: Vec<String>,
        substrate: Substrate,
        work_class: WorkClass,
        controller_id: Option<String>,
        now: i64,
    ) -> QueueItem {
        QueueItem {
            touches,
            substrate,
            work_class,
            controller_id,
            ..QueueItem::new(repo, item, now)
        }
    }

    /// This item's tmux window name — its sanitized branch (`/` → `-`), matching how `bn dispatch`
    /// names the agent's window. `None` until the item is dispatched (no branch, no window yet).
    pub fn tmux_window(&self) -> Option<String> {
        self.branch.as_deref().map(tmux_window_name)
    }

    /// Whether this is a scout/ask item (#378): a read-only [`WorkClass::Research`] agent that
    /// investigates and reports findings, dispatched with `--scout` and opening **no PR**. The one
    /// predicate the dispatch branch and the answer-harvest key off, so a `ship` is never routed
    /// through the scout path and a scout is never expected to produce a PR.
    pub fn is_scout(&self) -> bool {
        self.work_class == WorkClass::Research
    }

    /// Whether this scout/ask item has delivered its answer and is complete (#378) — its findings
    /// were harvested and routed back ([`OrchestrateState::record_answer`]). A complete scout is
    /// terminal with **no PR expected and no relaunch loop**: unlike a ship it doesn't merge, so the
    /// moment its answer exists there is nothing left to supervise. Pure over the item, so the
    /// scout-terminal-on-answer lifecycle is unit-testable in isolation.
    pub fn answer_complete(&self) -> bool {
        self.is_scout() && self.answer.is_some()
    }

    /// This item's **`(repo, id)` key** (#453) — the tuple the orchestrate queue is addressed by,
    /// so two repos' identically-named items never alias. `id` alone was ambiguous across repos.
    pub fn key(&self) -> (&str, &str) {
        (self.repo.as_str(), self.id.as_str())
    }

    fn reset_stuck_tracking(&mut self) {
        self.pane_activity_fingerprint = None;
        self.pane_activity_changed_at = 0;
        self.stuck = None;
        self.stuck_notified_key = None;
    }

    /// Whether the scheduler may begin a fresh dispatch transaction for this item.
    pub fn is_dispatchable(&self) -> bool {
        self.status == ItemStatus::Queued && self.dispatch_phase == DispatchPhase::Pending
    }

    /// Whether this item reserves a dispatch slot. A durable claim counts immediately so a daemon
    /// restart cannot fill the same capacity with replacement work before reconciling the claim.
    pub fn occupies_dispatch_slot(&self) -> bool {
        self.status.in_flight() || self.dispatch_phase == DispatchPhase::Claimed
    }

    /// Whether this item currently occupies a local/SSH dispatch slot rather than GitHub capacity.
    pub fn occupies_local_dispatch_slot(&self) -> bool {
        self.occupies_dispatch_slot() && !self.claimed_remote && self.substrate != Substrate::Remote
    }

    /// The canonical single-string form of this item's [`key`](Self::key) — `<repo>/<id>` (#453),
    /// for any map that needs one string key (a window→prompt / dispatch-tracking map). The
    /// repo-namespaced form of [`derive_id`]'s output; see [`scoped_key`].
    pub fn scoped_key(&self) -> String {
        scoped_key(&self.repo, &self.id)
    }
}

/// The tmux window name `bn dispatch` opens for a branch: `/` → `-` (the worktree-layout rule), so
/// a `dispatch/352` branch lives in window `dispatch-352`. Pure — the navigation half of #372.
pub fn tmux_window_name(branch: &str) -> String {
    branch.replace('/', "-")
}

/// A controller's identity for single-owner arbitration of a repo's run. Two concurrent daemons for
/// one repo (e.g. `run --all-repos` and `run --repo bn`) both adopt the same tmux lanes and
/// double-count in-flight, pinning the cap so the backlog stops pulling. The owner lease lets a
/// second controller detect a live first owner and defer, so exactly one loop drives a repo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerOwner {
    /// The driving process id.
    pub pid: u32,
    /// The host the process runs on (owners on other hosts can't be pid-probed, so they age out by
    /// [`OWNER_LEASE_TTL_SECS`] instead).
    pub host: String,
    /// Unix-epoch seconds of the last heartbeat; refreshed every tick by the owning loop.
    pub heartbeat: i64,
}

/// How long an owner lease stays authoritative without a heartbeat refresh. Comfortably above the
/// idle poll cadence (45s) so a healthy owner mid-idle-tick is never mistaken for abandoned, yet
/// short enough that a crashed owner on another host frees the repo within a couple of minutes.
pub const OWNER_LEASE_TTL_SECS: i64 = 120;

/// Whether `me` may claim (drive) a repo's run, given the recorded `existing` owner (if any),
/// whether that owner's process is still alive (`Some(true|false)` for a same-host pid probe,
/// `None` when unknowable — a different host or an unreadable record), and `now`.
///
/// **Fail-open**: yield *only* to an owner that is unambiguously live and different — a fresh
/// heartbeat within [`OWNER_LEASE_TTL_SECS`] and (when probeable) an alive pid. A crashed, stale, or
/// same-identity owner, or any uncertainty, resolves to "claim", so a transient glitch can never
/// leave a repo driverless.
pub fn may_claim_owner(
    existing: Option<&ControllerOwner>,
    me: &ControllerOwner,
    owner_alive: Option<bool>,
    now: i64,
    ttl: i64,
) -> bool {
    let Some(cur) = existing else {
        return true; // unowned
    };
    if cur.pid == me.pid && cur.host == me.host {
        return true; // our own lease (re-entrant refresh)
    }
    if owner_alive == Some(false) {
        return true; // same-host owner whose process is gone
    }
    now.saturating_sub(cur.heartbeat) >= ttl // stale heartbeat ⇒ abandoned
}

/// A recovery / lifecycle event recorded on the state for `bn orchestrate status` — the audit trail
/// that replaces the hand-typed "report one terse line per agent". Also mirrored to the event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEvent {
    pub ts: i64,
    /// The item id this event concerns.
    pub id: String,
    pub state: PaneState,
    pub action: SupervisorAction,
    /// Human-readable detail (e.g. "relaunch #2 after dns-outage").
    pub note: String,
}

/// Capacity for one dispatch lane at the end of the latest orchestrator tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCapacity {
    pub lane: String,
    pub cap: usize,
    pub in_flight: usize,
    pub queued: usize,
    pub available: usize,
    pub saturated: bool,
}

/// One candidate that remained queued after the latest dispatch tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCandidate {
    pub id: String,
    pub item: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
    pub waiting_since: i64,
}

/// Persisted explanation of the latest dispatch decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchDecision {
    pub at: i64,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub attempted: usize,
    pub launched: usize,
    pub lanes: Vec<LaneCapacity>,
    pub skipped: Vec<SkippedCandidate>,
}

pub const DEFAULT_KEEP_WARM_LOW_WATER: usize = 1;
pub const DEFAULT_KEEP_WARM_BRAINSTORM_COUNT: usize = 1;
pub const DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS: i64 = 60 * 60;

/// Persisted policy for a resident supervisor that keeps its repo's backlog supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepWarmPolicy {
    pub low_water: usize,
    pub brainstorm_count: usize,
    pub brainstorm_interval_secs: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_titles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_labels: Vec<String>,
}

impl Default for KeepWarmPolicy {
    fn default() -> Self {
        Self {
            low_water: DEFAULT_KEEP_WARM_LOW_WATER,
            brainstorm_count: DEFAULT_KEEP_WARM_BRAINSTORM_COUNT,
            brainstorm_interval_secs: DEFAULT_KEEP_WARM_BRAINSTORM_INTERVAL_SECS,
            exclude_titles: vec!["gap(".to_string()],
            exclude_labels: vec!["blocked".to_string()],
        }
    }
}

/// Minimal open-issue view consumed by the pure keep-warm planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepWarmIssue {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
    pub assigned: bool,
}

/// Side effects the CLI executor should realize for one keep-warm pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepWarmPlan {
    pub enqueue_issues: Vec<String>,
    pub brainstorm: Vec<String>,
}

/// Decide how to restore a repo to its low-water mark without touching disk or GitHub.
pub fn plan_keep_warm(
    repo: &str,
    policy: &KeepWarmPolicy,
    active_depth: usize,
    active_ids: &std::collections::HashSet<String>,
    issues: &[KeepWarmIssue],
    last_brainstorm_at: Option<i64>,
    now: i64,
) -> KeepWarmPlan {
    let deficit = policy.low_water.saturating_sub(active_depth);
    if deficit == 0 {
        return KeepWarmPlan::default();
    }

    let excluded_title = |title: &str| {
        let title = title.to_ascii_lowercase();
        policy
            .exclude_titles
            .iter()
            .any(|needle| title.contains(&needle.to_ascii_lowercase()))
    };
    let excluded_label = |labels: &[String]| {
        labels.iter().any(|label| {
            policy
                .exclude_labels
                .iter()
                .any(|excluded| label.eq_ignore_ascii_case(excluded))
        })
    };
    let enqueue_issues: Vec<String> = issues
        .iter()
        .filter(|issue| !issue.assigned)
        .filter(|issue| !excluded_title(&issue.title))
        .filter(|issue| !excluded_label(&issue.labels))
        .map(|issue| issue.number.to_string())
        .filter(|id| !active_ids.contains(id))
        .take(deficit)
        .collect();
    if !enqueue_issues.is_empty() || active_depth > 0 || policy.brainstorm_count == 0 {
        return KeepWarmPlan {
            enqueue_issues,
            brainstorm: Vec::new(),
        };
    }

    let rate_limited = last_brainstorm_at
        .is_some_and(|last| now.saturating_sub(last) < policy.brainstorm_interval_secs);
    if rate_limited {
        return KeepWarmPlan::default();
    }
    let bucket = now / policy.brainstorm_interval_secs.max(1);
    let brainstorm = (1..=policy.brainstorm_count)
        .map(|index| {
            format!(
                "keep-warm brainstorm {bucket}-{index} for {repo}: inspect the repository and file one deduplicated enhancement issue with `bn issue file --kind enhancement`; do not write code or open a PR"
            )
        })
        .collect();
    KeepWarmPlan {
        enqueue_issues,
        brainstorm,
    }
}

/// The whole orchestration run: a repo-scoped queue plus its concurrency policy and recovery log.
/// Persisted as one JSON file in the state dir so the daemon (and `bn orchestrate status`) share it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrateState {
    /// The single repo this run targets — every queued item dispatches into it (issues live here).
    pub repo: String,
    pub queue: Vec<QueueItem>,
    /// Max agents dispatched at once. `solo` overrides this to 1.
    pub concurrency: usize,
    /// The **research-lane** local cap (#385): when set, `scout`/research agents ([`WorkClass::Research`])
    /// scale on this budget independently of the coding lane (`concurrency`), so `--coding 5 --research 2`
    /// runs 5 ship + 2 scout agents concurrently. `None` = no separate research lane: research items
    /// share the single `concurrency` pool (the pre-#385 single-knob behavior). Adjustable live via
    /// `bn orchestrate scale` ([`OrchestrateState::scale`]) — the daemon picks it up on its next tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<usize>,
    /// Refactor mode (e.g. a main.rs-touching change like #333): one agent at a time, no overlap.
    #[serde(default)]
    pub solo: bool,
    /// The recovery / lifecycle audit trail (newest last).
    #[serde(default)]
    pub recovery: Vec<RecoveryEvent>,
    pub started_at: i64,
    pub updated_at: i64,
    /// Set by `bn orchestrate stop` — the daemon's next tick sees it and exits cleanly.
    #[serde(default)]
    pub stopped: bool,
    /// Set by `bn orchestrate supervise` — this run is a **standing, session-independent supervisor**
    /// (#426). Each tick it additionally *adopts* live standalone `bn dispatch` lanes for the repo
    /// (see [`OrchestrateState::adopt_dispatched_lanes`]) so their green PRs land and dead/stalled
    /// lanes recover with nobody at the keyboard, it lands ready PRs every pass (not only when it has
    /// its own in-flight items), and it does **not** exit when the queue drains — it keeps standing to
    /// catch future lanes and PRs. A plain `--queue` run leaves this `false` and behaves as before.
    #[serde(default)]
    pub supervise: bool,
    /// Opt-in policy for automatically replenishing this resident supervisor's durable backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<KeepWarmPolicy>,
    /// Last time this repo launched keep-warm ideation scouts. Persisted for restart-safe rate limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_warm_last_brainstorm_at: Option<i64>,
    /// Set by `bn orchestrate add --start` (any backlog-seeded run) — this run is a **resident,
    /// self-feeding** loop (#368): on drain it does **not** exit but stays alive pulling the next
    /// batch from the durable backlog (#375) as slots free, so a multi-wave backlog runs to
    /// completion with **zero scheduled prompts** even when waves are appended with gaps between
    /// them. Unlike `supervise` (#426) it does not adopt external standalone `bn dispatch` lanes —
    /// it only feeds itself from the backlog. Only `stop` ends it. A bounded `--queue` run leaves
    /// this `false` and exits on drain as before, preserving the one-shot-wave behavior.
    #[serde(default)]
    pub self_feed: bool,
    /// The harness the daemon dispatches new agents under (`bn orchestrate --agent`, #382), or the
    /// resolved config default. `None` on a pre-#382 run's state, which [`dispatch_harness`] reads as
    /// [`DEFAULT_HARNESS`] (`copilot`) — the prior hardcoded behavior. A per-item [`QueueItem::harness`]
    /// still wins for a lane the usage-limit fallback has already switched.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub harness: Option<String>,
    /// The system boot marker (`boot-id` / boot epoch) observed at the last daemon pass (#380).
    /// When the daemon restarts after a **reboot** this changes — every in-flight tmux window died
    /// with the machine, so the fleet would classify `Dead` → hard-restart and each re-dispatch
    /// would burn a relaunch `attempt` toward the [`Backoff::max_attempts`] cap for a *machine*
    /// event, not the item's fault. Detecting the fresh boot lets the daemon reset the backoff for
    /// in-flight items whose worktree survived (see [`OrchestrateState::boot_reset_candidates`]).
    /// `None` on a pre-#380 run or before the first pass records one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub boot_id: Option<String>,
    /// Why local dispatch was capped/paused on the last pass (#377) — the label of the binding cap
    /// ([`crate::scheduler::Bind::label`]: `ram` / `load` / `tokens` / `cost` / `local-cap`), or
    /// `None` when nothing bound (slots free, or the queue is empty). Set by the CLI executor after
    /// the resource-aware [`crate::scheduler::pick`], so `bn orchestrate status` shows *why* pending
    /// items aren't dispatching without re-observing the machine. Purely informational — the
    /// authoritative decision is always re-made live each pass.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dispatch_bind: Option<String>,
    /// Why the latest dispatch pass launched no work, or `None` when it launched at least one item.
    /// Unlike [`dispatch_bind`](Self::dispatch_bind), this covers the whole decision: empty backlog,
    /// all work already in flight, conflict holds, placement failures, and admission guards. The
    /// daemon persists the concise operator-facing line so every status surface reports the exact
    /// last-tick decision instead of re-deriving it from a later snapshot.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dispatch_reason: Option<String>,
    /// Full evidence for the latest dispatch decision, surfaced only by `orchestrate status --why`.
    /// Kept separate from the legacy summary fields so ordinary status output stays stable.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dispatch_decision: Option<DispatchDecision>,
    /// The worker-node registry this run may place work on (#717, M1 foundation) — the identity +
    /// capacity stubs the pure scheduler filters when resolving a
    /// [`QueueItem::target`]/[`ExecutionTarget::SshHost`] to a concrete, placement-eligible node.
    /// Empty on a pre-#717 run's state (and any run that never registered a node), which reads as
    /// "only the implicit local box is available" — so every item stays on `Local` exactly as
    /// before. No live capacity is fetched yet; this holds the stub records only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeRecord>,
}

/// Rolled-up counts for `bn orchestrate status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Counts {
    pub queued: usize,
    pub in_flight: usize,
    pub merged: usize,
    pub failed: usize,
    pub total: usize,
    /// Of `in_flight`, how many are on a **remote** runner (#377) — dispatched via
    /// `bn dispatch --remote`, occupying no local slot. `in_flight - remote` are the local agents.
    #[serde(default)]
    pub remote: usize,
}

// ===========================================================================
// Durability at scale (#380) — bounded state, run-history eviction, reboot-safe backoff
// ===========================================================================
//
// A daemon meant to run resident for weeks over huge queues must not let its per-tick atomic state
// rewrite grow without bound. Three pure policies keep it small and correct, with the CLI as the
// thin file/clock executor: a ring-buffer trim on the in-state recovery trail, an age-based
// eviction of terminal items to an append-only `run-history.jsonl`, and a fresh-boot reset of the
// relaunch backoff so a reboot (a machine event) never burns an item's failure budget.

/// Ring-buffer cap for the in-state [`OrchestrateState::recovery`] trail (#380): the daemon keeps
/// only the most recent this-many events on the persisted state. The complete, unbounded trail
/// already lives in `events.jsonl` (mirrored by the CLI's `log_event`), so the state copy needs
/// only enough for `bn orchestrate status` to show recent activity — which bounds the per-tick
/// atomic rewrite regardless of how long the run has been going.
pub const RECOVERY_CAP: usize = 200;

/// How long a **terminal** (merged/failed) queue item stays in the live `queue` before the daemon
/// evicts it to the append-only `run-history.jsonl` (#380). Two hours — comfortably past the 1-hour
/// [`ROLLUP_WINDOW_SECS`] throughput window, so recent outcomes stay live for `status`, while older
/// ones are moved out so a weeks-long run's state stays O(in-flight + recent) rather than accreting
/// every landed item forever.
pub const HISTORY_RETAIN_SECS: i64 = 2 * 3600;

/// Whether a queue item is old enough to evict from the live `queue` into `run-history.jsonl`
/// (#380): terminal (merged/failed) and untouched for at least `older_than_secs`. Pure — the single
/// predicate both the read ([`OrchestrateState::evictable_terminal`]) and the write
/// ([`OrchestrateState::evict_terminal`]) share, so the set written to history and the set removed
/// from the queue can never diverge.
fn is_evictable(item: &QueueItem, now: i64, older_than_secs: i64) -> bool {
    item.status.is_terminal() && now.saturating_sub(item.updated_at) >= older_than_secs
}

/// One evicted terminal item, appended as a line to `run-history.jsonl` (#380). A compact,
/// self-describing snapshot of a completed work item — its outcome, PR, relaunch attempts, and the
/// dispatch→land timestamps — so the per-item results survive being trimmed from the live `queue`
/// *and* survive a fresh `bn orchestrate --queue` overwriting the run state. `bn orchestrate status`
/// reads recent records back so merged/failed context persists across daemon restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecord {
    /// The repo the item ran in — history is one shared log, so readers filter by it.
    pub repo: String,
    pub id: String,
    pub item: String,
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub dispatched_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// When the item was evicted from the live queue into history (epoch secs).
    pub evicted_at: i64,
}

impl HistoryRecord {
    /// Snapshot a terminal queue `item` into a history record, stamped `evicted_at`.
    pub fn from_item(repo: &str, item: &QueueItem, evicted_at: i64) -> HistoryRecord {
        HistoryRecord {
            repo: repo.to_string(),
            id: item.id.clone(),
            item: item.item.clone(),
            status: item.status,
            branch: item.branch.clone(),
            pr: item.pr.clone(),
            attempts: item.attempts,
            dispatched_at: item.dispatched_at,
            updated_at: item.updated_at,
            evicted_at,
        }
    }

    /// The dispatch→land cycle time in seconds for a merged item, or `None` (not merged, or the
    /// timestamps are missing/inverted). Feeds the average-cycle rollup in [`summarize_history`].
    pub fn cycle_secs(&self) -> Option<i64> {
        (self.status == ItemStatus::Merged
            && self.dispatched_at > 0
            && self.updated_at >= self.dispatched_at)
            .then_some(self.updated_at - self.dispatched_at)
    }

    /// Serialize to a single JSONL line (no trailing newline) for the append-only history log.
    pub fn to_jsonl(&self) -> std::io::Result<String> {
        crate::store::to_json_line(self)
    }

    /// Parse one JSONL history line, or `None` if unparseable (a torn append never poisons reads).
    pub fn from_jsonl(line: &str) -> Option<HistoryRecord> {
        serde_json::from_str(line).ok()
    }
}

/// A pure rollup of `run-history.jsonl` records for one repo (#380) — the cross-restart outcome
/// context `bn orchestrate status` surfaces. Deduped by id, newest (largest `evicted_at`) wins: the
/// append-only log may hold a record twice if the daemon crashed between the history append and the
/// queue removal, so the read side is tolerant of that.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct HistorySummary {
    /// Distinct items that ended merged.
    pub merged: usize,
    /// Distinct items that ended failed.
    pub failed: usize,
    /// Mean dispatch→land cycle over merged records that carry timestamps; `None` if none do.
    pub avg_cycle_secs: Option<i64>,
    /// The most recent records, newest-first, capped to `recent_cap` — what `status` lists.
    pub recent: Vec<HistoryRecord>,
}

/// Summarize the `run-history.jsonl` `records` for `repo` (#380): count distinct merged/failed
/// outcomes, average the merged cycle times, and return the `recent_cap` newest records (newest
/// first). Pure over its inputs — the CLI reads the file, this does the math — so it's unit-testable
/// against a synthetic log. Dedup is by id (newest `evicted_at` wins) to absorb a crash-retry
/// double-append.
pub fn summarize_history(
    records: &[HistoryRecord],
    repo: &str,
    recent_cap: usize,
) -> HistorySummary {
    // Keep the newest record per id (largest evicted_at), so a crash-retry duplicate collapses.
    let mut latest: std::collections::HashMap<&str, &HistoryRecord> =
        std::collections::HashMap::new();
    for r in records.iter().filter(|r| r.repo == repo) {
        latest
            .entry(r.id.as_str())
            .and_modify(|cur| {
                if r.evicted_at >= cur.evicted_at {
                    *cur = r;
                }
            })
            .or_insert(r);
    }
    let mut deduped: Vec<&HistoryRecord> = latest.into_values().collect();
    deduped.sort_by(|a, b| b.evicted_at.cmp(&a.evicted_at).then(a.id.cmp(&b.id)));

    let merged = deduped
        .iter()
        .filter(|r| r.status == ItemStatus::Merged)
        .count();
    let failed = deduped
        .iter()
        .filter(|r| r.status == ItemStatus::Failed)
        .count();
    let cycles: Vec<i64> = deduped.iter().filter_map(|r| r.cycle_secs()).collect();
    let avg_cycle_secs = (!cycles.is_empty())
        .then(|| (cycles.iter().sum::<i64>() as f64 / cycles.len() as f64).round() as i64);
    let recent = deduped.into_iter().take(recent_cap).cloned().collect();
    HistorySummary {
        merged,
        failed,
        avg_cycle_secs,
        recent,
    }
}

/// Derive a stable id from a dispatch subject: the issue/PR number from `#338`, `338`, or a GitHub
/// issue/PR URL; otherwise a lowercase slug of the text (first few words). Pure — no network.
pub fn derive_id(item: &str) -> String {
    let t = item.trim();
    // A bare number or #number.
    let bare = t.strip_prefix('#').unwrap_or(t);
    if !bare.is_empty() && bare.chars().all(|c| c.is_ascii_digit()) {
        return bare.to_string();
    }
    // A GitHub issue/PR URL: .../issues/NNN or .../pull/NNN (tolerate a trailing slug/fragment).
    for marker in ["/issues/", "/pull/"] {
        if let Some(rest) = t.split(marker).nth(1) {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num.is_empty() {
                return num;
            }
        }
    }
    // Free text → slug of the first ~6 words.
    let slug: String = t
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "item".to_string()
    } else {
        slug
    }
}

/// The repo-namespaced key for an item — `<repo>/<id>` (#453). [`derive_id`] yields the per-repo
/// `id` (an issue number or a text slug); this composes it with the repo so the orchestrate queue,
/// the window→prompt map, and any dispatch-tracking map are keyed by the full `(repo, item)` pair,
/// and two repos' identically-named items (both `#338`) never collide. Pure — the keying half of
/// the multi-repo foundation. A single-repo run still shows/addresses the bare `id`; only the
/// *key* is namespaced, so its behavior is unchanged.
pub fn scoped_key(repo: &str, id: &str) -> String {
    format!("{}/{}", repo.trim(), id)
}

// ===========================================================================
// Conflict-aware scheduling (#376) — the coarse, pure predicted-overlap policy
// ===========================================================================
//
// Parallel lanes that edit the same file/registration point (e.g. `crates/bn-cli/src/cli.rs`
// enums, `main.rs` match arms, `config/crons.toml`) collide and pile up sequential rebases — today
// only caught at land time by `bn wt merge --all`, after the losing agent has already burned a
// full run. These functions let the scheduler *predict* that overlap from each item's declared
// `touches` globs and hold a conflicting candidate rather than co-schedule it — a bn-native
// per-touch-area lock. Prediction is a pure optimization and can stay coarse: any missed overlap
// is still serialized safely at land time, and an over-prediction only costs a little throughput.

/// Whether two declared touch-glob **sets** are predicted to overlap — the conflict policy at the
/// heart of #376. True iff some glob in `a` and some glob in `b` can match a common path. An empty
/// set means "no declared hint", which conflicts with nothing (the item stays schedulable with
/// anything; land-time serialization remains the safety net). Pure, total, and symmetric.
pub fn touches_conflict(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| glob_overlap(x, y)))
}

/// Whether a candidate's declared `touches` are predicted to conflict with **any** in-flight item's
/// — the issue's `conflicts(candidate, inflight)` overlap policy. `inflight` is each dispatched
/// item's touch-set. Pure over its inputs, so the whole scheduling decision is unit-testable
/// without the daemon's side effects.
pub fn conflicts(candidate: &[String], inflight: &[Vec<String>]) -> bool {
    inflight
        .iter()
        .any(|other| touches_conflict(candidate, other))
}

/// Whether two path globs can match a common path — a coarse, dependency-free glob *intersection*.
/// Understands the touch-hint vocabulary: literal path segments, `*` (any run within one segment),
/// `?` (exactly one char), and `**` (any run of whole segments, so `"**"` — the `--solo`
/// degenerate case — overlaps everything). Symmetric and pure; `/`-separated, order-insensitive.
pub fn glob_overlap(a: &str, b: &str) -> bool {
    fn segs(g: &str) -> Vec<&str> {
        g.split('/').filter(|s| !s.is_empty()).collect()
    }
    seg_overlap(&segs(a), &segs(b))
}

/// Segment-level glob intersection: can these two `/`-split glob segment lists match a common path?
/// `**` matches zero-or-more whole segments (recursing both ways); otherwise the two head segments
/// must intersect ([`seg_match`]) and the tails must too.
fn seg_overlap(a: &[&str], b: &[&str]) -> bool {
    match (a.first(), b.first()) {
        (None, None) => true,
        // One side exhausted: overlaps only if every remaining segment is `**` (matches zero).
        (None, Some(_)) => b.iter().all(|s| *s == "**"),
        (Some(_), None) => a.iter().all(|s| *s == "**"),
        (Some(&x), Some(&y)) => {
            if x == "**" {
                // `**` matches zero segments (advance past it) or ≥1 (consume one of b, keep `**`).
                seg_overlap(&a[1..], b) || seg_overlap(a, &b[1..])
            } else if y == "**" {
                seg_overlap(a, &b[1..]) || seg_overlap(&a[1..], b)
            } else {
                seg_match(x, y) && seg_overlap(&a[1..], &b[1..])
            }
        }
    }
}

/// Whether two single path *segments* (no `/`, may carry `*`/`?`) can match a common string. Fast
/// path for the common all-literal case; otherwise a small wildcard-intersection search.
fn seg_match(x: &str, y: &str) -> bool {
    let wild = |s: &str| s.bytes().any(|c| c == b'*' || c == b'?');
    if !wild(x) && !wild(y) {
        return x == y;
    }
    wild_overlap(x.as_bytes(), y.as_bytes())
}

/// Non-empty-intersection test for two single-segment glob patterns over `*` (any run of chars) and
/// `?` (one char): does some concrete string match *both*? A reachability search over the product of
/// the two patterns' glob automata — `*` self-loops (and epsilon-skips to match empty), `?`/literals
/// consume one char. Bounded by `(x+1)·(y+1)` states, so it always terminates.
fn wild_overlap(x: &[u8], y: &[u8]) -> bool {
    #[derive(Clone, Copy)]
    enum Class {
        Any,
        Lit(u8),
    }
    // The class the pattern at `k` accepts for one consumed char, and where it lands afterwards
    // (`*` stays put — it may match more; `?`/literal advance).
    fn accept(pat: &[u8], k: usize) -> (Class, usize) {
        match pat[k] {
            b'*' => (Class::Any, k),
            b'?' => (Class::Any, k + 1),
            c => (Class::Lit(c), k + 1),
        }
    }
    let (nx, ny) = (x.len(), y.len());
    let mut seen = vec![vec![false; ny + 1]; nx + 1];
    let mut stack = vec![(0usize, 0usize)];
    while let Some((i, j)) = stack.pop() {
        if seen[i][j] {
            continue;
        }
        seen[i][j] = true;
        // Accept: both remainders can match the empty string (all `*`).
        if x[i..].iter().all(|&c| c == b'*') && y[j..].iter().all(|&c| c == b'*') {
            return true;
        }
        // Epsilon: a leading `*` may match zero chars — advance past it.
        if i < nx && x[i] == b'*' {
            stack.push((i + 1, j));
        }
        if j < ny && y[j] == b'*' {
            stack.push((i, j + 1));
        }
        // Consume one common char on both sides.
        if i < nx && j < ny {
            let (cx, ni) = accept(x, i);
            let (cy, nj) = accept(y, j);
            let compatible = match (cx, cy) {
                (Class::Any, _) | (_, Class::Any) => true,
                (Class::Lit(p), Class::Lit(q)) => p == q,
            };
            if compatible {
                stack.push((ni, nj));
            }
        }
    }
    false
}

impl OrchestrateState {
    /// Seed a fresh run from a declared queue of dispatch subjects.
    pub fn new(
        repo: &str,
        items: &[String],
        concurrency: usize,
        solo: bool,
        now: i64,
    ) -> OrchestrateState {
        OrchestrateState {
            repo: repo.to_string(),
            queue: items.iter().map(|i| QueueItem::new(repo, i, now)).collect(),
            concurrency: concurrency.max(1),
            research: None,
            solo,
            recovery: Vec::new(),
            started_at: now,
            updated_at: now,
            stopped: false,
            supervise: false,
            keep_warm: None,
            keep_warm_last_brainstorm_at: None,
            self_feed: false,
            harness: None,
            boot_id: None,
            dispatch_bind: None,
            dispatch_reason: None,
            dispatch_decision: None,
            nodes: Vec::new(),
        }
    }

    /// Append declared items to the queue, skipping ids already present — the "same-repo declare
    /// extends the run" behavior (#395). Returns how many items were actually added. Never touches
    /// existing items, so an in-flight wave keeps its dispatched/merged lifecycle untouched.
    pub fn extend(&mut self, items: &[String], now: i64) -> usize {
        let with: Vec<(String, Vec<String>)> =
            items.iter().map(|i| (i.clone(), Vec::new())).collect();
        self.extend_with_touches(&with, now)
    }

    /// Like [`extend`], but each `(item, touches)` carries the declared touch-globs (#376) from the
    /// durable backlog (#375), so the leased run item keeps the conflict-aware scheduling hint. Same
    /// skip-existing-ids dedup and return count as [`extend`].
    pub fn extend_with_touches(&mut self, items: &[(String, Vec<String>)], now: i64) -> usize {
        let with: Vec<QueueEntrySpec> = items
            .iter()
            .map(|(i, t)| {
                (
                    i.clone(),
                    t.clone(),
                    Substrate::Auto,
                    WorkClass::Coding,
                    None,
                )
            })
            .collect();
        self.extend_with_entries(&with, now)
    }

    /// Like [`extend_with_touches`], but each entry also carries its [`Substrate`] (#377) and typed
    /// [`WorkClass`] lane (#385) from the leased [`crate::backlog::BacklogEntry`], so the
    /// resource-aware scheduler knows whether an item is local/remote/auto and which lane caps it
    /// before dispatch. Same skip-existing-ids dedup and return count as [`extend`].
    pub fn extend_with_entries(&mut self, items: &[QueueEntrySpec], now: i64) -> usize {
        let mut added = 0usize;
        let repo = self.repo.clone();
        for (item, touches, substrate, work_class, controller_id) in items {
            let id = derive_id(item);
            // #453: dedup on the `(repo, id)` key, not `id` alone — a declare extends *this* run's
            // repo, so an id already present for this repo is skipped, while the same id owned by
            // another repo (a future multi-repo run) stays a distinct item.
            if self.queue.iter().any(|q| q.repo == repo && q.id == id) {
                continue;
            }
            self.queue.push(QueueItem::with_entry(
                &repo,
                item,
                touches.clone(),
                *substrate,
                *work_class,
                controller_id.clone(),
                now,
            ));
            added += 1;
        }
        if added > 0 {
            self.updated_at = now;
        }
        added
    }

    /// Adopt live standalone `bn dispatch` lanes into supervision (#426) — the session-independent
    /// fix for the stalled drain. Given the branch names of worktree lanes observed **live** for this
    /// repo (each with a running agent window), push a `Dispatched` [`QueueItem`] for every branch not
    /// already tracked by an item's `branch`, so the daemon's existing land / recover / reconcile
    /// machinery covers a lane that was dispatched *outside* this run — the drain's standalone lanes,
    /// which otherwise get no background land-or-recover at all. Returns the branches newly adopted.
    ///
    /// Idempotent and safe to call every tick: a branch already tracked — this run's own dispatched
    /// item, or a lane adopted on an earlier pass, **including one already merged/failed** — is
    /// skipped, so a finished lane is never re-adopted and relaunch-looped. Pure over its input: the
    /// caller supplies the observed live branches (this never touches tmux/git), so the adoption
    /// decision is unit-testable in isolation from live-signal capture.
    pub fn adopt_dispatched_lanes(&mut self, branches: &[String], now: i64) -> Vec<String> {
        let repo = self.repo.clone();
        let mut adopted = Vec::new();
        for branch in branches {
            if self
                .queue
                .iter()
                .any(|i| i.repo == repo && i.branch.as_deref() == Some(branch.as_str()))
            {
                continue; // already tracked (dispatched by this run, or adopted earlier)
            }
            let mut item = QueueItem::new(&repo, branch, now);
            item.status = ItemStatus::Dispatched;
            item.branch = Some(branch.clone());
            item.dispatched_at = now;
            self.queue.push(item);
            adopted.push(branch.clone());
        }
        if !adopted.is_empty() {
            self.updated_at = now;
        }
        adopted
    }

    /// The effective concurrency cap: 1 in solo mode, else the configured cap (min 1).
    pub fn effective_concurrency(&self) -> usize {
        if self.solo {
            1
        } else {
            self.concurrency.max(1)
        }
    }

    /// The **coding-lane** local cap (#385): the ship agents' budget. Solo mode still collapses the
    /// whole run to a single lane of 1, so a refactor wave never overlaps; otherwise it's the
    /// configured `concurrency` (min 1). In an untyped run this is also the total local pool.
    pub fn coding_cap(&self) -> usize {
        self.effective_concurrency()
    }

    /// The **research-lane** local cap (#385), or `None` when no separate research lane is
    /// configured (research then shares the coding pool — the pre-#385 single-knob behavior). Solo
    /// mode folds every lane into the single coding slot, so it reports no separate research lane.
    pub fn research_cap(&self) -> Option<usize> {
        if self.solo {
            None
        } else {
            self.research
        }
    }

    /// Whether typed concurrency lanes are active (#385) — a distinct research lane is configured
    /// (and not folded away by solo).
    pub fn typed_lanes(&self) -> bool {
        self.research_cap().is_some()
    }

    /// The **total** number of local slots the run wants filled across every lane (#385): the coding
    /// cap plus the research cap when a research lane is configured. This is what the backlog
    /// refill tops the working set up to, so a `--coding 5 --research 2` run leases enough to fill
    /// both lanes. Untyped (or solo) runs report exactly the coding cap — the pre-#385 value.
    pub fn total_local_cap(&self) -> usize {
        self.coding_cap() + self.research_cap().unwrap_or(0)
    }

    /// How many **local** in-flight items belong to `class` (#385) — dispatched, non-remote items
    /// in that typed lane. The per-lane occupancy the scheduler counts against each lane's cap.
    pub fn local_in_flight_class(&self, class: WorkClass) -> usize {
        self.queue
            .iter()
            .filter(|i| i.occupies_local_dispatch_slot() && i.work_class == class)
            .count()
    }

    /// How many in-flight items run on this daemon's own box in `class`. SSH worker attempts keep
    /// their durable local-substrate lifecycle but consume worker capacity, not a local lane slot.
    pub fn local_node_in_flight_class(&self, class: WorkClass) -> usize {
        self.queue
            .iter()
            .filter(|i| {
                i.occupies_local_dispatch_slot() && i.node.is_none() && i.work_class == class
            })
            .count()
    }

    /// Live-adjust the typed concurrency lanes (#385) — the pure state mutation behind
    /// `bn orchestrate scale`. Applies each provided cap (a `None` argument leaves that lane
    /// untouched): `coding` sets the coding lane (`concurrency`, min 1); `research` sets/creates the
    /// research lane. Returns the resulting `(coding_cap, research_cap)` so the CLI can echo the new
    /// budgets. A running daemon picks the change up on its next tick — no restart, no re-declare.
    pub fn scale(
        &mut self,
        coding: Option<usize>,
        research: Option<usize>,
    ) -> (usize, Option<usize>) {
        if let Some(c) = coding {
            self.concurrency = c.max(1);
        }
        if let Some(r) = research {
            self.research = Some(r);
        }
        (self.concurrency.max(1), self.research)
    }

    /// How many items currently occupy an in-flight slot (local **and** remote). Used for
    /// reconcile / status / "is there work in flight" — not the local concurrency cap, which counts
    /// only [`local_in_flight`](Self::local_in_flight).
    pub fn in_flight(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| i.occupies_dispatch_slot())
            .count()
    }

    /// How many items occupy a **local** concurrency slot right now (#377): dispatched items that
    /// are not on a remote substrate. This is what the resource-aware scheduler caps — a remote
    /// fan-out runs on a GitHub-hosted runner and occupies no local slot.
    pub fn local_in_flight(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| i.occupies_local_dispatch_slot())
            .count()
    }

    /// How many in-flight items consume this daemon's own machine capacity rather than an SSH
    /// worker or GitHub-hosted runner.
    pub fn local_node_in_flight(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| i.occupies_local_dispatch_slot() && i.node.is_none())
            .count()
    }

    /// How many items are in flight on a **remote** runner (#377) — dispatched via
    /// `bn dispatch --remote`, supervised by PR/issue reconcile rather than the tmux pane machine.
    pub fn remote_in_flight(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| {
                i.occupies_dispatch_slot() && (i.claimed_remote || i.substrate == Substrate::Remote)
            })
            .count()
    }

    /// The non-terminal working set — queued plus in-flight items. The daemon leases only enough
    /// from the durable backlog (#375) to top this up to the concurrency cap, so the run state
    /// stays a small execution ledger regardless of how deep the backlog is.
    pub fn working_set(&self) -> usize {
        self.queue
            .iter()
            .filter(|i| !i.status.is_terminal())
            .count()
    }

    /// The index of the next queued item to dispatch, or `None` if the cap is full or every queued
    /// item is held. Honors the effective concurrency (and thus `--solo`) **and** conflict-aware
    /// scheduling (#376): a queued candidate whose declared `touches` overlap an in-flight item's is
    /// *skipped* rather than blocking the queue head, so non-overlapping items keep filling slots
    /// while would-be colliders serialize. See [`conflict_holder`](Self::conflict_holder).
    pub fn next_dispatchable(&self) -> Option<usize> {
        if self.local_in_flight() >= self.effective_concurrency() {
            return None;
        }
        self.queue
            .iter()
            .position(|i| i.is_dispatchable() && self.conflict_holder(i).is_none())
    }

    /// The in-flight item whose declared touch-area is predicted to overlap `candidate`'s, if any —
    /// the reason a queued candidate is *held* instead of dispatched (#376). `None` ⇒ no predicted
    /// conflict, so the candidate can fill an open slot. Drives both the skip in
    /// [`next_dispatchable`](Self::next_dispatchable) and the "held: conflicts with #NNN" line in
    /// `bn orchestrate status`. A candidate with no `touches` hint is never held (schedulable with
    /// anything); an in-flight item with no hint holds nobody.
    pub fn conflict_holder(&self, candidate: &QueueItem) -> Option<&QueueItem> {
        if candidate.touches.is_empty() {
            return None;
        }
        self.queue.iter().find(|i| {
            i.occupies_dispatch_slot() && touches_conflict(&candidate.touches, &i.touches)
        })
    }

    /// Refresh every queued item's [`held_by`](QueueItem::held_by) marker from the live conflict
    /// policy and return the items that *newly* became held this pass (`(id, holder_id)`) — the
    /// #376 observability half. Called once per dispatch pass (after slots are filled, so it reflects
    /// the post-dispatch in-flight set), so `status`/web read *why* a candidate is held without
    /// re-deriving, and the daemon logs each hold once per episode rather than every poll. Clearing:
    /// a queued item whose holder has since landed (no longer conflicts) is un-held; a dispatched or
    /// terminal item never carries the marker. Returns transitions *into* a hold only.
    pub fn refresh_holds(&mut self, now: i64) -> Vec<(String, String)> {
        // Resolve holders under a shared borrow, then apply under a mutable one — the two never overlap.
        let holders: Vec<Option<String>> = self
            .queue
            .iter()
            .map(|i| {
                i.is_dispatchable()
                    .then(|| self.conflict_holder(i).map(|h| h.id.clone()))
                    .flatten()
            })
            .collect();
        let mut newly = Vec::new();
        let mut changed = false;
        for (item, holder) in self.queue.iter_mut().zip(holders) {
            if item.held_by == holder {
                continue;
            }
            if let Some(h) = &holder {
                newly.push((item.id.clone(), h.clone()));
            }
            item.held_by = holder;
            changed = true;
        }
        if changed {
            self.updated_at = now;
        }
        newly
    }

    /// Whether the run is complete: no queued and no in-flight items remain (all merged/failed).
    pub fn is_drained(&self) -> bool {
        self.queue.iter().all(|i| i.status.is_terminal())
    }

    /// Whether this run stays *resident* once its ledger drains, rather than exiting: a standing
    /// supervisor (#426) or a self-feeding backlog drainer (#368). A resident run keeps polling —
    /// the supervisor for future standalone lanes + green PRs, the self-feeder for the next batch
    /// appended to the durable backlog (#375) — so a multi-wave backlog completes with no scheduled
    /// prompt. A bounded `--queue` run is not resident and finishes when drained.
    pub fn is_resident(&self) -> bool {
        self.supervise || self.self_feed
    }

    /// Whether the daemon loop should exit this pass (#368): the ledger has drained, the durable
    /// backlog holds nothing pending for the repo, and this is **not** a resident run. A resident
    /// run (supervisor / self-feeder) never reports complete on an empty backlog — it stays alive to
    /// catch future work. Pure over its inputs, so the drain-and-exit vs stay-resident decision is
    /// unit-testable without the daemon's side effects.
    pub fn run_complete(&self, backlog_pending: usize) -> bool {
        self.is_drained() && backlog_pending == 0 && !self.is_resident()
    }

    /// Rolled-up counts for status.
    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            total: self.queue.len(),
            ..Default::default()
        };
        for i in &self.queue {
            match i.status {
                ItemStatus::Queued => c.queued += 1,
                ItemStatus::Dispatched => {
                    c.in_flight += 1;
                    if i.substrate == Substrate::Remote {
                        c.remote += 1;
                    }
                }
                ItemStatus::Merged => c.merged += 1,
                ItemStatus::Failed => c.failed += 1,
            }
        }
        c
    }

    /// Find an item by id (mutable).
    pub fn item_mut(&mut self, id: &str) -> Option<&mut QueueItem> {
        self.queue.iter_mut().find(|i| i.id == id)
    }

    /// Find an item by its full **`(repo, id)` key** (#453) — the collision-free lookup for a
    /// multi-repo run, where `id` alone is ambiguous across repos. [`item_mut`](Self::item_mut)
    /// remains the single-repo convenience (first match by `id`).
    pub fn item_for(&self, repo: &str, id: &str) -> Option<&QueueItem> {
        self.queue.iter().find(|i| i.repo == repo && i.id == id)
    }

    /// Mutable [`item_for`](Self::item_for): find an item by its `(repo, id)` key (#453).
    pub fn item_mut_for(&mut self, repo: &str, id: &str) -> Option<&mut QueueItem> {
        self.queue.iter_mut().find(|i| i.repo == repo && i.id == id)
    }

    /// Mark the item at `idx` dispatched onto `branch`.
    pub fn mark_dispatched(&mut self, idx: usize, branch: Option<String>, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Dispatched;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            it.branch = branch;
            it.dispatched_at = now;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Mark the item at `idx` dispatched to a **remote** runner (#377): in-flight, substrate pinned
    /// to [`Substrate::Remote`], no local branch/worktree (the GitHub-hosted run owns its own
    /// branch). Counts against no local slot ([`local_in_flight`](Self::local_in_flight) excludes
    /// it) and is reconciled by PR/issue completion, not the tmux pane machine.
    pub fn mark_dispatched_remote(&mut self, idx: usize, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Dispatched;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            it.substrate = Substrate::Remote;
            it.dispatched_at = now;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Record the harness (`copilot` / `claude`) the item at `idx` was dispatched under — surfaced
    /// by `bn orchestrate status` so a human/cockpit knows what's running in the window (#372).
    /// Kept separate from [`mark_dispatched`] so the lifecycle transition stays harness-agnostic.
    pub fn set_harness(&mut self, idx: usize, harness: &str, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.harness = Some(harness.to_string());
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Mark the item at `idx` dispatched onto a **remote SSH worker node** (#729): in-flight, with
    /// the `node_id` it landed on and the `request_id`/`attempt_id` the remote `bn worker start` was
    /// admitted under (#720) persisted on this (control-plane) side, mirroring the worker's own
    /// registry. `worker_state` starts `QUEUED` (the admission verdict) and is refreshed by the
    /// liveness poll. Unlike [`mark_dispatched_remote`] this stays a *local*-substrate item (it
    /// occupies a placement slot and is reconciled via `bn worker status`, not a GitHub PR).
    pub fn mark_dispatched_worker(
        &mut self,
        idx: usize,
        node_id: &str,
        request_id: &str,
        attempt_id: &str,
        branch: Option<String>,
        now: i64,
    ) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Dispatched;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            it.branch = branch;
            it.node = Some(node_id.to_string());
            it.request_id = Some(request_id.to_string());
            it.attempt_id = Some(attempt_id.to_string());
            it.worker_state = Some("QUEUED".to_string());
            it.dispatched_at = now;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Persist stable identity before a dispatch side effect. Returns `true` only when this call
    /// moved a pending queued item into `claimed`; repeated calls leave the original identity intact.
    pub fn claim_dispatch(
        &mut self,
        idx: usize,
        request_id: &str,
        attempt_id: &str,
        now: i64,
    ) -> bool {
        let Some(it) = self.queue.get_mut(idx) else {
            return false;
        };
        if !it.is_dispatchable() {
            return false;
        }
        it.dispatch_phase = DispatchPhase::Claimed;
        if it.request_id.is_none() {
            it.request_id = Some(request_id.to_string());
        }
        if it.attempt_id.is_none() {
            it.attempt_id = Some(attempt_id.to_string());
        }
        it.updated_at = now;
        self.updated_at = now;
        true
    }

    /// Resolve a durable claim after observing whether its launch exists. An observed launch
    /// promotes the item into the normal dispatched lifecycle; an absent launch returns it to
    /// pending while retaining the stable IDs for an idempotent retry.
    pub fn reconcile_claim(
        &mut self,
        idx: usize,
        launched: bool,
        branch: Option<String>,
        now: i64,
    ) -> bool {
        let Some(it) = self.queue.get_mut(idx) else {
            return false;
        };
        if it.status != ItemStatus::Queued || it.dispatch_phase != DispatchPhase::Claimed {
            return false;
        }
        if launched {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Dispatched;
            it.dispatch_phase = DispatchPhase::Dispatched;
            if branch.is_some() {
                it.branch = branch;
            }
            it.dispatched_at = now;
        } else {
            it.dispatch_phase = DispatchPhase::Pending;
            it.claimed_remote = false;
        }
        it.updated_at = now;
        self.updated_at = now;
        true
    }

    /// Update the last-observed remote worker attempt state for item `id` (#729) — the liveness
    /// poll folds `bn worker status --json` (`RUNNING`/`SUCCEEDED`/`FAILED`) or, when the node is
    /// unreachable, `UNKNOWN` (which blocks redispatch until reconciled) into the control-plane
    /// mirror. No-op for an item with no remote attempt.
    pub fn set_worker_state(&mut self, id: &str, state: &str, now: i64) {
        if let Some(idx) = self.queue.iter().position(|i| i.id == id) {
            self.set_worker_state_at(idx, state, now);
        }
    }

    /// Index-addressed worker-state update for callers that already resolved the collision-free
    /// `(repo, id)` key.
    pub fn set_worker_state_at(&mut self, idx: usize, state: &str, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.worker_state = Some(state.to_string());
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// The harness the daemon dispatches new agents under (`bn orchestrate --agent`, #382), or
    /// [`DEFAULT_HARNESS`] (`copilot`) when unset — the back-compat default so a pre-#382 run keeps
    /// dispatching Copilot exactly as before.
    pub fn dispatch_harness(&self) -> &str {
        self.harness.as_deref().unwrap_or(DEFAULT_HARNESS)
    }

    /// Fall item `id` over to `harness` after a usage-limit stall (#382): record the new per-item
    /// harness (so status/relaunch use it) and stamp `last_switch_at` for the cooldown gate. Called
    /// after the in-place `bn switch` has respawned the lane's pane under the other harness.
    pub fn switch_harness(&mut self, id: &str, harness: &str, now: i64) {
        if let Some(it) = self.item_mut(id) {
            it.reset_stuck_tracking();
            it.harness = Some(harness.to_string());
            it.last_switch_at = now;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// The tmux `session:window` target to jump to an item's agent (`tmux switch-client -t …`), or
    /// `None` until the item is dispatched. The session is the **item's** repo — how `bn dispatch`
    /// opens the window (`<repo>:<sanitized-branch>`) — so status points straight at the running
    /// agent even in a multi-repo run (#453); a single-repo item's `repo` equals `self.repo`, so the
    /// target is unchanged. Falls back to `self.repo` for a legacy item whose `repo` is unset.
    pub fn tmux_target(&self, item: &QueueItem) -> Option<String> {
        let repo = if item.repo.is_empty() {
            self.repo.as_str()
        } else {
            item.repo.as_str()
        };
        item.tmux_window().map(|w| format!("{}:{}", repo, w))
    }

    /// Mark the item merged (terminal success), stamping its PR if known.
    pub fn mark_merged(&mut self, id: &str, pr: Option<String>, now: i64) {
        if let Some(it) = self.item_mut(id) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Merged;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            if pr.is_some() {
                it.pr = pr;
            }
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Mark the item failed (terminal — surfaced for a human).
    pub fn mark_failed(&mut self, id: &str, now: i64) {
        if let Some(idx) = self.queue.iter().position(|i| i.id == id) {
            self.mark_failed_at(idx, now);
        }
    }

    /// Index-addressed terminal failure for callers that already resolved `(repo, id)`.
    pub fn mark_failed_at(&mut self, idx: usize, now: i64) {
        if let Some(it) = self.queue.get_mut(idx) {
            it.reset_stuck_tracking();
            it.status = ItemStatus::Failed;
            it.dispatch_phase = DispatchPhase::Dispatched;
            it.claimed_remote = false;
            it.updated_at = now;
        }
        self.updated_at = now;
    }

    /// Restore a local dispatched/failed lane to a fresh in-flight state after the CLI has safely
    /// recovered its WIP and re-seeded the agent process. Selection accepts either stable item id or
    /// dispatched branch, while rejecting lifecycle states and execution targets that have no local
    /// tmux/worktree lane to replace.
    pub fn reset_lane(&mut self, lane: &str, now: i64) -> Result<LaneReset, LaneResetError> {
        let matches: Vec<usize> = self
            .queue
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                (item.id == lane || item.branch.as_deref() == Some(lane)).then_some(idx)
            })
            .collect();
        let idx = match matches.as_slice() {
            [] => return Err(LaneResetError::Missing(lane.to_string())),
            [idx] => *idx,
            _ => return Err(LaneResetError::Ambiguous(lane.to_string())),
        };

        let item = &mut self.queue[idx];
        match item.status {
            ItemStatus::Queued => return Err(LaneResetError::Queued(lane.to_string())),
            ItemStatus::Merged => return Err(LaneResetError::Merged(lane.to_string())),
            ItemStatus::Dispatched | ItemStatus::Failed => {}
        }
        if item.substrate == Substrate::Remote || !item.target.is_local() || item.node.is_some() {
            return Err(LaneResetError::NonLocal(lane.to_string()));
        }
        let Some(branch) = item.branch.clone() else {
            return Err(LaneResetError::MissingBranch(lane.to_string()));
        };
        let reset = LaneReset {
            id: item.id.clone(),
            item: item.item.clone(),
            repo: item.repo.clone(),
            branch,
            harness: item.harness.clone(),
            work_class: item.work_class,
            previous_status: item.status,
        };
        let observed = item.last_state.unwrap_or(PaneState::Dead);
        item.status = ItemStatus::Dispatched;
        item.dispatch_phase = DispatchPhase::Dispatched;
        item.claimed_remote = false;
        item.attempts = 0;
        item.last_relaunch_at = 0;
        item.last_state = None;
        item.reset_stuck_tracking();
        item.updated_at = now;
        self.record_recovery(
            &reset.id,
            observed,
            SupervisorAction::RelaunchToResume,
            "manual lane reset: fresh agent re-seeded",
            now,
        );
        self.updated_at = now;
        Ok(reset)
    }

    /// Route a scout/ask item's answer back and complete it (#378) — the pure terminal transition
    /// for the "delegate-for-answers" lane. Records the harvested findings `path` + the `question`
    /// on the item and flips it to [`ItemStatus::Merged`] (terminal **success**): a scout produces
    /// findings, not a PR, so answer-harvest *is* its completion — there is no PR to wait for and no
    /// relaunch to resume. Applies only to a [`WorkClass::Research`] item that hasn't already been
    /// answered, so it's idempotent (a re-harvest each poll is a no-op) and never fabricates an
    /// answer for a `ship`. Returns whether it applied. The CLI does the note-dir file IO to find
    /// `path`, then calls this to persist the outcome on the state file `answers`/`--wait` read.
    pub fn record_answer(&mut self, id: &str, path: &str, question: &str, now: i64) -> bool {
        let applied = if let Some(it) = self.item_mut(id) {
            if it.work_class != WorkClass::Research || it.answer.is_some() {
                false
            } else {
                it.reset_stuck_tracking();
                it.answer = Some(Answer {
                    question: question.to_string(),
                    path: path.to_string(),
                    harvested_at: now,
                });
                it.status = ItemStatus::Merged;
                it.updated_at = now;
                true
            }
        } else {
            false
        };
        if applied {
            self.updated_at = now;
        }
        applied
    }

    /// Record a relaunch attempt on the item, returning the new attempt count (for the backoff).
    pub fn bump_attempts(&mut self, id: &str, now: i64) -> u32 {
        let Some(idx) = self.queue.iter().position(|i| i.id == id) else {
            self.updated_at = now;
            return 0;
        };
        self.bump_attempts_at(idx, now)
    }

    /// Index-addressed relaunch accounting for callers that already resolved `(repo, id)`.
    pub fn bump_attempts_at(&mut self, idx: usize, now: i64) -> u32 {
        let n = if let Some(it) = self.queue.get_mut(idx) {
            it.attempts += 1;
            it.last_relaunch_at = now;
            it.updated_at = now;
            it.attempts
        } else {
            0
        };
        self.updated_at = now;
        n
    }

    /// Note the last observed pane state for an item (for status).
    pub fn set_state(&mut self, id: &str, state: PaneState, now: i64) {
        if let Some(it) = self.item_mut(id) {
            if it.last_state != Some(state) {
                it.last_state = Some(state);
                it.updated_at = now;
            }
        }
    }

    /// Fold one pane/transcript observation into the lane's durable no-change clock. Returns the
    /// seconds since either source last changed.
    pub fn observe_lane_activity(
        &mut self,
        id: &str,
        fingerprint: u64,
        transcript_activity_at: Option<i64>,
        now: i64,
    ) -> i64 {
        let Some(item) = self.item_mut(id) else {
            return 0;
        };
        if item.pane_activity_fingerprint != Some(fingerprint) {
            item.pane_activity_fingerprint = Some(fingerprint);
            item.pane_activity_changed_at = now;
            item.updated_at = now;
        } else if item.pane_activity_changed_at == 0 {
            item.pane_activity_changed_at = now;
            item.updated_at = now;
        }
        let latest = transcript_activity_at
            .unwrap_or(0)
            .max(item.pane_activity_changed_at)
            .max(item.dispatched_at);
        now.saturating_sub(latest)
    }

    /// Update the current stuck state. Clearing a state also clears its notification key so the
    /// same condition can wake the controller again after a genuine recovery.
    pub fn set_stuck_lane(&mut self, id: &str, stuck: Option<crate::agent::StuckLane>, now: i64) {
        let Some(item) = self.item_mut(id) else {
            return;
        };
        if item.stuck == stuck {
            return;
        }
        item.stuck = stuck;
        if item.stuck.is_none() {
            item.stuck_notified_key = None;
        }
        item.updated_at = now;
    }

    /// Record successful back-channel delivery for the lane's current stuck state.
    pub fn mark_stuck_notified(&mut self, id: &str, key: String, now: i64) {
        if let Some(item) = self.item_mut(id) {
            if item.stuck_notified_key.as_deref() != Some(key.as_str()) {
                item.stuck_notified_key = Some(key);
                item.updated_at = now;
            }
        }
    }

    /// Append a recovery/lifecycle event to the audit trail.
    pub fn record_recovery(
        &mut self,
        id: &str,
        state: PaneState,
        action: SupervisorAction,
        note: &str,
        now: i64,
    ) {
        self.recovery.push(RecoveryEvent {
            ts: now,
            id: id.to_string(),
            state,
            action,
            note: note.to_string(),
        });
        self.updated_at = now;
    }

    // -----------------------------------------------------------------------
    // Durability at scale (#380) — pure state-trimming / eviction / boot-reset
    // -----------------------------------------------------------------------

    /// Cap the in-state recovery trail to its most recent `cap` events (#380) — a pure ring-buffer
    /// trim that drops the oldest overflow from the front. Keeps the per-tick atomic state rewrite
    /// bounded no matter how long the run has been recovering agents; the complete trail is
    /// preserved in `events.jsonl`. Returns how many events were dropped (0 if already within cap).
    pub fn trim_recovery(&mut self, cap: usize) -> usize {
        if self.recovery.len() <= cap {
            return 0;
        }
        let drop = self.recovery.len() - cap;
        self.recovery.drain(0..drop);
        drop
    }

    /// The terminal items eligible for eviction to `run-history.jsonl` (#380): merged/failed and
    /// untouched for at least `older_than_secs`. Pure read — the CLI snapshots these to the history
    /// log *before* calling [`evict_terminal`] to remove them, so an outcome is durable before it
    /// leaves the queue. Shares [`is_evictable`] with the removal so the two sets never diverge.
    pub fn evictable_terminal(&self, now: i64, older_than_secs: i64) -> Vec<&QueueItem> {
        self.queue
            .iter()
            .filter(|i| is_evictable(i, now, older_than_secs))
            .collect()
    }

    /// Remove the terminal items older than `older_than_secs` from the live `queue` (#380) — the
    /// eviction other half of [`evictable_terminal`]. Returns how many were removed. Bounds the
    /// state so a 500-item run's per-tick rewrite stays O(in-flight + recent) instead of carrying
    /// every landed item for the life of the daemon. Idempotent: nothing old ⇒ no removal, no write.
    pub fn evict_terminal(&mut self, now: i64, older_than_secs: i64) -> usize {
        let before = self.queue.len();
        self.queue
            .retain(|i| !is_evictable(i, now, older_than_secs));
        let removed = before - self.queue.len();
        if removed > 0 {
            self.updated_at = now;
        }
        removed
    }

    /// Whether `current` marks a **fresh boot** relative to the recorded [`boot_id`](Self::boot_id)
    /// (#380): we have a previously-recorded marker and it differs. A first-ever run (`boot_id ==
    /// None`) returns `false` — there were no in-flight windows a reboot could have killed yet — so
    /// the daemon merely records the marker on its first pass and only resets backoff on a *later*
    /// boot change. Pure over the caller-supplied marker (the CLI reads it from the OS).
    pub fn is_fresh_boot(&self, current: &str) -> bool {
        self.boot_id.as_deref().is_some_and(|prev| prev != current)
    }

    /// Adopt the current boot marker (#380) so the next pass compares against it. Idempotent — a
    /// no-op (no `updated_at` bump) when it already matches.
    pub fn record_boot(&mut self, current: &str, now: i64) {
        if self.boot_id.as_deref() != Some(current) {
            self.boot_id = Some(current.to_string());
            self.updated_at = now;
        }
    }

    /// The in-flight item ids eligible for a boot-triggered backoff reset (#380): dispatched items
    /// that have made ≥1 relaunch attempt and whose worktree still exists (`worktree_live`, supplied
    /// by the CLI — the filesystem check is the executor's job). After a reboot every window is gone
    /// so the fleet will re-dispatch these `Dead` items; without a reset each re-dispatch burns an
    /// `attempt` toward [`Backoff::max_attempts`] for a machine event, eventually marking a healthy
    /// item `failed`. Pure over the live-worktree set, so the policy is unit-testable off-disk.
    pub fn boot_reset_candidates(
        &self,
        worktree_live: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        self.queue
            .iter()
            .filter(|i| i.status.in_flight() && i.attempts > 0 && worktree_live.contains(&i.id))
            .map(|i| i.id.clone())
            .collect()
    }

    /// Reset the relaunch backoff for the given item ids (#380): zero `attempts` and
    /// `last_relaunch_at` on each still-in-flight item, so a reboot re-dispatch begins a fresh
    /// backoff curve rather than inheriting a burned one. Returns the ids actually reset (those
    /// in-flight with attempts made). Paired with [`boot_reset_candidates`](Self::boot_reset_candidates).
    pub fn reset_backoff(&mut self, ids: &[String], now: i64) -> Vec<String> {
        let mut reset = Vec::new();
        for id in ids {
            if let Some(it) = self.item_mut(id) {
                if it.status.in_flight() && it.attempts > 0 {
                    it.attempts = 0;
                    it.last_relaunch_at = 0;
                    it.updated_at = now;
                    reset.push(id.clone());
                }
            }
        }
        if !reset.is_empty() {
            self.updated_at = now;
        }
        reset
    }

    /// Deserialize from JSON text (for load / tests).
    pub fn from_json(s: &str) -> Option<OrchestrateState> {
        let mut st: OrchestrateState = serde_json::from_str(s).ok()?;
        // #453: backfill the per-item `repo` on any pre-#453 state (items persisted without a
        // `repo` key), so every item carries its half of the `(repo, id)` key and a legacy
        // single-repo run keeps behaving exactly as before — the run's `repo` is the queue's repo.
        let repo = st.repo.clone();
        for it in &mut st.queue {
            if it.repo.is_empty() {
                it.repo = repo.clone();
            }
            if it.status != ItemStatus::Queued && it.dispatch_phase == DispatchPhase::Pending {
                it.dispatch_phase = DispatchPhase::Dispatched;
            }
        }
        Some(st)
    }

    /// Serialize to pretty JSON text.
    pub fn to_json(&self) -> std::io::Result<String> {
        crate::store::to_json_pretty(self)
    }

    /// Load the run state from `path`, or `None` if absent/unparseable.
    pub fn load(path: &Path) -> Option<OrchestrateState> {
        let s = std::fs::read_to_string(path).ok()?;
        OrchestrateState::from_json(&s)
    }

    /// Atomically persist to `path` (temp sibling + rename), creating the parent dir on demand — so
    /// a concurrent `bn orchestrate status` never reads a torn write.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::save_json_atomic(path, self)
    }

    // -----------------------------------------------------------------------
    // Web write-layer mutations (#392)
    // -----------------------------------------------------------------------

    /// Remove a queued item by id, returning whether one was removed. Only [`ItemStatus::Queued`]
    /// items can be dropped — an in-flight (dispatched) item owns a running agent and can't be
    /// silently forgotten (kill it instead), so this is a no-op for anything past `Queued`.
    pub fn remove_queued(&mut self, id: &str, now: i64) -> bool {
        let before = self.queue.len();
        self.queue.retain(|i| !(i.id == id && i.is_dispatchable()));
        let removed = self.queue.len() != before;
        if removed {
            self.updated_at = now;
        }
        removed
    }

    /// Reorder the **queued** subset of the queue to follow `ids` (ids not present are ignored;
    /// queued items missing from `ids` keep their relative order after the listed ones). Only the
    /// `Queued` slots are permuted — dispatched/merged/failed items keep their positions, so a
    /// reorder never disturbs an in-flight wave's lifecycle. Returns whether the order changed.
    pub fn reorder_queued(&mut self, ids: &[String], now: i64) -> bool {
        // The positions (indices into `self.queue`) currently holding a queued item.
        let slots: Vec<usize> = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, i)| i.is_dispatchable())
            .map(|(idx, _)| idx)
            .collect();
        // Desired order of queued items: those named in `ids` first (in that order), then any
        // queued item not named, in its existing order — so a partial list is a safe "bring these
        // to the front" rather than a drop.
        let mut desired: Vec<usize> = Vec::with_capacity(slots.len());
        for id in ids {
            if let Some(&idx) = slots
                .iter()
                .find(|&&idx| &self.queue[idx].id == id && !desired.contains(&idx))
            {
                desired.push(idx);
            }
        }
        for &idx in &slots {
            if !desired.contains(&idx) {
                desired.push(idx);
            }
        }
        if desired == slots {
            return false; // already in this order — no write
        }
        // Rebuild: pull the queued items out in the desired order and drop them back into the same
        // physical slots, leaving every non-queued item exactly where it was.
        let reordered: Vec<QueueItem> =
            desired.iter().map(|&idx| self.queue[idx].clone()).collect();
        for (slot, item) in slots.iter().zip(reordered) {
            self.queue[*slot] = item;
        }
        self.updated_at = now;
        true
    }

    /// Fold a daemon's post-tick copy (`worked`) back onto `self` (the *current* on-disk state,
    /// which may carry web edits made while the tick ran) — the merge half of the #392 "lock/merge"
    /// fix. `self` stays authoritative for run-level policy a web edit owns (`concurrency`, `solo`,
    /// `stopped`) and for queue **membership/order** (adds, removes, reorders); `worked` is
    /// authoritative for per-item lifecycle progress the daemon just made (dispatched / merged /
    /// failed / attempts / observed state) and for the recovery trail. Because the web never mutates
    /// an item's internal fields, any per-id difference is daemon-authored and safe to take.
    pub fn merge_worked(&mut self, worked: &OrchestrateState, now: i64) {
        for w in &worked.queue {
            match self
                .queue
                .iter_mut()
                .find(|d| d.repo == w.repo && d.id == w.id)
            {
                // Present in both: take the daemon's lifecycle if it differs (only it edits items).
                Some(d) => {
                    if d != w {
                        // #712: a claim/finalize/reconcile write is persisted during the tick,
                        // outside this stale `worked` snapshot. Never let a second daemon's older
                        // copy roll that transaction backward (pending over claimed, claimed over
                        // dispatched, or a different attempt). Equal timestamps are possible
                        // because the state clock is second-granularity, so phase/identity are the
                        // compare-and-set signal and `updated_at` only rejects genuinely older disk.
                        let dispatch_transaction_changed =
                            d.attempt_id != w.attempt_id || d.dispatch_phase != w.dispatch_phase;
                        if dispatch_transaction_changed && d.updated_at >= w.updated_at {
                            continue;
                        }
                        *d = w.clone();
                    }
                }
                // Only in `worked`: the web removed it mid-tick. Respect the removal for a still
                // -queued item, but re-adopt one the daemon has taken in-flight/terminal — dropping
                // that would orphan a running agent (an in-flight item can't be queue-removed).
                None => {
                    if w.status != ItemStatus::Queued {
                        self.queue.push(w.clone());
                    }
                }
            }
        }
        // The daemon is the sole writer of the recovery trail, so `worked.recovery` is a superset of
        // `self`'s — take it wholesale rather than diffing.
        if worked.recovery.len() >= self.recovery.len() {
            self.recovery = worked.recovery.clone();
        }
        // The binding cap (#377) is daemon-authored each pass and the web never edits it — take the
        // tick's value so `status` reflects the reason dispatch was capped/paused on the last pass.
        self.dispatch_bind = worked.dispatch_bind.clone();
        self.dispatch_reason = worked.dispatch_reason.clone();
        self.dispatch_decision = worked.dispatch_decision.clone();
        self.updated_at = now;
    }

    /// Record why local dispatch was capped/paused this pass (#377) — the binding-cap label, or
    /// `None` when nothing bound. Set by the CLI executor after [`crate::scheduler::pick`] so
    /// `bn orchestrate status` can show it. Only bumps `updated_at` on a real change.
    pub fn set_dispatch_bind(&mut self, label: Option<&str>, now: i64) {
        let next = label.map(str::to_string);
        if self.dispatch_bind != next {
            self.dispatch_bind = next;
            self.updated_at = now;
        }
    }

    /// Record the concise result of a pass that launched no work. `None` means the pass dispatched
    /// at least one item. Only bumps `updated_at` when the operator-facing reason changes.
    pub fn set_dispatch_reason(&mut self, reason: Option<&str>, now: i64) {
        let next = reason.map(str::to_string);
        if self.dispatch_reason != next {
            self.dispatch_reason = next;
            self.updated_at = now;
        }
    }

    /// Record the evidence from the latest scheduler pass.
    pub fn set_dispatch_decision(&mut self, decision: DispatchDecision, now: i64) {
        if self.dispatch_decision.as_ref() != Some(&decision) {
            self.dispatch_decision = Some(decision);
            self.updated_at = now;
        }
    }
}

// ===========================================================================
// Live monitoring rollups (#379) — pure throughput / ETA / recovery math
// ===========================================================================

/// The default trailing window the "recent" fleet signals (throughput, recovery counts) cover.
/// One hour: `merged_per_hour` reads directly off it, and it's short enough that the numbers track
/// the *current* wave rather than the whole run's history.
pub const ROLLUP_WINDOW_SECS: i64 = 3600;

/// Fleet-level monitoring rollups derived **purely** from the timestamped orchestrate lifecycle
/// events (`orchestrate_dispatch` / `_merged` / `_relaunch` / `_failed` in `events.jsonl`) plus a
/// few scalars the caller reads off the live run (queue depth, in-flight, effective cap). The math
/// is pure over its inputs — no disk, no clock beyond the `now` passed in — so throughput and ETA
/// are unit-testable against synthetic event logs (#379). Cost / token burn is layered on at the
/// CLI (it needs the #381 `cost::rollup` disk surface), so it rides in the payload block, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Rollups {
    /// Everything still waiting to land: run-queued + durable-backlog pending (#375).
    pub queue_depth: usize,
    /// Items occupying an in-flight concurrency slot right now.
    pub in_flight: usize,
    /// The effective concurrency cap the ETA divides by (solo ⇒ 1).
    pub effective_cap: usize,
    /// `orchestrate_merged` events in the trailing window — landings this hour.
    pub merged_recent: usize,
    /// Throughput as merged/hour: the recent count normalized to a per-hour rate.
    pub merged_per_hour: f64,
    /// Mean dispatch→merged cycle time over every paired (dispatch, merged) on the same branch,
    /// across all history; `None` until at least one item has landed.
    pub avg_cycle_secs: Option<i64>,
    /// Backlog burn-down estimate: `queue_depth × avg_cycle / effective_cap`, in seconds. `None`
    /// while no cycle time is known (nothing has landed yet) — an honest "unknown", never a fake 0.
    pub eta_secs: Option<i64>,
    /// `orchestrate_relaunch` events in the trailing window — recent recovery activity.
    pub relaunch_recent: usize,
    /// `orchestrate_failed` events in the trailing window — recent give-ups needing a human.
    pub failed_recent: usize,
    /// The trailing window the `*_recent` counts cover, in seconds (echoed for the renderer).
    pub window_secs: i64,
}

impl Rollups {
    /// Compute the rollups from the repo's lifecycle `events`, the live run scalars, and `now`.
    /// `window_secs` is the trailing window for the throughput / recovery counts (see
    /// [`ROLLUP_WINDOW_SECS`]); events for other repos are ignored via their `repo` field.
    ///
    /// Cycle times fold **all** history (a stable estimate), while the `*_recent` counts honor the
    /// window — so `avg_cycle_secs` stays meaningful even after a quiet hour. Pure: same inputs ⇒
    /// same output, no I/O.
    pub fn compute(
        events: &[crate::events::Event],
        repo: &str,
        queue_depth: usize,
        in_flight: usize,
        effective_cap: usize,
        window_secs: i64,
        now: i64,
    ) -> Rollups {
        let window = window_secs.max(1);
        let cutoff = now - window;
        let cap = effective_cap.max(1);

        let field = |e: &crate::events::Event, k: &str| -> Option<String> {
            e.fields.get(k).and_then(|v| v.as_str()).map(str::to_string)
        };

        // Only this repo's events, oldest-first, so each branch's dispatch anchors the merge that
        // follows it. `orchestrate_merged`/`_dispatch`/`_relaunch`/`_failed` all carry a `repo`.
        let mut chron: Vec<&crate::events::Event> = events
            .iter()
            .filter(|e| field(e, "repo").as_deref() == Some(repo))
            .collect();
        chron.sort_by_key(|e| e.ts);

        // Fold dispatch→merged pairs on the same branch → cycle times. Remember each branch's most
        // recent dispatch ts; when it merges, that pair's elapsed time is one cycle sample.
        let mut dispatched_at: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        let mut cycles: Vec<i64> = Vec::new();
        let (mut merged_recent, mut relaunch_recent, mut failed_recent) = (0usize, 0usize, 0usize);
        for e in &chron {
            match e.kind.as_str() {
                "orchestrate_dispatch" => {
                    if let Some(b) = field(e, "branch") {
                        dispatched_at.insert(b, e.ts);
                    }
                }
                "orchestrate_merged" => {
                    if let Some(d) = field(e, "branch").and_then(|b| dispatched_at.remove(&b)) {
                        if e.ts >= d {
                            cycles.push(e.ts - d);
                        }
                    }
                    if e.ts >= cutoff {
                        merged_recent += 1;
                    }
                }
                "orchestrate_relaunch" if e.ts >= cutoff => relaunch_recent += 1,
                "orchestrate_failed" if e.ts >= cutoff => failed_recent += 1,
                _ => {}
            }
        }

        let avg_cycle_secs = (!cycles.is_empty())
            .then(|| (cycles.iter().sum::<i64>() as f64 / cycles.len() as f64).round() as i64);
        let merged_per_hour = merged_recent as f64 * 3600.0 / window as f64;
        // depth × avg_cycle / cap — the backlog burn-down at the observed cycle time. Zero once the
        // queue is empty; unknown until a first cycle is measured.
        let eta_secs = avg_cycle_secs.map(|c| {
            if queue_depth == 0 {
                0
            } else {
                (c as f64 * queue_depth as f64 / cap as f64).round() as i64
            }
        });

        Rollups {
            queue_depth,
            in_flight,
            effective_cap: cap,
            merged_recent,
            merged_per_hour,
            avg_cycle_secs,
            eta_secs,
            relaunch_recent,
            failed_recent,
            window_secs: window,
        }
    }
}

/// Locked read-modify-write of a run's state file — the atomic primitive #392 shares between the
/// web write layer and the daemon's tick so a concurrent edit is never clobbered. Acquires the
/// required [`crate::store::FileLock`] on `path`, loads the current state, applies `f`, stamps
/// `updated_at`, and atomically saves. Returns the closure's value, or `None` when no run exists at
/// `path` (nothing to mutate). Lock and save failures are returned rather than permitting an
/// unlocked mutation. The lock is held only for the (fast) in-memory mutation — never across a
/// subprocess — so contention windows stay tiny.
pub fn mutate<R>(
    path: &Path,
    f: impl FnOnce(&mut OrchestrateState) -> R,
) -> std::io::Result<Option<R>> {
    let _lock = crate::store::FileLock::acquire_required(path)?;
    let Some(mut st) = OrchestrateState::load(path) else {
        return Ok(None);
    };
    let out = f(&mut st);
    st.updated_at = crate::util::now_epoch_secs();
    st.save(path)?;
    Ok(Some(out))
}
//
// One state file **per repo** — `orchestrate-<repo>.json` — so two concurrent orchestrations
// (different repos) never share a file and a second `--queue` can never overwrite a different live
// run. The pre-#395 layout was a single machine-wide `orchestrate.json`; [`migrate_legacy_state`]
// moves it to its per-repo path on first contact so an in-flight wave survives the upgrade.

/// Sanitize a repo name into a filename/launchd-label-safe run key: `[A-Za-z0-9._-]` pass through,
/// everything else becomes `-`. Empty input degrades to `"run"` so a path/label is always valid.
pub fn sanitize_run_key(repo: &str) -> String {
    let key: String = repo
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let key = key.trim_matches('-').to_string();
    if key.is_empty() {
        "run".to_string()
    } else {
        key
    }
}

/// The state file name for a repo's run: `orchestrate-<key>.json`.
pub fn state_file_name(repo: &str) -> String {
    format!("orchestrate-{}.json", sanitize_run_key(repo))
}

/// The canonical on-disk path for a repo's orchestration run state (#395: keyed per repo, so
/// concurrent runs in different repos never collide).
pub fn state_path_for(repo: &str) -> PathBuf {
    crate::resolver::state_dir().join(state_file_name(repo))
}

/// The single-controller owner-lease path for a repo's run, beside its state file. Holds the
/// [`ControllerOwner`] currently driving this repo so a second daemon defers instead of
/// double-driving (see [`may_claim_owner`]).
pub fn owner_path_for(repo: &str) -> PathBuf {
    crate::resolver::state_dir().join(format!("orchestrate-{}.owner.json", sanitize_run_key(repo)))
}

/// The owner lease currently recorded for `repo`, if any. An absent or unparseable file reads as
/// `None` (⇒ claimable), keeping the guard fail-open.
pub fn read_owner(repo: &str) -> Option<ControllerOwner> {
    let raw = std::fs::read_to_string(owner_path_for(repo)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist `owner` as `repo`'s controller lease (best-effort — a write failure just means the next
/// tick retries; a torn read fails open to claimable).
pub fn write_owner(repo: &str, owner: &ControllerOwner) {
    crate::util::best_effort(
        crate::store::save_json_atomic(&owner_path_for(repo), owner),
        "orchestrate: write owner lease",
    );
}

/// Remove `repo`'s owner lease iff it is still `me`'s — never steal another controller's lease.
pub fn clear_owner(repo: &str, me: &ControllerOwner) {
    if read_owner(repo).is_some_and(|cur| cur.pid == me.pid && cur.host == me.host) {
        let _ = std::fs::remove_file(owner_path_for(repo));
    }
}

/// The pre-#395 machine-wide state file. Only read for migration — never written anymore.
pub fn legacy_state_path() -> PathBuf {
    crate::resolver::state_dir().join("orchestrate.json")
}

/// Every per-run state file in `dir` (`orchestrate-*.json`), sorted for deterministic listing.
/// Excludes the legacy `orchestrate.json` and atomic-save `*.json.tmp` siblings.
pub fn run_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("orchestrate-") && n.ends_with(".json"))
        })
        .collect();
    files.sort();
    files
}

/// Every per-run state file in the state dir.
pub fn run_paths() -> Vec<PathBuf> {
    run_files_in(&crate::resolver::state_dir())
}

// ---------------------------------------------------------------------------
// Run history log (#380) — append-only per-item outcome trail in the state dir
// ---------------------------------------------------------------------------

/// The append-only run-history log path: `run-history.jsonl` in the state dir (#380). One line per
/// evicted terminal item ([`HistoryRecord`]); records carry their `repo`, so a single machine-wide
/// log serves every run and readers filter by repo (via [`summarize_history`]). Sibling to the
/// per-run `orchestrate-<repo>.json` state files and `events.jsonl`.
pub fn history_path() -> PathBuf {
    crate::resolver::state_dir().join("run-history.jsonl")
}

/// Append history `records` as JSONL lines to `path`, creating the parent dir on demand (#380).
/// Append-only: the durable outcome trail that outlives the live queue and a state overwrite. Each
/// line is one [`HistoryRecord::to_jsonl`]; a torn line is skipped on read, never poisoning the log.
pub fn append_history_at(path: &Path, records: &[HistoryRecord]) -> std::io::Result<()> {
    use std::io::Write;
    if records.is_empty() {
        return Ok(());
    }
    let lines = records
        .iter()
        .map(HistoryRecord::to_jsonl)
        .collect::<std::io::Result<Vec<_>>>()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for line in lines {
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// Read every history record from `path`, in file order (#380). Unparseable lines are skipped, and a
/// missing file reads as empty — a best-effort, never-panicking read of the append-only log.
pub fn read_history_at(path: &Path) -> Vec<HistoryRecord> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| HistoryRecord::from_jsonl(&l))
        .collect()
}

/// Append history records to the state dir's [`history_path`].
pub fn append_history(records: &[HistoryRecord]) -> std::io::Result<()> {
    append_history_at(&history_path(), records)
}

/// Read every history record from the state dir's [`history_path`].
pub fn read_history() -> Vec<HistoryRecord> {
    read_history_at(&history_path())
}

/// Migrate the legacy machine-wide `orchestrate.json` in `dir` to its per-repo path, preserving an
/// in-flight wave across the #395 upgrade. If the per-repo file already exists (a straggling save
/// from the old daemon racing the new layout), the copy with the **newer `updated_at`** wins.
/// Returns the migrated run's `(repo, path)`, or `None` when there is nothing to migrate.
pub fn migrate_legacy_state_in(dir: &Path) -> Option<(String, PathBuf)> {
    let legacy = dir.join("orchestrate.json");
    let st = OrchestrateState::load(&legacy)?;
    let dest = dir.join(state_file_name(&st.repo));
    let keep_legacy = match OrchestrateState::load(&dest) {
        Some(existing) => st.updated_at > existing.updated_at,
        None => true,
    };
    if keep_legacy && st.save(&dest).is_err() {
        return None; // couldn't persist the migrated copy — leave the legacy file untouched
    }
    let _ = std::fs::remove_file(&legacy);
    Some((st.repo, dest))
}

/// [`migrate_legacy_state_in`] against the state dir.
pub fn migrate_legacy_state() -> Option<(String, PathBuf)> {
    migrate_legacy_state_in(&crate::resolver::state_dir())
}










