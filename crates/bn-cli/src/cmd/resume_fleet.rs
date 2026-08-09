//! `bn resume --all` — bring back the work that was running when the machine last went down.
//!
//! The reconstruction lives in [`bn_core::prior_boot`]; this is the surface over it. Three
//! properties shape the code more than the feature does:
//!
//! **Plan everything, then act.** [`plan_restore`] resolves worktree, harness, session and tmux
//! target for *every* row before a single row is touched — the `close_plan` pattern from
//! `crates/bn-cli/src/wt/close.rs`. `--dry-run` returns that value and stops; the interactive path
//! consumes the same value, so what you were shown and what runs cannot disagree.
//!
//! **Adopt, never open a second window.** After a reboot tmux-continuum has already restored the
//! bn session's windows in the right worktrees, with scrollback — only the harness is missing,
//! because `claude` is not in resurrect's whitelist. So a row whose worktree already has a live
//! pane is *adopted*: the launch command is sent into that pane. Opening a fresh window would give
//! two per branch, and tidying that up runs straight into the rule that a tmux session is never
//! killed. Here nothing is ever closed, so the rule holds by construction rather than by care.
//!
//! **Never claim a resume you cannot perform.** `bn agent launch` silently degrades an unfindable
//! session id to a *fresh, unseeded* agent. A recovery command that prints "resumed" and hands back
//! a blank slate is worse than one that admits it is starting fresh, so `--resume-session` is
//! emitted only for a transcript-verified row whose checkout still exists, and every row prints
//! which of the two it got.

use crate::*;
use bn_core::prior_boot::{PriorRow, SessionSource};
use bn_core::render::Envelope;
use bn_core::resolver::Config;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

/// How a row will be put back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Placement {
    /// A live pane is already sitting in this checkout (usually continuum's) — send the launch
    /// command into it and keep the window.
    Adopt { target: String },
    /// Nothing is in the checkout; open a window the ordinary way.
    Create,
}

/// Why a row cannot be restored. Kept beside the plan rather than silently filtered, because a row
/// that vanishes without explanation reads as data loss.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Rejected {
    pub branch: String,
    pub worktree: String,
    pub reason: String,
}

/// One restorable unit, fully resolved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RestoreRow {
    /// Pool slot when the checkout is a pooled one — derived from the path, never stored.
    pub slot: Option<u32>,
    pub repo: String,
    pub branch: String,
    pub worktree: String,
    pub harness: String,
    pub session_id: String,
    pub session_source: SessionSource,
    /// Whether this row will resume its transcript (`true`) or start fresh (`false`).
    pub resuming: bool,
    pub placement: Placement,
    pub task_name: Option<String>,
}

impl RestoreRow {
    /// The one-word mode shown in the table and in `--json`.
    fn mode(&self) -> &'static str {
        if self.resuming {
            "resume"
        } else {
            "fresh"
        }
    }
}

/// The facts one row needs, gathered by the caller so the policy stays pure and testable.
pub(crate) struct RowFacts {
    pub worktree_exists: bool,
    pub pane_target: Option<String>,
    pub slot: Option<u32>,
    /// The branch the checkout is on *now*, not when the crash happened. `None` when it could not
    /// be read (detached HEAD, not a repo) — which is treated as "cannot vouch for it".
    pub current_branch: Option<String>,
}

