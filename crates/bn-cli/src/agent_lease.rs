//! CLI bridge between the durable core lease registry and tmux process/window liveness.

use bn_core::agent::{
    agent_lease_registry_path, begin_agent_lease, load_agent_leases, reconcile_agent_leases,
    release_agent_lease, AgentLease, AgentLeaseAcquireMode, AgentLeaseBegin, AgentLeaseKey,
    AgentLeaseKind, AgentLeaseRemovalReason, AgentLeaseReservation as CoreAgentLeaseReservation,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) struct AgentLeaseRequest<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
    pub worktree: &'a Path,
    pub session_id: &'a str,
    pub harness: &'a str,
    pub kind: AgentLeaseKind,
    pub tmux_target: &'a str,
    pub replace: bool,
    /// The agent runs in a window that **already exists** — the caller's own pane, not one the
    /// launcher is about to create. `bn agent launch` sets this for an in-place launch; a dispatch
    /// that opens a fresh window leaves it false, so a live window at its target still means a
    /// leftover to clear (see [`remove_dead_launch_target`]).
    pub in_place: bool,
}

pub(crate) struct AgentLeaseReservation {
    inner: CoreAgentLeaseReservation,
}

impl AgentLeaseReservation {
    pub(crate) fn lease(&self) -> &AgentLease {
        self.inner.lease()
    }

    pub(crate) fn commit(self) {
        self.inner.commit();
    }

    pub(crate) fn rollback(self) -> Result<(), String> {
        self.inner
            .rollback()
            .map_err(|error| format!("agent lease rollback: {error}"))
    }
}

fn target_is_live(target: &str) -> bool {
    crate::proc::tmux_raw(&[
        "list-panes",
        "-t",
        &format!("={target}"),
        "-F",
        "#{pane_dead}",
    ])
    .is_some_and(|panes| panes.lines().any(|dead| dead.trim() == "0"))
}

/// The pid of a window's (first live) pane — the handle a pool lease adopts for an agent it found
/// rather than handed out, so the lease's liveness is the agent's own.
pub(crate) fn pane_pid(target: &str) -> Option<u32> {
    crate::proc::tmux_raw(&[
        "list-panes",
        "-t",
        &format!("={target}"),
        "-F",
        "#{pane_pid} #{pane_dead}",
    ])?
    .lines()
    .filter_map(|line| line.split_once(' '))
    .find(|(_, dead)| dead.trim() == "0")
    .and_then(|(pid, _)| pid.trim().parse().ok())
}

/// The tmux window id hosting `target` (`session:window`), or `None` when no such window exists.
///
/// Enumerates and matches by name rather than asking `display-message -p -t =<target>`, because
/// **that question cannot express "no such window"**: on an unresolvable target `display-message`
/// does not fail — it prints the *current* window's id and exits 0. Every caller here feeds the
/// answer to `kill-window`, so the fallback turns "this agent's window is already gone" into
/// "kill whatever window is in front of you". `list-panes -t` does error properly, which is why
/// [`target_is_live`] can keep using it; `display-message` is the odd one out.
pub(crate) fn window_id_for_target(target: &str) -> Option<String> {
    let rows = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{session_name}:#{window_name}",
    ])?;
    window_id_from_rows(&rows, target)
}

