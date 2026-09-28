//! The fleet as it stood when the machine last went down — reconstructed, not checkpointed.
//!
//! The obvious way to survive a crash is to write a snapshot on a timer. It is also the wrong way
//! here, because the facts are *already* on disk in three stores a power cut cannot corrupt, and a
//! snapshot would add a rotation policy, a durability question, a carrier and a sync story to
//! rederive what can simply be read:
//!
//!   * **Harness transcripts.** [`crate::agent::supervise_from`] takes `now` as a parameter and its
//!     only liveness filter is "was this transcript touched in the last `QUIET_SECS`". Hand it the
//!     epoch this boot started and it answers a different question than usual — *which agents were
//!     live in the half hour before the machine came back* — from files written by the harness, not
//!     by us.
//!   * **`run_attribution` events.** Written synchronously at launch and never garbage-collected
//!     ([`crate::cost::launch_records`]), so they outlive both the agent and the boot.
//!   * **The agent lease registry.** Survives byte-for-byte; it is only ever pruned deliberately.
//!
//! None of the three is sufficient alone. Transcripts know the harness and the real session id but
//! go quiet after 30 minutes. Attribution records never expire but cannot say whether the session
//! was still alive. Leases know the tmux target but record `harness: "unknown"` often enough that
//! trusting them would launch the wrong binary. [`fold`] is the policy that combines them, and it
//! is pure so the precedence rules are testable without a filesystem, a clock or tmux.
//!
//! # Joining on the worktree, never the branch
//!
//! The stores disagree about branch spelling — the pool records `prefix-X-blocking-kill-all` where
//! the agent registry records `prefix-x-blocking-kill-all` ([`crate::agent::normalized_branch`]
//! lowercases and replaces `/`). A join on branch silently splits one agent into two rows. The
//! checkout path is the one key every store agrees on, and the pool slot is derivable from it, so
//! it is never stored either.

use crate::agent::{AgentLeaseRegistry, LiveAgent};
use crate::cost::LaunchRecord;
use std::collections::BTreeMap;

/// Which store supplied a row's session id — the difference between "we watched this agent working"
/// and "something once said it launched".
///
/// Ordered by trustworthiness, which is exactly the merge precedence: a transcript is evidence the
/// session existed and was writing, an attribution record is evidence it was asked to exist, a
/// lease is evidence only that a checkout was claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionSource {
    /// The agent lease registry — weakest: often carries `harness: "unknown"`.
    Lease,
    /// A `run_attribution` event written at launch.
    Attribution,
    /// A harness transcript on disk. The only source that proves the session is resumable.
    Transcript,
}

/// One unit of work that was in flight when the machine went down.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PriorRow {
    pub repo: String,
    pub branch: String,
    /// The checkout, and the join key. Canonicalized by the caller.
    pub worktree: String,
    pub harness: String,
    pub session_id: String,
    pub session_source: SessionSource,
    /// Whether a transcript backed this session id. Only a verified row may be handed to
    /// `--resume-session`: the harness silently degrades an unresumable id to a *fresh, unseeded*
    /// agent, which would make a recovery command report success and hand back a blank slate.
    pub verified: bool,
    /// Epoch secs of the last activity seen for this row, for ordering.
    pub last_activity_ts: i64,
    pub task_name: Option<String>,
}

/// A harness string worth acting on. `bn` writes `"unknown"` into a lease when it cannot tell, and
/// launching the wrong harness is worse than asking — so an unknown never wins a merge and never
/// overwrites a known value.
fn usable_harness(harness: &str) -> bool {
    let h = harness.trim();
    !h.is_empty() && !h.eq_ignore_ascii_case("unknown")
}

/// The join key: a worktree path without a trailing separator, so `/a/b` and `/a/b/` are one row.
fn key(worktree: &str) -> String {
    worktree.trim().trim_end_matches('/').to_string()
}

