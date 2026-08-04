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
        None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn live(harness: &str, session: &str) -> LiveAgent {
        LiveAgent {
            session_id: session.to_string(),
            harness: harness.to_string(),
            repo: "bn".to_string(),
            branch: "feat".to_string(),
            target: "bn:feat".to_string(),
            tmux_target: None,
            worktree: Some("/wt".to_string()),
            last_activity_ts: 0,
            idle_secs: 0,
            health: bn_core::agent::AgentHealth::Active,
            role: bn_core::agent::AgentRole::Worker,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            control: bn_core::agent::ControlChannel::Resume,
            task: None,
        }
    }

    fn obs<'a>(live: Option<&'a LiveAgent>, wt: Option<&str>) -> Observed<'a> {
        Observed {
            live,
            recorded: None,
            worktree: wt.map(PathBuf::from),
            minted_session: "NEW".to_string(),
        }
    }

    #[test]
    fn a_plain_relaunch_resumes_the_same_session() {
        let a = live("claude", "S1");
        let plan = decide("bn:feat", &obs(Some(&a), Some("/wt")), false, None).expect("plan");
        assert_eq!(plan.fresh_reason, None);
        assert_eq!(plan.mode(), "resume");
        // The whole point: the respawned agent picks up its own transcript, so the id is unchanged.
        assert_eq!(plan.new_session, "S1");
        assert_eq!(plan.old_session.as_deref(), Some("S1"));
        assert_eq!(plan.harness, "claude");
        // Canonical window name, so the agent stays addressable at repo:branch after a relaunch.
        assert_eq!(plan.window, "feat");
    }

    #[test]
    fn hard_and_harness_change_both_mint_a_fresh_session() {
        let a = live("claude", "S1");
        let hard = decide("bn:feat", &obs(Some(&a), Some("/wt")), true, None).expect("plan");
        assert_eq!(hard.fresh_reason, Some(FreshReason::Hard));
        assert_eq!(hard.new_session, "NEW");

        // A Claude transcript is not resumable by Copilot, so the switch forces a fresh session
        // even without --hard.
        let swap =
            decide("bn:feat", &obs(Some(&a), Some("/wt")), false, Some("copilot")).expect("plan");
        assert_eq!(swap.fresh_reason, Some(FreshReason::HarnessChange));
        assert_eq!(swap.harness, "copilot");
        assert_eq!(swap.new_session, "NEW");

        // --hard names itself even when it coincides with a harness change: it's the reason the
        // operator will recognize.
        let both =
            decide("bn:feat", &obs(Some(&a), Some("/wt")), true, Some("copilot")).expect("plan");
        assert_eq!(both.fresh_reason, Some(FreshReason::Hard));
    }

    #[test]
    fn requesting_the_same_harness_still_resumes() {
        let a = live("claude", "S1");
        // Case-insensitively — "Claude" is the same harness, not a switch.
        let plan =
            decide("bn:feat", &obs(Some(&a), Some("/wt")), false, Some("Claude")).expect("plan");
        assert_eq!(plan.fresh_reason, None);
        assert_eq!(plan.new_session, "S1");
    }

    #[test]
    fn a_dead_agent_falls_back_to_the_notes_recorded_session() {
        let mut o = obs(None, Some("/wt"));
        o.recorded = Some(("copilot".to_string(), "S9".to_string()));
        let plan = decide("bn:feat", &o, false, None).expect("plan");
        assert_eq!(plan.harness, "copilot");
        assert_eq!(plan.new_session, "S9");
        assert_eq!(plan.fresh_reason, None);
    }

    #[test]
    fn nothing_recorded_means_a_fresh_session_not_a_failure() {
        let mut o = obs(None, Some("/wt"));
        o.recorded = Some(("claude".to_string(), String::new()));
        let plan = decide("bn:feat", &o, false, None).expect("plan");
        assert_eq!(plan.fresh_reason, Some(FreshReason::NoSession));
        assert_eq!(plan.new_session, "NEW");
    }

    #[test]
    fn an_unlocatable_worktree_fails_before_anything_is_killed() {
        let a = live("claude", "S1");
        let err = decide("bn:feat", &obs(Some(&a), None), false, None).expect_err("no worktree");
        assert_eq!(err.0, "no_worktree");
        // The message must not repeat the conventional-path guess that caused this bug.
        assert!(err.1.contains("git lists no worktree"), "{}", err.1);
    }

    #[test]
    fn a_malformed_target_is_refused() {
        for bad in ["", "bn", "bn:", ":feat"] {
            let err = decide(bad, &obs(None, Some("/wt")), false, None).expect_err("bad target");
            assert_eq!(err.0, "invalid_target", "{bad}");
        }
    }

    #[test]
    fn an_unknown_harness_is_a_typed_failure() {
        let err = decide("bn:feat", &obs(None, Some("/wt")), false, None).expect_err("no harness");
        assert_eq!(err.0, "unknown_harness");
    }

    #[test]
    fn launch_command_resumes_or_seeds() {
        let a = live("claude", "S1");
        let resume = decide("bn:feat", &obs(Some(&a), Some("/wt")), false, None).unwrap();
        let cmd = launch_command(&resume, None);
        assert!(cmd.contains("--resume-session 'S1'"), "{cmd}");
        assert!(cmd.contains("--session 'S1'"), "{cmd}");
        assert!(!cmd.contains("--fresh"), "{cmd}");

        let hard = decide("bn:feat", &obs(Some(&a), Some("/wt")), true, None).unwrap();
        let cmd = launch_command(&hard, Some("/tmp/seed.md"));
        assert!(cmd.contains("--fresh"), "{cmd}");
        assert!(cmd.contains("--seed-file '/tmp/seed.md'"), "{cmd}");
        // A fresh session must never carry the old transcript in.
        assert!(!cmd.contains("--resume-session"), "{cmd}");
    }

    #[test]
    fn launch_command_quotes_every_interpolated_element() {
        let a = live("claude", "S1");
        let mut plan = decide("bn:feat", &obs(Some(&a), Some("/wt")), false, None).unwrap();
        plan.new_session = "it's odd".to_string();
        plan.old_session = Some("it's odd".to_string());
        let cmd = launch_command(&plan, None);
        // Single-quoted with the apostrophe escaped, so the shell can't split the command.
        assert!(cmd.contains(r"'it'\''s odd'"), "{cmd}");
    }
}