/// Exact-match `target` against `#{window_id}\t#{session_name}:#{window_name}` rows. Pure →
/// unit-testable. Exact, never a prefix: window indices renumber as lanes come and go, so anything
/// loose resolves a neighbour — and the caller kills what this returns.
fn window_id_from_rows(rows: &str, target: &str) -> Option<String> {
    let want = target.trim().trim_start_matches('=');
    if want.is_empty() {
        return None;
    }
    rows.lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(_, name)| name.trim() == want)
        .map(|(id, _)| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

pub(crate) fn lease_is_live(lease: &AgentLease) -> bool {
    target_is_live(&lease.tmux_target)
}

fn reap(leases: &[AgentLease]) -> Result<(), String> {
    for lease in leases {
        let Some(window_id) = window_id_for_target(&lease.tmux_target) else {
            continue;
        };
        reap_window_ids(std::slice::from_ref(&window_id))?;
        if target_is_live(&lease.tmux_target) {
            return Err(format!(
                "prior agent window {} is still live after reap",
                lease.tmux_target
            ));
        }
    }
    Ok(())
}

/// The window *this process* is running in.
///
/// Resolved through `$TMUX_PANE`. A bare `display-message -p` (no `-t`) does **not** answer this
/// question: with a client attached, tmux resolves the empty target to that client's *current*
/// window — whichever one the human happens to be looking at — so a command running unattended in
/// window A reports window B the moment you switch to it. Every caller is a safety gate ("is this
/// lease ours?", "refuse to reap the window we're standing in"), and there the wrong answer means
/// killing the wrong window. Falls back to the bare form only outside a pane, where there is no
/// better answer and no process to protect.
pub(crate) fn current_window_id() -> Option<String> {
    std::env::var_os("TMUX")?;
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    crate::proc::tmux_raw(&current_window_argv(pane.as_deref()))
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// The `display-message` argv [`current_window_id`] asks. Pure → unit-testable, because the whole
/// correctness of the answer is whether `-t <pane>` is present.
fn current_window_argv(pane: Option<&str>) -> Vec<&str> {
    match pane {
        Some(pane) => vec!["display-message", "-p", "-t", pane, "#{window_id}"],
        None => vec!["display-message", "-p", "#{window_id}"],
    }
}

fn reap_window_ids(window_ids: &[String]) -> Result<(), String> {
    let current = current_window_id();
    for window_id in window_ids {
        if current.as_ref() == Some(window_id) {
            return Err(format!(
                "refusing to reap the current agent window {window_id}; run recovery from another window"
            ));
        }
        crate::reap::kill_window_trees(std::slice::from_ref(window_id));
        if !crate::proc::tmux_ok(&["kill-window", "-t", window_id]) {
            return Err(format!("could not reap prior agent window {window_id}"));
        }
    }
    Ok(())
}

fn remove_dead_launch_target(target: &str) -> Result<(), String> {
    let Some(window_id) = window_id_for_target(target) else {
        return Ok(());
    };
    if target_is_live(target) {
        return Err(format!(
            "target window {target} is still live after lease reconciliation"
        ));
    }
    if !crate::proc::tmux_ok(&["kill-window", "-t", &window_id]) {
        return Err(format!(
            "could not remove dead target window {target} ({window_id})"
        ));
    }
    Ok(())
}

/// Stamp a leased window with everything needed to find it again: the session it hosts, the kind of
/// agent, and the repo/branch/worktree it belongs to.
///
/// The repo/branch/worktree options are what [`observed_agent_windows`] filters on and what the
/// supervisor's window join and `bn agent send`'s target resolution read — a window carrying only
/// `@bn_session_id` is invisible to all three. `wt::open_worktree_window_cmd_named` stamps the same
/// three when *it* creates a window; this covers the in-place case, where the window already existed
/// and the agent moved in.
pub(crate) fn mark_window(lease: &AgentLease) {
    let target = format!("={}", lease.tmux_target);
    // `@bn_agent_since` is read by the tmux status row, the jump picker, the attention list and
    // `agent_live` to answer "how long has this agent been going?" — and nothing wrote it, so every
    // one of them was reading an empty string. tmux has no per-window creation time to fall back
    // on (`#{window_created}` is not a tmux variable and renders empty), so the lease's own
    // acquisition time is the only honest source.
    let since = lease.acquired_at.to_string();
    for (option, value) in [
        ("@bn_session_id", lease.session_id.as_str()),
        ("@bn_agent_kind", lease.kind.label()),
        ("@bn_repo", lease.key.repo.as_str()),
        ("@bn_branch", lease.key.branch.as_str()),
        ("@bn_worktree", lease.key.worktree.as_str()),
        ("@bn_agent_since", since.as_str()),
    ] {
        let _ = crate::proc::tmux_run(&["set-window-option", "-t", &target, option, value]);
    }
}

/// Drop the lease held by `session_id` (idempotent). Called when a lease-holding agent is
/// deliberately stopped, so the worktree is free again immediately rather than waiting for the
/// next reconcile to notice the window is gone.
pub(crate) fn release(session_id: &str) -> Result<bool, String> {
    release_agent_lease(&agent_lease_registry_path(), session_id)
        .map_err(|error| format!("agent lease release: {error}"))
}

/// Drop the lease(s) *this very window* holds on `worktree`, so a stage handoff can lease it next.
/// Returns how many were released.
///
/// A pipeline handoff is issued **by** the agent being handed off from, running inside the window
/// that already holds the worktree's lease — so the successor's launch would always refuse itself
/// under one-agent-per-worktree. Calling the handoff is that agent declaring it is done, which
/// makes dropping its own lease the accurate record rather than a workaround.
///
/// Deliberately not [`reserve`] with `replace: true`: replacement *reaps* the replaced window, and
/// the window being replaced here is the caller's own. Only leases matching the current window are
/// touched, so an agent working in any other window keeps its lease and still blocks the launch.
pub(crate) fn release_own(worktree: &Path) -> usize {
    let Some(here) = current_window_id() else {
        return 0; // outside tmux there is no "own window" to identify — leave the registry alone
    };
    live_agents_in(worktree)
        .into_iter()
        .filter(|occupant| window_id_for_target(&occupant.tmux_target).as_ref() == Some(&here))
        .filter_map(|occupant| occupant.session_id)
        .filter(|session_id| release(session_id).unwrap_or(false))
        .count()
}

/// A live agent found working in a checkout — the answer to "is this worktree in use?".
///
/// Deliberately not an [`AgentLease`]: the third source below finds agents that never took one, and
/// those still occupy the checkout. `branch` is `None` for them.
#[derive(Debug, Clone)]
pub(crate) struct Occupant {
    /// The occupant's `repo:branch` identity as the lease registry normalizes it, or `None` for an
    /// agent that never registered (a bare `copilot` typed in the directory) — visible, but not
    /// nameable, so nothing can steer or kill it by target.
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    pub tmux_target: String,
    /// `dispatch`, `control`, … for a registered agent; the harness binary for an unregistered one.
    pub kind: String,
    /// When the occupant was first seen (lease acquisition, else window creation), for oldest-first.
    pub since: i64,
}

impl Occupant {
    /// How the pool ledger names this tenant: its branch when it has one, else its window — an
    /// unregistered agent still holds the slot, it just holds it under no branch.
    pub(crate) fn label(&self) -> String {
        self.branch
            .clone()
            .unwrap_or_else(|| format!("agent:{}", self.tmux_target))
    }

    /// A one-line description for a refusal message.
    pub(crate) fn describe(&self) -> String {
        match &self.session_id {
            Some(id) => format!("{} agent {id} in {}", self.kind, self.tmux_target),
            None => format!("{} in {} (unregistered)", self.kind, self.tmux_target),
        }
    }
}

/// Every live agent working in `worktree`, oldest first — the single occupancy oracle every
/// ownership decision reads (the launch guard, and each of the pool's lease/return/reclaim paths).
///
/// Three sources, unioned, because each covers the others' blind spot:
///
/// 1. **the lease registry** — agents that registered, however their window was later renamed;
/// 2. **`@bn_*`-stamped tmux windows** — an agent whose lease write was lost, or that predates the
///    registry;
/// 3. **any live pane rooted in the checkout running a harness binary** — the one that matters
///    most, because an agent started outside bn (a bare `copilot` in the directory, a launch with
///    `BN_AGENT_GUARD=0`) appears in neither of the first two, and is exactly the tenant a pool
///    slot got handed out from under.
///
/// Liveness is the same tmux pane check throughout, so "occupied" means one thing everywhere.
pub(crate) fn live_agents_in(worktree: &Path) -> Vec<Occupant> {
    let key = AgentLeaseKey::new("", "", worktree);
    let mut found: BTreeMap<String, Occupant> = BTreeMap::new();

    // (1) Registered and live.
    if let Ok(registry) = bn_core::agent::load_agent_leases(&agent_lease_registry_path()) {
        for lease in registry.leases {
            if lease.key.worktree != key.worktree || !lease_is_live(&lease) {
                continue;
            }
            found.insert(
                lease.tmux_target.clone(),
                Occupant {
                    repo: Some(lease.key.repo.clone()),
                    branch: Some(lease.key.branch.clone()),
                    session_id: Some(lease.session_id.clone()),
                    tmux_target: lease.tmux_target.clone(),
                    kind: lease.kind.label().to_string(),
                    since: lease.acquired_at,
                },
            );
        }
    }

    // (2) Stamped windows the registry missed.
    for window in observed_agent_windows() {
        if window.lease.key.worktree != key.worktree {
            continue;
        }
        found
            .entry(window.lease.tmux_target.clone())
            .or_insert_with(|| Occupant {
                repo: Some(window.lease.key.repo.clone()),
                branch: Some(window.lease.key.branch.clone()),
                session_id: Some(window.lease.session_id.clone()),
                tmux_target: window.lease.tmux_target.clone(),
                kind: window.lease.kind.label().to_string(),
                since: window.created_at,
            });
    }

    // (3) Unregistered harness panes rooted in the checkout.
    for pane in harness_panes_in(Path::new(&key.worktree)) {
        found.entry(pane.tmux_target.clone()).or_insert(pane);
    }

    let mut out: Vec<Occupant> = found.into_values().collect();
    out.sort_by(|a, b| {
        a.since
            .cmp(&b.since)
            .then_with(|| a.tmux_target.cmp(&b.tmux_target))
    });
    out
}

/// Live panes whose working directory is inside `worktree` and whose foreground command is a known
/// agent harness — an agent bn never registered. Same signal the stale-agent reaper uses,
/// read straight off tmux rather than the process table so it needs no `/proc` (which does not
/// exist on macOS, where the pool's old in-use check therefore always answered "idle").
fn harness_panes_in(worktree: &Path) -> Vec<Occupant> {
    let format = "#{session_name}:#{window_name}\t#{pane_current_path}\t#{pane_current_command}\t#{@bn_agent_since}\t#{pane_dead}";
    let Some(output) = crate::proc::tmux_raw(&["list-panes", "-a", "-F", format]) else {
        return Vec::new();
    };
    parse_harness_panes(&output, worktree, bn_core::util::now_epoch_secs())
}

/// Whether *any* live tmux pane is sitting in `worktree` — an agent, a build, or just a shell
/// someone has open there.
///
/// Weaker than [`live_agents_in`] and used for a different question: not "who owns this?" but
/// "would resetting this directory pull the rug from under someone?". It is the portable
/// counterpart of the `/proc` cwd scan, which sees nothing at all on macOS.
pub(crate) fn any_live_pane_in(worktree: &Path) -> bool {
    let format = "#{pane_current_path}\t#{pane_dead}";
    crate::proc::tmux_raw(&["list-panes", "-a", "-F", format]).is_some_and(|output| {
        output.lines().any(|line| {
            let Some((cwd, dead)) = line.split_once('\t') else {
                return false;
            };
            dead.trim() == "0" && Path::new(cwd).starts_with(worktree)
        })
    })
}

/// The exact `session:window` of the first live pane sitting in `worktree`, if any.
///
/// [`any_live_pane_in`] answers "is anyone in here?"; this answers "*where* are they", which is
/// what a crash restore needs. After a reboot tmux-continuum has already rebuilt the windows in
/// their old working directories (`@continuum-restore 'on'`), but the harness is not in resurrect's
/// process whitelist, so each one comes back as a plain shell. Restoring into that pane keeps the
/// window — and the scrollback continuum saved — instead of opening a second window per branch and
/// then needing to close one, which is the operation that must never touch a session.
pub(crate) fn live_pane_target_in(worktree: &Path) -> Option<String> {
    let format = "#{pane_current_path}\t#{pane_dead}\t#{session_name}:#{window_index}";
    crate::proc::tmux_raw(&["list-panes", "-a", "-F", format]).and_then(|output| {
        output.lines().find_map(|line| {
            let mut parts = line.split('\t');
            let cwd = parts.next()?;
            let dead = parts.next()?;
            let target = parts.next()?;
            (dead.trim() == "0" && Path::new(cwd).starts_with(worktree))
                .then(|| target.trim().to_string())
        })
    })
}

/// Read an `@bn_agent_since` stamp, falling back to `now` when it is absent or unparseable.
///
/// **Never the epoch.** This defaulted to `0`, and the field it read was `#{window_created}` — not
/// a tmux format variable at all, so it rendered empty for *every* window and every observed agent
/// was stamped "acquired in 1970". That silently disabled the thing the timestamp exists for: the
/// [`AGENT_LEASE_STARTING_GRACE_SECS`] window in which a just-reserved lease is authoritative
/// before its tmux window appears. `now - 0` is never under thirty seconds, so a starting agent had
/// no grace at all, and the zero propagated into the pool ledger through `adopt_occupied_slots`
/// (a slot reading `age=20673d`, which is `now - 0`).
///
/// `now` is the conservative default: an agent whose start we cannot read is treated as having
/// *just* started, so it keeps its grace rather than presenting as infinitely old.
///
/// [`AGENT_LEASE_STARTING_GRACE_SECS`]: bn_core::agent::AGENT_LEASE_STARTING_GRACE_SECS
fn stamped_since(field: &str, now: i64) -> i64 {
    match field.trim().parse::<i64>() {
        Ok(ts) if ts > 0 => ts,
        _ => now,
    }
}

/// The pure half of [`harness_panes_in`], so the "is this pane an agent in this checkout?" rule is
/// testable without a tmux server.
fn parse_harness_panes(output: &str, worktree: &Path, now: i64) -> Vec<Occupant> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 || fields[4] != "0" {
                return None;
            }
            let cwd = Path::new(fields[1]);
            if !cwd.starts_with(worktree) {
                return None;
            }
            // Match the harness *binary* exactly rather than through `harness::normalize`, whose
            // aliases include `gh` — someone running `gh pr view` in a slot is not an agent.
            let command = fields[2].trim();
            let harness = bn_core::harness::KNOWN_HARNESSES
                .iter()
                .find(|known| **known == command)
                .copied()
                // Claude Code sets its process title to its bare version number, so
                // `#{pane_current_command}` reads "2.1.226", never "claude" — an exact-name
                // match alone leaves every such pane invisible to occupancy, which is how a
                // slot with a live agent typing in it got warm-reset. Nothing else titles a
                // foreground process as a dotted version, so the shape is the identity.
                .or_else(|| bare_version(command).then_some("claude"))?;
            Some(Occupant {
                repo: None,
                branch: None,
                session_id: None,
                tmux_target: fields[0].to_string(),
                kind: harness.to_string(),
                since: stamped_since(fields[3], now),
            })
        })
        .collect()
}

