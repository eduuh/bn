//! `bn agent relaunch <repo:branch>` — kill a running agent and respawn it in the same worktree.
//!
//! The fleet board has always had a "Relaunch" button, and it was wired to `bn agent recover
//! --yes` — which is not a relaunch at all: it looks for a worktree with commits ahead of base and
//! no PR, then pushes the branch and opens a GitHub PR. When it found nothing the dashboard said
//! "relaunch failed"; when it found something it opened a PR behind a confirm that promised an
//! in-place restart. Neither outcome was the one word on the button.
//!
//! This is that word. Kill the agent's **process tree** (a bare `kill-window` reparents the harness
//! and leaves it running, because agent windows carry `remain-on-exit on`), then start it again in
//! the checkout it was actually in — resolved from git via [`super::agent::agent_worktree`], never
//! reconstructed as `<worktree_dir>/<repo>/<branch>`, which is wrong for every pooled slot.
//!
//! Two modes on the same target:
//!   * default — respawn `--resume-session <same id>`, so the agent picks up its own transcript.
//!     This is what "restart the stuck agent" means to an operator.
//!   * `--hard` (or any `--harness` change) — a *fresh* session seeded from the branch note's
//!     handoff packet. A Claude transcript is not resumable by Copilot, so a harness change forces
//!     this; the envelope's `fresh_reason` always says which of the two applied.
//!
//! The pool lease is deliberately **kept**: a relaunch reuses the worktree, so the slot must not go
//! back to the pool (that is `bn agent kill`'s job). It is re-stamped instead — the recorded holder
//! pid belongs to the process we just killed, and the pool's stale-lease reclaim would otherwise
//! read the slot as abandoned and hand it to another dispatch mid-relaunch.
//!
//! Every failure is typed and exits non-zero, because the one thing worse than a relaunch that
//! didn't happen is a relaunch that didn't happen and said it did.

use crate::*;
use bn_core::agent::{AgentLeaseKind, LiveAgent};
use bn_core::render::Envelope;
use bn_core::resolver::Config;
use std::path::PathBuf;

/// Why a relaunch is starting a new session instead of resuming the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FreshReason {
    /// `--hard` — the operator asked for a clean slate.
    Hard,
    /// `--harness <other>` — a transcript can't cross harnesses.
    HarnessChange,
    /// Nothing recorded a session id to resume.
    NoSession,
}

/// Everything a relaunch needs, decided before anything is killed.
///
/// Separated from the doing so the decision is unit-testable and so every unresolvable input
/// (missing worktree, unknown harness) fails *before* the old agent is torn down — a relaunch that
/// kills and then discovers it cannot start is the worst possible outcome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RelaunchPlan {
    pub target: String,
    pub repo: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub harness: String,
    pub fresh_reason: Option<FreshReason>,
    pub old_session: Option<String>,
    pub new_session: String,
    pub window: String,
}

impl RelaunchPlan {
    /// True when the respawn starts a new session rather than resuming the recorded one.
    pub(crate) fn fresh(&self) -> bool {
        self.fresh_reason.is_some()
    }

    /// `"fresh"` / `"resume"` — the mode name the envelope and the operator both use.
    pub(crate) fn mode(&self) -> &'static str {
        if self.fresh() {
            "fresh"
        } else {
            "resume"
        }
    }
}

/// What the caller already knows about the agent, gathered by the effectful layer so [`decide`]
/// stays pure.
pub(crate) struct Observed<'a> {
    /// The live fleet row, when the agent is still running — the same row the operator clicked.
    pub live: Option<&'a LiveAgent>,
    /// `(harness, session_id)` recorded on the branch note, for an agent that already died.
    pub recorded: Option<(String, String)>,
    /// The real checkout, from git.
    pub worktree: Option<PathBuf>,
    /// A freshly minted id, used when the plan comes out `fresh`. Passed in so `decide` is pure.
    pub minted_session: String,
}