/// Turn reconstructed rows into a restore plan. Pure over `facts`; the caller does the tmux and
/// filesystem probing.
///
/// `facts` is parallel to `rows`. A row with no facts entry is treated as a missing checkout, which
/// is the conservative reading.
pub(crate) fn plan_restore(
    rows: &[PriorRow],
    facts: &[RowFacts],
    slots: &[u32],
) -> (Vec<RestoreRow>, Vec<Rejected>) {
    let mut plan = Vec::new();
    let mut rejected = Vec::new();
    for (row, fact) in rows.iter().zip(facts.iter()) {
        if !slots.is_empty() && !fact.slot.is_some_and(|s| slots.contains(&s)) {
            continue; // Explicitly narrowed by --slot; not a rejection.
        }
        if !fact.worktree_exists {
            rejected.push(Rejected {
                branch: row.branch.clone(),
                worktree: row.worktree.clone(),
                reason: "checkout is gone".into(),
            });
            continue;
        }
        if !matches!(row.harness.as_str(), "claude" | "copilot") {
            rejected.push(Rejected {
                branch: row.branch.clone(),
                worktree: row.worktree.clone(),
                reason: format!("no resumable harness (recorded: {})", row.harness),
            });
            continue;
        }
        if row.session_id.trim().is_empty() {
            rejected.push(Rejected {
                branch: row.branch.clone(),
                worktree: row.worktree.clone(),
                reason: "no session id recorded".into(),
            });
            continue;
        }
        // The checkout has moved on. Pool slots are recycled, so a branch that was in `_pool/2`
        // before the crash is very often not what is in `_pool/2` now — restoring anyway would
        // adopt a pane someone is currently working in and lease it under the wrong branch, and
        // the transcript would resume against code that is no longer there. Whoever holds the
        // checkout now wins; the row is reported, never silently relocated to a free slot.
        if let Some(current) = fact
            .current_branch
            .as_deref()
            .filter(|current| !same_branch(current, &row.branch))
        {
            rejected.push(Rejected {
                branch: row.branch.clone(),
                worktree: row.worktree.clone(),
                reason: format!("checkout now holds {current}"),
            });
            continue;
        }
        // A transcript is keyed by encoded cwd, so a session is only resumable from the very
        // checkout that produced it — `verified` already encodes "a transcript backed this id".
        let resuming = row.verified;
        plan.push(RestoreRow {
            slot: fact.slot,
            repo: row.repo.clone(),
            branch: row.branch.clone(),
            worktree: row.worktree.clone(),
            harness: row.harness.clone(),
            session_id: row.session_id.clone(),
            session_source: row.session_source,
            resuming,
            placement: match fact.pane_target.clone() {
                Some(target) => Placement::Adopt { target },
                None => Placement::Create,
            },
            task_name: row.task_name.clone(),
        });
    }
    (plan, rejected)
}

/// Whether two branch spellings name the same branch.
///
/// The stores disagree — a lease records `feature-x` where git reports `feature/X` — so a literal
/// comparison would reject every row whose branch contains a slash or a capital.
fn same_branch(a: &str, b: &str) -> bool {
    bn_core::agent::normalized_branch(a) == bn_core::agent::normalized_branch(b)
}

/// Gather the per-row facts `plan_restore` needs.
fn gather_facts(rows: &[PriorRow]) -> Vec<RowFacts> {
    rows.iter()
        .map(|row| {
            let path = PathBuf::from(&row.worktree);
            let exists = path.is_dir();
            RowFacts {
                worktree_exists: exists,
                // Only probe tmux for checkouts that still exist.
                pane_target: exists
                    .then(|| agent_lease::live_pane_target_in(&path))
                    .flatten(),
                slot: pool_slot_of(&path),
                current_branch: exists
                    .then(|| {
                        crate::git::git_capture(
                            &["rev-parse", "--abbrev-ref", "HEAD"],
                            Some(&path),
                            true,
                        )
                    })
                    .flatten()
                    .filter(|b| b != "HEAD"), // detached — nothing to compare against.
            }
        })
        .collect()
}

/// The pool slot number for a checkout, when it is a pooled one (`…/<repo>/_pool/<n>`).
///
/// Derived from the path rather than read from `leases.json`, so it stays correct for a slot whose
/// lease was already reclaimed — which, after a reboot, is most of them. Duplicated in miniature
/// rather than borrowed from `wt_pool::slot_parts` because that file sits at its size ratchet and
/// may not gain the `pub(crate)` line.
fn pool_slot_of(worktree: &std::path::Path) -> Option<u32> {
    let n: u32 = worktree.file_name()?.to_string_lossy().parse().ok()?;
    (worktree.parent()?.file_name()? == "_pool").then_some(n)
}

/// Options parsed from the CLI.
pub(crate) struct ResumeFleetOpts {
    pub dry_run: bool,
    pub yes: bool,
    pub slots: Vec<u32>,
}

const TAG: &str = "resume_fleet";

