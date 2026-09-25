//! `bn agent kill` / `bn agent kill-repo` — the destructive stop, for one agent and for a repo's
//! whole tmux session.
//!
//! Split out of `cmd::agent` because it is one self-contained concern: what it means to take an
//! agent down properly. Closing the window is only the visible part — the tree has to be reaped, the
//! lease released and the pool slot handed back, and every one of those steps exists because
//! skipping it produced a specific, expensive bug. Keeping the single-agent and whole-repo kills
//! next to each other is what stops the repo-wide one from drifting into a cheaper, leakier copy.
use crate::*;
use super::{agent_worktree, resolve_tmux_target, tmux_exact_target};

/// Kill a running agent: its process tree, its tmux window, its agent lease, and its pool slot.
///
/// Closing the window is only the visible part. Agent windows are created with `remain-on-exit on`
/// ([`crate::wt::window`]), so a bare `kill-window` leaves the harness *reparented and running* —
/// the machine keeps paying for an agent nobody can see. And a killed agent used to keep its warm
/// pool slot forever, because nothing ever handed a worker slot back: a day's kills filled the pool,
/// and every later dispatch silently fell back to cutting a cold worktree. So a kill now reaps the
/// tree ([`crate::reap::kill_window_trees`], the same primitive `wt cleanup` and the lease reaper
/// use), verifies the window is actually gone, releases the agent lease, and returns the slot.
///
/// Idempotent: a `repo:branch` with no live window is "already gone", not an error, so the dashboard
/// button and the orchestrator's give-up path can both call it without racing the window's teardown.
/// The target is resolved through the window join first ([`resolve_tmux_target`]) so an agent whose
/// window is named after its pooled slot is found rather than reported as already gone, and
/// normalized to an exact-match name so a `kill-window` never resolves a prefix/fnmatch neighbor.
pub(crate) fn emit_agent_kill(cfg: &Config, target: &str, as_json: bool) {
    let target = target.trim();
    // Guard the shape (`repo:branch`) so a malformed/empty input can't widen the tmux target. The
    // arg is passed to tmux as a distinct argv element (never a shell), so there's no injection —
    // this is purely a "don't kill the wrong thing" check.
    if target.is_empty() || !target.contains(':') {
        let msg = format!("invalid agent target '{target}' (expected repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_kill", "invalid_target", &msg).to_json()
            );
        } else {
            eprintln!("bn agent kill: {msg}");
        }
        return;
    }
    let (repo, branch) = target.split_once(':').unwrap_or_default();

    // Resolved once, before anything is killed: the worktree is what tells us which pool slot to
    // hand back, and after the window is gone the join can no longer find it.
    let worktree = agent_worktree(cfg, repo, branch);
    let exact = resolve_tmux_target(cfg, target);

    let outcome = match reap_agent_window(&exact) {
        Ok(o) => o,
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("agent_kill", "kill_failed", &e).to_json()
                );
            } else {
                eprintln!("bn agent kill: {target}: {e}");
            }
            return;
        }
    };

    // Release the agent lease so the worktree isn't reported as occupied by a session that no
    // longer exists — otherwise the next launch there refuses. Best-effort: the window is already
    // down, and a stale lease is reconciled away on the next `reserve`.
    let lease_released = outcome
        .session_id
        .as_deref()
        .and_then(|sid| agent_lease::release(sid).ok())
        .unwrap_or(false);

    // Hand the warm slot back — but only if we actually stopped an agent. A kill that found no
    // window ("already gone") stopped nothing, and the worktree it resolved came from a tiered
    // join that will happily answer with a stale lease entry or with git's current checkout of the
    // branch: returning a slot on that basis is how a *live* agent's slot got reset under it and
    // re-leased to the next dispatch. A slot holding uncommitted work, or another live agent, is
    // likewise left leased — the outcome says so rather than discarding it.
    let slot = worktree
        .as_deref()
        .filter(|_| outcome.killed)
        .map(|wt| crate::wt_pool::release_dispatch_slot(cfg, wt, outcome.session_id.as_deref()))
        .unwrap_or(crate::wt_pool::SlotRelease::NotASlot);

    if as_json {
        let data = serde_json::json!({
            "target": target,
            "killed": outcome.killed,
            "gone": true,
            "window_id": outcome.window_id,
            "processes": outcome.processes,
            "lease_released": lease_released,
            "worktree": worktree,
            "slot": slot,
        });
        println!("{}", Envelope::success("agent_kill", data).to_json());
        return;
    }
    if outcome.killed {
        println!(
            "bn agent kill: {target} — window closed, {} process(es) signalled ✓",
            outcome.processes
        );
    } else {
        println!("bn agent kill: {target} — already gone");
    }
    if let Some(m) = slot.message() {
        println!("bn agent kill: {m}");
    }
}