/// Whether `s` is a bare dotted version number (`2.1.226`) — the process title Claude Code runs
/// under. At least two numeric parts, so a plain integer (a pid-named tool, a bare `2`) never
/// matches.
fn bare_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Who holds `worktree` right now, from the perspective of a launch about to happen in it.
pub(crate) enum WorktreeHolder {
    /// Nobody — the launch should take a lease of its own.
    Free,
    /// Only this window holds it: a lease taken moments ago by the launcher that seeded this agent
    /// (`bn dispatch`, `bn control start`). Not a conflict, and not something to lease over.
    Ours,
    /// A different live agent. The launch must refuse rather than put two agents in one checkout.
    Other(Box<Occupant>),
}

/// Read the occupancy of `worktree` for the one-agent-per-worktree guard in `bn agent launch`.
///
/// [`WorktreeHolder::Ours`] exists because a seeded launch runs *inside* the window its launcher
/// just leased — without it every dispatched agent would refuse itself. But `Ours` describes the
/// whole checkout, not one lease in it: a live agent in **any other** window wins, and only when
/// this window is the sole occupant is the launch ours to make. Returning `Ours` on the first
/// self-match (as this once did) meant a dispatched launch — which always holds a lease for its own
/// window — could never refuse, so a slot handed out from under a running agent got a second one
/// seeded straight into it.
///
/// Outside tmux there is no window to compare and pane liveness can't be read either, so the check
/// degrades to `Free` — which is why the shell wrapper, not this, is the enforcing layer for a
/// hand-typed launch on a bare terminal.
pub(crate) fn worktree_holder(worktree: &Path) -> WorktreeHolder {
    let here = current_window_id();
    let occupants = live_agents_in(worktree);
    let mut ours = false;
    for occupant in occupants {
        if here.is_some() && window_id_for_target(&occupant.tmux_target) == here {
            ours = true;
            continue;
        }
        return WorktreeHolder::Other(Box::new(occupant));
    }
    if ours {
        WorktreeHolder::Ours
    } else {
        WorktreeHolder::Free
    }
}