/// Decide the relaunch. Pure — every input is already resolved, so this is exhaustively testable.
///
/// `Err` is `(code, message)`, matching the envelope's typed failure codes.
pub(crate) fn decide(
    target: &str,
    obs: &Observed<'_>,
    hard: bool,
    requested_harness: Option<&str>,
) -> Result<RelaunchPlan, (&'static str, String)> {
    let target = target.trim();
    // Same shape guard as `bn agent kill`: `repo:branch` is an identity, and a malformed one must
    // never widen into a tmux target.
    let Some((repo, branch)) = target.split_once(':') else {
        return Err((
            "invalid_target",
            format!("invalid agent target '{target}' (expected repo:branch)"),
        ));
    };
    if repo.is_empty() || branch.is_empty() {
        return Err((
            "invalid_target",
            format!("invalid agent target '{target}' (expected repo:branch)"),
        ));
    }

    let Some(worktree) = obs.worktree.clone() else {
        return Err((
            "no_worktree",
            format!(
                "no checkout for {target} — git lists no worktree for the branch, so there is \
                 nothing to relaunch into"
            ),
        ));
    };

    let current_harness = obs
        .live
        .map(|a| a.harness.clone())
        .or_else(|| obs.recorded.as_ref().map(|(h, _)| h.clone()));
    let harness = match requested_harness {
        Some(h) => h.to_string(),
        None => match current_harness.clone() {
            Some(h) if !h.is_empty() => h,
            _ => {
                return Err((
                    "unknown_harness",
                    format!(
                        "no harness recorded for {target} — pass --harness claude|copilot to say \
                         what to start"
                    ),
                ))
            }
        },
    };

    let old_session = obs
        .live
        .map(|a| a.session_id.clone())
        .or_else(|| obs.recorded.as_ref().map(|(_, s)| s.clone()))
        .filter(|s| !s.is_empty());

    // Order matters: an explicit `--hard` is the reason the operator will recognize, so it wins
    // over the harness change it may coincide with.
    let harness_changed = current_harness
        .as_deref()
        .is_some_and(|c| !c.eq_ignore_ascii_case(&harness));
    let fresh_reason = if hard {
        Some(FreshReason::Hard)
    } else if harness_changed {
        Some(FreshReason::HarnessChange)
    } else if old_session.is_none() {
        Some(FreshReason::NoSession)
    } else {
        None
    };

    // Reuse the canonical window name. The old window is killed first, so the name is free — and
    // keeping it means the agent stays addressable at `repo:branch` and the window-name join tier
    // keeps resolving, instead of the fleet drifting onto a suffixed name after every relaunch.
    let window = bn_core::orchestrate::tmux_window_name(branch);

    let new_session = match (&fresh_reason, &old_session) {
        (None, Some(sid)) => sid.clone(),
        _ => obs.minted_session.clone(),
    };

    Ok(RelaunchPlan {
        target: target.to_string(),
        repo: repo.to_string(),
        branch: branch.to_string(),
        worktree,
        harness,
        fresh_reason,
        old_session,
        new_session,
        window,
    })
}

/// The shell command the respawned window runs.
///
/// `bn agent launch` is addressed the same way `bn dispatch` addresses it — via
/// [`crate::cmd::dispatch::agent_launcher_bin`], an absolute `$HOME`-derived path — because the
/// tmux server's PATH does not contain the bindir. Every interpolated element is single-quoted, so
/// a branch or path with a space or apostrophe can't split the command.
pub(crate) fn launch_command(plan: &RelaunchPlan, seed_file: Option<&str>) -> String {
    let q = wt::shell_single_quote;
    let mut cmd = format!(
        "{} {} --session {}",
        crate::cmd::dispatch::agent_launcher_bin(),
        q(&plan.harness),
        q(&plan.new_session),
    );
    if plan.fresh() {
        cmd.push_str(" --fresh --yolo");
        if let Some(f) = seed_file {
            cmd.push_str(&format!(" --seed-file {}", q(f)));
        }
    } else if let Some(old) = &plan.old_session {
        cmd.push_str(&format!(" --resume-session {}", q(old)));
    }
    cmd
}