/// `bn agent kill-repo <repo>` — force-kill a repo's whole tmux session.
///
/// Session is repo and window is branch ([`crate::wt::window`]), so "kill this repo's session" means
/// every agent under it at once. It is the last-resort valve for a session that is wedged as a
/// whole, not a routine stop: prefer [`emit_agent_kill`] on the one branch that is stuck.
///
/// Implemented as a loop over [`reap_agent_window`] rather than a bare `tmux kill-session`, because
/// the per-window path is where leases are released and pool slots are handed back. A raw
/// `kill-session` closes the windows and leaves every one of this repo's slots leased to a session
/// that no longer exists, which fills the pool and quietly degrades every later dispatch to a cold
/// worktree — the exact failure [`emit_agent_kill`] was written to end.
///
/// Work is not destroyed: worktrees are left on disk untouched, and a slot holding uncommitted
/// changes stays leased rather than being reset ([`crate::wt_pool::SlotRelease::Dirty`]). What is
/// lost is the agents' in-flight context, which is the point of a force kill.
///
/// Idempotent: a repo with no live session is "already gone", not an error. Note it does not spare
/// the caller's own window — running it from inside the target repo's session kills that session
/// too.
pub(crate) fn emit_repo_kill(cfg: &Config, repo: &str, as_json: bool) {
    let repo = repo.trim();
    // Same "don't widen the target" guard as `kill`: a session name is one segment, so anything
    // carrying a `:` is a `repo:branch` sent to the wrong command rather than a repo.
    if repo.is_empty() || repo.contains(':') {
        let msg = format!("invalid repo '{repo}' (expected a bare repo name, not repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_kill_repo", "invalid_repo", &msg).to_json()
            );
        } else {
            eprintln!("bn agent kill-repo: {msg}");
        }
        return;
    }

    // One listing up front, so the set being killed is decided before anything is torn down and a
    // window opening mid-kill can't extend the blast radius past what was reported.
    let rows = bn_core::agent::parse_window_rows(
        &proc::tmux_raw(&["list-windows", "-a", "-F", bn_core::agent::WINDOW_FORMAT])
            .unwrap_or_default(),
    );
    let targets: Vec<(String, String, String)> = rows
        .iter()
        .filter_map(|row| {
            let (session, _) = row.target.split_once(':')?;
            if !session.eq_ignore_ascii_case(repo) {
                return None;
            }
            let worktree = if row.worktree.is_empty() {
                row.pane_path.clone()
            } else {
                row.worktree.clone()
            };
            Some((row.target.clone(), row.session_id.clone(), worktree))
        })
        .collect();

    let mut killed = 0usize;
    let mut processes = 0usize;
    let mut windows = Vec::new();
    let mut slot_messages = Vec::new();
    let mut failures = Vec::new();
    for (target, session_id, worktree) in targets {
        let outcome = match reap_agent_window(&tmux_exact_target(&target)) {
            Ok(o) => o,
            Err(e) => {
                // One window refusing to die is not a reason to leave the rest running: record it
                // and keep going, then report a partial kill honestly at the end.
                failures.push(serde_json::json!({ "target": target, "error": e }));
                continue;
            }
        };
        if !outcome.killed {
            continue;
        }
        killed += 1;
        processes += outcome.processes;

        let session_id = outcome
            .session_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(session_id).filter(|s| !s.is_empty()));
        let lease_released = session_id
            .as_deref()
            .and_then(|sid| agent_lease::release(sid).ok())
            .unwrap_or(false);
        let slot = Some(std::path::PathBuf::from(&worktree))
            .filter(|p| p.is_dir())
            .map(|wt| crate::wt_pool::release_dispatch_slot(cfg, &wt, session_id.as_deref()))
            .unwrap_or(crate::wt_pool::SlotRelease::NotASlot);
        slot_messages.extend(slot.message());
        windows.push(serde_json::json!({
            "target": target,
            "processes": outcome.processes,
            "lease_released": lease_released,
            "slot": slot,
        }));
    }

    let ok = failures.is_empty();
    if as_json {
        let data = serde_json::json!({
            "repo": repo,
            "killed": killed,
            "processes": processes,
            "windows": windows,
            "failed": failures,
        });
        // A partial kill reports `ok: false` but keeps the data: which windows did go down, and
        // which slots came back, is exactly what the operator needs after a failure.
        let envelope = bn_core::render::Envelope {
            v: bn_core::render::ENVELOPE_VERSION,
            ok,
            command: "agent_kill_repo".to_string(),
            data,
            error: (!ok).then(|| bn_core::render::EnvelopeError {
                code: "kill_failed".to_string(),
                message: format!("{} window(s) survived the kill", failures.len()),
            }),
        };
        println!("{}", envelope.to_json());
        return;
    }
    if killed == 0 && ok {
        println!("bn agent kill-repo: {repo} — already gone (no live windows)");
    } else {
        println!(
            "bn agent kill-repo: {repo} — {killed} window(s) closed, {processes} process(es) signalled ✓"
        );
    }
    for message in &slot_messages {
        println!("bn agent kill-repo: {message}");
    }
    for failure in &failures {
        eprintln!(
            "bn agent kill-repo: {}",
            failure.get("error").and_then(|e| e.as_str()).unwrap_or("")
        );
    }
}