pub(crate) fn emit_resume_fleet(cfg: &Config, opts: &ResumeFleetOpts, json: bool) {
    let Some(boot_at) = bn_core::boot::boot_started_at() else {
        fail_json(
            json,
            TAG,
            "unknown_boot",
            "cannot tell when this machine booted, so prior-boot work cannot be identified",
        );
    };
    let rows = bn_core::prior_boot::reconstruct(cfg, boot_at);
    let facts = gather_facts(&rows);
    let (plan, rejected) = plan_restore(&rows, &facts, &opts.slots);

    if plan.is_empty() {
        if json {
            println!(
                "{}",
                Envelope::success(
                    TAG,
                    serde_json::json!({
                        "boot_at": boot_at,
                        "restored": [],
                        "planned": [],
                        "rejected": rejected,
                    }),
                )
                .to_json()
            );
            return;
        }
        println!("nothing to recover from before this boot.");
        print_rejected(&rejected);
        return;
    }

    if json && opts.dry_run {
        println!(
            "{}",
            Envelope::success(
                TAG,
                serde_json::json!({
                    "boot_at": boot_at,
                    "planned": plan,
                    "rejected": rejected,
                    "restored": [],
                }),
            )
            .to_json()
        );
        return;
    }

    if !json {
        print_plan(boot_at, &plan);
        print_rejected(&rejected);
    }
    if opts.dry_run {
        if !json {
            println!("\n--dry-run: nothing was changed.");
        }
        return;
    }

    let chosen = match choose(&plan, opts.yes, json) {
        Some(chosen) if !chosen.is_empty() => chosen,
        Some(_) => {
            if !json {
                println!("nothing selected.");
            }
            return;
        }
        None => {
            fail_json(
                json,
                TAG,
                "declined",
                "refusing to restore without confirmation; pass --yes",
            );
        }
    };

    // Sequential and one reservation at a time, deliberately: a reservation holds a lock on the
    // agent-lease registry for its lifetime and its rollback writes back a whole-registry snapshot,
    // so two open at once deadlock and a batch rollback would undo rows that already succeeded.
    let mut restored = Vec::new();
    let mut failed = Vec::new();
    for row in plan.iter().filter(|r| chosen.contains(&r.worktree)) {
        match restore_one(row) {
            Ok(()) => {
                if !json {
                    println!("  ✓ {} ({})", row.branch, row.mode());
                }
                restored.push(row.clone());
            }
            Err(error) => {
                if !json {
                    eprintln!("  ✗ {}: {error}", row.branch);
                }
                failed.push(serde_json::json!({ "branch": row.branch, "error": error }));
            }
        }
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                TAG,
                serde_json::json!({
                    "boot_at": boot_at,
                    "planned": plan,
                    "restored": restored,
                    "failed": failed,
                    "rejected": rejected,
                }),
            )
            .to_json()
        );
    }
}

/// Put one row back. Adopt sends into the existing pane; create opens a window.
fn restore_one(row: &RestoreRow) -> Result<(), String> {
    let worktree = PathBuf::from(&row.worktree);
    let command = launch_command(row);
    match &row.placement {
        Placement::Adopt { target } => adopt_into_pane(row, target, &command),
        Placement::Create => crate::cmd::sessions::launch_resume_window(
            &row.repo,
            &row.branch,
            row.task_name.as_deref().unwrap_or(&row.branch),
            &worktree,
            &command,
            &row.session_id,
            &row.harness,
        ),
    }
}

/// The `bn agent launch` invocation for a row. `--resume-session` only when the row is verified;
/// otherwise a plain launch, which the table has already labelled `fresh`.
fn launch_command(row: &RestoreRow) -> String {
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let mut command = format!(
        "{} agent launch {} --session {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(&row.harness),
        wt::shell_single_quote(&row.session_id),
    );
    if row.resuming {
        command.push_str(&format!(
            " --resume-session {}",
            wt::shell_single_quote(&row.session_id)
        ));
    }
    command
}