/// How far back before the crash evidence is still "work that was running".
///
/// `run_attribution` records are never garbage-collected — there are 86 on the machine this was
/// written for, going back weeks — so filtering on `launched_at < boot_at` alone matches *every
/// launch ever made* and offers to restore branches abandoned a fortnight ago. A lower bound is
/// what makes the answer mean "in flight when the lights went out" rather than "once existed".
///
/// A day is chosen because it comfortably spans a working session plus an overnight sleep, and
/// because the alternative — bounding at the *previous* boot — needs a boot history that does not
/// exist yet (that is the increment-2 ring, which will supersede this).
pub const PRIOR_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Combine the three stores into one row per checkout, newest activity first.
///
/// Evidence must fall in `(until - PRIOR_WINDOW_SECS, until)`: strictly before the cutoff, so a row
/// can only describe work that predates the restart, and not so far before it that it is archaeology.
/// Pure — no clock, no filesystem, no tmux.
pub fn fold(
    live: &[LiveAgent],
    launches: &[LaunchRecord],
    leases: &AgentLeaseRegistry,
    until: i64,
) -> Vec<PriorRow> {
    let boot_at = until;
    let since = until.saturating_sub(PRIOR_WINDOW_SECS);
    let mut rows: BTreeMap<String, PriorRow> = BTreeMap::new();

    // Weakest first, so the stronger sources overwrite rather than merge around them.
    for lease in &leases.leases {
        if lease.acquired_at >= boot_at || lease.acquired_at < since {
            continue;
        }
        absorb(
            &mut rows,
            PriorRow {
                repo: lease.key.repo.clone(),
                branch: lease.key.branch.clone(),
                worktree: key(&lease.key.worktree),
                harness: lease.harness.clone(),
                session_id: lease.session_id.clone(),
                session_source: SessionSource::Lease,
                verified: false,
                last_activity_ts: lease.acquired_at,
                task_name: None,
            },
        );
    }

    for launch in launches {
        if launch.launched_at >= boot_at
            || launch.launched_at < since
            || launch.worktree.trim().is_empty()
        {
            continue;
        }
        absorb(
            &mut rows,
            PriorRow {
                repo: launch.repo.clone(),
                branch: launch.branch.clone(),
                worktree: key(&launch.worktree),
                harness: launch.harness.clone(),
                session_id: launch.session_id.clone(),
                session_source: SessionSource::Attribution,
                verified: false,
                last_activity_ts: launch.launched_at,
                task_name: (!launch.task_name.trim().is_empty()).then(|| launch.task_name.clone()),
            },
        );
    }

    for agent in live {
        // An agent with no resolved checkout cannot be restored into one, and the worktree is the
        // join key — so it is dropped rather than guessed at from `<worktree_dir>/<repo>/<branch>`,
        // which is wrong for every pooled slot.
        let Some(worktree) = agent.worktree.as_deref().filter(|w| !w.trim().is_empty()) else {
            continue;
        };
        if agent.last_activity_ts >= boot_at || agent.last_activity_ts < since {
            continue; // This boot's work, or too old to be the crash's.
        }
        absorb(
            &mut rows,
            PriorRow {
                repo: agent.repo.clone(),
                branch: agent.branch.clone(),
                worktree: key(worktree),
                harness: agent.harness.clone(),
                session_id: agent.session_id.clone(),
                session_source: SessionSource::Transcript,
                verified: true,
                last_activity_ts: agent.last_activity_ts,
                task_name: agent.task_name.clone(),
            },
        );
    }

    let mut out: Vec<PriorRow> = rows.into_values().collect();
    // Newest first: what you were touching when the lights went out is what you want back first.
    out.sort_by(|a, b| {
        b.last_activity_ts
            .cmp(&a.last_activity_ts)
            .then_with(|| a.worktree.cmp(&b.worktree))
    });
    out
}

/// The fleet as it stood before this boot, read from the live stores.
///
/// The impure half: it does the filesystem reads and hands them to [`fold`], which owns every
/// decision. `boot_at` is normally [`crate::boot::boot_started_at`]; it stays a parameter so a
/// caller can ask the same question about any instant, and so the whole path is drivable in a test.
///
/// Note what is *not* passed to `supervise_from`: the current time. Handing it the boot epoch is
/// the entire trick — the same scan that normally answers "who is working now" answers "who was
/// working when the machine went down", because its liveness filter is a window around whatever
/// `now` it is given.
pub fn reconstruct(cfg: &crate::resolver::Config, boot_at: i64) -> Vec<PriorRow> {
    let (claude_root, copilot_root) = crate::agent::supervision_roots();
    // Anchor the transcript scan at the moment work *stopped*, not at the moment the machine came
    // back. `supervise_from`'s liveness filter is a 30-minute window around the `now` it is given,
    // and the gap between a crash and the next boot is arbitrary — this machine was down from
    // 23:05 to 10:31 the next morning. Anchored at boot, that window sits eleven hours after the
    // last transcript write and finds nothing at all.
    let anchor =
        newest_transcript_before(&[&claude_root, &copilot_root], boot_at).unwrap_or(boot_at);
    let fleet = crate::agent::supervise_from(
        cfg,
        &claude_root,
        &copilot_root,
        anchor,
        crate::agent::DEFAULT_IDLE_MINS,
        crate::agent::DEFAULT_STUCK_MINS,
    );
    let launches = crate::cost::launch_records();
    let leases = crate::agent::load_agent_leases(&crate::agent::agent_lease_registry_path())
        .unwrap_or_default();
    // `fold`'s cutoff stays the boot, so this boot's own work is still excluded — only the scan
    // window moved.
    fold(&fleet.agents, &launches, &leases, boot_at)
}