pub(crate) fn reserve(request: AgentLeaseRequest<'_>) -> Result<AgentLeaseReservation, String> {
    let lease = AgentLease {
        key: AgentLeaseKey::new(request.repo, request.branch, request.worktree),
        session_id: request.session_id.to_string(),
        harness: request.harness.to_string(),
        kind: request.kind,
        tmux_target: request.tmux_target.to_string(),
        acquired_at: bn_core::util::now_epoch_secs(),
    };
    let mode = if request.replace {
        AgentLeaseAcquireMode::Replace
    } else {
        AgentLeaseAcquireMode::Refuse
    };
    match begin_agent_lease(
        &agent_lease_registry_path(),
        lease,
        mode,
        bn_core::util::now_epoch_secs(),
        lease_is_live,
    )
    .map_err(|error| format!("agent lease: {error}"))?
    {
        AgentLeaseBegin::Held(owner) => Err(format!(
            "worktree already has a live {} agent (session {}, window {})",
            owner.kind.label(),
            owner.session_id,
            owner.tmux_target
        )),
        AgentLeaseBegin::Reserved(mut reservation) => {
            if let Err(error) = reap(reservation.replaced()) {
                let rollback = reservation.rollback();
                if let Err(rollback_error) = rollback {
                    return Err(format!("{error}; {rollback_error}"));
                }
                return Err(error);
            }
            if !request.in_place {
                if let Err(error) = remove_dead_launch_target(&reservation.lease().tmux_target) {
                    let rollback = reservation.rollback();
                    if let Err(rollback_error) = rollback {
                        return Err(format!("{error}; {rollback_error}"));
                    }
                    return Err(error);
                }
            }
            reservation.discard_replaced();
            Ok(AgentLeaseReservation { inner: reservation })
        }
    }
}

