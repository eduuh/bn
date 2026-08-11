//! The "starting…" placeholders — dispatched agents that have not come up yet.
//!
//! Split out of `supervise.rs` for the 1000-line ratchet, along the seam #1150 created. It answers
//! something the fleet listing cannot: a `bn dispatch` that has taken a slot but whose harness has
//! not yet written a transcript is real, committed work, and a fleet that showed nothing there left
//! every client believing the machine was idle.
//!
//! The hard half is **not** synthesizing those rows; it is knowing when to stop. A placeholder must
//! vanish the moment the launch it stands for is accounted for, or one dispatch renders as two rows
//! — the same work counted twice. [`Claimed`] is that ledger, and it tracks two independent ways a
//! launch is answered for, because the session id alone catches only one of them.
use super::supervise::LiveAgent;
use super::{AgentHealth, AgentRole, ControlChannel};
use crate::cost::{canonical_session_id, LaunchRecord};
use crate::resolver::Config;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The harness session ids of already-finished (ingested) sessions, read from the machine-local
/// metrics log — the same `agent_session` events [`crate::agent::session::ingest`] appends. Used to
/// dedup pending "starting…" entries so a completed run whose launch record is still recent is not
/// resurrected as pending. Keys are [`canonical_session_id`]-normalized to match the launch side.
pub(super) fn finished_session_ids(cfg: &Config) -> HashSet<String> {
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

/// What the fleet already accounts for, so a synthesized "starting…" entry never duplicates a
/// session that is right there in the list.
///
/// Two ways a launch is already answered for, and the session id catches only the first:
///
/// * **The id came up.** A transcript exists for exactly the id bn pinned, or ingest has already
///   filed that id as finished.
/// * **The checkout is busy.** A session is at work in the worktree the launch targeted, under some
///   *other* id. Harnesses do not always keep the id bn asks for — a `--resume`, a relaunch, a
///   compaction can each write a fresh one — and then the id half never matches, so the placeholder
///   sat beside the very agent it was waiting for until [`DEFAULT_STARTING_MINS`] retired it. One
///   dispatch rendering as two rows is the same work counted twice, which is what the queue page
///   reads as a duplicate.
pub(super) struct Claimed {
    /// Canonical session ids that are live or already ingested.
    sessions: HashSet<String>,
    /// Per `repo/branch`, the newest sign of life from a live session in that checkout — compared
    /// against the launch time rather than merely present, so a *stale* session (a ghost transcript
    /// still readable from the run before) cannot suppress the placeholder for a launch that fired
    /// after it fell silent.
    worktrees: HashMap<String, i64>,
}

impl Claimed {
    pub(super) fn from(live: &[LiveAgent], finished: HashSet<String>) -> Claimed {
        let mut sessions = finished;
        let mut worktrees: HashMap<String, i64> = HashMap::new();
        for a in live {
            sessions.insert(canonical_session_id(&a.session_id));
            let Some(key) = worktree_key(&a.repo, &a.branch) else {
                continue;
            };
            worktrees
                .entry(key)
                .and_modify(|ts| *ts = (*ts).max(a.last_activity_ts))
                .or_insert(a.last_activity_ts);
        }
        Claimed {
            sessions,
            worktrees,
        }
    }

    /// Whether the fleet already shows this launch — either its own id, or a session that has
    /// spoken in the launch's checkout since it fired.
    fn covers(&self, launch: &LaunchRecord) -> bool {
        if self
            .sessions
            .contains(&canonical_session_id(&launch.session_id))
        {
            return true;
        }
        worktree_key(&launch.repo, &launch.branch)
            .and_then(|key| self.worktrees.get(&key))
            .is_some_and(|ts| *ts >= launch.launched_at)
    }
}

/// The one spelling of a checkout both sides compare on: repo and branch in the form the worktree
/// dirs already use, so a launch record's `feature/x` and a live agent's `feature-x` are one place.
///
/// `None` for a branchless record (a launch that names no checkout, an agent whose transcript could
/// not be attributed to one). Those must not fall together into a repo-wide bucket, where any live
/// agent in the repo would answer for a launch that has nothing to do with it.
fn worktree_key(repo: &str, branch: &str) -> Option<String> {
    let branch = super::lease::normalized_branch(branch);
    (!branch.is_empty()).then(|| format!("{}/{branch}", repo.trim().to_lowercase()))
}

/// Synthesize pending "starting…" [`LiveAgent`]s from launch records (#545): a launch within
/// `starting_mins` that the fleet does not already account for (see [`Claimed`]) was dispatched but
/// hasn't come up yet. Pure over its inputs so it is unit-testable without touching the filesystem.
/// The latest launch record per session wins (a re-dispatch re-pins the same id), and entries carry
/// the launch time as their `last_activity_ts` so they sort to the top of the fleet as the newest
/// work.
pub(super) fn pending_agents(
    launches: &[LaunchRecord],
    claimed: &Claimed,
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
        .into_values()
        .filter_map(|l| {
            if claimed.covers(l) {
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
                work_class: Some(l.work_class),
                task_name: (!l.task_name.is_empty()).then(|| l.task_name.clone()),
                turns: 0,
                tool_calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                // Stamped by `stamp_control` once the whole fleet is assembled.
                control: ControlChannel::None,
                task: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::WorkClass;

    /// A minimal live agent — only the fields these tests assert on carry meaning.
    fn blank() -> LiveAgent {
        LiveAgent {
            session_id: String::new(),
            harness: "claude".to_string(),
            repo: "bn".to_string(),
            branch: String::new(),
            target: String::new(),
            tmux_target: None,
            worktree: None,
            last_activity_ts: 0,
            idle_secs: 0,
            health: AgentHealth::Active,
            role: AgentRole::Worker,
            work_class: None,
            task_name: None,
            turns: 1,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            control: ControlChannel::None,
            task: None,
        }
    }

    fn launch(session_id: &str, branch: &str, launched_at: i64) -> LaunchRecord {
        LaunchRecord {
            session_id: session_id.to_string(),
            task_name: String::new(),
            harness: "claude".to_string(),
            repo: "bn".to_string(),
            branch: branch.to_string(),
            worktree: String::new(),
            work_class: WorkClass::Coding,
            launched_at,
            controller_id: None,
        }
    }

    /// A live agent in the launch's checkout, last heard from at `last_activity_ts`.
    fn working(branch: &str, session_id: &str, last_activity_ts: i64) -> LiveAgent {
        LiveAgent {
            session_id: session_id.to_string(),
            branch: branch.to_string(),
            last_activity_ts,
            ..blank()
        }
    }

    #[test]
    fn launch_that_came_up_under_another_id_is_not_also_pending() {
        // The harness wrote its own transcript id, so the id bn pinned never goes live — only the
        // checkout says the launch arrived.
        let claimed = Claimed::from(
            &[working("repo-page", "actually-this-one", 1_100)],
            HashSet::new(),
        );

        let pending = pending_agents(
            &[launch("pinned-id", "repo-page", 1_000)],
            &claimed,
            1_200,
            20,
        );

        assert!(pending.is_empty(), "one dispatch must render as one row");
    }

    #[test]
    fn ghost_from_the_previous_run_does_not_hide_a_fresh_launch() {
        // Same checkout, but its last sign of life predates the dispatch: that transcript is the run
        // before, and the agent this launch is waiting on has genuinely not come up yet.
        let claimed = Claimed::from(
            &[working("repo-page", "yesterdays-run", 900)],
            HashSet::new(),
        );

        let pending = pending_agents(
            &[launch("pinned-id", "repo-page", 1_000)],
            &claimed,
            1_200,
            20,
        );

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].health, AgentHealth::Starting);
    }

    #[test]
    fn checkout_match_ignores_branch_spelling() {
        let claimed = Claimed::from(&[working("feature-x", "live", 1_100)], HashSet::new());

        let pending = pending_agents(
            &[launch("pinned-id", "feature/X", 1_000)],
            &claimed,
            1_200,
            20,
        );

        assert!(
            pending.is_empty(),
            "`feature/X` and `feature-x` are one checkout"
        );
    }

    #[test]
    fn a_branchless_launch_is_not_answered_for_by_the_repo() {
        // Neither side names a checkout, so there is nothing to match on but the repo — which is
        // not the same work, and the placeholder is all this launch has.
        let claimed = Claimed::from(&[working("", "some-other-agent", 1_100)], HashSet::new());

        let pending = pending_agents(&[launch("pinned-id", "", 1_000)], &claimed, 1_200, 20);

        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn finished_and_live_ids_still_win_on_an_untouched_checkout() {
        let claimed = Claimed::from(&[], HashSet::from(["ingested".to_string()]));

        let pending = pending_agents(
            &[
                launch("ingested", "done-branch", 1_000),
                launch("fresh", "new-branch", 1_000),
            ],
            &claimed,
            1_200,
            20,
        );

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id, "fresh");
    }
}