/// The newest transcript modification time strictly before `cutoff`, across every harness root —
/// i.e. when work last happened before the machine went down.
///
/// One shallow walk of `<root>/<encoded-cwd>/*.jsonl`; a missing root is simply skipped.
fn newest_transcript_before(roots: &[&std::path::Path], cutoff: i64) -> Option<i64> {
    let mut newest: Option<i64> = None;
    for root in roots {
        let Ok(projects) = std::fs::read_dir(root) else {
            continue;
        };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for file in files.flatten() {
                let Ok(modified) = file.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                let secs = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default();
                if secs < cutoff && newest.is_none_or(|best| secs > best) {
                    newest = Some(secs);
                }
            }
        }
    }
    newest
}

/// Merge one candidate into the accumulator under the precedence rules.
///
/// A stronger source replaces a weaker one wholesale, but never *loses* information doing so: a
/// winning row with an unknown harness keeps the loser's known one, and a winning row with no task
/// name keeps the loser's.
fn absorb(rows: &mut BTreeMap<String, PriorRow>, candidate: PriorRow) {
    match rows.get_mut(&candidate.worktree) {
        None => {
            rows.insert(candidate.worktree.clone(), candidate);
        }
        Some(existing) if candidate.session_source > existing.session_source => {
            let fallback_harness = existing.harness.clone();
            let fallback_task = existing.task_name.clone();
            let previous = std::mem::replace(existing, candidate);
            if !usable_harness(&existing.harness) && usable_harness(&fallback_harness) {
                existing.harness = fallback_harness;
            }
            existing.task_name = existing.task_name.take().or(fallback_task);
            // Keep the most recent evidence of activity, whichever store held it.
            existing.last_activity_ts = existing.last_activity_ts.max(previous.last_activity_ts);
        }
        Some(existing) => {
            if !usable_harness(&existing.harness) && usable_harness(&candidate.harness) {
                existing.harness = candidate.harness;
            }
            if existing.task_name.is_none() {
                existing.task_name = candidate.task_name;
            }
            existing.last_activity_ts = existing.last_activity_ts.max(candidate.last_activity_ts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentLease, AgentLeaseKey, AgentLeaseKind};
    use crate::cost::WorkClass;

    const BOOT: i64 = 1_000_000;

    fn lease(worktree: &str, harness: &str, session: &str, at: i64) -> AgentLease {
        AgentLease {
            key: AgentLeaseKey {
                repo: "bn".into(),
                branch: "feature-x".into(),
                worktree: worktree.into(),
            },
            session_id: session.into(),
            harness: harness.into(),
            kind: AgentLeaseKind::Dispatch,
            tmux_target: "bn:feature-x".into(),
            acquired_at: at,
            trusted: false,
        }
    }

    fn launch(worktree: &str, harness: &str, session: &str, at: i64) -> LaunchRecord {
        LaunchRecord {
            session_id: session.into(),
            task_name: "ship the thing".into(),
            harness: harness.into(),
            repo: "bn".into(),
            branch: "feature/X".into(),
            worktree: worktree.into(),
            work_class: WorkClass::Coding,
            launched_at: at,
            controller_id: None,
        }
    }

    fn agent(worktree: &str, harness: &str, session: &str, at: i64) -> LiveAgent {
        LiveAgent {
            session_id: session.into(),
            harness: harness.into(),
            repo: "bn".into(),
            branch: "feature-x".into(),
            target: "bn:feature-x".into(),
            tmux_target: None,
            worktree: Some(worktree.into()),
            last_activity_ts: at,
            idle_secs: 0,
            health: crate::agent::AgentHealth::Idle,
            role: Default::default(),
            work_class: None,
            task_name: None,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            control: Default::default(),
            task: None,
            context: None,
        }
    }

    fn registry(leases: Vec<AgentLease>) -> AgentLeaseRegistry {
        AgentLeaseRegistry { leases }
    }

    /// `run_attribution` records are never garbage-collected — 86 of them on the machine this was
    /// written for. Without a lower bound, `bn resume --all` offered to restore branches abandoned
    /// weeks earlier, including one named after a date.
    #[test]
    fn evidence_older_than_the_window_is_archaeology_not_prior_work() {
        let stale = BOOT - PRIOR_WINDOW_SECS - 1;
        let rows = fold(
            &[agent("/w/a", "claude", "s1", stale)],
            &[launch("/w/b", "claude", "s2", stale)],
            &registry(vec![lease("/w/c", "claude", "s3", stale)]),
            BOOT,
        );
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn evidence_just_inside_the_window_is_kept() {
        let recent = BOOT - PRIOR_WINDOW_SECS + 1;
        let rows = fold(
            &[],
            &[launch("/w/b", "claude", "s2", recent)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
    }

    #[test]
    fn work_after_the_boot_is_not_prior_work() {
        let rows = fold(
            &[agent("/w/a", "claude", "s1", BOOT + 10)],
            &[launch("/w/b", "claude", "s2", BOOT + 10)],
            &registry(vec![lease("/w/c", "claude", "s3", BOOT + 10)]),
            BOOT,
        );
        assert!(rows.is_empty(), "{rows:?}");
    }

    /// The three stores describe the same checkout; the transcript wins because it is the only one
    /// that proves the session is resumable.
    #[test]
    fn a_transcript_outranks_an_attribution_which_outranks_a_lease() {
        let rows = fold(
            &[agent("/w/a", "claude", "from-transcript", BOOT - 10)],
            &[launch("/w/a", "claude", "from-attribution", BOOT - 20)],
            &registry(vec![lease("/w/a", "claude", "from-lease", BOOT - 30)]),
            BOOT,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "from-transcript");
        assert_eq!(rows[0].session_source, SessionSource::Transcript);
        assert!(rows[0].verified);
    }

    /// Only a transcript-backed row may be handed to `--resume-session`.
    #[test]
    fn a_row_without_a_transcript_is_never_verified() {
        let rows = fold(
            &[],
            &[launch("/w/a", "claude", "s1", BOOT - 20)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].verified);
        assert_eq!(rows[0].session_source, SessionSource::Attribution);
    }

    /// 5 of 6 leases on the machine this was written for carry `harness: "unknown"`. Launching the
    /// wrong binary is worse than not knowing, so a weaker store's *known* harness survives a
    /// stronger store's unknown one.
    #[test]
    fn an_unknown_harness_never_wins_and_never_erases_a_known_one() {
        let rows = fold(
            &[agent("/w/a", "unknown", "s1", BOOT - 10)],
            &[launch("/w/a", "claude", "s1", BOOT - 20)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, "claude");
        assert_eq!(rows[0].session_id, "s1", "transcript still wins the id");
    }

    /// The stores spell branches differently (`feature/X` vs `feature-x`); joining on branch would
    /// split one agent into two rows and offer to restore it twice.
    #[test]
    fn rows_join_on_the_worktree_not_the_branch() {
        let rows = fold(
            &[agent("/w/a", "claude", "s1", BOOT - 10)],
            &[launch("/w/a", "claude", "s1", BOOT - 20)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
    }

    #[test]
    fn a_trailing_slash_is_the_same_checkout() {
        let rows = fold(
            &[agent("/w/a/", "claude", "s1", BOOT - 10)],
            &[launch("/w/a", "claude", "s1", BOOT - 20)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
    }

    /// An agent whose checkout was never resolved cannot be restored into one — and guessing
    /// `<worktree_dir>/<repo>/<branch>` is wrong for every pooled slot.
    #[test]
    fn an_agent_with_no_worktree_is_dropped() {
        let mut a = agent("/w/a", "claude", "s1", BOOT - 10);
        a.worktree = None;
        assert!(fold(&[a], &[], &registry(vec![]), BOOT).is_empty());
    }

    #[test]
    fn distinct_checkouts_stay_distinct_and_sort_newest_first() {
        let rows = fold(
            &[
                agent("/w/old", "claude", "s1", BOOT - 900),
                agent("/w/new", "claude", "s2", BOOT - 5),
            ],
            &[],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].worktree, "/w/new");
        assert_eq!(rows[1].worktree, "/w/old");
    }

    /// A lease-only row is still worth offering — it is how an agent that went quiet more than
    /// `QUIET_SECS` before the crash stays visible.
    #[test]
    fn a_lease_only_checkout_still_produces_a_row() {
        let rows = fold(
            &[],
            &[],
            &registry(vec![lease("/w/a", "copilot", "s9", BOOT - 60)]),
            BOOT,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_source, SessionSource::Lease);
        assert_eq!(rows[0].harness, "copilot");
    }

    /// The attribution record carries the readable task name; the transcript usually does not.
    /// Winning the merge must not throw it away.
    #[test]
    fn a_stronger_source_inherits_the_task_name_it_lacks() {
        let rows = fold(
            &[agent("/w/a", "claude", "s1", BOOT - 10)],
            &[launch("/w/a", "claude", "s1", BOOT - 20)],
            &registry(vec![]),
            BOOT,
        );
        assert_eq!(rows[0].task_name.as_deref(), Some("ship the thing"));
    }
}