#[derive(Debug, Clone)]
struct ObservedAgentWindow {
    lease: AgentLease,
    window_id: String,
    created_at: i64,
}

fn observed_agent_windows() -> Vec<ObservedAgentWindow> {
    let format = "#{window_id}\t#{session_name}:#{window_name}\t#{@bn_repo}\t#{@bn_branch}\t#{@bn_worktree}\t#{pane_current_path}\t#{@bn_agent}\t#{@bn_session_id}\t#{@bn_agent_kind}\t#{@bn_agent_since}\t#{pane_dead}";
    let Some(output) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", format]) else {
        return Vec::new();
    };
    parse_observed_agent_windows(&output, bn_core::util::now_epoch_secs())
}

fn parse_observed_agent_windows(output: &str, now: i64) -> Vec<ObservedAgentWindow> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() < 11 {
                return None;
            }
            let window_id = fields[0];
            let target = fields[1];
            let repo = fields[2];
            let branch = fields[3];
            let worktree = if fields[4].is_empty() {
                fields[5]
            } else {
                fields[4]
            };
            let state = fields[6];
            let session_id = fields[7];
            let kind = fields[8];
            if repo.is_empty()
                || branch.is_empty()
                || worktree.is_empty()
                || (state.is_empty() && session_id.is_empty())
                || fields[10] != "0"
            {
                return None;
            }
            Some(ObservedAgentWindow {
                lease: AgentLease {
                    key: AgentLeaseKey::new(repo, branch, Path::new(worktree)),
                    session_id: if session_id.is_empty() {
                        format!("tmux:{target}")
                    } else {
                        session_id.to_string()
                    },
                    harness: "unknown".into(),
                    kind: match kind {
                        "control" => AgentLeaseKind::Control,
                        "today" => AgentLeaseKind::Today,
                        "pipeline-plan" => AgentLeaseKind::PipelinePlan,
                        "pipeline-investigate" => AgentLeaseKind::PipelineInvestigate,
                        _ if target.contains("-plan-") => AgentLeaseKind::PipelinePlan,
                        _ => AgentLeaseKind::Dispatch,
                    },
                    tmux_target: target.to_string(),
                    acquired_at: stamped_since(fields[9], now),
                },
                window_id: window_id.to_string(),
                created_at: stamped_since(fields[9], now),
            })
        })
        .collect()
}