/// `bn agent relaunch <repo:branch> [--hard] [--harness <h>]`.
pub(crate) fn emit(
    cfg: &Config,
    target: &str,
    hard: bool,
    harness: Option<&str>,
    as_json: bool,
) -> ! {
    let fail = |code: &str, msg: &str| -> ! { fail_json(as_json, "agent_relaunch", code, msg) };

    // Availability-gate an explicit harness before anything else: an org-disabled claude must be a
    // typed error, never a silent no-op or a half-torn-down agent.
    let canonical = match harness.map(str::trim).filter(|s| !s.is_empty()) {
        Some(h) => match bn_core::harness::resolve_relaunch(cfg, h) {
            Ok(c) => Some(c),
            Err(reason) => fail("harness_unavailable", &reason),
        },
        None => None,
    };

    let Some((repo, branch)) = target.trim().split_once(':') else {
        fail(
            "invalid_target",
            &format!("invalid agent target '{target}' (expected repo:branch)"),
        )
    };

    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let live = fleet.agents.iter().find(|a| a.target == target.trim());
    let obs = Observed {
        live,
        recorded: super::agent::recorded_session(cfg, repo, branch),
        // The live row's own worktree wins — it is the directory the transcript was written from —
        // and `agent_worktree` is the fallback for an agent that has already died.
        worktree: live
            .and_then(|a| a.worktree.clone())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| super::agent::agent_worktree(cfg, repo, branch)),
        minted_session: super::dispatch::gen_uuid(),
    };

    let plan = match decide(target, &obs, hard, canonical) {
        Ok(p) => p,
        Err((code, msg)) => fail(code, &msg),
    };

    // --- kill ------------------------------------------------------------------------------
    let exact = super::agent::resolve_tmux_target(cfg, &plan.target);
    let killed = match super::agent::reap_agent_window(&exact) {
        Ok(k) => k,
        Err(e) => fail(
            "kill_failed",
            &format!(
                "{e} — refusing to respawn on top of a live agent; the old one is still running"
            ),
        ),
    };

    // Keep the slot: a relaunch reuses this worktree. Re-stamp so the pool's stale-lease reclaim
    // doesn't see the just-killed holder pid and hand the slot to someone else mid-relaunch.
    let slot_kept = crate::wt_pool::touch_dispatch_lease(cfg, &plan.worktree);

    // --- seed (fresh only) -----------------------------------------------------------------
    let seed_file = plan.fresh().then(|| seed_for_fresh(cfg, &plan)).flatten();
    if plan.fresh() {
        log_fresh_start(cfg, &plan, seed_file.is_some());
    }

    // --- lease + respawn -------------------------------------------------------------------
    let dashed = plan.branch.replace('/', "-");
    let lease_target = format!("{}:{}", plan.repo, plan.window);
    let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &plan.repo,
        branch: &dashed,
        worktree: &plan.worktree,
        session_id: &plan.new_session,
        harness: &plan.harness,
        kind: AgentLeaseKind::Dispatch,
        tmux_target: &lease_target,
        replace: true,
        in_place: false,
        trusted: false,
    }) {
        Ok(r) => r,
        Err(e) => fail(
            "lease_failed",
            &format!("{e} — the old agent was killed; it was not restarted"),
        ),
    };

    let command = launch_command(&plan, seed_file.as_deref());
    let opened = wt::open_worktree_window_cmd_named_quiet(
        &plan.repo,
        &dashed,
        &plan.window,
        &plan.worktree,
        // Mandatory: without it `decide_window` selects the existing window and the launch command
        // is never run, so a "relaunch" would silently do nothing.
        true,
        Some(&command),
        true,
    );
    if !opened {
        let _ = reservation.rollback();
        fail(
            "tmux_failed",
            &format!(
                "could not open a tmux window for {} — the old agent was killed; it was not \
                 restarted. Retry, or run `bn agent launch {}` in {}",
                plan.target,
                plan.harness,
                plan.worktree.display()
            ),
        );
    }
    // Stamp `@bn_session_id` &c so the very next fleet poll joins this window to the agent.
    agent_lease::mark_window(reservation.lease());
    reservation.commit();

    let data = serde_json::json!({
        "target": plan.target,
        "repo": plan.repo,
        "branch": plan.branch,
        "worktree": plan.worktree,
        "harness": plan.harness,
        "mode": plan.mode(),
        "fresh_reason": plan.fresh_reason,
        "killed": killed.as_json(),
        "session": { "old": plan.old_session, "new": plan.new_session },
        "window": plan.window,
        "tmux_target": lease_target,
        "slot_kept": slot_kept,
        "seed_file": seed_file,
    });
    if as_json {
        println!("{}", Envelope::success("agent_relaunch", data).to_json());
    } else {
        println!(
            "bn agent relaunch: {} — {} process(es) killed, {} restarted in {} ({})",
            plan.target,
            killed.processes,
            plan.harness,
            plan.worktree.display(),
            plan.mode(),
        );
    }
    std::process::exit(0);
}