/// Send the launch into a pane that already exists, then take the lease for it.
///
/// `in_place: true` marks this as a reservation for a window that is already there, so the
/// dead-target cleanup branch is never reached — there is no stale window to remove, and reaching
/// for one is how a restore would end up closing something.
fn adopt_into_pane(row: &RestoreRow, target: &str, command: &str) -> Result<(), String> {
    let reservation = agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &row.repo,
        branch: &row.branch,
        worktree: &PathBuf::from(&row.worktree),
        session_id: &row.session_id,
        harness: &row.harness,
        kind: bn_core::agent::AgentLeaseKind::Dispatch,
        tmux_target: target,
        replace: false,
        in_place: true,
    })?;
    if !crate::proc::tmux_ok(&["send-keys", "-t", target, command, "Enter"]) {
        let _ = reservation.rollback();
        return Err(format!("could not send the launch into {target}"));
    }
    agent_lease::mark_window(reservation.lease());
    reservation.commit();
    Ok(())
}

fn print_plan(boot_at: i64, plan: &[RestoreRow]) {
    println!(
        "work running before this boot (machine came back {}):\n",
        bn_core::util::fmt_local_short(boot_at)
    );
    println!(
        "  {:>3}  {:<10} {:<32} {:<8} {:<7} window",
        "#", "slot", "branch", "harness", "mode"
    );
    for (i, row) in plan.iter().enumerate() {
        let window = match &row.placement {
            Placement::Adopt { target } => format!("adopt {target}"),
            Placement::Create => "new window".to_string(),
        };
        // Qualified by repo: pool slots are per-repo, so a bare `[2]` names two different
        // checkouts when more than one repo is in play.
        let slot = row
            .slot
            .map(|s| format!("{}/{s}", row.repo))
            .unwrap_or_default();
        println!(
            "  {:>3}  {:<10} {:<32} {:<8} {:<7} {window}",
            i + 1,
            truncate(&slot, 10),
            truncate(&row.branch, 32),
            row.harness,
            row.mode(),
        );
    }
}