fn current_target() -> Option<String> {
    std::env::var_os("TMUX").and_then(|_| {
        crate::proc::tmux_raw(&["display-message", "-p", "#{session_name}:#{window_name}"])
            .map(|target| target.trim().to_string())
            .filter(|target| !target.is_empty())
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AgentLeaseReconcileReport {
    pub conflicts: usize,
    pub duplicate_windows_reaped: usize,
    pub stale_leases_removed: usize,
    pub errors: Vec<String>,
}

pub(crate) fn reconcile() -> AgentLeaseReconcileReport {
    let path = agent_lease_registry_path();
    let now = bn_core::util::now_epoch_secs();
    let mut report = AgentLeaseReconcileReport::default();
    match reconcile_agent_leases(&path, now, lease_is_live) {
        Ok(core) => {
            report.stale_leases_removed = core
                .removed
                .iter()
                .filter(|removal| removal.reason == AgentLeaseRemovalReason::Stale)
                .count();
            // Actual duplicate windows are reconciled below after choosing a keeper that never
            // kills the caller's current window. Registry-only duplicates need no process action.
        }
        Err(error) => {
            report
                .errors
                .push(format!("could not reconcile agent lease registry: {error}"));
            return report;
        }
    }

    let registry = load_agent_leases(&path).unwrap_or_default();
    let leased_targets = registry
        .leases
        .iter()
        .map(|lease| (lease.key.clone(), lease.tmux_target.clone()))
        .collect::<BTreeMap<_, _>>();
    let current = current_target();
    let current_id = current_window_id();
    let mut grouped = BTreeMap::<AgentLeaseKey, Vec<ObservedAgentWindow>>::new();
    for window in observed_agent_windows() {
        grouped
            .entry(window.lease.key.clone())
            .or_default()
            .push(window);
    }

    for (key, mut windows) in grouped {
        windows.sort_by_key(|window| (window.created_at, window.lease.tmux_target.clone()));
        let keeper_index = current_id
            .as_ref()
            .and_then(|window_id| {
                windows
                    .iter()
                    .position(|window| &window.window_id == window_id)
            })
            .or_else(|| {
                current.as_ref().and_then(|target| {
                    windows
                        .iter()
                        .position(|window| &window.lease.tmux_target == target)
                })
            })
            .or_else(|| {
                leased_targets.get(&key).and_then(|target| {
                    windows
                        .iter()
                        .position(|window| &window.lease.tmux_target == target)
                })
            })
            .unwrap_or(0);
        let preferred = windows[keeper_index].lease.clone();
        let mut conflict_recorded = windows.len() > 1;
        if conflict_recorded {
            report.conflicts += 1;
        }
        let mut candidate = leased_targets
            .get(&key)
            .and_then(|target| {
                windows
                    .iter()
                    .find(|window| &window.lease.tmux_target == target)
                    .map(|window| window.lease.clone())
            })
            .unwrap_or(preferred);
        let mut reconciled = false;
        let errors_before = report.errors.len();
        for _ in 0..3 {
            let transaction = begin_agent_lease(
                &path,
                candidate.clone(),
                AgentLeaseAcquireMode::Replace,
                now,
                lease_is_live,
            );
            let AgentLeaseBegin::Reserved(mut reservation) = (match transaction {
                Ok(transaction) => transaction,
                Err(error) => {
                    report.errors.push(format!(
                        "could not reserve owner for {}: {error}",
                        candidate.tmux_target
                    ));
                    break;
                }
            }) else {
                unreachable!("replace mode always reserves")
            };
            if let Some(owner) = reservation
                .replaced()
                .iter()
                .find(|owner| owner.tmux_target != candidate.tmux_target && lease_is_live(owner))
                .cloned()
            {
                let _ = reservation.rollback();
                if !conflict_recorded {
                    report.conflicts += 1;
                    conflict_recorded = true;
                }
                candidate = owner;
                continue;
            }

            let keep_id = windows
                .iter()
                .filter(|window| window.lease.tmux_target == candidate.tmux_target)
                .min_by_key(|window| {
                    (
                        current_id.as_ref() != Some(&window.window_id),
                        window.created_at,
                        window.window_id.clone(),
                    )
                })
                .map(|window| window.window_id.clone());
            let duplicates = windows
                .iter()
                .filter(|window| Some(&window.window_id) != keep_id.as_ref())
                .map(|window| window.window_id.clone())
                .collect::<Vec<_>>();
            match reap_window_ids(&duplicates) {
                Ok(()) => {
                    reservation.discard_replaced();
                    reservation.commit();
                    report.duplicate_windows_reaped += duplicates.len();
                    reconciled = true;
                }
                Err(error) => {
                    let _ = reservation.rollback();
                    report.errors.push(error);
                }
            }
            break;
        }
        if !reconciled && report.errors.len() == errors_before {
            report.errors.push(format!(
                "agent ownership changed repeatedly while reconciling {}/{}",
                key.repo, key.branch
            ));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    #[test]
    fn a_missing_stamp_reads_as_now_not_the_epoch() {
        // The regression: `#{window_created}` is not a tmux variable, so this field arrived empty
        // for every window and every agent was stamped "acquired in 1970" — which is never within
        // the starting grace, so a launching agent had none.
        assert_eq!(stamped_since("", NOW), NOW);
        assert_eq!(stamped_since("   ", NOW), NOW);
        assert_eq!(stamped_since("not-a-number", NOW), NOW);
        // A zero on disk is the old bug's own output; treat it as unknown rather than the epoch.
        assert_eq!(stamped_since("0", NOW), NOW);
    }

    #[test]
    fn a_real_stamp_is_kept() {
        assert_eq!(stamped_since("1699999000", NOW), 1_699_999_000);
    }

    #[test]
    fn an_observed_window_keeps_its_grace_when_unstamped() {
        // Nine tab-separated fields then the stamp and pane_dead: an agent window bn marked before
        // `@bn_agent_since` was written carries no stamp at all.
        let row = "@7\tbn:feat\tbn\tfeat\t/wt/bn/feat\t/wt/bn/feat\tworking\ts-1\tdispatch\t\t0";
        let observed = parse_observed_agent_windows(row, NOW);
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].lease.acquired_at, NOW);
        assert_eq!(observed[0].created_at, NOW);
        // The point of the fix: it lands inside the starting grace, so a lease reserved moments
        // ago is authoritative until its window is observable.
        assert!(
            NOW - observed[0].lease.acquired_at < bn_core::agent::AGENT_LEASE_STARTING_GRACE_SECS
        );
    }

    #[test]
    fn a_stamped_window_reports_its_real_age() {
        let row = "@7\tbn:feat\tbn\tfeat\t/wt/bn/feat\t/wt/bn/feat\tworking\ts-1\tdispatch\t1699999000\t0";
        let observed = parse_observed_agent_windows(row, NOW);
        assert_eq!(observed[0].lease.acquired_at, 1_699_999_000);
    }

    #[test]
    fn an_unstamped_harness_pane_is_not_ancient() {
        // This zero is what `adopt_occupied_slots` copied into the pool ledger as the lease's `at`,
        // producing a slot that reported an age of 20673 days.
        let row = "bn:feat\t/wt/bn/feat\tclaude\t\t0";
        let panes = parse_harness_panes(row, Path::new("/wt/bn/feat"), NOW);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].since, NOW);
    }

    #[test]
    fn a_version_titled_claude_pane_is_an_occupant() {
        // Claude Code's process title is its bare version, so the command column never says
        // "claude". The regression: every such pane was invisible to occupancy, and a slot with a
        // live agent typing in it was warm-reset underneath it.
        let row = "bn:feat\t/wt/bn/feat\t2.1.226\t\t0";
        let panes = parse_harness_panes(row, Path::new("/wt/bn/feat"), NOW);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].kind, "claude");
        // Non-version commands stay invisible: a build tool or shell is not an agent.
        for cmd in ["zsh", "node", "2", "gh", "1.2.x", "..", "2..3"] {
            let row = format!("bn:feat\t/wt/bn/feat\t{cmd}\t\t0");
            assert!(
                parse_harness_panes(&row, Path::new("/wt/bn/feat"), NOW).is_empty(),
                "{cmd} must not read as a harness"
            );
        }
    }
}