/// Compose the handoff packet a `--hard` relaunch seeds the new session with, and park the prompt
/// in a file.
///
/// Same composition `bn switch` uses (narrative from the outgoing session, packet written from the
/// note), so a hard relaunch and a harness switch hand the incoming agent the same orientation.
/// Best-effort: a note that can't be read still gets a relaunch, just without a seed — refusing to
/// restart an agent because its handoff didn't compose would be the wrong trade.
fn seed_for_fresh(cfg: &Config, plan: &RelaunchPlan) -> Option<String> {
    let ctx = bn_core::resolver::Resolved {
        repo: plan.repo.clone(),
        branch: plan.branch.clone(),
    };
    let narrative = bn_core::agent::switch_narrative(&plan.harness, None, None, None);
    let input = bn_core::handoff::HandoffInput {
        narrative: Some(narrative),
        ..Default::default()
    };
    if let Err(e) = bn_core::handoff::write(cfg, &ctx, &bn_core::util::today(), &input) {
        eprintln!("bn agent relaunch: handoff not written ({e}) — starting without a seed");
        return None;
    }
    let prompt = super::agent::default_resume_prompt(&ctx, Some(&plan.harness));
    wt::prompt_to_file("relaunch", &plan.repo, &plan.branch, &prompt)
        .map(|p| p.to_string_lossy().into_owned())
}


/// Record a hard relaunch in the branch's compaction log as the restore it is: a fresh session,
/// seeded from the handoff (`fresh-primed`) or not (`fresh`). Best-effort.
fn log_fresh_start(cfg: &Config, plan: &RelaunchPlan, seeded: bool) {
    use bn_core::compact::{Entry, Outcome, Phase};
    let ctx = bn_core::resolver::Resolved {
        repo: plan.repo.clone(),
        branch: plan.branch.clone(),
    };
    let entry = Entry {
        ts: bn_core::util::now_epoch_secs(),
        phase: Phase::Restore,
        session: None,
        trigger: Some("relaunch".into()),
        ctx_pct: None,
        recap: false,
        outcome: Some(if seeded {
            Outcome::FreshPrimed
        } else {
            Outcome::Fresh
        }),
    };
    bn_core::util::best_effort(bn_core::compact::append(cfg, &ctx, &entry), "relaunch: log");
}