fn print_rejected(rejected: &[Rejected]) {
    if rejected.is_empty() {
        return;
    }
    println!("\nnot restorable:");
    for r in rejected {
        println!("  · {} — {}", r.branch, r.reason);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// One prompt, one answer, over the whole plan.
///
/// A per-row `[y/N]` sequence is the wrong shape after a crash: you cannot see what is coming
/// before answering, and there may be eight of them. Returns the chosen worktrees, or `None` when
/// the operator declined (or could not be asked).
fn choose(plan: &[RestoreRow], assume_yes: bool, json: bool) -> Option<Vec<String>> {
    if assume_yes {
        return Some(plan.iter().map(|r| r.worktree.clone()).collect());
    }
    if json || !std::io::stdin().is_terminal() {
        return None;
    }
    print!("\nrestore? [a]ll / [n]one / numbers (e.g. 1,3): ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return None;
    }
    let answer = buf.trim();
    match answer {
        "a" | "A" | "all" | "y" | "Y" | "yes" => {
            Some(plan.iter().map(|r| r.worktree.clone()).collect())
        }
        "" | "n" | "N" | "none" | "no" => Some(Vec::new()),
        _ => Some(
            answer
                .split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .filter_map(|n| plan.get(n.checked_sub(1)?))
                .map(|r| r.worktree.clone())
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(branch: &str, harness: &str, session: &str, verified: bool) -> PriorRow {
        PriorRow {
            repo: "bn".into(),
            branch: branch.into(),
            worktree: format!("/w/{branch}"),
            harness: harness.into(),
            session_id: session.into(),
            session_source: SessionSource::Transcript,
            verified,
            last_activity_ts: 10,
            task_name: None,
        }
    }

    fn facts(exists: bool, pane: Option<&str>, slot: Option<u32>) -> RowFacts {
        RowFacts {
            worktree_exists: exists,
            pane_target: pane.map(str::to_string),
            slot,
            current_branch: None,
        }
    }

    /// Facts for a checkout sitting on `branch` right now.
    fn facts_on(branch: &str) -> RowFacts {
        RowFacts {
            worktree_exists: true,
            pane_target: None,
            slot: Some(2),
            current_branch: Some(branch.to_string()),
        }
    }

    #[test]
    fn a_live_pane_makes_the_row_adopt_rather_than_create() {
        let (plan, _) = plan_restore(
            &[row("a", "claude", "s1", true)],
            &[facts(true, Some("bn:2"), Some(2))],
            &[],
        );
        assert_eq!(
            plan[0].placement,
            Placement::Adopt {
                target: "bn:2".into()
            }
        );
    }

    #[test]
    fn no_pane_means_a_new_window() {
        let (plan, _) = plan_restore(
            &[row("a", "claude", "s1", true)],
            &[facts(true, None, None)],
            &[],
        );
        assert_eq!(plan[0].placement, Placement::Create);
    }

    /// The safety rule: an unverified session id would silently become a fresh *unseeded* agent, so
    /// it must never be handed to `--resume-session`.
    #[test]
    fn only_a_verified_row_resumes_its_transcript() {
        let (plan, _) = plan_restore(
            &[row("a", "claude", "s1", false)],
            &[facts(true, None, None)],
            &[],
        );
        assert!(!plan[0].resuming);
        assert_eq!(plan[0].mode(), "fresh");
        assert!(!launch_command(&plan[0]).contains("--resume-session"));
    }

    #[test]
    fn a_verified_row_carries_resume_session() {
        let (plan, _) = plan_restore(
            &[row("a", "claude", "s1", true)],
            &[facts(true, None, None)],
            &[],
        );
        assert!(launch_command(&plan[0]).contains("--resume-session"));
    }

    /// A vanished checkout is reported, never silently dropped — a row disappearing without a
    /// reason reads as lost work.
    #[test]
    fn a_missing_checkout_is_rejected_with_a_reason() {
        let (plan, rejected) = plan_restore(
            &[row("a", "claude", "s1", true)],
            &[facts(false, None, None)],
            &[],
        );
        assert!(plan.is_empty());
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].reason.contains("gone"));
    }

    /// The bug a real crash drill surfaced: pool slots get recycled, so `_pool/2` held one branch
    /// before the crash and a different one now. Restoring anyway would adopt the pane someone is
    /// working in and lease it under the wrong branch.
    #[test]
    fn a_checkout_that_moved_on_is_rejected_not_restored() {
        let (plan, rejected) = plan_restore(
            &[row("a", "claude", "s1", true)],
            &[facts_on("something-else-entirely")],
            &[],
        );
        assert!(plan.is_empty(), "{plan:?}");
        assert_eq!(rejected.len(), 1);
        assert!(
            rejected[0].reason.contains("something-else-entirely"),
            "{:?}",
            rejected[0]
        );
    }

    /// Branch spellings differ across stores (`feature/X` in git, `feature-x` in a lease); the
    /// guard must not reject a checkout that is in fact on the right branch.
    #[test]
    fn a_differently_spelled_branch_is_still_the_same_checkout() {
        let mut r = row("feature-x", "claude", "s1", true);
        r.worktree = "/w/feature-x".into();
        let (plan, rejected) = plan_restore(&[r], &[facts_on("feature/X")], &[]);
        assert_eq!(plan.len(), 1, "{rejected:?}");
    }

    #[test]
    fn an_unresumable_harness_is_rejected() {
        let (plan, rejected) = plan_restore(
            &[row("a", "unknown", "s1", true)],
            &[facts(true, None, None)],
            &[],
        );
        assert!(plan.is_empty());
        assert!(rejected[0].reason.contains("no resumable harness"));
    }

    /// `--slot` narrows rather than rejects: an unlisted slot is not a failure to report.
    #[test]
    fn slot_filter_narrows_without_rejecting() {
        let (plan, rejected) = plan_restore(
            &[row("a", "claude", "s1", true), row("b", "claude", "s2", true)],
            &[facts(true, None, Some(2)), facts(true, None, Some(5))],
            &[5],
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].branch, "b");
        assert!(rejected.is_empty());
    }

    #[test]
    fn choosing_numbers_selects_those_rows() {
        let (plan, _) = plan_restore(
            &[
                row("a", "claude", "s1", true),
                row("b", "claude", "s2", true),
                row("c", "claude", "s3", true),
            ],
            &[
                facts(true, None, None),
                facts(true, None, None),
                facts(true, None, None),
            ],
            &[],
        );
        assert_eq!(choose(&plan, true, false), Some(vec![
            "/w/a".to_string(),
            "/w/b".to_string(),
            "/w/c".to_string()
        ]));
    }
}