/// What tearing an agent's window down actually did.
pub(crate) struct KillOutcome {
    /// False when there was no such window (idempotent "already gone").
    pub killed: bool,
    /// The tmux window id the tree was reaped through — stable across a rename mid-kill.
    pub window_id: Option<String>,
    /// How many processes the tree kill signalled.
    pub processes: usize,
    /// The window's `@bn_session_id` stamp, so the agent lease can be released by id.
    pub session_id: Option<String>,
}

impl KillOutcome {
    /// The `killed` field of a kill/relaunch envelope: `null` when there was no window to begin
    /// with, so "nothing was running" reads differently from "a tree was reaped".
    pub(crate) fn as_json(&self) -> serde_json::Value {
        if !self.killed {
            return serde_json::Value::Null;
        }
        serde_json::json!({
            "window_id": self.window_id,
            "processes": self.processes,
            "session_id": self.session_id,
        })
    }
}

/// Reap the process tree behind `exact`, close the window, and verify it is gone.
///
/// Split out so the sequence — *tree first, window second, then check* — is stated once. The order
/// matters: `kill-window` alone reparents the pane's children (`remain-on-exit`), and skipping the
/// verification is how a kill came to report success over a window that was still live.
/// `Err` is a real failure (tmux unreachable, or the window survived); `Ok(killed: false)` is the
/// idempotent "nothing was there". Shared with `bn agent relaunch`, which must tear the old agent
/// down by exactly these rules before it respawns.
pub(crate) fn reap_agent_window(exact: &str) -> Result<KillOutcome, String> {
    let windows = crate::proc::tmux(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
        .ok_or("tmux unreachable")?;
    if !windows.lines().any(|w| tmux_exact_target(w) == exact) {
        return Ok(KillOutcome {
            killed: false,
            window_id: None,
            processes: 0,
            session_id: None,
        });
    }
    // Address the tree by window *id*: a window renamed between the lookup and the kill would
    // otherwise move the target out from under us.
    let window_id = crate::proc::tmux_raw(&["display-message", "-p", "-t", exact, "#{window_id}"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("could not resolve a window id for {exact}"))?;
    let session_id =
        crate::proc::tmux_raw(&["display-message", "-p", "-t", &window_id, "#{@bn_session_id}"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

    let processes = crate::reap::kill_window_trees(std::slice::from_ref(&window_id));
    let _ = crate::proc::tmux_ok(&["kill-window", "-t", &window_id]);

    // Verify rather than trust: `kill-window`'s exit status says the command ran, not that the
    // window is gone, and reporting a live agent as killed is the failure mode this whole path
    // exists to remove.
    if crate::proc::tmux_raw(&["display-message", "-p", "-t", &window_id, "#{window_id}"])
        .map(|s| s.trim() == window_id)
        .unwrap_or(false)
    {
        return Err(format!(
            "window {exact} ({window_id}) is still live after reaping {processes} process(es)"
        ));
    }
    Ok(KillOutcome {
        killed: true,
        window_id: Some(window_id),
        processes,
        session_id,
    })
}