/// `bn agent relaunch-repo <repo> [--hard] [--harness <h>]` — restart every agent in a repo.
///
/// The repair a wedged repo usually wants. `bn agent kill-repo` stops the work; this stops it and
/// starts it again in the same worktrees, so the answer to "the whole session is stuck" does not
/// have to be "lose it and re-dispatch by hand".
///
/// Re-enters `bn agent relaunch` once per agent rather than looping the kill/lease/respawn sequence
/// inline. That sequence has
/// exactly one correct implementation and a second copy of it would drift; re-entering costs a
/// process per agent and cannot.
///
/// **Sequential on purpose.** Respawning a repo's harnesses at once spikes RAM on the box that is
/// already struggling enough to need this command, and a relaunch that OOMs half the fleet is a
/// worse outcome than one that takes a few seconds longer.
///
/// The unit is a *live fleet agent*, not a tmux window: relaunching needs a `repo:branch` identity
/// to respawn under, which a plain shell window in the session does not have. Those are left alone
/// — `kill-repo` is the command that takes everything down.
///
/// One agent failing does not stop the rest, and the envelope reports per-target outcomes: a
/// partial relaunch is a real state the operator has to be able to see, not something to average
/// into a single boolean.
pub(crate) fn emit_repo(cfg: &Config, repo: &str, hard: bool, harness: Option<&str>, as_json: bool) {
    let repo = repo.trim();
    if repo.is_empty() || repo.contains(':') {
        let msg = format!("invalid repo '{repo}' (expected a bare repo name, not repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_relaunch_repo", "invalid_repo", &msg).to_json()
            );
        } else {
            eprintln!("bn agent relaunch-repo: {msg}");
        }
        return;
    }

    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let targets: Vec<String> = fleet
        .agents
        .iter()
        .filter(|a| a.repo.eq_ignore_ascii_case(repo))
        .map(|a| a.target.clone())
        .collect();

    let Ok(exe) = std::env::current_exe() else {
        let msg = "cannot resolve the bn executable".to_string();
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_relaunch_repo", "no_executable", &msg).to_json()
            );
        } else {
            eprintln!("bn agent relaunch-repo: {msg}");
        }
        return;
    };

    let mut relaunched = Vec::new();
    let mut failed = Vec::new();
    for target in &targets {
        let mut args = vec!["agent".to_string(), "relaunch".to_string(), target.clone()];
        if hard {
            args.push("--hard".to_string());
        }
        if let Some(h) = harness {
            args.extend(["--harness".to_string(), h.to_string()]);
        }
        // `--json` so a child failure is a typed code rather than prose this has to parse.
        args.push("--json".to_string());
        match std::process::Command::new(&exe).args(&args).output() {
            Ok(out) if out.status.success() => relaunched.push(target.clone()),
            Ok(out) => failed.push(serde_json::json!({
                "target": target,
                "error": String::from_utf8_lossy(&out.stderr).trim(),
                "envelope": serde_json::from_slice::<serde_json::Value>(&out.stdout)
                    .unwrap_or(serde_json::Value::Null),
            })),
            Err(e) => failed.push(serde_json::json!({
                "target": target,
                "error": e.to_string(),
            })),
        }
    }

    let ok = failed.is_empty();
    if as_json {
        let data = serde_json::json!({
            "repo": repo,
            "relaunched": relaunched,
            "failed": failed,
        });
        let envelope = bn_core::render::Envelope {
            v: bn_core::render::ENVELOPE_VERSION,
            ok,
            command: "agent_relaunch_repo".to_string(),
            data,
            error: (!ok).then(|| bn_core::render::EnvelopeError {
                code: "relaunch_failed".to_string(),
                message: format!("{} agent(s) did not restart", failed.len()),
            }),
        };
        println!("{}", envelope.to_json());
        return;
    }
    if targets.is_empty() {
        println!("bn agent relaunch-repo: {repo} — no live agents to relaunch");
        return;
    }
    println!(
        "bn agent relaunch-repo: {repo} — {}/{} agent(s) restarted{}",
        relaunched.len(),
        targets.len(),
        if ok { " ✓" } else { "" }
    );
    for failure in &failed {
        eprintln!(
            "bn agent relaunch-repo: {} — {}",
            failure.get("target").and_then(|t| t.as_str()).unwrap_or("?"),
            failure.get("error").and_then(|e| e.as_str()).unwrap_or(""),
        );
    }
}
